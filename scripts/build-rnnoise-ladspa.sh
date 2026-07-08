#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT="${1:-$ROOT_DIR/crates/app/appimage-extra/usr/wavelinux-runtime/lib/ladspa/librnnoise_ladspa.so}"
RNNOISE_PLUGIN_REPO="${WAVELINUX_RNNOISE_LADSPA_REPO:-https://github.com/werman/noise-suppression-for-voice.git}"
RNNOISE_PLUGIN_REF="${WAVELINUX_RNNOISE_LADSPA_REF:-9c4e5c28d8950e2cef837d8a0abd36c2fd9b5c2d}"

for program in git cmake cc c++; do
  if ! command -v "$program" >/dev/null 2>&1; then
    echo "Missing build tool for RNNoise LADSPA plugin: $program" >&2
    exit 127
  fi
done

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux-rnnoise-ladspa.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT

src_dir="$work_dir/src"
build_dir="$work_dir/build"

git init -q "$src_dir"
git -C "$src_dir" remote add origin "$RNNOISE_PLUGIN_REPO"
git -C "$src_dir" fetch --depth 1 origin "$RNNOISE_PLUGIN_REF"
git -c advice.detachedHead=false -C "$src_dir" checkout --detach FETCH_HEAD >/dev/null

cmake \
  -S "$src_dir" \
  -B "$build_dir" \
  -DBUILD_LADSPA_PLUGIN=ON \
  -DBUILD_VST_PLUGIN=OFF \
  -DBUILD_VST3_PLUGIN=OFF \
  -DBUILD_LV2_PLUGIN=OFF \
  -DBUILD_AU_PLUGIN=OFF \
  -DBUILD_AUV3_PLUGIN=OFF \
  -DBUILD_TESTS=OFF \
  -DBUILD_FOR_RELEASE=ON \
  -DCMAKE_SHARED_LINKER_FLAGS="-static-libgcc -static-libstdc++"

cmake --build "$build_dir" --target rnnoise_ladspa --parallel "${WAVELINUX_BUILD_JOBS:-2}"

mkdir -p "$(dirname "$OUTPUT")"
install -m 0644 "$build_dir/bin/ladspa/librnnoise_ladspa.so" "$OUTPUT"
echo "Built RNNoise LADSPA plugin: $OUTPUT"
