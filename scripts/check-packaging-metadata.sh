#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/runtime-dependencies.sh
source "$ROOT_DIR/scripts/runtime-dependencies.sh"

failures=()

require_member() {
  local label="$1"
  local expected="$2"
  shift
  shift
  local values=("$@")
  local value
  for value in "${values[@]}"; do
    [[ "$value" == "$expected" ]] && return 0
  done
  failures+=("$label is missing dependency: $expected")
}

mapfile -t deb_dependencies < <(
  node -e '
    const config = require(process.argv[1]);
    for (const dependency of config.bundle.linux.deb.depends ?? []) console.log(dependency);
  ' "$ROOT_DIR/crates/app/tauri.conf.json"
)
mapfile -t rpm_dependencies < <(
  node -e '
    const config = require(process.argv[1]);
    for (const dependency of config.bundle.linux.rpm.depends ?? []) console.log(dependency);
  ' "$ROOT_DIR/crates/app/tauri.conf.json"
)
mapfile -t aur_dependencies < <(
  sed -n "/^depends=(/,/^)/s/^[[:space:]]*'\([^']*\)'.*/\1/p" \
    "$ROOT_DIR/packaging/aur/PKGBUILD"
)

while IFS= read -r dependency; do
  [[ -n "$dependency" ]] && require_member "Debian metadata" "$dependency" "${deb_dependencies[@]}"
done < <(wavelinux_runtime_packages apt)

while IFS= read -r dependency; do
  [[ -n "$dependency" ]] && require_member "RPM metadata" "$dependency" "${rpm_dependencies[@]}"
done < <(wavelinux_runtime_packages dnf)

while IFS= read -r dependency; do
  [[ -n "$dependency" ]] && require_member "AUR metadata" "$dependency" "${aur_dependencies[@]}"
done < <(wavelinux_runtime_packages pacman)

for format in deb rpm; do
  mapfile -t files < <(
    FORMAT="$format" node -e '
      const config = require(process.argv[1]);
      const files = config.bundle.linux[process.env.FORMAT].files ?? {};
      for (const path of Object.keys(files)) console.log(path);
    ' "$ROOT_DIR/crates/app/tauri.conf.json"
  )
  require_member "$format package files" /usr/bin/wavelinux6-audio-core "${files[@]}"
  require_member "$format package files" /usr/bin/wavelinux6-peripheral-plugin "${files[@]}"
  require_member "$format package files" /usr/lib/wavelinux6/check-dependencies.sh "${files[@]}"
  require_member "$format package files" /usr/lib/wavelinux6/runtime-dependencies.sh "${files[@]}"
  require_member "$format package files" /usr/lib/wavelinux6/verify-install.sh "${files[@]}"
  require_member "$format package files" \
    /usr/share/metainfo/io.github.duskyprojects.WaveLinux6.appdata.xml "${files[@]}"
done

grep -Fq -- '--bin wavelinux6-peripheral-plugin' "$ROOT_DIR/packaging/aur/PKGBUILD" \
  || failures+=("AUR build does not compile wavelinux6-peripheral-plugin")
grep -Fq 'target/release/wavelinux6-peripheral-plugin' "$ROOT_DIR/packaging/aur/PKGBUILD" \
  || failures+=("AUR package does not install wavelinux6-peripheral-plugin")
for script in check-dependencies.sh runtime-dependencies.sh verify-install.sh; do
  grep -Fq "scripts/$script" "$ROOT_DIR/packaging/aur/PKGBUILD" \
    || failures+=("AUR package does not install $script")
done

if ((${#failures[@]})); then
  echo "WaveLinux packaging metadata is inconsistent:" >&2
  printf '  - %s\n' "${failures[@]}" >&2
  exit 1
fi

echo "WaveLinux package metadata matches the shared runtime dependency contract."
