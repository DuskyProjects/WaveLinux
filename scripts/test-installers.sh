#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/runtime-dependencies.sh
source "$ROOT_DIR/scripts/runtime-dependencies.sh"

fail() {
  echo "Installer regression test failed: $*" >&2
  exit 1
}

package_version="$(node -e 'console.log(require(process.argv[1]).version)' "$ROOT_DIR/package.json")"
tauri_version="$(node -e 'console.log(require(process.argv[1]).version)' "$ROOT_DIR/crates/app/tauri.conf.json")"
cargo_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/crates/app/Cargo.toml" | head -n1)"
aur_version="$(sed -n 's/^pkgver=//p' "$ROOT_DIR/packaging/aur/PKGBUILD" | head -n1)"

[[ "$package_version" == "$tauri_version" ]] || fail "package.json and Tauri versions differ"
[[ "$package_version" == "$cargo_version" ]] || fail "package.json and Cargo versions differ"
[[ "$package_version" == "$aur_version" ]] || fail "package.json and AUR versions differ"

for manager in apt dnf pacman zypper; do
  mapfile -t packages < <(wavelinux_runtime_packages "$manager")
  ((${#packages[@]} > 0)) || fail "$manager runtime dependency array is empty"
  [[ " ${packages[*]} " == *" gawk "* ]] || fail "$manager runtime dependencies omit gawk"
  duplicates="$(printf '%s\n' "${packages[@]}" | sort | uniq -d)"
  [[ -z "$duplicates" ]] || fail "$manager runtime dependency array contains duplicates: $duplicates"
done

mapfile -t terminal_helpers < <(wavelinux_privilege_helper_order 1 1 1)
[[ "${terminal_helpers[*]}" == "sudo pkexec" ]] \
  || fail "terminal dependency install does not prefer sudo"
mapfile -t graphical_helpers < <(wavelinux_privilege_helper_order 0 1 1)
[[ "${graphical_helpers[*]}" == "pkexec sudo" ]] \
  || fail "graphical dependency install does not prefer pkexec"

if grep -Eq 'pacman[[:space:]]+-Si' "$ROOT_DIR/scripts/check-dependencies.sh"; then
  fail "Arch dependency setup queries package metadata before pacman -Syu"
fi

if grep -Eq 'pacman[[:space:]]+-Si|const [A-Z_]+RUNTIME_PACKAGES' \
  "$ROOT_DIR/crates/app/src/main.rs"; then
  fail "the Rust application duplicates or bypasses the authoritative dependency helper"
fi
grep -Fq 'check-dependencies.sh' "$ROOT_DIR/crates/app/src/main.rs" \
  || fail "the Rust runtime preflight does not invoke the authoritative dependency helper"

if grep -Eq '^for command in .*awk' "$ROOT_DIR/scripts/build-standalone-installer.sh"; then
  fail "standalone extraction requires awk before dependencies can be installed"
fi

process_fixture="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux-process-test.XXXXXX")"
trap 'rm -rf "$process_fixture"' EXIT
printf '%s\n' 101 102 103 104 105 106 107 108 109 110 111 >"$process_fixture/pids"
for pid in 101 102 103 104 105 106 107 108 109 110 111; do
  mkdir -p "$process_fixture/proc/$pid"
done
ln -s /usr/bin/bash "$process_fixture/proc/101/exe"
ln -s /tmp/.mount_WaveLinux/usr/bin/wavelinux6 "$process_fixture/proc/102/exe"
ln -s /home/test/WaveLinux6_6.0.2_amd64.AppImage "$process_fixture/proc/103/exe"
ln -s /home/test/.local/bin/wavelinux6-audio-core "$process_fixture/proc/104/exe"
ln -s /home/test/.local/bin/wavelinux6-peripheral-plugin "$process_fixture/proc/105/exe"
ln -s /usr/bin/pipewire "$process_fixture/proc/106/exe"
ln -s /usr/bin/pipewire "$process_fixture/proc/107/exe"
ln -s /home/test/WaveLinux5_5.0.3_amd64.AppImage "$process_fixture/proc/108/exe"
ln -s /usr/bin/wavelinux "$process_fixture/proc/109/exe"
ln -s /home/test/.local/bin/wavelinux5-dsp-helper "$process_fixture/proc/110/exe"
ln -s /usr/bin/pipewire "$process_fixture/proc/111/exe"
printf '%s\0' bash /tmp/WaveLinux6_6.0.2_amd64_Installer.sh \
  WaveLinux6_fake_amd64.AppImage >"$process_fixture/proc/101/cmdline"
printf '%s\0' pipewire -c /home/test/.config/wavelinux6/effects/wavelinux6-chain-input.conf \
  >"$process_fixture/proc/106/cmdline"
printf '%s\0' pipewire -c /home/test/.config/wavelinux5/effects/wavelinux-chain-input.conf \
  >"$process_fixture/proc/107/cmdline"
printf '%s\0' pipewire -c /home/test/.local/share/wavelinux5/effects/wavelinux5-chain-input.conf \
  >"$process_fixture/proc/111/cmdline"

# shellcheck source=scripts/wavelinux-processes.sh
source "$ROOT_DIR/scripts/wavelinux-processes.sh"
export WAVELINUX_PROCESS_PROC_ROOT="$process_fixture/proc"
export WAVELINUX_PROCESS_PID_FILE="$process_fixture/pids"
mapfile -t app_processes < <(wavelinux_collect_process_pids app)
[[ "${app_processes[*]}" == "102 103" ]] \
  || fail "installer process matching selected unexpected app PIDs: ${app_processes[*]}"
mapfile -t app_runtime_processes < <(wavelinux_collect_process_pids app-runtime)
[[ "${app_runtime_processes[*]}" == "102" ]] \
  || fail "application health matching accepted a mount helper: ${app_runtime_processes[*]}"
mapfile -t legacy_app_processes < <(wavelinux_collect_process_pids legacy-app)
[[ "${legacy_app_processes[*]}" == "108" ]] \
  || fail "legacy process matching selected stable or unrelated app PIDs: ${legacy_app_processes[*]}"
mapfile -t core_processes < <(wavelinux_collect_process_pids audio-core)
[[ "${core_processes[*]}" == "104" ]] || fail "audio-core process matching is not exact"
mapfile -t peripheral_processes < <(wavelinux_collect_process_pids peripheral)
[[ "${peripheral_processes[*]}" == "105" ]] || fail "peripheral process matching is not exact"
mapfile -t legacy_helper_processes < <(wavelinux_collect_process_pids legacy-helper)
[[ "${legacy_helper_processes[*]}" == "110" ]] \
  || fail "legacy helper process matching is not exact"
mapfile -t filter_processes < <(wavelinux_collect_filter_chain_pids)
[[ "${filter_processes[*]}" == "106" ]] \
  || fail "filter-chain process matching selected an unrelated PipeWire process"
mapfile -t legacy_filter_processes < <(wavelinux_collect_legacy_filter_chain_pids)
[[ "${legacy_filter_processes[*]}" == "111" ]] \
  || fail "legacy filter-chain matching selected stable or unrelated PipeWire processes"
unset WAVELINUX_PROCESS_PROC_ROOT WAVELINUX_PROCESS_PID_FILE

installer_env=(WAVELINUX_INSTALLER_ALLOW_ROOT_FOR_TESTS=1)
latest_output="$(env "${installer_env[@]}" bash "$ROOT_DIR/install.sh" --dry-run)"
grep -Fq '/releases/latest/download/WaveLinux6_amd64_Installer.sh' <<<"$latest_output" \
  || fail "latest auto install does not select the stable standalone alias"
grep -Fq '/releases/latest/download/SHA256SUMS' <<<"$latest_output" \
  || fail "latest auto install does not select release checksums"

tagged_output="$(env "${installer_env[@]}" bash "$ROOT_DIR/install.sh" \
  --tag "v$package_version" --format appimage --dry-run)"
grep -Fq "/releases/download/v$package_version/WaveLinux6_amd64_Installer.sh" \
  <<<"$tagged_output" || fail "tagged AppImage install does not select the standalone installer"

deb_output="$(env "${installer_env[@]}" bash "$ROOT_DIR/install.sh" \
  --tag "$package_version" --format deb --dry-run)"
grep -Fq "/releases/download/v$package_version/WaveLinux6_${package_version}_amd64.deb" \
  <<<"$deb_output" || fail "tagged Debian asset selection is incorrect"

rpm_output="$(env "${installer_env[@]}" bash "$ROOT_DIR/install.sh" \
  --tag "$package_version" --format rpm --dry-run)"
grep -Fq "/releases/download/v$package_version/WaveLinux6-${package_version}-1.x86_64.rpm" \
  <<<"$rpm_output" || fail "tagged RPM asset selection is incorrect"

if env "${installer_env[@]}" bash "$ROOT_DIR/install.sh" --format deb --dry-run \
  >/dev/null 2>&1; then
  fail "unversioned native package selection should be rejected"
fi

bash "$ROOT_DIR/scripts/check-packaging-metadata.sh"

if [[ -n "${WAVELINUX_TEST_STANDALONE_INSTALLER:-}" ]]; then
  bash "$ROOT_DIR/scripts/check-standalone-installer.sh" \
    "$WAVELINUX_TEST_STANDALONE_INSTALLER"
fi

echo "Installer dependency, asset-selection, and packaging regression tests passed."
