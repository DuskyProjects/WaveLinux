#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROVIDER="${1:-}"
PACK_VERSION="${WAVELINUX_PROVIDER_PACK_VERSION:-1.0.0}"
MODEL="$ROOT_DIR/providers/rnnoise/rnnoise-neural-v1.onnx"
FIXTURE="$ROOT_DIR/providers/rnnoise/rnnoise-neural-v1-golden.json"
PROVIDER_BIN="$ROOT_DIR/target/release/wavelinux6-onnx-provider"
QUALIFIER_BIN="$ROOT_DIR/target/release/wavelinux6-accelerator-qualify"

case "$PROVIDER" in
  cuda|openvino|migraphx) ;;
  *)
    echo "Usage: $0 cuda|openvino|migraphx" >&2
    exit 2
    ;;
esac

export PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$PATH"

if python3 -c 'import onnx, onnxruntime' >/dev/null 2>&1; then
  nice -n 10 ionice -c3 python3 "$ROOT_DIR/scripts/generate-rnnoise-onnx.py" --verify --provider cpu
else
  echo "Python ONNX tooling is unavailable; using the checked reproducible model artifact." >&2
fi

expected_model_sha256="0d8c5664b9f4c677a6950464d900ee127cfeb6489b81070e39db1f415b604c71"
model_sha256="$(sha256sum "$MODEL" | awk '{print $1}')"
if [[ "$model_sha256" != "$expected_model_sha256" ]]; then
  echo "RNNoise ONNX artifact is not reproducible: expected $expected_model_sha256, got $model_sha256" >&2
  exit 1
fi

nice -n 10 ionice -c3 cargo build \
  --manifest-path "$ROOT_DIR/Cargo.toml" \
  --release \
  -p wavelinux-accelerator \
  --features provider-runtime \
  --bin wavelinux6-onnx-provider \
  --bin wavelinux6-accelerator-qualify

PACK_DIR="$ROOT_DIR/target/accelerator-packs/$PROVIDER"
rm -rf "$PACK_DIR"
install -d -m 0700 "$PACK_DIR/bin" "$PACK_DIR/models"
install -m 0700 "$PROVIDER_BIN" "$PACK_DIR/bin/wavelinux6-onnx-provider"
install -m 0700 "$QUALIFIER_BIN" "$PACK_DIR/bin/wavelinux6-accelerator-qualify"
install -m 0600 "$MODEL" "$PACK_DIR/models/rnnoise-neural-v1.onnx"
install -m 0600 "$FIXTURE" "$PACK_DIR/models/rnnoise-neural-v1-golden.json"
install -m 0600 "$ROOT_DIR/providers/rnnoise/LICENSE.nnnoiseless" "$PACK_DIR/LICENSE.nnnoiseless"

executable_sha256="$(sha256sum "$PACK_DIR/bin/wavelinux6-onnx-provider" | awk '{print $1}')"
fixture_sha256="$(sha256sum "$PACK_DIR/models/rnnoise-neural-v1-golden.json" | awk '{print $1}')"

jq -n \
  --arg pack_version "$PACK_VERSION" \
  --arg provider "$PROVIDER" \
  --arg executable_sha256 "$executable_sha256" \
  --arg model_sha256 "$model_sha256" \
  --arg fixture_sha256 "$fixture_sha256" \
  '{
    protocol_version: 1,
    pack_version: $pack_version,
    provider: $provider,
    executable: "bin/wavelinux6-onnx-provider",
    executable_sha256: $executable_sha256,
    model: "models/rnnoise-neural-v1.onnx",
    model_sha256: $model_sha256,
    golden_fixture: "models/rnnoise-neural-v1-golden.json",
    golden_fixture_sha256: $fixture_sha256,
    onnx_runtime_library: null
  }' > "$PACK_DIR/manifest.json"
chmod 0600 "$PACK_DIR/manifest.json"

echo "Built unqualified $PROVIDER provider pack: $PACK_DIR"
echo "The pack remains disabled until machine-local live audio qualification passes."
