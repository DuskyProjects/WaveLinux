#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/runtime-dependencies.sh
source "$SCRIPT_DIR/runtime-dependencies.sh"

INSTALL=0
STRICT=0
STRICT_RUNTIME=0
INSTALLED_PACKAGES=0

usage() {
  cat <<'HELP'
Check or install the WaveLinux 6 host audio runtime.

Usage:
  check-dependencies.sh [--install] [--strict] [--strict-runtime]

Options:
  --install          Install missing host packages using apt, dnf, pacman, or zypper.
  --strict           Exit nonzero when any required capability is missing.
  --strict-runtime   Alias for the runtime-focused strict check used by installers.

Environment:
  WAVELINUX_INSTALL_DEPS=1  Enable dependency installation.
HELP
}

for arg in "$@"; do
  case "$arg" in
    --install) INSTALL=1 ;;
    --strict) STRICT=1 ;;
    --strict-runtime) STRICT_RUNTIME=1 ;;
    --help|-h) usage; exit 0 ;;
    *)
      echo "Unknown option: $arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[[ "${WAVELINUX_INSTALL_DEPS:-0}" == "1" ]] && INSTALL=1

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

detect_manager() {
  if command_exists apt-get; then
    printf '%s\n' apt
  elif command_exists dnf; then
    printf '%s\n' dnf
  elif command_exists pacman; then
    printf '%s\n' pacman
  elif command_exists zypper; then
    printf '%s\n' zypper
  else
    printf '%s\n' unknown
  fi
}

stdin_is_terminal() {
  [[ -t 0 ]]
}

privilege_helpers() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    return 0
  fi

  local terminal=0 sudo_available=0 pkexec_available=0
  stdin_is_terminal && terminal=1
  command_exists sudo && sudo_available=1
  command_exists pkexec && pkexec_available=1
  wavelinux_privilege_helper_order "$terminal" "$sudo_available" "$pkexec_available"
}

run_privileged() {
  local program="$1"
  shift
  local program_path
  program_path="$(command -v "$program" 2>/dev/null || true)"
  [[ -n "$program_path" ]] || {
    echo "Required package-manager command is unavailable: $program" >&2
    return 127
  }

  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    "$program_path" "$@"
    return $?
  fi

  local helpers=()
  mapfile -t helpers < <(privilege_helpers)
  if ((${#helpers[@]} == 0)); then
    echo "No sudo or pkexec command is available." >&2
    printf 'Run this manually as an administrator: %q' "$program_path" >&2
    printf ' %q' "$@" >&2
    printf '\n' >&2
    return 1
  fi

  local helper status
  for helper in "${helpers[@]}"; do
    echo "Requesting administrator permission with $helper for: $program $*" >&2
    "$helper" "$program_path" "$@" && return 0
    status=$?
    echo "$helper failed with status $status; trying the next privilege helper." >&2
  done

  echo "Administrator command failed: $program $*" >&2
  return 1
}

refresh_package_metadata() {
  local manager="$1"
  case "$manager" in
    apt)
      run_privileged apt-get update
      ;;
    dnf)
      run_privileged dnf makecache --refresh -y
      ;;
    pacman) return 0 ;;
    zypper)
      run_privileged zypper --non-interactive refresh
      ;;
    *)
      echo "No supported package manager detected." >&2
      return 1
      ;;
  esac
}

package_available() {
  local manager="$1"
  local package="$2"
  case "$manager" in
    apt) apt-cache show "$package" >/dev/null 2>&1 ;;
    dnf) dnf -q info "$package" >/dev/null 2>&1 ;;
    # Arch/CachyOS package resolution intentionally happens only inside the
    # single full-upgrade install transaction. Querying stale sync databases
    # before `pacman -Syu` is the clean-install failure this helper prevents.
    pacman) return 1 ;;
    zypper) zypper --non-interactive search --match-exact "$package" >/dev/null 2>&1 ;;
    *) return 1 ;;
  esac
}

package_installed() {
  local manager="$1"
  local package="$2"
  case "$manager" in
    apt)
      dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -Fq 'install ok installed'
      ;;
    dnf|zypper)
      rpm -q "$package" >/dev/null 2>&1
      ;;
    pacman)
      pacman -Q "$package" >/dev/null 2>&1
      ;;
    *) return 1 ;;
  esac
}

resolve_install_packages() {
  local manager="$1"
  local resolved=()
  local -A seen=()
  local package

  while IFS= read -r package; do
    [[ -n "$package" ]] || continue
    if [[ -n "${seen[$package]+x}" ]]; then
      continue
    fi
    seen["$package"]=1
    if [[ "$manager" == pacman ]] || package_available "$manager" "$package"; then
      resolved+=("$package")
    else
      echo "Package is unavailable in the configured $manager repositories: $package" >&2
    fi
  done < <(wavelinux_runtime_packages "$manager")

  local portal_installed=0
  while IFS= read -r package; do
    [[ -n "$package" ]] || continue
    if package_installed "$manager" "$package"; then
      portal_installed=1
      break
    fi
  done < <(wavelinux_portal_candidates "$manager")

  if ((portal_installed == 0)); then
    while IFS= read -r package; do
      [[ -n "$package" ]] || continue
      if [[ "$manager" == pacman ]] || package_available "$manager" "$package"; then
        [[ -n "${seen[$package]+x}" ]] || resolved+=("$package")
        break
      fi
    done < <(wavelinux_portal_candidates "$manager")
  fi

  ((${#resolved[@]})) && printf '%s\n' "${resolved[@]}"
}

install_packages() {
  local manager="$1"
  shift
  local packages=("$@")
  ((${#packages[@]})) || {
    echo "No installable WaveLinux runtime packages were resolved." >&2
    return 1
  }

  case "$manager" in
    apt)
      run_privileged env DEBIAN_FRONTEND=noninteractive \
        apt-get install -y --no-install-recommends "${packages[@]}"
      ;;
    dnf)
      run_privileged dnf install -y --setopt=install_weak_deps=False "${packages[@]}"
      ;;
    pacman)
      run_privileged pacman -Syu --needed --noconfirm "${packages[@]}"
      ;;
    zypper)
      run_privileged zypper --non-interactive install --no-recommends "${packages[@]}"
      ;;
    *)
      echo "No supported package manager detected." >&2
      return 1
      ;;
  esac
}

library_available() {
  local expression="$1"
  shift
  local cache=""
  if command_exists ldconfig; then
    cache="$(ldconfig -p 2>/dev/null || true)"
    if grep -Eq "$expression" <<<"$cache"; then
      return 0
    fi
  fi

  local path
  for path in "$@"; do
    if compgen -G "$path" >/dev/null; then
      return 0
    fi
  done
  return 1
}

manager="$(detect_manager)"

if ((INSTALL == 1)); then
  [[ "$manager" != unknown ]] || {
    echo "WaveLinux cannot install dependencies: apt, dnf, pacman, and zypper are unavailable." >&2
    exit 1
  }
  if [[ "$manager" == pacman ]]; then
    echo "Preparing one Arch/CachyOS full-upgrade dependency transaction..."
  else
    echo "Refreshing $manager package metadata before dependency resolution..."
    refresh_package_metadata "$manager"
  fi
  packages=()
  mapfile -t packages < <(resolve_install_packages "$manager")
  ((${#packages[@]})) || {
    echo "No WaveLinux runtime packages could be resolved after refreshing $manager metadata." >&2
    exit 1
  }
  echo "Installing WaveLinux runtime packages: ${packages[*]}"
  install_packages "$manager" "${packages[@]}"
  INSTALLED_PACKAGES=1
fi

missing_commands=()
for program in awk ps pgrep pipewire pactl wpctl pw-cli pw-dump pw-metadata pw-top aseqdump bwrap xdg-dbus-proxy fc-list; do
  command_exists "$program" || missing_commands+=("$program")
done
if [[ "${XDG_SESSION_TYPE:-}" == wayland ]] && ! command_exists Xwayland; then
  missing_commands+=(Xwayland)
fi

missing_libraries=()
library_available 'libpipewire-0\.3\.so\.0' \
  /usr/lib/libpipewire-0.3.so.0 \
  /usr/lib64/libpipewire-0.3.so.0 \
  /usr/lib/x86_64-linux-gnu/libpipewire-0.3.so.0 \
  || missing_libraries+=(libpipewire-0.3)
library_available 'libusb-1\.0\.so\.0' \
  /usr/lib/libusb-1.0.so.0 \
  /usr/lib64/libusb-1.0.so.0 \
  /usr/lib/x86_64-linux-gnu/libusb-1.0.so.0 \
  || missing_libraries+=(libusb-1.0)
library_available 'libEGL\.so\.1' /usr/lib/libEGL.so.1 /usr/lib64/libEGL.so.1 /usr/lib/x86_64-linux-gnu/libEGL.so.1 \
  || missing_libraries+=(libEGL)
library_available 'libGL\.so\.1' /usr/lib/libGL.so.1 /usr/lib64/libGL.so.1 /usr/lib/x86_64-linux-gnu/libGL.so.1 \
  || missing_libraries+=(libGL)
library_available 'libgbm\.so\.1' /usr/lib/libgbm.so.1 /usr/lib64/libgbm.so.1 /usr/lib/x86_64-linux-gnu/libgbm.so.1 \
  || missing_libraries+=(libgbm)
library_available 'libdrm\.so\.2' /usr/lib/libdrm.so.2 /usr/lib64/libdrm.so.2 /usr/lib/x86_64-linux-gnu/libdrm.so.2 \
  || missing_libraries+=(libdrm)
library_available 'libwayland-client\.so\.0' /usr/lib/libwayland-client.so.0 /usr/lib64/libwayland-client.so.0 /usr/lib/x86_64-linux-gnu/libwayland-client.so.0 \
  || missing_libraries+=(libwayland-client)
library_available 'libwayland-cursor\.so\.0' /usr/lib/libwayland-cursor.so.0 /usr/lib64/libwayland-cursor.so.0 /usr/lib/x86_64-linux-gnu/libwayland-cursor.so.0 \
  || missing_libraries+=(libwayland-cursor)
library_available 'libwayland-egl\.so\.1' /usr/lib/libwayland-egl.so.1 /usr/lib64/libwayland-egl.so.1 /usr/lib/x86_64-linux-gnu/libwayland-egl.so.1 \
  || missing_libraries+=(libwayland-egl)
library_available 'libwayland-server\.so\.0' /usr/lib/libwayland-server.so.0 /usr/lib64/libwayland-server.so.0 /usr/lib/x86_64-linux-gnu/libwayland-server.so.0 \
  || missing_libraries+=(libwayland-server)

if ! compgen -G '/usr/lib*/alsa-lib/libasound_module_pcm_pulse.so' >/dev/null \
  && ! compgen -G '/usr/lib/*/alsa-lib/libasound_module_pcm_pulse.so' >/dev/null \
  && ! compgen -G '/usr/lib*/alsa-lib/libasound_module_pcm_pipewire.so' >/dev/null \
  && ! compgen -G '/usr/lib/*/alsa-lib/libasound_module_pcm_pipewire.so' >/dev/null; then
  missing_libraries+=(ALSA-audio-compatibility-plugin)
fi

portal_backend=""
if [[ "$manager" != unknown ]]; then
  while IFS= read -r package; do
    [[ -n "$package" ]] || continue
    if package_installed "$manager" "$package"; then
      portal_backend="$package"
      break
    fi
  done < <(wavelinux_portal_candidates "$manager")
fi

sandbox_note=ok
if command_exists bwrap && ! bwrap --ro-bind / / /usr/bin/true >/dev/null 2>&1; then
  sandbox_note="installed but unavailable in this container/session"
fi

echo "WaveLinux dependency check"
echo "Package manager: $manager"
if ((${#missing_commands[@]})); then
  echo "Runtime commands missing: ${missing_commands[*]}"
else
  echo "Runtime commands: ok"
fi
if ((${#missing_libraries[@]})); then
  echo "Runtime libraries missing: ${missing_libraries[*]}"
else
  echo "Runtime libraries: ok"
fi
if [[ -n "$portal_backend" ]]; then
  echo "Desktop portal backend: $portal_backend"
else
  echo "Desktop portal backend: not detected"
fi
echo "Bubblewrap sandbox: $sandbox_note"
echo "Native effects: bundled in wavelinux6-audio-core"

runtime_missing=0
if ((${#missing_commands[@]} || ${#missing_libraries[@]})) || [[ -z "$portal_backend" ]]; then
  runtime_missing=1
fi

if ((INSTALLED_PACKAGES == 1)); then
  verify_args=()
  ((STRICT == 1)) && verify_args+=(--strict)
  ((STRICT_RUNTIME == 1)) && verify_args+=(--strict-runtime)
  echo "Rechecking WaveLinux dependencies after package installation..."
  exec env WAVELINUX_INSTALL_DEPS=0 bash "$0" "${verify_args[@]}"
fi

if ((runtime_missing == 1 && (STRICT == 1 || STRICT_RUNTIME == 1))); then
  exit 1
fi
