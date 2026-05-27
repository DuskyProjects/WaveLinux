#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$ROOT_DIR/crates/app"
export RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=native"
export NO_STRIP="${NO_STRIP:-0}"
exec "$ROOT_DIR/node_modules/.bin/tauri" build
