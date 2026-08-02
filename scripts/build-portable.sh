#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONTAINERFILE="$ROOT_DIR/containers/release/Containerfile"
ENGINE="${WAVELINUX_CONTAINER_ENGINE:-}"
IMAGE="${WAVELINUX_PORTABLE_IMAGE:-localhost/wavelinux6-release-builder:bookworm}"
PORTABLE_TARGET="$ROOT_DIR/target/portable"
PROMOTE="${WAVELINUX_PORTABLE_PROMOTE:-1}"

if [[ -z "$ENGINE" ]]; then
  if command -v podman >/dev/null 2>&1; then
    ENGINE=podman
  elif command -v docker >/dev/null 2>&1; then
    ENGINE=docker
  else
    echo "Podman or Docker is required for the portable release build" >&2
    exit 1
  fi
fi

if [[ ! -f "$CONTAINERFILE" ]]; then
  echo "Portable build Containerfile is missing: $CONTAINERFILE" >&2
  exit 1
fi

if [[ "${WAVELINUX_REBUILD_PORTABLE_IMAGE:-0}" == "1" ]] \
  || ! "$ENGINE" image inspect "$IMAGE" >/dev/null 2>&1; then
  "$ENGINE" build --tag "$IMAGE" --file "$CONTAINERFILE" "$ROOT_DIR"
fi

mkdir -p "$PORTABLE_TARGET"

container_args=(
  run --rm
  -v "$ROOT_DIR:/work:rw"
  -v "$PORTABLE_TARGET:/work/target:rw"
  -v wavelinux6-cargo-registry:/root/.cargo/registry
  -v wavelinux6-cargo-git:/root/.cargo/git
  -v wavelinux6-node-modules:/work/node_modules
  -e APPIMAGE_EXTRACT_AND_RUN=1
  -e NO_STRIP=1
  -e WAVELINUX_GLIBC_BASELINE=2.39
  -w /work
  "$IMAGE"
  bash -lc 'yarn install --frozen-lockfile && bash scripts/build-local.sh'
)
"$ENGINE" "${container_args[@]}"

portable_release="$PORTABLE_TARGET/release"
bash "$ROOT_DIR/scripts/check-package-contents.sh" "$portable_release/bundle"
bash "$ROOT_DIR/scripts/check-glibc-baseline.sh" \
  "$portable_release/wavelinux6" \
  "$portable_release/wavelinux6-audio-core" \
  "$portable_release/wavelinux6-peripheral-plugin"

if [[ "$PROMOTE" == "1" ]]; then
  canonical_release="$ROOT_DIR/target/release"
  smoke_assets="$canonical_release/smoke-assets"
  mkdir -p \
    "$canonical_release/bundle/appimage" \
    "$canonical_release/bundle/deb" \
    "$canonical_release/bundle/rpm" \
    "$smoke_assets"
  rm -f \
    "$canonical_release/bundle/appimage"/WaveLinux6_*.AppImage \
    "$canonical_release/bundle/deb"/WaveLinux6_*.deb \
    "$canonical_release/bundle/rpm"/WaveLinux6-*.rpm \
    "$smoke_assets"/WaveLinux6_*.AppImage \
    "$smoke_assets"/WaveLinux6_*.deb \
    "$smoke_assets"/WaveLinux6-*.rpm
  install -m 0755 "$portable_release/bundle/appimage"/WaveLinux6_*.AppImage \
    "$canonical_release/bundle/appimage/"
  install -m 0644 "$portable_release/bundle/deb"/WaveLinux6_*.deb \
    "$canonical_release/bundle/deb/"
  install -m 0644 "$portable_release/bundle/rpm"/WaveLinux6-*.rpm \
    "$canonical_release/bundle/rpm/"
  install -m 0755 \
    "$portable_release/wavelinux6" \
    "$portable_release/wavelinux6-audio-core" \
    "$portable_release/wavelinux6-peripheral-plugin" \
    "$canonical_release/"
  install -m 0755 "$portable_release/bundle/appimage"/WaveLinux6_*.AppImage \
    "$smoke_assets/"
  install -m 0644 \
    "$portable_release/bundle/deb"/WaveLinux6_*.deb \
    "$portable_release/bundle/rpm"/WaveLinux6-*.rpm \
    "$smoke_assets/"
  echo "Promoted portable artifacts to $canonical_release"
fi

echo "Portable WaveLinux artifacts passed the glibc compatibility gate."
