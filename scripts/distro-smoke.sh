#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="${WAVELINUX_SMOKE_REPO:-DuskyProjects/WaveLinux}"
PRODUCT_NAME="${WAVELINUX_SMOKE_PRODUCT_NAME:-WaveLinux6}"
DISTRO="${WAVELINUX_SMOKE_DISTRO:-all}"
TARGET="${WAVELINUX_SMOKE_TARGET:-appimage}"
RELEASE_TAG="${WAVELINUX_SMOKE_RELEASE_TAG:-latest}"
CONTAINER_ENGINE="${WAVELINUX_CONTAINER_ENGINE:-}"
LOCAL_ASSET_DIR="${WAVELINUX_SMOKE_LOCAL_ASSET_DIR:-}"
INSIDE=0

usage() {
  cat <<'HELP'
Run WaveLinux release smoke tests in clean Linux containers.

Usage:
  bash scripts/distro-smoke.sh [--distro NAME|--all] [--target appimage|native|source-helper] [--release-tag vX.Y.Z]

Distros:
  debian13, ubuntu2404, fedora, arch, cachyos

Targets:
  appimage       Download the release AppImage, run its dependency installer, then check runtime deps.
  native         Install the release deb/rpm package and check runtime deps. Arch is skipped.
  source-helper  Run scripts/check-dependencies.sh --install --strict-runtime from the checkout.

  Environment:
  WAVELINUX_CONTAINER_ENGINE=docker|podman
  WAVELINUX_SMOKE_PRODUCT_NAME=WaveLinux6
  WAVELINUX_SMOKE_ARTIFACT_VERSION=6.0.0
  WAVELINUX_SMOKE_PRIVILEGED=0  Disable privileged container mode.
HELP
}

while (($#)); do
  case "$1" in
    --all)
      DISTRO=all
      ;;
    --distro)
      DISTRO="${2:?missing distro}"
      shift
      ;;
    --target)
      TARGET="${2:?missing target}"
      shift
      ;;
    --release-tag)
      RELEASE_TAG="${2:?missing release tag}"
      shift
      ;;
    --inside)
      INSIDE=1
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

image_for_distro() {
  case "$1" in
    debian13) echo "debian:trixie-slim" ;;
    ubuntu2404) echo "ubuntu:24.04" ;;
    fedora) echo "fedora:latest" ;;
    arch) echo "archlinux:latest" ;;
    cachyos) echo "cachyos/cachyos:latest" ;;
    *)
      echo "Unknown distro: $1" >&2
      return 1
      ;;
  esac
}

manager_id() {
  if command -v apt-get >/dev/null 2>&1; then
    echo apt
  elif command -v dnf >/dev/null 2>&1; then
    echo dnf
  elif command -v pacman >/dev/null 2>&1; then
    echo pacman
  else
    echo unknown
  fi
}

bootstrap_container_tools() {
  local manager
  manager="$(manager_id)"
  case "$manager" in
    apt)
      export DEBIAN_FRONTEND=noninteractive
      apt-get update
      apt-get install -y --no-install-recommends bash ca-certificates curl coreutils file
      ;;
    dnf)
      dnf install -y --setopt=install_weak_deps=False bash ca-certificates curl coreutils file findutils
      ;;
    pacman)
      # Recent pacman releases sandbox package hooks with network namespaces.
      # Rootless Podman intentionally cannot create those namespaces, so let the
      # container boundary provide isolation for this disposable smoke system.
      if [[ "$WAVELINUX_SMOKE_DISTRO" == "cachyos" ]] \
        && ! grep -Eq '^[[:space:]]*DisableSandboxNetwork([[:space:]]|$)' /etc/pacman.conf; then
        sed -i '/^\[options\]$/a DisableSandboxNetwork' /etc/pacman.conf
      fi
      pacman -Sy --needed --noconfirm archlinux-keyring
      pacman -Syu --needed --noconfirm bash ca-certificates curl coreutils file
      ;;
    *)
      echo "No supported package manager in smoke container" >&2
      exit 1
      ;;
  esac
}

resolve_latest_tag() {
  if [[ "$RELEASE_TAG" != "latest" ]]; then
    echo "$RELEASE_TAG"
    return 0
  fi
  local url
  url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")"
  echo "${url##*/}"
}

download_release_asset() {
  local tag="$1"
  local asset="$2"
  local output="$3"
  local url="https://github.com/$REPO/releases/download/$tag/$asset"
  echo "Downloading $url"
  curl -fL --retry 3 --retry-delay 2 -o "$output" "$url"
}

obtain_release_asset() {
  local tag="$1"
  local asset="$2"
  local output="$3"
  if [[ -n "$LOCAL_ASSET_DIR" ]]; then
    local source="$LOCAL_ASSET_DIR/$asset"
    if [[ ! -f "$source" ]]; then
      local bundle_kind
      for bundle_kind in appimage deb rpm; do
        if [[ -f "$LOCAL_ASSET_DIR/$bundle_kind/$asset" ]]; then
          source="$LOCAL_ASSET_DIR/$bundle_kind/$asset"
          break
        fi
      done
    fi
    if [[ ! -f "$source" ]]; then
      echo "Local release asset is missing: $source" >&2
      return 1
    fi
    echo "Using local release asset $source"
    cp "$source" "$output"
    return 0
  fi
  download_release_asset "$tag" "$asset" "$output"
}

runtime_check_accepts_headless_container() {
  local output="$1"
  grep -Fxq "Runtime packages: ok" <<<"$output" \
    && grep -Fxq "Arch runtime packages: ok" <<<"$output" \
    && grep -Fxq "PipeWire client stack: ok" <<<"$output" \
    && grep -Fxq "Host Wayland stack: ok" <<<"$output" \
    && grep -Fxq "AppImage PipeWire bundle: ok" <<<"$output" \
    && grep -Fxq "AppImage Wayland bundle: ok" <<<"$output" \
    && grep -Fxq "Standard effects: bundled in wavelinux6-audio-core" <<<"$output" \
    && grep -Fxq "bwrap available: true" <<<"$output" \
    && grep -Fxq "xdg-dbus-proxy available: true" <<<"$output" \
    && grep -Eq '^DBus session bus: (unavailable or not a unix:path address|.+ exists=false)$' <<<"$output" \
    && grep -Eq '^webkit env: .* WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1$' <<<"$output"
}

run_runtime_check() {
  local label="$1"
  shift
  local output status
  set +e
  output="$("$@" 2>&1)"
  status=$?
  set -e
  printf '%s\n' "$output"
  if (( status == 0 )); then
    return 0
  fi
  if runtime_check_accepts_headless_container "$output"; then
    echo "Accepting the absent DBus session bus for headless container check: $label."
    return 0
  fi
  echo "$label failed runtime dependency check" >&2
  return "$status"
}

installed_wavelinux_command() {
  if command -v wavelinux6 >/dev/null 2>&1; then
    command -v wavelinux6
  elif command -v wavelinux >/dev/null 2>&1; then
    command -v wavelinux
  elif command -v wavelinux-app >/dev/null 2>&1; then
    command -v wavelinux-app
  else
    return 1
  fi
}

smoke_appimage() {
  local tag version artifact_version asset appimage
  tag="$(resolve_latest_tag)"
  version="${tag#v}"
  artifact_version="${WAVELINUX_SMOKE_ARTIFACT_VERSION:-${version%%-*}}"
  asset="${PRODUCT_NAME}_${artifact_version}_amd64.AppImage"
  appimage="/tmp/$asset"

  obtain_release_asset "$tag" "$asset" "$appimage"
  chmod +x "$appimage"
  bash "$ROOT_DIR/scripts/sanitize-appimage-pipewire.sh" --check "$appimage"
  bash "$ROOT_DIR/scripts/sanitize-appimage-wayland.sh" --check "$appimage"

  echo "Initial AppImage runtime report for $tag:"
  APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --check-runtime-dependencies || true

  echo "Installing AppImage runtime dependencies for $tag:"
  WAVELINUX_ASSUME_YES=1 APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --install-runtime-dependencies

  echo "Final AppImage runtime report for $tag:"
  run_runtime_check "AppImage $tag" env APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --check-runtime-dependencies

  echo "Probing the bundled application executable for $tag:"
  WAVELINUX_SKIP_APPIMAGE_PREFLIGHT=1 APPIMAGE_EXTRACT_AND_RUN=1 \
    "$appimage" --probe-binary

  if [[ "$WAVELINUX_SMOKE_DISTRO" == "cachyos" ]]; then
    smoke_appimage_ui "$appimage"
  fi
}

install_ui_smoke_dependencies() {
  local manager
  manager="$(manager_id)"
  case "$manager" in
    apt)
      export DEBIAN_FRONTEND=noninteractive
      apt-get install -y --no-install-recommends dbus-x11 imagemagick xvfb x11-apps passwd
      ;;
    dnf)
      dnf install -y --setopt=install_weak_deps=False dbus-daemon ImageMagick \
        xorg-x11-server-Xvfb xorg-x11-apps shadow-utils
      ;;
    pacman)
      pacman -S --needed --noconfirm dbus imagemagick xorg-server-xvfb xorg-xwd shadow
      ;;
    *)
      echo "No UI smoke dependency path for package manager: $manager" >&2
      return 1
      ;;
  esac
}

smoke_appimage_ui() {
  local appimage="$1"
  local user="wavelinux-ui-smoke"
  local output="/tmp/wavelinux-ui-smoke-output"

  echo "Running non-root AppImage WebKit pixel smoke on $WAVELINUX_SMOKE_DISTRO:"
  install_ui_smoke_dependencies
  id "$user" >/dev/null 2>&1 || useradd --create-home "$user"
  install -d -m 0700 -o "$user" -g "$user" "$output"
  runuser -u "$user" -- \
    bash "$ROOT_DIR/scripts/appimage-ui-smoke.sh" "$appimage" "$output"
}

smoke_native() {
  local tag version artifact_version manager asset package
  tag="$(resolve_latest_tag)"
  version="${tag#v}"
  artifact_version="${WAVELINUX_SMOKE_ARTIFACT_VERSION:-${version%%-*}}"
  manager="$(manager_id)"
  case "$manager" in
    apt)
      asset="${PRODUCT_NAME}_${artifact_version}_amd64.deb"
      package="/tmp/$asset"
      obtain_release_asset "$tag" "$asset" "$package"
      export DEBIAN_FRONTEND=noninteractive
      apt-get update
      apt-get install -y --no-install-recommends "$package"
      ;;
    dnf)
      asset="${PRODUCT_NAME}-${artifact_version}-1.x86_64.rpm"
      package="/tmp/$asset"
      obtain_release_asset "$tag" "$asset" "$package"
      dnf install -y --setopt=install_weak_deps=False "$package"
      ;;
    pacman)
      echo "Skipping native package smoke on Arch; WaveLinux publishes AppImage plus AUR metadata, not a pacman package."
      return 0
      ;;
    *)
      echo "No native package smoke path for package manager: $manager" >&2
      return 1
      ;;
  esac

  local command_path
  command_path="$(installed_wavelinux_command)" || {
    echo "No WaveLinux command was installed by the native package" >&2
    return 1
  }
  "$command_path" --probe-binary
  command -v wavelinux6-audio-core >/dev/null 2>&1 || {
    echo "Native package omitted the required wavelinux6-audio-core executable" >&2
    return 1
  }
  command -v wavelinux6-peripheral-plugin >/dev/null 2>&1 || {
    echo "Native package omitted the required wavelinux6-peripheral-plugin executable" >&2
    return 1
  }
  run_runtime_check "native package $tag" "$command_path" --check-runtime-dependencies
}

smoke_source_helper() {
  bash "$ROOT_DIR/scripts/check-dependencies.sh" --install --strict-runtime
}

inside_main() {
  if [[ "$TARGET" == "native" ]] && command -v pacman >/dev/null 2>&1; then
    echo "Skipping native package smoke on Arch; WaveLinux publishes AppImage plus AUR metadata, not a pacman package."
    return 0
  fi
  bootstrap_container_tools
  case "$TARGET" in
    appimage) smoke_appimage ;;
    native) smoke_native ;;
    source-helper) smoke_source_helper ;;
    *)
      echo "Unknown smoke target: $TARGET" >&2
      exit 1
      ;;
  esac
}

choose_container_engine() {
  if [[ -n "$CONTAINER_ENGINE" ]]; then
    echo "$CONTAINER_ENGINE"
  elif command -v docker >/dev/null 2>&1; then
    echo docker
  elif command -v podman >/dev/null 2>&1; then
    echo podman
  else
    echo "docker or podman is required for distro smoke tests" >&2
    return 1
  fi
}

host_main() {
  local engine distros distro image
  engine="$(choose_container_engine)"
  if [[ "$DISTRO" == "all" ]]; then
    distros=(debian13 ubuntu2404 fedora arch cachyos)
  else
    distros=("$DISTRO")
  fi

  for distro in "${distros[@]}"; do
    image="$(image_for_distro "$distro")"
    echo "==> WaveLinux distro smoke: distro=$distro image=$image target=$TARGET tag=$RELEASE_TAG"
    container_args=(run --rm -v "$ROOT_DIR:/work:ro" -w /work)
    if [[ -n "$LOCAL_ASSET_DIR" ]]; then
      container_args+=(
        -v "$LOCAL_ASSET_DIR:/wavelinux-artifacts:ro"
        -e "WAVELINUX_SMOKE_LOCAL_ASSET_DIR=/wavelinux-artifacts"
      )
    fi
    if [[ "${WAVELINUX_SMOKE_PRIVILEGED:-0}" != "0" ]]; then
      container_args+=(--privileged --security-opt seccomp=unconfined)
    fi
    if [[ -n "${WAVELINUX_SMOKE_ARTIFACT_VERSION:-}" ]]; then
      container_args+=(-e "WAVELINUX_SMOKE_ARTIFACT_VERSION=$WAVELINUX_SMOKE_ARTIFACT_VERSION")
    fi
    container_args+=(
      -e "WAVELINUX_SMOKE_DISTRO=$distro"
      -e "WAVELINUX_SMOKE_TARGET=$TARGET"
      -e "WAVELINUX_SMOKE_RELEASE_TAG=$RELEASE_TAG"
      -e "WAVELINUX_SMOKE_REPO=$REPO"
      -e "WAVELINUX_SMOKE_PRODUCT_NAME=$PRODUCT_NAME"
      "$image"
      bash scripts/distro-smoke.sh --inside
    )
    "$engine" "${container_args[@]}"
  done
}

if (( INSIDE == 1 )); then
  inside_main
else
  host_main
fi
