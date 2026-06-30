#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HELPER="${WAVELINUX_DSP_HELPER:-$ROOT_DIR/target/release/wavelinux5-dsp-helper}"
FRAMES="${WAVELINUX_BENCH_FRAMES:-240000}"
SAMPLE_RATE="${WAVELINUX_BENCH_SAMPLE_RATE:-48000}"
OUT_DIR="${WAVELINUX_BENCH_OUT_DIR:-$ROOT_DIR/target/bench}"
OUT_FILE="$OUT_DIR/audio-runtime-$(date +%Y%m%d-%H%M%S).jsonl"
MODES=(pipewire_filter_chain dsp_cpu dsp_auto)

install -d "$OUT_DIR"

if [[ ! -x "$HELPER" ]]; then
  (cd "$ROOT_DIR" && cargo build --release -p wavelinux-dsp --bin wavelinux5-dsp-helper)
fi

echo "WaveLinux5 audio runtime benchmark"
echo "helper=$HELPER"
echo "frames=$FRAMES sample_rate=$SAMPLE_RATE"
echo "output=$OUT_FILE"

for mode in "${MODES[@]}"; do
  echo "Running $mode..."
  tmp="$(mktemp)"
  time_file="$(mktemp)"
  if command -v /usr/bin/time >/dev/null 2>&1; then
    env WAVELINUX_AUDIO_RUNTIME="$mode" \
      /usr/bin/time -f '{"mode":"'"$mode"'","time_user_sec":%U,"time_system_sec":%S,"max_rss_kb":%M}' \
      -o "$time_file" \
      "$HELPER" --bench-fixture --frames "$FRAMES" --sample-rate "$SAMPLE_RATE" >"$tmp"
  else
    env WAVELINUX_AUDIO_RUNTIME="$mode" \
      "$HELPER" --bench-fixture --frames "$FRAMES" --sample-rate "$SAMPLE_RATE" >"$tmp"
    printf '{"mode":"%s","time_user_sec":null,"time_system_sec":null,"max_rss_kb":null}\n' "$mode" >"$time_file"
  fi
  {
    printf '{"mode":"%s","report":' "$mode"
    tr -d '\n' < "$tmp"
    printf ',"process":'
    tr -d '\n' < "$time_file"
    printf '}\n'
  } | tee -a "$OUT_FILE"
  rm -f "$tmp" "$time_file"
done

cat <<'NOTE'

Benchmark gate:
  Keep the accelerated path experimental until dsp_auto shows at least 30%
  lower helper CPU than pipewire_filter_chain on the hardware input chain,
  with no latency regression and no new PipeWire underruns/errors.

For live underrun checks, run this benchmark while WaveLinux5 is routing the
hardware input chain and compare with:
  journalctl --user --since "5 minutes ago" | grep -Ei "pipewire|underrun|xrun|error"
NOTE
