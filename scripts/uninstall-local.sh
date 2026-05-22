#!/usr/bin/env bash
set -euo pipefail

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
SUPPORT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
ICON_BASE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

paths=(
  "$BIN_DIR/wavelinux"
  "$BIN_DIR/WaveLinux.AppImage"
  "$BIN_DIR/WaveLinux.AppImage.bak"
  "$APP_DIR/wavelinux.desktop"
  "$APP_DIR/io.github.duskyprojects.WaveLinux.desktop"
  "$APP_DIR/WaveLinux.desktop"
  "$AUTOSTART_DIR/wavelinux.desktop"
  "$AUTOSTART_DIR/io.github.duskyprojects.WaveLinux.desktop"
  "$AUTOSTART_DIR/WaveLinux.desktop"
  "$ICON_BASE/32x32/apps/wavelinux.png"
  "$ICON_BASE/128x128/apps/wavelinux.png"
  "$ICON_BASE/256x256/apps/wavelinux.png"
  "$ICON_BASE/512x512/apps/wavelinux.png"
  "$ICON_BASE/scalable/apps/wavelinux.svg"
)

for path in "${paths[@]}"; do
  if [[ -e "$path" || -L "$path" ]]; then
    rm -f "$path"
    echo "Removed $path"
  fi
done

if [[ -d "$SUPPORT_DIR" ]]; then
  rm -rf "$SUPPORT_DIR"
  echo "Removed $SUPPORT_DIR"
fi

if [[ -x "$ROOT_DIR/scripts/remove-alsa-aliases.sh" ]]; then
  "$ROOT_DIR/scripts/remove-alsa-aliases.sh" || {
    echo "Warning: failed to remove WaveLinux ALSA aliases" >&2
  }
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q "$ICON_BASE" >/dev/null 2>&1 || true
fi

echo "Local WaveLinux install removed. User config under ~/.config/wavelinux was left in place."
