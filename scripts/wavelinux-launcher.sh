#!/usr/bin/env bash
set -euo pipefail

PROGRAM_NAME="$(basename "$0")"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
if [[ "$PROGRAM_NAME" == "wavelinux5" ]]; then
  SUPPORT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux5"
  PRODUCT_NAME="WaveLinux5"
  export WAVELINUX_XDG_APP_NAME="${WAVELINUX_XDG_APP_NAME:-WaveLinux5}"
  export WAVELINUX_GRAPH_PREFIX="${WAVELINUX_GRAPH_PREFIX:-wavelinux5}"
  export WAVELINUX_GRAPH_PROPERTY_PREFIX="${WAVELINUX_GRAPH_PROPERTY_PREFIX:-wavelinux5}"
  export WAVELINUX_APP_DISPLAY_NAME="${WAVELINUX_APP_DISPLAY_NAME:-WaveLinux5}"
  LOCAL_DSP_HELPER="$BIN_DIR/wavelinux5-dsp-helper"
  if [[ -x "$LOCAL_DSP_HELPER" ]]; then
    export WAVELINUX_DSP_HELPER="${WAVELINUX_DSP_HELPER:-$LOCAL_DSP_HELPER}"
  fi
  if [[ -z "${WAVELINUX_FILTER_CHAIN_PIPEWIRE:-}" ]]; then
    if HOST_PIPEWIRE="$(command -v pipewire 2>/dev/null)" && [[ -x "$HOST_PIPEWIRE" ]]; then
      export WAVELINUX_FILTER_CHAIN_PIPEWIRE="$HOST_PIPEWIRE"
    fi
  fi
else
  SUPPORT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux"
  PRODUCT_NAME="WaveLinux"
fi
APPIMAGE="${WAVELINUX_APPIMAGE:-}"
SANITIZER="$SUPPORT_DIR/sanitize-runtime-env.sh"

if [[ -z "$APPIMAGE" ]]; then
  APPIMAGE="$({ find "$SUPPORT_DIR" -maxdepth 1 -type f -name "${PRODUCT_NAME}_*_amd64.AppImage" -print 2>/dev/null || true; } | sort -V | tail -n1)"
  APPIMAGE="${APPIMAGE:-$SUPPORT_DIR/${PRODUCT_NAME}.AppImage}"
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

  if [[ -z "${WAVELINUX_DISABLE_WEBKIT_WORKAROUNDS:-}" ]]; then
    export WEBKIT_DISABLE_DMABUF_RENDERER="${WEBKIT_DISABLE_DMABUF_RENDERER:-1}"
    export WEBKIT_DISABLE_COMPOSITING_MODE="${WEBKIT_DISABLE_COMPOSITING_MODE:-1}"

    if [[ -z "${WAVELINUX_KEEP_WEBKIT_SANDBOX:-}" && -z "${WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS+x}" ]]; then
      if ! command -v bwrap >/dev/null 2>&1 || ! command -v xdg-dbus-proxy >/dev/null 2>&1; then
        export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
      fi
    fi
  fi
fi

if [[ ! -x "$APPIMAGE" ]]; then
  echo "$PRODUCT_NAME AppImage is missing or not executable: $APPIMAGE" >&2
  echo "Run bash scripts/build-local.sh && bash scripts/install-local.sh from the WaveLinux source tree to reinstall it." >&2
  exit 1
fi

exec "$APPIMAGE" "$@"
