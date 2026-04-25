#!/bin/bash
# WaveLinux Installer for CachyOS / Arch-based distros (KDE)
set -e

echo "╔══════════════════════════════════════════╗"
echo "║        WaveLinux Installer               ║"
echo "╚══════════════════════════════════════════╝"
echo ""

# Install dependencies via pacman.
# Why each one is here:
#   python / python-pyqt6 — runtime + GUI
#   pipewire / pipewire-pulse / wireplumber — the audio server we drive
#   libpulse — provides `pactl` and `parec` (peak meters use parec)
#   rnnoise — base library for the noise-suppressor LADSPA plugin
#   swh-plugins — provides sc4_1882 (compressor), gate_1410 (gate), and
#                 fast_lookahead_limiter_1913 (limiter / Clipguard).
#                 NOTE: WaveLinux ships a builtin clamp-based fallback
#                 for the limiter, so Clipguard works without
#                 swh-plugins; the LADSPA limiter is just better-sounding
#                 when present.
echo "→ Installing dependencies..."
sudo pacman -S --needed --noconfirm \
    python \
    python-pyqt6 \
    pipewire \
    pipewire-pulse \
    wireplumber \
    libpulse \
    rnnoise \
    swh-plugins

# Install the RNNoise LADSPA plugin from the AUR. Required for the
# Noise-Suppression effect; without it, the FX dialog grey-outs that row.
# Skipped if WaveLinux's installer is being run inside an environment
# without an AUR helper (CI, docker, fresh chroot).
echo "→ Installing RNNoise plugin (noise-suppression-for-voice)..."
if ! pacman -Qi noise-suppression-for-voice > /dev/null 2>&1; then
    if command -v paru >/dev/null 2>&1; then
        paru -S --needed --noconfirm noise-suppression-for-voice
    elif command -v yay >/dev/null 2>&1; then
        yay -S --needed --noconfirm noise-suppression-for-voice
    else
        echo "⚠ Could not find 'paru' or 'yay'. Please install 'noise-suppression-for-voice' manually from the AUR."
    fi
else
    echo "  Already installed."
fi

# Install .desktop file & Icon
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DESKTOP_FILE="$HOME/.local/share/applications/wavelinux.desktop"
ICON_DIR="$HOME/.local/share/icons/hicolor/512x512/apps"

echo "→ Installing desktop launcher and icon..."
mkdir -p "$HOME/.local/share/applications"
mkdir -p "$ICON_DIR"

if [ -f "$SCRIPT_DIR/icon.png" ]; then
    cp "$SCRIPT_DIR/icon.png" "$ICON_DIR/wavelinux.png"
fi

cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Name=WaveLinux
Comment=PipeWire Audio Router & Mixer
Exec=python3 ${SCRIPT_DIR}/main.py
Icon=wavelinux
Type=Application
Categories=AudioVideo;Audio;Mixer;
Keywords=audio;mixer;pipewire;routing;
StartupNotify=true
EOF

# Refresh the desktop-file database so the KDE menu picks up the launcher.
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

echo ""
echo "✅  WaveLinux installed!"
echo ""
echo "You can now:"
echo "  1. Find 'WaveLinux' in your KDE application menu"
echo "  2. Or run it directly:  python3 ${SCRIPT_DIR}/main.py"
echo ""
echo "To uninstall and wipe state (useful if app names look stuck on"
echo "old values like 'audio-src'):"
echo "  ${SCRIPT_DIR}/uninstall.sh"
echo ""
