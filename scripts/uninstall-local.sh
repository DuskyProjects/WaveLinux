#!/usr/bin/env bash
set -euo pipefail

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
SUPPORT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux6"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
ICON_BASE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"

stop_wavelinux6_processes() {
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
      $0 !~ /awk / && $0 !~ /uninstall-local\.sh/ &&
      /wavelinux6-audio-core/ {
        print $1
      }
    ' | sort -u
  )
  stop_pids "WaveLinux6 audio core" TERM "${helper_pids[@]}"

  mapfile -t peripheral_pids < <(
    ps -eo pid=,args= | awk '
      $0 !~ /awk / && $0 !~ /uninstall-local\.sh/ &&
      /wavelinux6-peripheral-plugin/ {
        print $1
      }
    ' | sort -u
  )
  stop_pids "WaveLinux6 peripheral plugin" TERM "${peripheral_pids[@]}"

  mapfile -t filter_chain_pids < <(
    ps -eo pid=,args= | awk '
      $0 !~ /awk / && $0 !~ /uninstall-local\.sh/ &&
      /pipewire -c .*\/wavelinux6\/effects\/wavelinux6-chain-/ {
        print $1
      }
    ' | sort -u
  )
  stop_pids "WaveLinux6 filter-chain" KILL "${filter_chain_pids[@]}"

  mapfile -t app_pids < <(
    ps -eo pid=,args= | awk '
      $0 !~ /awk / && $0 !~ /uninstall-local\.sh/ &&
      (/(^|[\/ ])wavelinux6([ ]|$)/ ||
       /WaveLinux6_[^ ]*_amd64\.AppImage/) {
        print $1
      }
    ' | sort -u
  )
  mapfile -t app_child_pids < <(collect_descendant_pids "${app_pids[@]}")
  stop_pids "WaveLinux6 app child" KILL "${app_child_pids[@]}"
  stop_pids "WaveLinux6 app" TERM "${app_pids[@]}"
}

cleanup_wavelinux6_audio_modules() {
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

  echo "Unloading existing WaveLinux6 audio module(s): ${modules[*]}"
  for module in "${modules[@]}"; do
    pactl unload-module "$module" 2>/dev/null || true
  done
}

cleanup_wavelinux6_audio_modules
stop_wavelinux6_processes
cleanup_wavelinux6_audio_modules

paths=(
  "$BIN_DIR/wavelinux6"
  "$BIN_DIR/wavelinux6-audio-core"
  "$BIN_DIR/wavelinux6-peripheral-plugin"
  "$BIN_DIR/WaveLinux6.AppImage"
  "$BIN_DIR/WaveLinux6.AppImage.bak"
  "$APP_DIR/wavelinux6.desktop"
  "$APP_DIR/io.github.duskyprojects.WaveLinux6.desktop"
  "$APP_DIR/WaveLinux6.desktop"
  "$AUTOSTART_DIR/wavelinux6.desktop"
  "$AUTOSTART_DIR/io.github.duskyprojects.WaveLinux6.desktop"
  "$AUTOSTART_DIR/WaveLinux6.desktop"
  "$ICON_BASE/32x32/apps/wavelinux6.png"
  "$ICON_BASE/128x128/apps/wavelinux6.png"
  "$ICON_BASE/256x256/apps/wavelinux6.png"
  "$ICON_BASE/512x512/apps/wavelinux6.png"
  "$ICON_BASE/scalable/apps/wavelinux6.svg"
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

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q "$ICON_BASE" >/dev/null 2>&1 || true
fi

echo "Local WaveLinux6 install removed. User config under ~/.config/wavelinux6 was left in place."
