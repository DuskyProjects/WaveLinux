#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/crates/app/appimage-extra/usr/wavelinux-runtime"
BIN_DIR="$OUT_DIR/bin"
LIB_DIR="$OUT_DIR/lib"

rm -rf "$BIN_DIR" "$LIB_DIR"
install -d "$BIN_DIR" "$LIB_DIR"
touch "$BIN_DIR/.gitkeep" "$LIB_DIR/.gitkeep"

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
