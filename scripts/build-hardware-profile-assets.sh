#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_DIR="$ROOT_DIR/profiles/v1"
OUT_DIR="${WAVELINUX_PROFILE_ASSET_DIR:-$ROOT_DIR/target/hardware-profiles/v1}"
MINISIGN_KEY="${WAVELINUX_PROFILE_MINISIGN_KEY:-}"

rm -rf "$OUT_DIR"
install -d "$OUT_DIR"

cp "$SOURCE_DIR/index.json" "$OUT_DIR/hardware-profiles-v1-index.json"
cp "$SOURCE_DIR"/devices/*.json "$OUT_DIR/"

if [[ -n "$MINISIGN_KEY" ]]; then
  if ! command -v minisign >/dev/null 2>&1; then
    echo "minisign is required when WAVELINUX_PROFILE_MINISIGN_KEY is set" >&2
    exit 1
  fi
  for asset in "$OUT_DIR"/*.json; do
    minisign -S -s "$MINISIGN_KEY" -m "$asset" -x "$asset.sig" -q
  done
else
  echo "Profile assets copied without signatures." >&2
  echo "Set WAVELINUX_PROFILE_MINISIGN_KEY to create .sig files for GitHub Releases." >&2
fi

echo "Hardware profile release assets: $OUT_DIR"
