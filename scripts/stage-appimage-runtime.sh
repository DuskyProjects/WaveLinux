#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/crates/app/appimage-extra/usr/wavelinux-runtime"
BIN_DIR="$OUT_DIR/bin"
LIB_DIR="$OUT_DIR/lib"
LADSPA_DIR="$LIB_DIR/ladspa"
STARTUP_LIB_DIR="$ROOT_DIR/crates/app/appimage-extra/usr/lib"

rm -rf "$BIN_DIR" "$LIB_DIR" "$STARTUP_LIB_DIR"
install -d "$BIN_DIR" "$LIB_DIR" "$LADSPA_DIR" "$STARTUP_LIB_DIR"
touch "$BIN_DIR/.gitkeep" "$LIB_DIR/.gitkeep" "$LADSPA_DIR/.gitkeep" "$STARTUP_LIB_DIR/.gitkeep"

stage_binary() {
  local name="$1"
  local path
  path="$(command -v "$name" 2>/dev/null || true)"
  if [[ -n "$path" && -x "$path" ]]; then
    install -m 0755 "$path" "$BIN_DIR/$name"
    echo "Staged AppImage runtime helper: $name"
  else
    echo "Warning: AppImage runtime helper not found: $name" >&2
  fi
}

stage_library() {
  local output_name="$1"
  shift
  local path
  for path in "$@"; do
    if [[ -r "$path" ]]; then
      install -m 0644 "$path" "$LIB_DIR/$output_name"
      echo "Staged AppImage runtime library: $output_name"
      return 0
    fi
  done
  echo "Warning: AppImage runtime library not found: $output_name" >&2
}

stage_startup_library() {
  local output_name="$1"
  shift
  local path
  for path in "$@"; do
    if [[ -r "$path" ]]; then
      install -m 0644 "$path" "$STARTUP_LIB_DIR/$output_name"
      echo "Staged AppImage startup library: $output_name"
      return 0
    fi
  done
  echo "Warning: AppImage startup library not found: $output_name" >&2
}

existing_ladspa_paths() {
  local paths=()
  if [[ -n "${LADSPA_PATH:-}" ]]; then
    IFS=':' read -r -a paths <<< "$LADSPA_PATH"
  fi
  paths+=(
    /usr/lib/ladspa
    /usr/lib64/ladspa
    /usr/local/lib/ladspa
    /usr/local/lib64/ladspa
    /usr/lib/x86_64-linux-gnu/ladspa
    /usr/lib/aarch64-linux-gnu/ladspa
    /usr/lib/arm-linux-gnueabihf/ladspa
  )
  printf '%s\n' "${paths[@]}" | awk 'NF && !seen[$0]++'
}

stage_ladspa_plugins() {
  local label="$1"
  shift
  local root pattern path basename staged=0
  local -A seen=()
  while IFS= read -r root; do
    [[ -d "$root" ]] || continue
    for pattern in "$@"; do
      for path in "$root"/$pattern; do
        [[ -f "$path" ]] || continue
        basename="$(basename "$path")"
        [[ -n "${seen[$basename]:-}" ]] && continue
        seen[$basename]=1
        install -m 0644 "$path" "$LADSPA_DIR/$basename"
        echo "Staged AppImage LADSPA plugin: $basename"
        staged=1
      done
    done
  done < <(existing_ladspa_paths)

  if (( staged == 0 )); then
    echo "Warning: AppImage LADSPA plugin not found: $label" >&2
  fi
}

stage_binary bwrap
stage_binary xdg-dbus-proxy

stage_library libusb-1.0.so.0 \
  /usr/lib/x86_64-linux-gnu/libusb-1.0.so.0 \
  /usr/lib64/libusb-1.0.so.0 \
  /usr/lib/libusb-1.0.so.0

stage_library libcap.so.2 \
  /usr/lib/x86_64-linux-gnu/libcap.so.2 \
  /usr/lib64/libcap.so.2 \
  /usr/lib/libcap.so.2

stage_startup_library libfribidi.so.0 \
  /usr/lib/x86_64-linux-gnu/libfribidi.so.0 \
  /usr/lib64/libfribidi.so.0 \
  /usr/lib/libfribidi.so.0

stage_startup_library libfontconfig.so.1 \
  /usr/lib/x86_64-linux-gnu/libfontconfig.so.1 \
  /usr/lib64/libfontconfig.so.1 \
  /usr/lib/libfontconfig.so.1

stage_startup_library libfreetype.so.6 \
  /usr/lib/x86_64-linux-gnu/libfreetype.so.6 \
  /usr/lib64/libfreetype.so.6 \
  /usr/lib/libfreetype.so.6

stage_startup_library libharfbuzz.so.0 \
  /usr/lib/x86_64-linux-gnu/libharfbuzz.so.0 \
  /usr/lib64/libharfbuzz.so.0 \
  /usr/lib/libharfbuzz.so.0

stage_startup_library libgraphite2.so.3 \
  /usr/lib/x86_64-linux-gnu/libgraphite2.so.3 \
  /usr/lib64/libgraphite2.so.3 \
  /usr/lib/libgraphite2.so.3

stage_startup_library libX11.so.6 \
  /usr/lib/x86_64-linux-gnu/libX11.so.6 \
  /usr/lib64/libX11.so.6 \
  /usr/lib/libX11.so.6

stage_startup_library libxcb.so.1 \
  /usr/lib/x86_64-linux-gnu/libxcb.so.1 \
  /usr/lib64/libxcb.so.1 \
  /usr/lib/libxcb.so.1

stage_startup_library libXau.so.6 \
  /usr/lib/x86_64-linux-gnu/libXau.so.6 \
  /usr/lib64/libXau.so.6 \
  /usr/lib/libXau.so.6

stage_startup_library libXdmcp.so.6 \
  /usr/lib/x86_64-linux-gnu/libXdmcp.so.6 \
  /usr/lib64/libXdmcp.so.6 \
  /usr/lib/libXdmcp.so.6

stage_startup_library libgbm.so.1 \
  /usr/lib/x86_64-linux-gnu/libgbm.so.1 \
  /usr/lib64/libgbm.so.1 \
  /usr/lib/libgbm.so.1

stage_startup_library libdrm.so.2 \
  /usr/lib/x86_64-linux-gnu/libdrm.so.2 \
  /usr/lib64/libdrm.so.2 \
  /usr/lib/libdrm.so.2

stage_startup_library libEGL.so.1 \
  /usr/lib/x86_64-linux-gnu/libEGL.so.1 \
  /usr/lib64/libEGL.so.1 \
  /usr/lib/libEGL.so.1

stage_startup_library libGLX.so.0 \
  /usr/lib/x86_64-linux-gnu/libGLX.so.0 \
  /usr/lib64/libGLX.so.0 \
  /usr/lib/libGLX.so.0

stage_startup_library libGLdispatch.so.0 \
  /usr/lib/x86_64-linux-gnu/libGLdispatch.so.0 \
  /usr/lib64/libGLdispatch.so.0 \
  /usr/lib/libGLdispatch.so.0

stage_startup_library libX11-xcb.so.1 \
  /usr/lib/x86_64-linux-gnu/libX11-xcb.so.1 \
  /usr/lib64/libX11-xcb.so.1 \
  /usr/lib/libX11-xcb.so.1

stage_ladspa_plugins "DeepFilterNet3" \
  libdeep_filter_ladspa.so \
  deep_filter_ladspa.so \
  libdeepfilternet_ladspa.so \
  deepfilternet_ladspa.so \
  libdeep_filter_net_ladspa.so \
  deep_filter_net_ladspa.so

stage_ladspa_plugins "RNNoise" \
  librnnoise_ladspa.so \
  rnnoise_ladspa.so

stage_ladspa_plugins "SWH compressor/gate/limiter" \
  sc4_1882.so \
  compressor.so \
  gate_1410.so \
  fast_lookahead_limiter_1913.so \
  hard_limiter_1413.so
