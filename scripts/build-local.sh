#!/usr/bin/env bash
set -euo pipefail

# Local desktop builds go through Tauri first. If AppImage bundling fails
# because cached linuxdeploy cannot strip newer ELF sections, retry with the
# host-strip fallback and then rebuild a sanitized AppImage.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PRODUCT_NAME="$(node -e 'console.log(require(process.argv[1]).productName || "WaveLinux6")' "$ROOT_DIR/crates/app/tauri.conf.json")"
MAIN_BINARY_NAME="$(node -e 'console.log(require(process.argv[1]).mainBinaryName || "wavelinux6")' "$ROOT_DIR/crates/app/tauri.conf.json")"
PACKAGE_VERSION="$(node -e 'console.log(require(process.argv[1]).version)' "$ROOT_DIR/package.json")"

cd "$ROOT_DIR/crates/app"
export NO_STRIP="${NO_STRIP:-1}"
(cd "$ROOT_DIR" && cargo build --release -p wavelinux-dsp --bin wavelinux6-audio-core)
(cd "$ROOT_DIR" && cargo build --release -p wavelinux-app --bin wavelinux6-peripheral-plugin)
"$ROOT_DIR/scripts/stage-appimage-runtime.sh"
rm -rf "$ROOT_DIR/target/release/bundle/appimage/${PRODUCT_NAME}.AppDir"
rm -f "$ROOT_DIR/target/release/bundle/appimage/${PRODUCT_NAME}"*.AppImage
rm -f \
  "$ROOT_DIR/target/release/bundle/deb/${PRODUCT_NAME}_${PACKAGE_VERSION}_amd64.deb" \
  "$ROOT_DIR/target/release/bundle/rpm/${PRODUCT_NAME}-${PACKAGE_VERSION}-1.x86_64.rpm"
rm -f \
  "$ROOT_DIR/target/release/$MAIN_BINARY_NAME" \
  "$ROOT_DIR/target/release/wavelinux5" \
  "$ROOT_DIR/target/release/wavelinux5-dsp-helper" \
  "$ROOT_DIR/target/release/wavelinux5-dsp-helper.d"

# Build native packages independently so an AppImage tooling failure cannot
# leave an apparently current but stale DEB or RPM behind.
"$ROOT_DIR/node_modules/.bin/tauri" build --bundles deb,rpm

if ! "$ROOT_DIR/node_modules/.bin/tauri" build --bundles appimage; then
  echo "Tauri AppImage bundling failed; retrying with host strip fallback" >&2
  "$ROOT_DIR/scripts/rebuild-appimage-with-host-strip.sh"
fi
APPIMAGE_EXTRACT_AND_RUN="${APPIMAGE_EXTRACT_AND_RUN:-1}" "$ROOT_DIR/scripts/finalize-appimage.sh"
bash "$ROOT_DIR/scripts/check-package-contents.sh" "$ROOT_DIR/target/release/bundle"
