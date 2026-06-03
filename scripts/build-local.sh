#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR/crates/app"
export NO_STRIP="${NO_STRIP:-1}"
"$ROOT_DIR/scripts/stage-appimage-runtime.sh"
exec "$ROOT_DIR/node_modules/.bin/tauri" build
