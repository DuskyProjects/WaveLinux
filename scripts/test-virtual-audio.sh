#!/usr/bin/env bash
# Optional, short acceptance checks using synthetic audio in a private session.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
for command in cargo dbus-run-session pipewire pipewire-pulse wireplumber pactl paplay parec pw-dump timeout; do
  command -v "$command" >/dev/null || { echo "Missing test dependency: $command" >&2; exit 1; }
done
HELPER="${WAVELINUX_DSP_HELPER:-$ROOT/target/release/wavelinux6-audio-core}"
[[ -x "$HELPER" ]] || { echo "Build the native audio core first." >&2; exit 1; }
REPORT_DIR="${1:-$ROOT/target/virtual-audio-checks}"
install -d -m 0700 "$REPORT_DIR"
REPORT_DIR="$(realpath "$REPORT_DIR")"
SESSION_ROOT="$(mktemp -d /tmp/wl6-virtual.XXXXXX)"
cleanup() {
  # Include helper descendants, but only inside this invocation's runtime.
  local candidate entry
  if [[ -d "$SESSION_ROOT/engine" ]]; then
    cp -a "$SESSION_ROOT/engine" "$REPORT_DIR/engine-session" || true
    rm -f -- "$REPORT_DIR/engine-session/"*.raw
  fi
  for candidate in /proc/[0-9]*; do
    [[ -r "$candidate/environ" ]] || continue
    while IFS= read -r -d '' entry; do
      if [[ "$entry" == "XDG_RUNTIME_DIR=$SESSION_ROOT" ]]; then
        kill -TERM "${candidate##*/}" 2>/dev/null || true
        break
      fi
    done 2>/dev/null < "$candidate/environ" || true
  done
  rm -rf -- "$SESSION_ROOT"
}
trap cleanup EXIT
# Keep HOME intact; all session state and sockets have explicit private paths.
env XDG_RUNTIME_DIR="$SESSION_ROOT" PIPEWIRE_RUNTIME_DIR="$SESSION_ROOT" \
  PIPEWIRE_REMOTE=pipewire-0 PULSE_SERVER="unix:$SESSION_ROOT/pulse/native" \
  XDG_CONFIG_HOME="$SESSION_ROOT/config-home" XDG_DATA_HOME="$SESSION_ROOT/data-home" \
  XDG_STATE_HOME="$SESSION_ROOT/state-home" XDG_CACHE_HOME="$SESSION_ROOT/cache-home" \
  WAVELINUX_VIRTUAL_AUDIO_TEST=1 WAVELINUX_TEST_REPORT_DIR="$REPORT_DIR" \
  WAVELINUX_GRAPH_PREFIX=wavelinux6 WAVELINUX_GRAPH_PROPERTY_PREFIX=wavelinux6 \
  WAVELINUX_SKIP_AUDIO_SERVICE_START=1 WAVELINUX_ASSUME_RUNTIME_DEPS=1 \
  WAVELINUX_DSP_HELPER="$HELPER" \
  dbus-run-session -- timeout --kill-after=10s 180s \
  cargo test -p wavelinux-engine --test virtual_audio -- --ignored --nocapture --test-threads=1 \
  2>&1 | tee "$REPORT_DIR/results.log"
