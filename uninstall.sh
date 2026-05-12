#!/usr/bin/env bash
# WaveLinux uninstaller for local/AppImage desktop integration.

set -euo pipefail

CONFIG_DIR="$HOME/.config/wavelinux"
PIPEWIRE_FX_DIR="$HOME/.config/pipewire"
DESKTOP_FILE="$HOME/.local/share/applications/io.github.excalprimeacct_gif.WaveLinux.desktop"
LEGACY_DESKTOP_FILE="$HOME/.local/share/applications/wavelinux.desktop"
ICON_FILE="$HOME/.local/share/icons/hicolor/512x512/apps/wavelinux.png"
LOCK_FILE="$HOME/.wavelinux.lock"
AUTOSTART_FILE="$HOME/.config/autostart/io.github.excalprimeacct_gif.WaveLinux.desktop"
LEGACY_AUTOSTART_FILE="$HOME/.config/autostart/wavelinux.desktop"
WRAPPER_FILE="$HOME/.local/bin/wavelinux"
APPIMAGE_FILE="$HOME/.local/bin/WaveLinux.AppImage"
REMOVE_ARCH_DEPS=0

for arg in "$@"; do
    case "$arg" in
        --remove-arch-deps)
            REMOVE_ARCH_DEPS=1
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            echo "Usage: ./uninstall.sh [--remove-arch-deps]" >&2
            exit 1
            ;;
    esac
done

echo "╔══════════════════════════════════════════╗"
echo "║          WaveLinux Uninstaller           ║"
echo "╚══════════════════════════════════════════╝"
echo ""

echo "→ Tearing down running WaveLinux modules in PipeWire..."
if command -v pactl >/dev/null 2>&1; then
    while IFS= read -r mod_id; do
        [ -n "$mod_id" ] && pactl unload-module "$mod_id" 2>/dev/null || true
    done < <(
        pactl list modules 2>/dev/null \
            | awk '
                /^Module #/ { id=$2; sub("#","",id); next }
                /Argument:.*wavelinux/ { print id }
            '
    )
fi

if command -v pkill >/dev/null 2>&1; then
    pkill -f 'pipewire -c [^ ]*\.config/pipewire/wavelinux-' 2>/dev/null || true
fi

echo "→ Removing config and generated FX configs..."
rm -rf "$CONFIG_DIR"
if [ -d "$PIPEWIRE_FX_DIR" ]; then
    find "$PIPEWIRE_FX_DIR" -maxdepth 1 -type f \
        \( -name 'wavelinux-fx-*.conf' \
        -o -name 'wavelinux-rnnoise-*.conf' \
        -o -name 'wavelinux-chain-*.conf' \) \
        -delete 2>/dev/null || true
fi

echo "→ Removing desktop launchers and icons..."
rm -f \
    "$DESKTOP_FILE" \
    "$LEGACY_DESKTOP_FILE" \
    "$ICON_FILE" \
    "$LOCK_FILE" \
    "$AUTOSTART_FILE" \
    "$LEGACY_AUTOSTART_FILE" \
    "$WRAPPER_FILE" \
    "$APPIMAGE_FILE"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

if [ "${REMOVE_ARCH_DEPS}" -eq 1 ]; then
    if command -v pacman >/dev/null 2>&1; then
        echo "→ Removing optional Arch-only WaveLinux packages..."
        sudo pacman -Rns --noconfirm \
            swh-plugins rnnoise noise-suppression-for-voice 2>/dev/null || true
    else
        echo "--remove-arch-deps was requested, but pacman is not available; skipping."
    fi
fi

echo ""
echo "✅  WaveLinux uninstalled."
echo ""
echo "If you also want to remove the cloned source tree, run:"
echo "  rm -rf \"$(cd "$(dirname "$0")" && pwd)\""
