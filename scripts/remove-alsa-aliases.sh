#!/usr/bin/env bash
set -euo pipefail

ASOUNDRC="${WAVELINUX_ASOUNDRC:-$HOME/.asoundrc}"
APP_DISPLAY_NAME="${WAVELINUX_APP_DISPLAY_NAME:-WaveLinux5}"
START_MARKER="# >>> $APP_DISPLAY_NAME ALSA aliases >>>"
END_MARKER="# <<< $APP_DISPLAY_NAME ALSA aliases <<<"

if [[ ! -f "$ASOUNDRC" ]]; then
  exit 0
fi

tmp_output="$(mktemp)"
trap 'rm -f "$tmp_output"' EXIT

awk -v start="$START_MARKER" -v end="$END_MARKER" '
  $0 == start { skip = 1; removed = 1; next }
  $0 == end { skip = 0; next }
  !skip { print }
  END { if (skip) exit 2; if (!removed) exit 3 }
' "$ASOUNDRC" > "$tmp_output" || status=$?

case "${status:-0}" in
  0)
    install -m 0644 "$tmp_output" "$ASOUNDRC"
    echo "Removed $APP_DISPLAY_NAME ALSA aliases from $ASOUNDRC"
    ;;
  2)
    echo "$APP_DISPLAY_NAME ALSA alias block in $ASOUNDRC is missing its end marker" >&2
    exit 1
    ;;
  3)
    echo "No $APP_DISPLAY_NAME ALSA aliases found in $ASOUNDRC"
    ;;
  *)
    exit "$status"
    ;;
esac
