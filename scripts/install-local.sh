#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPIMAGE="$ROOT_DIR/target/release/bundle/appimage/WaveLinux_4.1.3_amd64.AppImage"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
SUPPORT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wavelinux"
ICON_BASE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
DESKTOP_FILE="$APP_DIR/wavelinux.desktop"
LAUNCHER="$BIN_DIR/wavelinux"
INSTALLED_APPIMAGE="$SUPPORT_DIR/WaveLinux_4.1.3_amd64.AppImage"
INSTALLED_SANITIZER="$SUPPORT_DIR/sanitize-runtime-env.sh"
LOCAL_PROFILE_SEED_DIR="$CONFIG_DIR/hardware-profiles/v1/local/wavelinux-local-seed"

if [[ ! -f "$APPIMAGE" ]]; then
  echo "Missing AppImage: $APPIMAGE" >&2
  echo "Run bash scripts/build-local.sh first." >&2
  exit 1
fi

dependency_args=()
if [[ "${WAVELINUX_INSTALL_DEPS:-0}" == "1" ]]; then
  dependency_args+=(--install)
fi
if [[ "${WAVELINUX_INSTALL_EFFECTS:-0}" == "1" ]]; then
  dependency_args+=(--install-effects)
fi
bash "$ROOT_DIR/scripts/check-dependencies.sh" "${dependency_args[@]}"

install -d "$BIN_DIR" "$SUPPORT_DIR" "$APP_DIR" "$ICON_BASE/32x32/apps" "$ICON_BASE/128x128/apps" "$ICON_BASE/256x256/apps" "$ICON_BASE/512x512/apps" "$ICON_BASE/scalable/apps"
install -m 0755 "$APPIMAGE" "$INSTALLED_APPIMAGE"
install -m 0755 "$ROOT_DIR/scripts/wavelinux-launcher.sh" "$LAUNCHER"
install -m 0755 "$ROOT_DIR/scripts/check-dependencies.sh" "$SUPPORT_DIR/check-dependencies.sh"
install -m 0755 "$ROOT_DIR/scripts/install-alsa-aliases.sh" "$SUPPORT_DIR/install-alsa-aliases.sh"
install -m 0755 "$ROOT_DIR/scripts/remove-alsa-aliases.sh" "$SUPPORT_DIR/remove-alsa-aliases.sh"
install -m 0644 "$ROOT_DIR/scripts/sanitize-runtime-env.sh" "$INSTALLED_SANITIZER"
install -m 0644 "$ROOT_DIR/crates/app/icons/32x32.png" "$ICON_BASE/32x32/apps/wavelinux.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/128x128.png" "$ICON_BASE/128x128/apps/wavelinux.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/128x128@2x.png" "$ICON_BASE/256x256/apps/wavelinux.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/icon.png" "$ICON_BASE/512x512/apps/wavelinux.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/icon.svg" "$ICON_BASE/scalable/apps/wavelinux.svg"

rm -f \
  "$AUTOSTART_DIR/io.github.duskyprojects.WaveLinux.desktop" \
  "$AUTOSTART_DIR/WaveLinux.desktop"

cat > "$DESKTOP_FILE" <<DESKTOP
[Desktop Entry]
Type=Application
Name=WaveLinux
Comment=Linux creator audio mixer
Exec=$LAUNCHER
Icon=wavelinux
Terminal=false
Categories=Audio;AudioVideo;Mixer;
StartupWMClass=io.github.duskyprojects.WaveLinux
DESKTOP

chmod 0644 "$DESKTOP_FILE"

if [[ "${WAVELINUX_INSTALL_LOCAL_PROFILE_SEEDS:-1}" != "0" && -d "$ROOT_DIR/profiles/v1/devices" ]]; then
  rm -rf "$LOCAL_PROFILE_SEED_DIR"
  install -d "$LOCAL_PROFILE_SEED_DIR"
  find "$ROOT_DIR/profiles/v1/devices" -maxdepth 1 -type f -name '*.json' -exec install -m 0644 {} "$LOCAL_PROFILE_SEED_DIR/" \;
  echo "Installed local hardware profile seeds to $LOCAL_PROFILE_SEED_DIR"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q "$ICON_BASE" >/dev/null 2>&1 || true
fi

if [[ "${WAVELINUX_INSTALL_ALSA_ALIASES:-0}" == "1" ]]; then
  "$ROOT_DIR/scripts/install-alsa-aliases.sh" || {
    echo "Warning: failed to install WaveLinux ALSA aliases" >&2
  }
else
  echo "Skipped ALSA aliases. Run yarn install:alsa-aliases if an ALSA-only app needs WaveLinux devices."
fi

if [[ "${WAVELINUX_PREWARM_HARDWARE_PROFILES:-1}" != "0" ]]; then
  echo "Checking audio hardware for signed WaveLinux profiles..."
  "$LAUNCHER" --prewarm-hardware-profiles || {
    echo "Warning: hardware profile prewarm failed; WaveLinux will try again when it starts." >&2
  }
fi

echo "Installed WaveLinux AppImage to $INSTALLED_APPIMAGE"
echo "Installed sanitized launcher to $LAUNCHER"
echo "Installed desktop entry to $DESKTOP_FILE"
