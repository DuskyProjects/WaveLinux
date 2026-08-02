#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

cargo fmt --all -- --check
cargo test --workspace
cargo test -p wavelinux-accelerator --all-features --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p wavelinux-accelerator --all-features --all-targets -- -D warnings
node node_modules/.bin/tsc --noEmit
node node_modules/.bin/vitest run
node node_modules/.bin/vite build
node node_modules/.bin/playwright test
shellcheck scripts/*.sh
bash scripts/check-docs.sh
bash scripts/check-dependencies.sh
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

expected_weights_sha256="e6de5fbfadf7ec91d1b24d6a6ccfd0290cb4d8bf555c5eab3ce41506f67a58b1"
expected_model_sha256="0d8c5664b9f4c677a6950464d900ee127cfeb6489b81070e39db1f415b604c71"
expected_fixture_sha256="412d0c126160f5782708c105e530cd5033f96dcbca6bd18c0b8991e5d469564b"
printf '%s  %s\n' \
  "$expected_weights_sha256" providers/rnnoise/weights.rnn \
  "$expected_model_sha256" providers/rnnoise/rnnoise-neural-v1.onnx \
  "$expected_fixture_sha256" providers/rnnoise/rnnoise-neural-v1-golden.json | sha256sum --check --strict
if python3 -c 'import numpy, onnx' >/dev/null 2>&1; then
  python3 scripts/generate-rnnoise-onnx.py \
    --model "$tmp_dir/rnnoise-neural-v1.onnx" \
    --fixture "$tmp_dir/rnnoise-neural-v1-golden.json"
  cmp providers/rnnoise/rnnoise-neural-v1.onnx "$tmp_dir/rnnoise-neural-v1.onnx"
  cmp providers/rnnoise/rnnoise-neural-v1-golden.json "$tmp_dir/rnnoise-neural-v1-golden.json"
else
  echo "Python ONNX tooling unavailable; checked committed RNNoise artifact hashes only." >&2
fi

printf 'pcm.keep { type pulse }\n' > "$tmp_dir/asoundrc"
WAVELINUX_ASOUNDRC="$tmp_dir/asoundrc" XDG_CONFIG_HOME="$tmp_dir/config" bash scripts/install-alsa-aliases.sh
grep -q "WaveLinux6 ALSA aliases" "$tmp_dir/asoundrc"
grep -q "pcm.wavelinux6_mic" "$tmp_dir/asoundrc"
grep -q 'device "wavelinux6-mic"' "$tmp_dir/asoundrc"
grep -q "pcm.wavelinux6_mix_monitor" "$tmp_dir/asoundrc"
grep -q 'device "wavelinux6_mix_monitor_source"' "$tmp_dir/asoundrc"
WAVELINUX_ASOUNDRC="$tmp_dir/asoundrc" bash scripts/remove-alsa-aliases.sh
grep -q "pcm.keep" "$tmp_dir/asoundrc"
if grep -q "WaveLinux6 ALSA aliases" "$tmp_dir/asoundrc"; then
  echo "WaveLinux6 ALSA aliases remained after uninstall" >&2
  exit 1
fi

mkdir -p "$tmp_dir/config/wavelinux6"
cat > "$tmp_dir/config/wavelinux6/config.json" <<'JSON'
{
  "mixes": [
    {
      "id": "monitor",
      "name": "Monitor",
      "virtual_source_name": "wavelinux6_mix_monitor_source"
    }
  ],
  "channels": [
    {
      "id": "hardware_in",
      "name": "Input",
      "virtual_sink_name": "wavelinux6_channel_hardware_in"
    }
  ]
}
JSON
WAVELINUX_ASOUNDRC="$tmp_dir/asoundrc" XDG_CONFIG_HOME="$tmp_dir/config" bash scripts/install-alsa-aliases.sh
grep -q "WaveLinux6 ALSA aliases" "$tmp_dir/asoundrc"
grep -q "pcm.wavelinux6_mic" "$tmp_dir/asoundrc"
grep -q "pcm.wavelinux6_channel_hardware_in" "$tmp_dir/asoundrc"
grep -q 'device "wavelinux6_channel_hardware_in.monitor"' "$tmp_dir/asoundrc"
if grep -q "pcm.wavelinux_channel_hardware_in" "$tmp_dir/asoundrc"; then
  echo "Stable WaveLinux ALSA aliases leaked into the WaveLinux 6 install" >&2
  exit 1
fi
git diff --check

if [[ "${WAVELINUX_RUN_LIVE_TESTS:-0}" == "1" ]]; then
  cargo test -p wavelinux-engine -- --ignored --test-threads=1
fi
