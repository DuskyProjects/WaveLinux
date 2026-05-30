#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

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
if [[ -z "${WAVELINUX_RELEASE_TAG:-}" ]]; then
  WAVELINUX_RELEASE_TAG="${GITHUB_REF_NAME:-}"
fi
if [[ -z "${WAVELINUX_RELEASE_TAG:-}" ]]; then
  WAVELINUX_RELEASE_TAG="$(git -C "$ROOT_DIR" describe --tags --exact-match HEAD 2>/dev/null || true)"
fi
export WAVELINUX_RELEASE_TAG
exec "$ROOT_DIR/node_modules/.bin/tauri" build \
  --config '{"bundle":{"createUpdaterArtifacts":true}}'
