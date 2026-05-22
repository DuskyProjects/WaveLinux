#!/usr/bin/env bash
set -euo pipefail

ASOUNDRC="${WAVELINUX_ASOUNDRC:-$HOME/.asoundrc}"
START_MARKER="# >>> WaveLinux ALSA aliases >>>"
END_MARKER="# <<< WaveLinux ALSA aliases <<<"

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
    echo "Removed WaveLinux ALSA aliases from $ASOUNDRC"
    ;;
  2)
    echo "WaveLinux ALSA alias block in $ASOUNDRC is missing its end marker" >&2
    exit 1
    ;;
  3)
    echo "No WaveLinux ALSA aliases found in $ASOUNDRC"
    ;;
  *)
    exit "$status"
    ;;
esac
