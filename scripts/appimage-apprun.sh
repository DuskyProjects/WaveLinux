#!/usr/bin/env bash
set -euo pipefail

this_dir="$(readlink -f "$(dirname "$0")")"
runtime_dir="$this_dir/usr/wavelinux-runtime"
runtime_bin_dir="$runtime_dir/bin"
dependency_script="$runtime_bin_dir/check-dependencies.sh"

run_dependency_helper() {
  if [[ ! -x "$dependency_script" ]]; then
    echo "WaveLinux AppImage dependency helper is missing: $dependency_script" >&2
    return 127
  fi

  APPDIR="${APPDIR:-$this_dir}" \
    PATH="$runtime_bin_dir:$PATH" \
    "$dependency_script" "$@"
}

check_runtime_dependencies() {
  run_dependency_helper --strict-runtime "$@"
}

install_runtime_dependencies() {
  WAVELINUX_INSTALL_DEPS=1 \
    run_dependency_helper --install "$@"
}

case "${1:-}" in
  --check-runtime-dependencies|--check-runtime)
    shift
    check_runtime_dependencies "$@"
    exit $?
    ;;
  --install-runtime-dependencies|--install-runtime)
    shift
    install_runtime_dependencies "$@"
    exit $?
    ;;
esac

if [[ "${WAVELINUX_SKIP_APPIMAGE_PREFLIGHT:-0}" != "1" ]]; then
  preflight_log="$(mktemp "${TMPDIR:-/tmp}/wavelinux-appimage-preflight.XXXXXX")"
  if ! check_runtime_dependencies >"$preflight_log" 2>&1; then
    cat "$preflight_log" >&2
    echo "WaveLinux AppImage runtime dependencies are missing; attempting installer." >&2
    if ! install_runtime_dependencies; then
      echo "WaveLinux AppImage runtime dependency install failed." >&2
      echo "Run this manually from a terminal for details:" >&2
      echo "  $0 --install-runtime-dependencies" >&2
      rm -f "$preflight_log"
      exit 1
    fi
  fi
  rm -f "$preflight_log"
fi

source_hook_if_present() {
  local hook="$1"
  if [[ -f "$hook" ]]; then
    # shellcheck source=/dev/null
    source "$hook"
  fi
}

# linuxdeploy's GTK hook appends to this variable without guarding for a
# minimal environment. Use the freedesktop default before sourcing it so the
# strict wrapper remains usable in containers, display managers, and shells.
export XDG_DATA_DIRS="${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"

prepare_fontconfig_sysroot() {
  local base root
  if [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
    base="$XDG_RUNTIME_DIR/wavelinux6"
  else
    base="${TMPDIR:-/tmp}/wavelinux6-$(id -u)"
  fi
  root="$base/fontconfig-root-$$"

  install -d -m 0700 \
    "$base" \
    "$root" \
    "$root/usr/wavelinux-runtime/etc" \
    "$root/usr/share/fontconfig" \
    "$root/usr/local/share"
  ln -s "$runtime_dir/etc/fonts" "$root/usr/wavelinux-runtime/etc/fonts"
  ln -s "$runtime_dir/etc/fonts/conf.avail" "$root/usr/share/fontconfig/conf.avail"
  ln -s /usr/share/fonts "$root/usr/share/fonts"
  ln -s /usr/local/share/fonts "$root/usr/local/share/fonts"

  # XDG font caches and user font directories normally live below HOME. Map
  # the current home dynamically so Fontconfig keeps its cache writable.
  if [[ "${HOME:-}" == /* ]]; then
    install -d "$root$(dirname "$HOME")"
    ln -s "$HOME" "$root$HOME"
  fi
  printf '%s\n' "$root"
}

# FONTCONFIG_FILE alone is insufficient because Fontconfig also scans a
# compiled-in conf.avail path. A process-local sysroot isolates parser rules
# while retaining host fonts and writable user caches through explicit links.
fontconfig_file="/usr/wavelinux-runtime/etc/fonts/fonts.conf"
if [[ -r "$runtime_dir/etc/fonts/fonts.conf" \
  && -d "$runtime_dir/etc/fonts/conf.avail" ]]; then
  fontconfig_sysroot="$(prepare_fontconfig_sysroot)"
  export FONTCONFIG_SYSROOT="$fontconfig_sysroot"
  export FONTCONFIG_FILE="$fontconfig_file"
  export FONTCONFIG_PATH="/usr/wavelinux-runtime/etc/fonts"
fi

source_hook_if_present "$this_dir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
source_hook_if_present "$this_dir/apprun-hooks/linuxdeploy-plugin-gstreamer.sh"

exec "$this_dir/AppRun.wrapped" "$@"
