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
    WAVELINUX_INSTALL_EFFECTS=1 \
    run_dependency_helper --install --install-effects "$@"
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

source_hook_if_present "$this_dir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
source_hook_if_present "$this_dir/apprun-hooks/linuxdeploy-plugin-gstreamer.sh"

exec "$this_dir/AppRun.wrapped" "$@"
