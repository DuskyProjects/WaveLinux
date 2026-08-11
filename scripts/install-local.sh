#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/wavelinux-processes.sh
source "$ROOT_DIR/scripts/wavelinux-processes.sh"
APPIMAGE_DIR="$ROOT_DIR/target/release/bundle/appimage"
APPIMAGE="$({ find "$APPIMAGE_DIR" -maxdepth 1 -type f -name 'WaveLinux6_*_amd64.AppImage' -print 2>/dev/null || true; } | sort -V | tail -n1)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
SUPPORT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux6"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wavelinux6"
ICON_BASE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
DESKTOP_FILE="$APP_DIR/wavelinux6.desktop"
LAUNCHER="$BIN_DIR/wavelinux6"
DSP_HELPER="$BIN_DIR/wavelinux6-audio-core"
PERIPHERAL_PLUGIN="$BIN_DIR/wavelinux6-peripheral-plugin"
INSTALLED_APPIMAGE="$SUPPORT_DIR/$(basename "${APPIMAGE:-WaveLinux6.AppImage}")"
INSTALLED_SANITIZER="$SUPPORT_DIR/sanitize-runtime-env.sh"
INSTALLED_PROCESS_MATCHER="$SUPPORT_DIR/wavelinux-processes.sh"
LOCAL_PROFILE_SEED_DIR="$CONFIG_DIR/hardware-profiles/v1/local/wavelinux6-local-seed"

if [[ -z "$APPIMAGE" || ! -f "$APPIMAGE" ]]; then
  echo "Missing WaveLinux6 AppImage in $APPIMAGE_DIR" >&2
  echo "Run bash scripts/build-local.sh first." >&2
  exit 1
fi

stop_previous_wavelinux_processes() {
  stop_pids() {
    local label="$1"
    local signal="$2"
    shift
    shift
    local pids=("$@")
    if ((${#pids[@]} == 0)); then
      return 0
    fi
    echo "Stopping existing $label process(es): ${pids[*]}"
    kill "-$signal" "${pids[@]}" 2>/dev/null || true
    if [[ "$signal" != "KILL" ]]; then
      for _ in {1..50}; do
        local running=0
        for pid in "${pids[@]}"; do
          if kill -0 "$pid" 2>/dev/null; then
            running=1
            break
          fi
        done
        ((running == 0)) && break
        sleep 0.02
      done
      for pid in "${pids[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
          kill -KILL "$pid" 2>/dev/null || true
        fi
      done
    fi
    for _ in {1..25}; do
      local running=0
      for pid in "${pids[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
          running=1
          break
        fi
      done
      ((running == 0)) && break
      sleep 0.02
    done
  }

  collect_descendant_pids() {
    local queue=("$@")
    local descendants=()
    local pid child children
    while ((${#queue[@]})); do
      pid="${queue[0]}"
      queue=("${queue[@]:1}")
      mapfile -t children < <(pgrep -P "$pid" 2>/dev/null || true)
      for child in "${children[@]}"; do
        descendants+=("$child")
        queue+=("$child")
      done
    done
    if ((${#descendants[@]})); then
      printf '%s\n' "${descendants[@]}" | sort -u
    fi
  }

  mapfile -t app_pids < <(wavelinux_collect_process_pids app | sort -u)
  mapfile -t legacy_app_pids < <(wavelinux_collect_process_pids legacy-app | sort -u)
  mapfile -t app_child_pids < <(
    collect_descendant_pids "${app_pids[@]}" "${legacy_app_pids[@]}"
  )
  # Stop the graph owner first. Killing its helpers while it is still alive can
  # make the engine reconstruct them during installation.
  stop_pids "WaveLinux app" TERM "${app_pids[@]}"
  stop_pids "WaveLinux5 app" TERM "${legacy_app_pids[@]}"
  stop_pids "WaveLinux app child" KILL "${app_child_pids[@]}"

  mapfile -t helper_pids < <(wavelinux_collect_process_pids audio-core | sort -u)
  stop_pids "WaveLinux audio core" TERM "${helper_pids[@]}"

  mapfile -t legacy_helper_pids < <(wavelinux_collect_process_pids legacy-helper | sort -u)
  stop_pids "WaveLinux5 DSP helper" TERM "${legacy_helper_pids[@]}"

  mapfile -t peripheral_pids < <(wavelinux_collect_process_pids peripheral | sort -u)
  stop_pids "WaveLinux peripheral plugin" TERM "${peripheral_pids[@]}"

  mapfile -t filter_chain_pids < <(wavelinux_collect_filter_chain_pids | sort -u)
  stop_pids "WaveLinux filter-chain" KILL "${filter_chain_pids[@]}"

  mapfile -t legacy_filter_chain_pids < <(wavelinux_collect_legacy_filter_chain_pids | sort -u)
  stop_pids "WaveLinux5 filter-chain" KILL "${legacy_filter_chain_pids[@]}"

}

cleanup_previous_wavelinux_audio_modules() {
  if ! command -v pactl >/dev/null 2>&1; then
    return 0
  fi

  mapfile -t modules < <(
    pactl list short modules 2>/dev/null | awk '
      /wavelinux6|WaveLinux6/ {
        priority = 50
        if ($2 == "module-loopback") {
          priority = 10
        } else if ($2 == "module-remap-source") {
          priority = 20
        } else if ($2 == "module-null-sink") {
          priority = 30
        }
        printf "%03d %s\n", priority, $1
      }
    ' | sort -k1,1n -k2,2n | awk '{ print $2 }'
  )
  if ((${#modules[@]} == 0)); then
    return 0
  fi

  echo "Unloading existing WaveLinux audio module(s): ${modules[*]}"
  for module in "${modules[@]}"; do
    pactl unload-module "$module" 2>/dev/null || true
  done
}

stop_previous_wavelinux_processes
cleanup_previous_wavelinux_audio_modules

dependency_args=()
if [[ "${WAVELINUX_INSTALL_DEPS:-0}" == "1" ]]; then
  dependency_args+=(--install)
fi
bash "$ROOT_DIR/scripts/check-dependencies.sh" "${dependency_args[@]}"

install -d "$BIN_DIR" "$SUPPORT_DIR" "$APP_DIR" "$ICON_BASE/32x32/apps" "$ICON_BASE/128x128/apps" "$ICON_BASE/256x256/apps" "$ICON_BASE/512x512/apps" "$ICON_BASE/scalable/apps"
rm -f "$SUPPORT_DIR"/WaveLinux6_*_amd64.AppImage
install -m 0755 "$APPIMAGE" "$INSTALLED_APPIMAGE"
install -m 0755 "$ROOT_DIR/scripts/wavelinux-launcher.sh" "$LAUNCHER"
if [[ -x "$ROOT_DIR/target/release/wavelinux6-audio-core" ]]; then
  install -m 0755 "$ROOT_DIR/target/release/wavelinux6-audio-core" "$DSP_HELPER"
else
  echo "Warning: missing wavelinux6-audio-core; run bash scripts/build-local.sh to build it." >&2
fi
if [[ -x "$ROOT_DIR/target/release/wavelinux6-peripheral-plugin" ]]; then
  install -m 0755 "$ROOT_DIR/target/release/wavelinux6-peripheral-plugin" "$PERIPHERAL_PLUGIN"
else
  echo "Warning: missing wavelinux6-peripheral-plugin; run bash scripts/build-local.sh to build it." >&2
fi
install -m 0755 "$ROOT_DIR/scripts/check-dependencies.sh" "$SUPPORT_DIR/check-dependencies.sh"
install -m 0755 "$ROOT_DIR/scripts/runtime-dependencies.sh" "$SUPPORT_DIR/runtime-dependencies.sh"
install -m 0755 "$ROOT_DIR/scripts/verify-install.sh" "$SUPPORT_DIR/verify-install.sh"
install -m 0755 "$ROOT_DIR/scripts/wavelinux-processes.sh" "$INSTALLED_PROCESS_MATCHER"
install -m 0755 "$ROOT_DIR/scripts/install-alsa-aliases.sh" "$SUPPORT_DIR/install-alsa-aliases.sh"
install -m 0755 "$ROOT_DIR/scripts/remove-alsa-aliases.sh" "$SUPPORT_DIR/remove-alsa-aliases.sh"
install -m 0644 "$ROOT_DIR/scripts/sanitize-runtime-env.sh" "$INSTALLED_SANITIZER"
install -m 0644 "$ROOT_DIR/crates/app/icons/32x32.png" "$ICON_BASE/32x32/apps/wavelinux6.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/128x128.png" "$ICON_BASE/128x128/apps/wavelinux6.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/128x128@2x.png" "$ICON_BASE/256x256/apps/wavelinux6.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/icon.png" "$ICON_BASE/512x512/apps/wavelinux6.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/icon.svg" "$ICON_BASE/scalable/apps/wavelinux6.svg"

# Keep the old config until WaveLinux 6 validates its first graph. Everything
# else from the replaced WaveLinux5 installation can be removed immediately.
rm -f \
  "$BIN_DIR/wavelinux5" \
  "$BIN_DIR/wavelinux5-dsp-helper" \
  "$APP_DIR/wavelinux5.desktop" \
  "$APP_DIR/io.github.duskyprojects.WaveLinux5.desktop" \
  "$APP_DIR/WaveLinux5.desktop" \
  "$AUTOSTART_DIR/wavelinux5.desktop" \
  "$AUTOSTART_DIR/io.github.duskyprojects.WaveLinux5.desktop" \
  "$AUTOSTART_DIR/WaveLinux5.desktop" \
  "$ICON_BASE/32x32/apps/wavelinux5.png" \
  "$ICON_BASE/128x128/apps/wavelinux5.png" \
  "$ICON_BASE/256x256/apps/wavelinux5.png" \
  "$ICON_BASE/512x512/apps/wavelinux5.png" \
  "$ICON_BASE/scalable/apps/wavelinux5.svg"
rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux5"

rm -f \
  "$AUTOSTART_DIR/io.github.duskyprojects.WaveLinux6.desktop" \
  "$AUTOSTART_DIR/WaveLinux6.desktop"

cat > "$DESKTOP_FILE" <<DESKTOP
[Desktop Entry]
Type=Application
Name=WaveLinux 6
Comment=Linux creator audio mixer
Exec=$LAUNCHER
Icon=wavelinux6
Terminal=false
Categories=Audio;AudioVideo;Mixer;
StartupWMClass=io.github.duskyprojects.WaveLinux6
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

if [[ "${WAVELINUX_INSTALL_ALSA_ALIASES:-1}" != "0" ]]; then
  WAVELINUX_APP_DISPLAY_NAME="WaveLinux 6" \
    WAVELINUX_GRAPH_PREFIX=wavelinux6 \
    WAVELINUX_CONFIG_DIR="$CONFIG_DIR" \
    "$ROOT_DIR/scripts/install-alsa-aliases.sh" || {
      echo "Warning: failed to install WaveLinux6 ALSA aliases" >&2
    }
else
  echo "Skipped ALSA aliases. Run yarn install:alsa-aliases if an ALSA-only app needs WaveLinux6 devices."
fi

if [[ "${WAVELINUX_PREWARM_HARDWARE_PROFILES:-1}" != "0" ]]; then
  echo "Checking audio hardware for signed WaveLinux profiles..."
  "$LAUNCHER" --prewarm-hardware-profiles || {
    echo "Warning: hardware profile prewarm failed; WaveLinux6 will try again when it starts." >&2
  }
fi

echo "Installed WaveLinux6 AppImage to $INSTALLED_APPIMAGE"
echo "Installed sanitized launcher to $LAUNCHER"
echo "Installed audio core to $DSP_HELPER"
echo "Installed peripheral plugin to $PERIPHERAL_PLUGIN"
echo "Installed desktop entry to $DESKTOP_FILE"
