#!/usr/bin/env bash
set -euo pipefail

APPIMAGE="${1:?usage: appimage-ui-smoke.sh APPIMAGE [OUTPUT_DIR]}"
OUTPUT_DIR="${2:-$(mktemp -d "${TMPDIR:-/tmp}/wavelinux-ui-smoke.XXXXXX")}"
WAIT_SECONDS="${WAVELINUX_UI_SMOKE_WAIT_SECONDS:-12}"
DISPLAY_NUMBER="${WAVELINUX_UI_SMOKE_DISPLAY:-:99}"

APPIMAGE="$(realpath "$APPIMAGE")"
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(realpath "$OUTPUT_DIR")"
LOG="$OUTPUT_DIR/wavelinux-ui.log"
XWD="$OUTPUT_DIR/wavelinux-ui.xwd"
PNG="$OUTPUT_DIR/wavelinux-ui.png"
XVFB_LOG="$OUTPUT_DIR/xvfb.log"
RUNTIME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux-ui-runtime.XXXXXX")"
APPIMAGE_TMP_DIR="$RUNTIME_DIR/appimage"

cleanup() {
  if [[ -n "${XVFB_PID:-}" ]]; then
    kill "$XVFB_PID" 2>/dev/null || true
    wait "$XVFB_PID" 2>/dev/null || true
  fi
  rm -rf "$RUNTIME_DIR"
}
trap cleanup EXIT

for command in Xvfb xwd magick dbus-run-session; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "AppImage UI smoke requires $command" >&2
    exit 1
  }
done

export DISPLAY="$DISPLAY_NUMBER"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"
export TMPDIR="$APPIMAGE_TMP_DIR"
mkdir -p "$TMPDIR"
chmod 0700 "$XDG_RUNTIME_DIR"

Xvfb "$DISPLAY" -screen 0 1440x900x24 -nolisten tcp >"$XVFB_LOG" 2>&1 &
XVFB_PID=$!
sleep 1

# Positional arguments are intentionally expanded by the inner shell.
# shellcheck disable=SC2016
dbus-run-session -- bash -c '
  set -euo pipefail
  appimage="$1"
  log="$2"
  capture="$3"
  wait_seconds="$4"

  env \
    APPIMAGE_EXTRACT_AND_RUN=1 \
    WAVELINUX_SKIP_APPIMAGE_PREFLIGHT=1 \
    WAVELINUX_ASSUME_RUNTIME_DEPS=1 \
    WAVELINUX_SKIP_AUDIO_SERVICE_START=1 \
    "$appimage" >"$log" 2>&1 &
  app_pid=$!
  sleep "$wait_seconds"
  xwd -root -silent -display "$DISPLAY" -out "$capture"
  kill "$app_pid" 2>/dev/null || true
  wait "$app_pid" 2>/dev/null || true
' bash "$APPIMAGE" "$LOG" "$XWD" "$WAIT_SECONDS"

magick "$XWD" "$PNG"

if grep -Eq 'EGL_BAD_PARAMETER|Could not create default EGL display|Web process crashed' "$LOG"; then
  echo "AppImage UI smoke detected a WebKit renderer failure:" >&2
  cat "$LOG" >&2
  exit 1
fi

colors="$(magick identify -format '%k' "$PNG")"
mean="$(magick identify -format '%[fx:mean]' "$PNG")"
if ! [[ "$colors" =~ ^[0-9]+$ ]] || (( colors < 64 )); then
  echo "AppImage UI smoke rendered only $colors colors; expected the WaveLinux UI" >&2
  cat "$LOG" >&2
  exit 1
fi

echo "AppImage UI render: ok colors=$colors mean=$mean screenshot=$PNG"
