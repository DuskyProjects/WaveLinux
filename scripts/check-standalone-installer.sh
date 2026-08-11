#!/usr/bin/env bash
set -euo pipefail

INSTALLER="${1:?usage: check-standalone-installer.sh INSTALLER}"
[[ -x "$INSTALLER" ]] || {
  echo "Standalone installer is missing or not executable: $INSTALLER" >&2
  exit 1
}

for option in --help --no-launch; do
  grep -Fq -- "$option" "$INSTALLER" || {
    echo "Standalone installer does not advertise $option" >&2
    exit 1
  }
done

payload_line=0
payload_marker_found=0
while IFS= read -r line; do
  payload_line=$((payload_line + 1))
  if [[ "$line" == __WAVELINUX_PAYLOAD_BELOW__ ]]; then
    payload_line=$((payload_line + 1))
    payload_marker_found=1
    break
  fi
done <"$INSTALLER"
if ((payload_marker_found == 0)); then
  echo "Standalone installer payload marker is missing" >&2
  exit 1
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux6-installer-check.XXXXXX")"
trap 'rm -rf "$work_dir"' EXIT
tail -n +"$payload_line" "$INSTALLER" | tar -xzf - -C "$work_dir"
(
  cd "$work_dir"
  sha256sum --check --strict SHA256SUMS
)

required_executables=(
  target/release/bundle/appimage/WaveLinux6_\*_amd64.AppImage
  target/release/wavelinux6-audio-core
  target/release/wavelinux6-peripheral-plugin
  scripts/check-dependencies.sh
  scripts/runtime-dependencies.sh
  scripts/wavelinux-processes.sh
  scripts/install-local.sh
  scripts/verify-install.sh
)
for pattern in "${required_executables[@]}"; do
  matches=()
  mapfile -t matches < <(compgen -G "$work_dir/$pattern" || true)
  ((${#matches[@]} == 1)) || {
    echo "Standalone payload expected one executable matching $pattern" >&2
    exit 1
  }
  [[ -x "${matches[0]}" ]] || {
    echo "Standalone payload file is not executable: ${matches[0]}" >&2
    exit 1
  }
done

echo "Standalone installer payload and checksums verified: $INSTALLER"
