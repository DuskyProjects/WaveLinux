#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/target/aur/wavelinux"

rm -rf "$OUT_DIR"
install -d "$OUT_DIR"
install -m 0644 "$ROOT_DIR/packaging/aur/PKGBUILD" "$OUT_DIR/PKGBUILD"
install -m 0644 "$ROOT_DIR/packaging/aur/.SRCINFO" "$OUT_DIR/.SRCINFO"

if command -v makepkg >/dev/null 2>&1; then
  (
    cd "$OUT_DIR"
    makepkg --printsrcinfo > .SRCINFO
  )
  echo "AUR package files staged at $OUT_DIR"
else
  echo "AUR PKGBUILD staged at $OUT_DIR"
  echo "Install pacman/makepkg to generate .SRCINFO."
fi
