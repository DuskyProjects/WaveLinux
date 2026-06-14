#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/target/aur/wavelinux"

if command -v makepkg >/dev/null 2>&1; then
  (
    cd "$ROOT_DIR/packaging/aur"
    makepkg --printsrcinfo > .SRCINFO
  )
fi

rm -rf "$OUT_DIR"
install -d "$OUT_DIR"
install -m 0644 "$ROOT_DIR/packaging/aur/PKGBUILD" "$OUT_DIR/PKGBUILD"
install -m 0644 "$ROOT_DIR/packaging/aur/.SRCINFO" "$OUT_DIR/.SRCINFO"
install -m 0644 "$ROOT_DIR/packaging/aur/wavelinux.install" "$OUT_DIR/wavelinux.install"

if command -v makepkg >/dev/null 2>&1; then
  echo "AUR package files staged at $OUT_DIR"
else
  echo "AUR PKGBUILD staged at $OUT_DIR"
  echo "Install pacman/makepkg to generate .SRCINFO."
fi
