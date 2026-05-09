#!/bin/bash
# WaveLinux Uninstaller — removes the desktop entry / icon / config so a
# fresh `install.sh` is genuinely fresh. Optionally removes the pacman
# packages we pulled in. Does NOT touch PipeWire's own config.

set -e

CONFIG_DIR="$HOME/.config/wavelinux"
PIPEWIRE_FX_DIR="$HOME/.config/pipewire"
DESKTOP_FILE="$HOME/.local/share/applications/wavelinux.desktop"
ICON_FILE="$HOME/.local/share/icons/hicolor/512x512/apps/wavelinux.png"
LOCK_FILE="$HOME/.wavelinux.lock"
AUTOSTART_FILE="$HOME/.config/autostart/wavelinux.desktop"
WRAPPER_FILE="$HOME/.local/bin/wavelinux"

echo "╔══════════════════════════════════════════╗"
echo "║       WaveLinux Uninstaller              ║"
echo "╚══════════════════════════════════════════╝"
echo ""

# Unload any WaveLinux pactl modules that are still in PipeWire's running
# state. Without this the FX chains, virtual sinks, and submix loopbacks
# we created stay loaded until the next PipeWire restart.
echo "→ Tearing down running WaveLinux modules in PipeWire..."
if command -v pactl >/dev/null 2>&1; then
    # Modules whose Argument= line mentions any of our names.
    while IFS= read -r mod_id; do
        [ -n "$mod_id" ] && pactl unload-module "$mod_id" 2>/dev/null || true
    done < <(
        pactl list modules 2>/dev/null \
            | awk '
                /^Module #/ { id=$2; sub("#","",id); next }
                /Argument:.*wavelinux/ { print id }
            '
    )
    # Also kill any lingering filter-chain pipewire client we spawned.
    # The pattern is anchored to OUR canonical config location +
    # filename prefix so we don't accidentally kill an unrelated user
    # `pipewire -c` invocation that happens to have the substring
    # "wavelinux" elsewhere in its command line.
    pkill -f 'pipewire -c [^ ]*\.config/pipewire/wavelinux-' 2>/dev/null || true
fi

# Per-user state. This is the bit that fixes "stuck audio-src" caches —
# clearing config.json wipes app_routing / app_last_seen so app names get
# re-resolved from scratch the next time you start WaveLinux.
echo "→ Removing config and FX configs..."
rm -rf "$CONFIG_DIR"
# We only delete pipewire configs *we* generated, never the user's own.
if [ -d "$PIPEWIRE_FX_DIR" ]; then
    find "$PIPEWIRE_FX_DIR" -maxdepth 1 -type f \
        \( -name 'wavelinux-fx-*.conf' \
        -o -name 'wavelinux-rnnoise-*.conf' \
        -o -name 'wavelinux-chain-*.conf' \) \
        -delete 2>/dev/null || true
fi

# Desktop integration. The wrapper at ~/.local/bin/wavelinux is what
# the .desktop's Exec= line points at; remove both together.
echo "→ Removing desktop launcher, wrapper and icon..."
rm -f "$DESKTOP_FILE" "$ICON_FILE" "$LOCK_FILE" "$AUTOSTART_FILE" "$WRAPPER_FILE"

# Refresh the desktop database so KDE / GNOME drop the stale launcher.
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

# Optional package removal. Defaults to NO — these packages are useful for
# more than just WaveLinux (rnnoise / swh-plugins are reused by Carla,
# OBS plugins, JACK setups, etc.) and removing them can break unrelated
# software. We ask first and never strip pipewire itself.
echo ""
if [ -t 0 ]; then
    read -r -p "Also remove WaveLinux pacman dependencies (swh-plugins, rnnoise, noise-suppression-for-voice)? [y/N] " yn
else
    yn="n"
    echo "Non-interactive session detected; keeping pacman packages installed."
fi
case "$yn" in
    [yY]|[yY][eE][sS])
        echo "→ Removing optional packages..."
        sudo pacman -Rns --noconfirm \
            swh-plugins rnnoise noise-suppression-for-voice 2>/dev/null || true
        ;;
    *)
        echo "  Keeping pacman packages installed."
        ;;
esac

echo ""
echo "✅  WaveLinux uninstalled."
echo ""
echo "If you also want to remove the cloned source tree, run:"
echo "  rm -rf \"$(cd "$(dirname "$0")" && pwd)\""
echo ""
