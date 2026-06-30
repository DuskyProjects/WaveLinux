#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPIMAGE_DIR="$ROOT_DIR/target/release/bundle/appimage"
APPIMAGE="$({ find "$APPIMAGE_DIR" -maxdepth 1 -type f -name 'WaveLinux5_*_amd64.AppImage' -print 2>/dev/null || true; } | sort -V | tail -n1)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
SUPPORT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux5"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/wavelinux5"
ICON_BASE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
DESKTOP_FILE="$APP_DIR/wavelinux5.desktop"
LAUNCHER="$BIN_DIR/wavelinux5"
DSP_HELPER="$BIN_DIR/wavelinux5-dsp-helper"
INSTALLED_APPIMAGE="$SUPPORT_DIR/$(basename "${APPIMAGE:-WaveLinux5.AppImage}")"
INSTALLED_SANITIZER="$SUPPORT_DIR/sanitize-runtime-env.sh"
LOCAL_PROFILE_SEED_DIR="$CONFIG_DIR/hardware-profiles/v1/local/wavelinux5-local-seed"

if [[ -z "$APPIMAGE" || ! -f "$APPIMAGE" ]]; then
  echo "Missing WaveLinux5 AppImage in $APPIMAGE_DIR" >&2
  echo "Run bash scripts/build-local.sh first." >&2
  exit 1
fi

stop_wavelinux5_processes() {
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
    if [[ "$signal" == "KILL" ]]; then
      return 0
    fi
    sleep 1
    for pid in "${pids[@]}"; do
      if kill -0 "$pid" 2>/dev/null; then
        kill -KILL "$pid" 2>/dev/null || true
      fi
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

  mapfile -t helper_pids < <(
    ps -eo pid=,args= | awk '
      $0 !~ /awk / && $0 !~ /install-local\.sh/ &&
      /wavelinux5-dsp-helper/ {
        print $1
      }
    ' | sort -u
  )
  stop_pids "WaveLinux5 DSP helper" TERM "${helper_pids[@]}"

  mapfile -t filter_chain_pids < <(
    ps -eo pid=,args= | awk '
      $0 !~ /awk / && $0 !~ /install-local\.sh/ &&
      /pipewire -c .*\/wavelinux5\/effects\/wavelinux5-chain-/ {
        print $1
      }
    ' | sort -u
  )
  stop_pids "WaveLinux5 filter-chain" KILL "${filter_chain_pids[@]}"

  mapfile -t app_pids < <(
    ps -eo pid=,args= | awk '
      $0 !~ /awk / && $0 !~ /install-local\.sh/ &&
      (/(^|[\/ ])wavelinux5([ ]|$)/ ||
       /WaveLinux5_[^ ]*_amd64\.AppImage/) {
        print $1
      }
    ' | sort -u
  )
  mapfile -t app_child_pids < <(collect_descendant_pids "${app_pids[@]}")
  stop_pids "WaveLinux5 app child" KILL "${app_child_pids[@]}"
  stop_pids "WaveLinux5 app" TERM "${app_pids[@]}"
}

cleanup_wavelinux5_audio_modules() {
  if ! command -v pactl >/dev/null 2>&1; then
    return 0
  fi

  mapfile -t modules < <(
    pactl list short modules 2>/dev/null | awk '
      /wavelinux5|WaveLinux5/ {
        print $1
      }
    ' | sort -nu
  )
  if ((${#modules[@]} == 0)); then
    return 0
  fi

  echo "Unloading existing WaveLinux5 audio module(s): ${modules[*]}"
  for module in "${modules[@]}"; do
    pactl unload-module "$module" 2>/dev/null || true
  done
}

stop_wavelinux5_processes
cleanup_wavelinux5_audio_modules

dependency_args=()
if [[ "${WAVELINUX_INSTALL_DEPS:-0}" == "1" ]]; then
  dependency_args+=(--install)
fi
if [[ "${WAVELINUX_INSTALL_EFFECTS:-0}" == "1" ]]; then
  dependency_args+=(--install-effects)
fi
bash "$ROOT_DIR/scripts/check-dependencies.sh" "${dependency_args[@]}"

install -d "$BIN_DIR" "$SUPPORT_DIR" "$APP_DIR" "$ICON_BASE/32x32/apps" "$ICON_BASE/128x128/apps" "$ICON_BASE/256x256/apps" "$ICON_BASE/512x512/apps" "$ICON_BASE/scalable/apps"
rm -f "$SUPPORT_DIR"/WaveLinux5_*_amd64.AppImage
install -m 0755 "$APPIMAGE" "$INSTALLED_APPIMAGE"
install -m 0755 "$ROOT_DIR/scripts/wavelinux-launcher.sh" "$LAUNCHER"
if [[ -x "$ROOT_DIR/target/release/wavelinux5-dsp-helper" ]]; then
  install -m 0755 "$ROOT_DIR/target/release/wavelinux5-dsp-helper" "$DSP_HELPER"
else
  echo "Warning: missing wavelinux5-dsp-helper; run bash scripts/build-local.sh to build it." >&2
fi
install -m 0755 "$ROOT_DIR/scripts/check-dependencies.sh" "$SUPPORT_DIR/check-dependencies.sh"
install -m 0755 "$ROOT_DIR/scripts/install-alsa-aliases.sh" "$SUPPORT_DIR/install-alsa-aliases.sh"
install -m 0755 "$ROOT_DIR/scripts/remove-alsa-aliases.sh" "$SUPPORT_DIR/remove-alsa-aliases.sh"
install -m 0644 "$ROOT_DIR/scripts/sanitize-runtime-env.sh" "$INSTALLED_SANITIZER"
install -m 0644 "$ROOT_DIR/crates/app/icons/32x32.png" "$ICON_BASE/32x32/apps/wavelinux5.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/128x128.png" "$ICON_BASE/128x128/apps/wavelinux5.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/128x128@2x.png" "$ICON_BASE/256x256/apps/wavelinux5.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/icon.png" "$ICON_BASE/512x512/apps/wavelinux5.png"
install -m 0644 "$ROOT_DIR/crates/app/icons/icon.svg" "$ICON_BASE/scalable/apps/wavelinux5.svg"

rm -f \
  "$AUTOSTART_DIR/io.github.duskyprojects.WaveLinux5.desktop" \
  "$AUTOSTART_DIR/WaveLinux5.desktop"

cat > "$DESKTOP_FILE" <<DESKTOP
[Desktop Entry]
Type=Application
Name=WaveLinux5
Comment=Linux creator audio mixer test line
Exec=$LAUNCHER
Icon=wavelinux5
Terminal=false
Categories=Audio;AudioVideo;Mixer;
StartupWMClass=io.github.duskyprojects.WaveLinux5
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
    echo "Warning: hardware profile prewarm failed; WaveLinux5 will try again when it starts." >&2
  }
fi

echo "Installed WaveLinux5 AppImage to $INSTALLED_APPIMAGE"
echo "Installed sanitized launcher to $LAUNCHER"
echo "Installed DSP helper to $DSP_HELPER"
echo "Installed desktop entry to $DESKTOP_FILE"
