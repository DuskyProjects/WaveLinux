#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wavelinux"
KEY_PATH="${WAVELINUX_RELEASE_KEY_PATH:-$CONFIG_DIR/release.key}"
PASSWORD_PATH="${WAVELINUX_RELEASE_KEY_PASSWORD_FILE:-$KEY_PATH.password}"
FORCE_FLAG=""

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  cat <<HELP
Generate a local Tauri release signing key.

Usage:
  bash scripts/generate-release-key.sh
  bash scripts/generate-release-key.sh --force

Environment:
  WAVELINUX_RELEASE_KEY_PATH           Override private key path.
  WAVELINUX_RELEASE_KEY_PASSWORD_FILE  Override password file path.
HELP
  exit 0
fi

if [[ "${1:-}" == "--force" ]]; then
  FORCE_FLAG="--force"
fi

if [[ -n "${1:-}" && -z "$FORCE_FLAG" ]]; then
  echo "Unknown option: $1" >&2
  echo "Run bash scripts/generate-release-key.sh --help for usage." >&2
  exit 1
fi

if [[ -z "$FORCE_FLAG" && ( -e "$KEY_PATH" || -e "$PASSWORD_PATH" ) ]]; then
  echo "Release key already exists at $KEY_PATH" >&2
  echo "Run bash scripts/generate-release-key.sh --force to rotate it." >&2
  exit 1
fi

install -d -m 0700 "$(dirname "$KEY_PATH")"

if command -v openssl >/dev/null 2>&1; then
  openssl rand -base64 48 > "$PASSWORD_PATH"
else
  head -c 48 /dev/urandom | base64 > "$PASSWORD_PATH"
fi
chmod 0600 "$PASSWORD_PATH"

"$ROOT_DIR/node_modules/.bin/tauri" signer generate \
  --ci \
  $FORCE_FLAG \
  --write-keys "$KEY_PATH" \
  --password "$(cat "$PASSWORD_PATH")"

chmod 0600 "$KEY_PATH"
chmod 0644 "$KEY_PATH.pub"

echo "Release private key: $KEY_PATH"
echo "Release public key:  $KEY_PATH.pub"
echo "Password file:       $PASSWORD_PATH"
