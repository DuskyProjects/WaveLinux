#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

(
  cd "$ROOT_DIR"
  cargo build --release --locked -p wavelinux-dsp --bin wavelinux6-audio-core
  cargo build --release --locked -p wavelinux-app --bin wavelinux6-peripheral-plugin
)

cd "$ROOT_DIR/crates/app"
export NO_STRIP="${NO_STRIP:-0}"
exec "$ROOT_DIR/node_modules/.bin/tauri" build
