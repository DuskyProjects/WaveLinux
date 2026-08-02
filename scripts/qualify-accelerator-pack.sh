#!/usr/bin/env bash
set -euo pipefail

PROVIDER="${1:-}"
PROVIDER_ROOT="${WAVELINUX_PROVIDER_ROOT:-${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux6/providers}"
PACK_DIR="$PROVIDER_ROOT/$PROVIDER"
QUALIFIER="$PACK_DIR/bin/wavelinux6-accelerator-qualify"

case "$PROVIDER" in
  cuda|openvino|migraphx) ;;
  *)
    echo "Usage: $0 cuda|openvino|migraphx" >&2
    exit 2
    ;;
esac

if [[ ! -x "$QUALIFIER" ]]; then
  echo "Provider pack is not installed: $PACK_DIR" >&2
  exit 1
fi

args=(--pack "$PACK_DIR" --blocks "${WAVELINUX_PROVIDER_QUALIFICATION_BLOCKS:-5000}" --write)
if [[ -n "${WAVELINUX_ONNXRUNTIME_LIBRARY:-}" ]]; then
  args+=(--runtime "$WAVELINUX_ONNXRUNTIME_LIBRARY")
fi

nice -n 10 ionice -c3 "$QUALIFIER" "${args[@]}"

cat <<'NOTE'

The isolated numerical/IPC test intentionally does not approve a provider by
itself. WaveLinux only marks a record qualified after the live audio fallback,
continuity, latency, and total active-core CPU gates also pass.
NOTE
