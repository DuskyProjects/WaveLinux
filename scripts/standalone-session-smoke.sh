#!/usr/bin/env bash
set -euo pipefail

MODE=installer
if [[ "${1:-}" == "--native" ]]; then
  MODE=native
  shift
fi

TARGET="${1:?usage: standalone-session-smoke.sh [--native] EXECUTABLE [OUTPUT_DIR]}"
OUTPUT_DIR="${2:-$(mktemp -d "${TMPDIR:-/tmp}/wavelinux6-session-smoke.XXXXXX")}"
DISPLAY_NUMBER="${WAVELINUX_SMOKE_DISPLAY:-:98}"

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "The standalone session smoke must run as a non-root user." >&2
  exit 1
fi
[[ -x "$TARGET" ]] || {
  echo "Session target is missing or not executable: $TARGET" >&2
  exit 1
}

VERIFY_SCRIPT="${WAVELINUX_SMOKE_VERIFY_SCRIPT:-/usr/lib/wavelinux6/verify-install.sh}"
if [[ "$MODE" == native && ! -x "$VERIFY_SCRIPT" ]]; then
  echo "Native package verifier is missing or not executable: $VERIFY_SCRIPT" >&2
  exit 1
fi

for command in dbus-run-session pipewire pipewire-pulse wireplumber pactl pw-dump Xvfb; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "Standalone session smoke requires: $command" >&2
    exit 1
  }
done

install -d -m 0700 "$OUTPUT_DIR"
OUTPUT_DIR="$(realpath "$OUTPUT_DIR")"
SESSION_RUNTIME="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux6-session-runtime.XXXXXX")"
XVFB_LOG="$OUTPUT_DIR/xvfb.log"
SESSION_LOG="$OUTPUT_DIR/session.log"
XWD="$OUTPUT_DIR/wavelinux6.xwd"
PNG="$OUTPUT_DIR/wavelinux6.png"

cleanup() {
  if [[ -n "${XVFB_PID:-}" ]]; then
    kill "$XVFB_PID" 2>/dev/null || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi
  rm -rf "$SESSION_RUNTIME"
}
trap cleanup EXIT

export DISPLAY="$DISPLAY_NUMBER"
export XDG_RUNTIME_DIR="$SESSION_RUNTIME"
export APPIMAGE_EXTRACT_AND_RUN=1
export WAVELINUX_SKIP_AUDIO_SERVICE_START=1
chmod 0700 "$XDG_RUNTIME_DIR"

Xvfb "$DISPLAY" -screen 0 1440x900x24 -nolisten tcp >"$XVFB_LOG" 2>&1 &
XVFB_PID=$!
sleep 1

# Positional arguments are intentionally passed into the isolated session.
# shellcheck disable=SC2016
dbus-run-session -- bash -c '
  set -euo pipefail
  mode="$1"
  target="$2"
  session_log="$3"
  capture="$4"
  verify_script="$5"

  pipewire >"$session_log.pipewire" 2>&1 &
  pipewire_pid=$!
  pipewire-pulse >"$session_log.pipewire-pulse" 2>&1 &
  pulse_pid=$!
  wireplumber >"$session_log.wireplumber" 2>&1 &
  wireplumber_pid=$!

  cleanup_session() {
    wavelinux_pids=()
    while read -r candidate_pid; do
      executable="$(readlink "/proc/$candidate_pid/exe" 2>/dev/null || true)"
      case "${executable##*/}" in
        wavelinux6|wavelinux6-audio-core|WaveLinux6_*_amd64.AppImage)
          wavelinux_pids+=("$candidate_pid")
          ;;
      esac
    done < <(ps -u "$(id -u)" -o pid=)
    ((${#wavelinux_pids[@]} == 0)) || kill "${wavelinux_pids[@]}" 2>/dev/null || true
    kill "$wireplumber_pid" "$pulse_pid" "$pipewire_pid" 2>/dev/null || true
    wait "$wireplumber_pid" "$pulse_pid" "$pipewire_pid" 2>/dev/null || true
  }
  trap cleanup_session EXIT

  ready=0
  for _ in {1..80}; do
    if pactl info >/dev/null 2>&1 && pw-dump >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 0.25
  done
  ((ready == 1)) || {
    echo "Isolated PipeWire/Pulse session did not become ready." >&2
    cat "$session_log.pipewire" "$session_log.pipewire-pulse" "$session_log.wireplumber" >&2
    exit 1
  }

  if [[ "$mode" == installer ]]; then
    "$target" 2>&1 | tee "$session_log"
  else
    "$target" >"$session_log" 2>&1 &
    app_pid=$!
    WAVELINUX_VERIFY_TIMEOUT_SECONDS=30 "$verify_script"
    kill -0 "$app_pid"
  fi
  if command -v xwd >/dev/null 2>&1; then
    sleep 1
    xwd -root -silent -display "$DISPLAY" -out "$capture"
  fi
' bash "$MODE" "$TARGET" "$SESSION_LOG" "$XWD" "$VERIFY_SCRIPT"

if [[ -s "$XWD" ]] && command -v magick >/dev/null 2>&1; then
  magick "$XWD" "$PNG"
  colors="$(magick identify -format '%k' "$PNG")"
  if ! [[ "$colors" =~ ^[0-9]+$ ]] || ((colors < 64)); then
    echo "Standalone AppImage rendered only $colors colors." >&2
    cat "$SESSION_LOG" >&2
    exit 1
  fi
  echo "WaveLinux UI render: ok colors=$colors screenshot=$PNG"
fi

echo "WaveLinux $MODE non-root D-Bus/PipeWire session smoke passed."
