#!/usr/bin/env bash
set -euo pipefail

BASELINE="${WAVELINUX_GLIBC_BASELINE:-2.39}"

usage() {
  cat <<'HELP'
Verify that ELF binaries do not require a glibc newer than the release baseline.

Usage:
  bash scripts/check-glibc-baseline.sh BINARY [BINARY ...]

Environment:
  WAVELINUX_GLIBC_BASELINE=2.39
HELP
}

if (($# == 0)); then
  usage >&2
  exit 2
fi

command -v readelf >/dev/null 2>&1 || {
  echo "readelf is required for the glibc compatibility gate" >&2
  exit 1
}

failed=0
for binary in "$@"; do
  if [[ ! -f "$binary" ]]; then
    echo "Missing binary for glibc compatibility check: $binary" >&2
    failed=1
    continue
  fi
  if ! file -Lb "$binary" | grep -q 'ELF'; then
    echo "Not an ELF binary: $binary" >&2
    failed=1
    continue
  fi

  required="$({ readelf --version-info "$binary" 2>/dev/null || true; } \
    | grep -Eo 'GLIBC_[0-9]+\.[0-9]+' \
    | sed 's/^GLIBC_//' \
    | sort -Vu \
    | tail -n1)"
  if [[ -z "$required" ]]; then
    echo "glibc baseline: static-or-none binary=$binary baseline=$BASELINE"
    continue
  fi

  highest="$(printf '%s\n%s\n' "$BASELINE" "$required" | sort -V | tail -n1)"
  if [[ "$highest" != "$BASELINE" ]]; then
    echo "glibc baseline exceeded: binary=$binary requires=$required baseline=$BASELINE" >&2
    failed=1
  else
    echo "glibc baseline: ok binary=$binary requires=$required baseline=$BASELINE"
  fi
done

exit "$failed"
