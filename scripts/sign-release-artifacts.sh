#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wavelinux"
KEY_PATH="${WAVELINUX_RELEASE_KEY_PATH:-$CONFIG_DIR/release.key}"
PASSWORD_PATH="${WAVELINUX_RELEASE_KEY_PASSWORD_FILE:-$KEY_PATH.password}"

if [[ ! -f "$KEY_PATH" || ! -f "$PASSWORD_PATH" ]]; then
  echo "Missing release key or password." >&2
  echo "Run: bash scripts/generate-release-key.sh" >&2
  exit 1
fi

shopt -s nullglob
artifacts=(
  "$ROOT_DIR"/target/release/bundle/appimage/*.AppImage
  "$ROOT_DIR"/target/release/bundle/appimage/*.AppImage.tar.gz
  "$ROOT_DIR"/target/release/bundle/deb/*.deb
  "$ROOT_DIR"/target/release/bundle/rpm/*.rpm
)

if (( ${#artifacts[@]} == 0 )); then
  echo "No release artifacts found. Run bash scripts/build-local.sh first." >&2
  exit 1
fi

for artifact in "${artifacts[@]}"; do
  "$ROOT_DIR/node_modules/.bin/tauri" signer sign \
    --private-key-path "$KEY_PATH" \
    --password "$(cat "$PASSWORD_PATH")" \
    "$artifact" >/dev/null
  echo "Signed $artifact"
done
