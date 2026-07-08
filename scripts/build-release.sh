#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PRODUCT_NAME="$(node -e 'console.log(require(process.argv[1]).productName || "WaveLinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"
MAIN_BINARY_NAME="$(node -e 'console.log(require(process.argv[1]).mainBinaryName || "wavelinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" && -z "${TAURI_SIGNING_PRIVATE_KEY_PATH:-}" ]]; then
  if [[ -f "$ROOT_DIR/scripts/release-env.sh" ]]; then
    # shellcheck source=/dev/null
    source "$ROOT_DIR/scripts/release-env.sh"
  fi
fi

if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" && -z "${TAURI_SIGNING_PRIVATE_KEY_PATH:-}" ]]; then
  echo "Missing Tauri signing key. Run yarn release:key or set TAURI_SIGNING_PRIVATE_KEY." >&2
  exit 1
fi

cd "$ROOT_DIR/crates/app"
export NO_STRIP="${NO_STRIP:-0}"
(cd "$ROOT_DIR" && cargo build --release -p wavelinux-dsp --bin wavelinux5-dsp-helper)
WAVELINUX_REQUIRE_RNNOISE_LADSPA=1 "$ROOT_DIR/scripts/stage-appimage-runtime.sh"
rm -rf "$ROOT_DIR/target/release/bundle/appimage/${PRODUCT_NAME}.AppDir"
rm -f "$ROOT_DIR/target/release/bundle/appimage/${PRODUCT_NAME}"*.AppImage
rm -f "$ROOT_DIR/target/release/$MAIN_BINARY_NAME"
if [[ -z "${WAVELINUX_RELEASE_TAG:-}" ]]; then
  WAVELINUX_RELEASE_TAG="${GITHUB_REF_NAME:-}"
fi
if [[ -z "${WAVELINUX_RELEASE_TAG:-}" ]]; then
  WAVELINUX_RELEASE_TAG="$(git -C "$ROOT_DIR" describe --tags --exact-match HEAD 2>/dev/null || true)"
fi
export WAVELINUX_RELEASE_TAG
if [[ -z "${WAVELINUX_UPDATE_VERSION:-}" ]]; then
  if [[ "${GITHUB_REF_NAME:-}" == v* ]]; then
    WAVELINUX_UPDATE_VERSION="${GITHUB_REF_NAME#v}"
  elif [[ "${WAVELINUX_RELEASE_TAG:-}" == v* ]]; then
    WAVELINUX_UPDATE_VERSION="${WAVELINUX_RELEASE_TAG#v}"
  fi
fi
export WAVELINUX_UPDATE_VERSION
"$ROOT_DIR/node_modules/.bin/tauri" build \
  --config '{"bundle":{"createUpdaterArtifacts":true}}'
"$ROOT_DIR/scripts/finalize-appimage.sh" --updater
