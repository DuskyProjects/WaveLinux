#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROVIDER="${1:-}"
SOURCE_DIR="$ROOT_DIR/target/accelerator-packs/$PROVIDER"
PROVIDER_ROOT="${WAVELINUX_PROVIDER_ROOT:-${XDG_DATA_HOME:-$HOME/.local/share}/wavelinux6/providers}"
DESTINATION="$PROVIDER_ROOT/$PROVIDER"

case "$PROVIDER" in
  cuda|openvino|migraphx) ;;
  *)
    echo "Usage: $0 cuda|openvino|migraphx" >&2
    exit 2
    ;;
esac

if [[ ! -f "$SOURCE_DIR/manifest.json" ]]; then
  echo "Provider pack has not been built: $SOURCE_DIR" >&2
  echo "Run scripts/build-accelerator-pack.sh $PROVIDER first." >&2
  exit 1
fi

install -d -m 0700 "$PROVIDER_ROOT"
temporary="$PROVIDER_ROOT/.${PROVIDER}-install-$$"
rm -rf "$temporary"
install -d -m 0700 "$temporary/bin" "$temporary/models"
install -m 0700 "$SOURCE_DIR/bin/wavelinux6-onnx-provider" "$temporary/bin/"
install -m 0700 "$SOURCE_DIR/bin/wavelinux6-accelerator-qualify" "$temporary/bin/"
install -m 0600 "$SOURCE_DIR/models/rnnoise-neural-v1.onnx" "$temporary/models/"
install -m 0600 "$SOURCE_DIR/models/rnnoise-neural-v1-golden.json" "$temporary/models/"
install -m 0600 "$SOURCE_DIR/LICENSE.nnnoiseless" "$temporary/"
install -m 0600 "$SOURCE_DIR/manifest.json" "$temporary/"

# Qualification is hardware-, model-, and binary-specific. Never carry a
# previous record across a pack replacement.
rm -rf "$DESTINATION"
mv "$temporary" "$DESTINATION"
echo "Installed unqualified $PROVIDER provider pack at $DESTINATION"
echo "WaveLinux will keep using CPU until the complete qualification gate passes."
