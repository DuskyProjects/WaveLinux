#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPIMAGE="$ROOT_DIR/target/release/bundle/appimage/WaveLinux_4.0.0_amd64.AppImage"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_BASE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
DESKTOP_FILE="$APP_DIR/wavelinux.desktop"
INSTALLED_APPIMAGE="$BIN_DIR/wavelinux"

if [[ ! -f "$APPIMAGE" ]]; then
  echo "Missing AppImage: $APPIMAGE" >&2
  echo "Run yarn desktop:build first." >&2
  exit 1
fi

install -d "$BIN_DIR" "$APP_DIR" "$ICON_BASE/32x32/apps" "$ICON_BASE/128x128/apps" "$ICON_BASE/256x256/apps" "$ICON_BASE/512x512/apps" "$ICON_BASE/scalable/apps"
install -m 0755 "$APPIMAGE" "$INSTALLED_APPIMAGE"
install -m 0644 "$ROOT_DIR/crates/app/icons/32x32.png" "$ICON_BASE/32x32/apps/wavelinux.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/128x128.png" "$ICON_BASE/128x128/apps/wavelinux.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/128x128@2x.png" "$ICON_BASE/256x256/apps/wavelinux.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/icon.png" "$ICON_BASE/512x512/apps/wavelinux.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/icon.svg" "$ICON_BASE/scalable/apps/wavelinux.svg"

cat > "$DESKTOP_FILE" <<DESKTOP
[Desktop Entry]
Type=Application
Name=WaveLinux
Comment=Linux creator audio mixer
Exec=$INSTALLED_APPIMAGE
Icon=wavelinux
Terminal=false
Categories=Audio;AudioVideo;Mixer;
StartupWMClass=WaveLinux
DESKTOP

chmod 0644 "$DESKTOP_FILE"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q "$ICON_BASE" >/dev/null 2>&1 || true
fi

echo "Installed WaveLinux to $INSTALLED_APPIMAGE"
echo "Installed desktop entry to $DESKTOP_FILE"
