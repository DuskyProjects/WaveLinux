#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
node node_modules/.bin/tsc --noEmit
node node_modules/.bin/vite build
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
printf 'pcm.keep { type pulse }\n' > "$tmp_dir/asoundrc"
WAVELINUX_ASOUNDRC="$tmp_dir/asoundrc" XDG_CONFIG_HOME="$tmp_dir/config" bash scripts/install-alsa-aliases.sh
grep -q "WaveLinux ALSA aliases" "$tmp_dir/asoundrc"
WAVELINUX_ASOUNDRC="$tmp_dir/asoundrc" bash scripts/remove-alsa-aliases.sh
grep -q "pcm.keep" "$tmp_dir/asoundrc"
! grep -q "WaveLinux ALSA aliases" "$tmp_dir/asoundrc"
git diff --check

if [[ "${WAVELINUX_RUN_LIVE_TESTS:-0}" == "1" ]]; then
  cargo test -p wavelinux-engine -- --ignored --test-threads=1
fi
