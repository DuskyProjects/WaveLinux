#!/usr/bin/env bash

CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wavelinux"
KEY_PATH="${WAVELINUX_RELEASE_KEY_PATH:-$CONFIG_DIR/release.key}"
PASSWORD_PATH="${WAVELINUX_RELEASE_KEY_PASSWORD_FILE:-$KEY_PATH.password}"

if [[ ! -f "$KEY_PATH" || ! -f "$PASSWORD_PATH" ]]; then
  echo "Missing release key or password." >&2
  echo "Run: bash scripts/generate-release-key.sh" >&2
  return 1 2>/dev/null || exit 1
fi

export TAURI_SIGNING_PRIVATE_KEY_PATH="$KEY_PATH"
export TAURI_SIGNING_PRIVATE_KEY
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD
TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY_PATH")"
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "$PASSWORD_PATH")"

echo "TAURI_SIGNING_PRIVATE_KEY_PATH=$TAURI_SIGNING_PRIVATE_KEY_PATH"
