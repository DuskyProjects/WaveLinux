#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

required_docs=(
  README.md
  docs/architecture.md
  docs/audio-core.md
  docs/setup.md
  docs/testing.md
  docs/troubleshooting.md
  docs/themes.md
  docs/profiles.md
  docs/acceleration.md
  docs/integrations.md
  docs/migration.md
  docs/releasing.md
  profiles/v1/README.md
)

failed=0
if ! command -v rg >/dev/null 2>&1; then
  echo "Documentation checks require ripgrep (rg)" >&2
  exit 1
fi

for file in "${required_docs[@]}"; do
  if [[ ! -s "$file" ]]; then
    echo "Missing or empty documentation file: $file" >&2
    failed=1
  fi
done

if rg -n -i \
  'wavelinux5-hardware-acceleration\.md|run[[:space:]]+wavelinux5|DeepFilterNet3, RNNoise|dynamically loaded bundled/system RNNoise|\.local/share/wavelinux6/effects[^`\n]*\.sock|io\.github\.duskyprojects\.WaveLinux/themes' \
  README.md docs; then
  echo "Active documentation contains a stale WaveLinux5, DSP, socket, or theme claim" >&2
  failed=1
fi

for file in "${required_docs[@]}"; do
  base_dir="$(dirname "$file")"
  while IFS= read -r target; do
    target="${target%%#*}"
    target="${target%% *}"
    [[ -z "$target" ]] && continue
    case "$target" in
      http://*|https://*|mailto:*|app://*) continue ;;
    esac
    if [[ ! -e "$base_dir/$target" && ! -e "$ROOT_DIR/$target" ]]; then
      echo "Broken local documentation link: $file -> $target" >&2
      failed=1
    fi
  done < <(grep -oE '\]\([^) ]+([ ][^)]*)?' "$file" | sed 's/^](//')
done

if ((failed)); then
  exit 1
fi

echo "Documentation checks: ok"
