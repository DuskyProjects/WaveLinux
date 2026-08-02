#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE_DIR="$ROOT_DIR/profiles/v1"
INDEX_PATH="$PROFILE_DIR/index.json"
SIGNATURE_PATH="$PROFILE_DIR/index.json.sig"
KEY_PATH="${WAVELINUX_PROFILE_SIGNING_KEY:-$HOME/.local/share/wavelinux6/release-keys/hardware-profiles-ed25519.pem}"

for command in jq openssl sha256sum base64; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'Missing required command: %s\n' "$command" >&2
    exit 1
  }
done

[[ -f "$KEY_PATH" ]] || {
  printf 'Profile signing key not found: %s\n' "$KEY_PATH" >&2
  exit 1
}

work_index="$(mktemp)"
next_index="$(mktemp)"
raw_signature="$(mktemp)"
encoded_signature="$(mktemp)"
trap 'rm -f "$work_index" "$next_index" "$raw_signature" "$encoded_signature"' EXIT

jq '.' "$INDEX_PATH" > "$work_index"
while IFS= read -r asset; do
  [[ "$asset" == "$(basename "$asset")" && "$asset" == *.json ]] || {
    printf 'Invalid profile asset in index: %s\n' "$asset" >&2
    exit 1
  }
  asset_path="$PROFILE_DIR/devices/$asset"
  [[ -f "$asset_path" ]] || {
    printf 'Profile asset not found: %s\n' "$asset_path" >&2
    exit 1
  }
  digest="$(sha256sum "$asset_path" | cut -d' ' -f1)"
  jq --arg asset "$asset" --arg digest "$digest" \
    '(.profiles[] | select(.asset == $asset) | .sha256) = $digest' \
    "$work_index" > "$next_index"
  mv "$next_index" "$work_index"
done < <(jq -r '.profiles[].asset' "$INDEX_PATH")

jq '.' "$work_index" > "$next_index"
mv "$next_index" "$INDEX_PATH"
openssl pkeyutl -sign -rawin -inkey "$KEY_PATH" -in "$INDEX_PATH" -out "$raw_signature"
base64 -w0 "$raw_signature" > "$encoded_signature"
printf '\n' >> "$encoded_signature"
mv "$encoded_signature" "$SIGNATURE_PATH"

printf 'Signed %s profile entries with %s\n' "$(jq '.profiles | length' "$INDEX_PATH")" "$KEY_PATH"
