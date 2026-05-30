#!/usr/bin/env bash
set -euo pipefail

SUPPORT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux"
APPIMAGE="${WAVELINUX_APPIMAGE:-}"
SANITIZER="$SUPPORT_DIR/sanitize-runtime-env.sh"

if [[ -z "$APPIMAGE" ]]; then
  APPIMAGE="$({ find "$SUPPORT_DIR" -maxdepth 1 -type f -name 'WaveLinux_*_amd64.AppImage' -print 2>/dev/null || true; } | sort -V | tail -n1)"
  APPIMAGE="${APPIMAGE:-$SUPPORT_DIR/WaveLinux.AppImage}"
fi

if [[ -f "$SANITIZER" ]]; then
  # shellcheck source=/dev/null
  source "$SANITIZER"
else
  unset CEF_PATH CEF_ROOT GIO_EXTRA_MODULES GIO_MODULE_DIR GI_TYPELIB_PATH \
    GST_PLUGIN_PATH GST_PLUGIN_PATH_1_0 GST_PLUGIN_SCANNER \
    GST_PLUGIN_SCANNER_1_0 GST_PLUGIN_SYSTEM_PATH GST_PLUGIN_SYSTEM_PATH_1_0 \
    GTK_PATH LD_AUDIT LD_LIBRARY_PATH LD_PRELOAD LIBRARY_PATH \
    WEBKIT_EXEC_PATH 2>/dev/null || true
fi

if [[ ! -x "$APPIMAGE" ]]; then
  echo "WaveLinux AppImage is missing or not executable: $APPIMAGE" >&2
  echo "Run bash scripts/build-local.sh && bash scripts/install-local.sh from the WaveLinux source tree to reinstall it." >&2
  exit 1
fi

exec "$APPIMAGE" "$@"
