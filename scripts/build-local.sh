#!/usr/bin/env bash
set -euo pipefail

# Local desktop builds go through Tauri first. If AppImage bundling fails
# because cached linuxdeploy cannot strip newer ELF sections, retry with the
# host-strip fallback and then rebuild a sanitized AppImage.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PRODUCT_NAME="$(node -e 'console.log(require(process.argv[1]).productName || "WaveLinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"
MAIN_BINARY_NAME="$(node -e 'console.log(require(process.argv[1]).mainBinaryName || "wavelinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"

cd "$ROOT_DIR/crates/app"
export NO_STRIP="${NO_STRIP:-1}"
(cd "$ROOT_DIR" && cargo build --release -p wavelinux-dsp --bin wavelinux5-dsp-helper)
"$ROOT_DIR/scripts/stage-appimage-runtime.sh"
rm -rf "$ROOT_DIR/target/release/bundle/appimage/${PRODUCT_NAME}.AppDir"
rm -f "$ROOT_DIR/target/release/bundle/appimage/${PRODUCT_NAME}"*.AppImage
rm -f "$ROOT_DIR/target/release/$MAIN_BINARY_NAME"
if ! "$ROOT_DIR/node_modules/.bin/tauri" build; then
  echo "Tauri AppImage bundling failed; retrying with host strip fallback" >&2
  "$ROOT_DIR/scripts/rebuild-appimage-with-host-strip.sh"
fi
APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" "$ROOT_DIR/scripts/finalize-appimage.sh"
