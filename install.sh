#!/usr/bin/env bash
set -euo pipefail

REPOSITORY="DuskyProjects/WaveLinux"
REQUESTED_TAG="${WAVELINUX_RELEASE_TAG:-}"
REQUESTED_FORMAT=auto
DRY_RUN=0
NO_LAUNCH=0

usage() {
  cat <<'HELP'
Download and run the verified WaveLinux 6 installer.

Usage:
  ./install.sh [options]

Options:
  --tag TAG                    Install a specific release tag (for example v6.0.1).
  --format auto|appimage|deb|rpm
                               auto and appimage use the canonical self-extracting installer.
  --dry-run                    Print the selected release assets without downloading them.
  --no-launch                  Install without launching WaveLinux.
  -h, --help                   Show this help.

Environment:
  WAVELINUX_RELEASE_TAG        Same as --tag.
HELP
}

while (($#)); do
  case "$1" in
    --tag)
      REQUESTED_TAG="${2:?--tag requires a value}"
      shift
      ;;
    --format)
      REQUESTED_FORMAT="${2:?--format requires a value}"
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      ;;
    --no-launch)
      NO_LAUNCH=1
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

case "$REQUESTED_FORMAT" in
  auto|appimage|deb|rpm) ;;
  *)
    echo "Unsupported format: $REQUESTED_FORMAT" >&2
    exit 2
    ;;
esac

if [[ "${EUID:-$(id -u)}" -eq 0 && "${WAVELINUX_INSTALLER_ALLOW_ROOT_FOR_TESTS:-0}" != 1 ]]; then
  echo "Run this installer as your normal desktop user, not with sudo." >&2
  echo "It requests administrator permission only for system packages." >&2
  exit 1
fi

case "$(uname -m)" in
  x86_64|amd64) ;;
  *)
    echo "WaveLinux 6 supports x86_64 only; detected $(uname -m)." >&2
    exit 1
    ;;
esac

for command in sha256sum mktemp; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "Required installer command is missing: $command" >&2
    exit 1
  }
done

http_download() {
  local url="$1"
  local destination="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fL --retry 3 --retry-delay 2 --output "$destination" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget --tries=3 --output-document="$destination" "$url"
  else
    echo "curl or wget is required to download WaveLinux." >&2
    return 1
  fi
}

privilege_helpers() {
  local graphical=0
  [[ -t 0 && -t 1 ]] || graphical=1
  [[ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" && ! -t 0 ]] && graphical=1
  if ((graphical == 1)); then
    command -v pkexec >/dev/null 2>&1 && printf '%s\n' pkexec
    command -v sudo >/dev/null 2>&1 && printf '%s\n' sudo
  else
    command -v sudo >/dev/null 2>&1 && printf '%s\n' sudo
    command -v pkexec >/dev/null 2>&1 && printf '%s\n' pkexec
  fi
}

run_privileged() {
  local helpers=()
  mapfile -t helpers < <(privilege_helpers)
  ((${#helpers[@]})) || {
    echo "Neither sudo nor pkexec is available." >&2
    printf 'Run this manually as an administrator:' >&2
    printf ' %q' "$@" >&2
    printf '\n' >&2
    return 1
  }

  local helper status
  for helper in "${helpers[@]}"; do
    "$helper" "$@" && return 0
    status=$?
    echo "$helper failed with status $status; trying the next privilege helper." >&2
  done
  return 1
}

if [[ -n "$REQUESTED_TAG" && "$REQUESTED_TAG" != v* ]]; then
  REQUESTED_TAG="v$REQUESTED_TAG"
fi

release_path=latest/download
release_label="latest stable release"
if [[ -n "$REQUESTED_TAG" ]]; then
  release_path="download/$REQUESTED_TAG"
  release_label="$REQUESTED_TAG"
fi
release_base="https://github.com/$REPOSITORY/releases/$release_path"

asset_name=WaveLinux6_amd64_Installer.sh
case "$REQUESTED_FORMAT" in
  auto|appimage)
    ;;
  deb|rpm)
    [[ -n "$REQUESTED_TAG" ]] || {
      echo "--format $REQUESTED_FORMAT requires --tag so the versioned native asset is unambiguous." >&2
      echo "Use --format auto for the latest verified standalone installer." >&2
      exit 2
    }
    version="${REQUESTED_TAG#v}"
    if [[ "$REQUESTED_FORMAT" == deb ]]; then
      asset_name="WaveLinux6_${version}_amd64.deb"
    else
      asset_name="WaveLinux6-${version}-1.x86_64.rpm"
    fi
    ;;
esac

asset_url="$release_base/$asset_name"
checksum_url="$release_base/SHA256SUMS"

echo "Selected release: $release_label"
echo "Selected format: $REQUESTED_FORMAT"
echo "Installer asset: $asset_url"
echo "Checksum asset: $checksum_url"
if ((DRY_RUN == 1)); then
  exit 0
fi

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux6-network-install.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT
asset_path="$WORK_DIR/$asset_name"
checksum_path="$WORK_DIR/SHA256SUMS"

http_download "$checksum_url" "$checksum_path"
http_download "$asset_url" "$asset_path"

expected_checksum=""
while read -r checksum filename _; do
  filename="${filename#\*}"
  if [[ "$filename" == "$asset_name" ]]; then
    expected_checksum="$checksum"
    break
  fi
done <"$checksum_path"
[[ -n "$expected_checksum" ]] || {
  echo "SHA256SUMS does not contain $asset_name; refusing an unverified installation." >&2
  exit 1
}
read -r actual_checksum _ < <(sha256sum "$asset_path")
[[ "$actual_checksum" == "$expected_checksum" ]] || {
  echo "Checksum verification failed for $asset_name." >&2
  echo "Expected: $expected_checksum" >&2
  echo "Actual:   $actual_checksum" >&2
  exit 1
}
echo "Checksum verified: $asset_name"

case "$REQUESTED_FORMAT" in
  auto|appimage)
    chmod 0755 "$asset_path"
    installer_args=()
    ((NO_LAUNCH == 1)) && installer_args+=(--no-launch)
    "$asset_path" "${installer_args[@]}"
    ;;
  deb)
    command -v apt-get >/dev/null 2>&1 || {
      echo "The Debian package requires apt-get. Use --format auto on this distribution." >&2
      exit 1
    }
    run_privileged apt-get update
    run_privileged env DEBIAN_FRONTEND=noninteractive apt-get install -y "$asset_path"
    ;;
  rpm)
    command -v dnf >/dev/null 2>&1 || {
      echo "The RPM is supported on Fedora-family systems with dnf." >&2
      echo "openSUSE and other distributions must use --format auto." >&2
      exit 1
    }
    run_privileged dnf makecache --refresh -y
    run_privileged dnf install -y "$asset_path"
    ;;
esac

if [[ "$REQUESTED_FORMAT" == deb || "$REQUESTED_FORMAT" == rpm ]]; then
  command -v wavelinux6 >/dev/null 2>&1 || {
    echo "Native package installation completed but wavelinux6 is unavailable on PATH." >&2
    exit 1
  }
  wavelinux6 --probe-binary
  verifier=/usr/lib/wavelinux6/verify-install.sh
  [[ -x "$verifier" ]] || {
    echo "Native package omitted its installation verifier: $verifier" >&2
    exit 1
  }
  verify_args=(--timeout 30)
  ((NO_LAUNCH == 1)) && verify_args+=(--no-launch)
  "$verifier" "${verify_args[@]}"
fi
