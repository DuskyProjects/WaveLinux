#!/usr/bin/env bash
# Sanitize only the WaveLinux child-process environment.

if [[ -z "${BASH_VERSION:-}" ]]; then
  echo "sanitize-runtime-env.sh requires bash" >&2
  return 2 2>/dev/null || exit 2
fi

wavelinux_sanitize_runtime_env() {
  unset \
    CEF_PATH \
    CEF_ROOT \
    GIO_EXTRA_MODULES \
    GIO_MODULE_DIR \
    GI_TYPELIB_PATH \
    GST_PLUGIN_PATH \
    GST_PLUGIN_PATH_1_0 \
    GST_PLUGIN_SCANNER \
    GST_PLUGIN_SCANNER_1_0 \
    GST_PLUGIN_SYSTEM_PATH \
    GST_PLUGIN_SYSTEM_PATH_1_0 \
    GTK_PATH \
    LD_AUDIT \
    LD_LIBRARY_PATH \
    LD_PRELOAD \
    LIBRARY_PATH \
    WEBKIT_EXEC_PATH \
    2>/dev/null || true
}

wavelinux_sanitize_runtime_env

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  if (( $# > 0 )); then
    exec "$@"
  fi

  exit 0
fi
