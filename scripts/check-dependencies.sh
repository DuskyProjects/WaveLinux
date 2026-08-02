#!/usr/bin/env bash
set -euo pipefail

INSTALL=0
STRICT=0
STRICT_RUNTIME=0
INSTALLED_PACKAGES=0

usage() {
  cat <<'HELP'
Check WaveLinux runtime and effect dependencies.

Usage:
  bash scripts/check-dependencies.sh [--install] [--strict] [--strict-runtime]

Environment:
  WAVELINUX_INSTALL_DEPS=1      Install missing runtime dependencies.
HELP
}

for arg in "$@"; do
  case "$arg" in
    --install)
      INSTALL=1
      ;;
    --strict)
      STRICT=1
      ;;
    --strict-runtime)
      STRICT_RUNTIME=1
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ "${WAVELINUX_INSTALL_DEPS:-0}" == "1" ]]; then
  INSTALL=1
fi

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

library_available() {
  local soname="$1"
  shift

  if command_exists ldconfig && ldconfig -p 2>/dev/null | grep -q "$soname"; then
    return 0
  fi

  local path
  for path in "$@"; do
    [[ -e "$path" ]] && return 0
  done

  return 1
}

detect_manager() {
  if command_exists apt-get; then
    echo apt
  elif command_exists dnf; then
    echo dnf
  elif command_exists pacman; then
    echo pacman
  elif command_exists zypper; then
    echo zypper
  else
    echo unknown
  fi
}

stdin_is_terminal() {
  [[ -t 0 ]]
}

privilege_helpers() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    return 0
  fi

  if stdin_is_terminal; then
    command_exists sudo && printf '%s\n' sudo
    command_exists pkexec && printf '%s\n' pkexec
  else
    command_exists pkexec && printf '%s\n' pkexec
    command_exists sudo && printf '%s\n' sudo
  fi
}

run_privileged() {
  local program="$1"
  shift

  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    "$program" "$@"
    return $?
  fi

  local helpers=()
  mapfile -t helpers < <(privilege_helpers)
  if (( ${#helpers[@]} == 0 )); then
    echo "No sudo or pkexec command is available; install manually: $program $*" >&2
    return 1
  fi

  local helper status
  for helper in "${helpers[@]}"; do
    echo "Requesting administrator permission with $helper for: $program $*" >&2
    "$helper" "$program" "$@" && return 0
    status=$?
    echo "$helper failed with status $status; trying next privilege helper if available." >&2
  done

  echo "Privileged command failed: $program $*" >&2
  return 1
}

package_available() {
  local manager="$1"
  local package="$2"
  case "$manager" in
    apt)
      apt-cache show "$package" >/dev/null 2>&1
      ;;
    dnf)
      dnf -q info "$package" >/dev/null 2>&1
      ;;
    pacman)
      pacman -Si "$package" >/dev/null 2>&1
      ;;
    zypper)
      zypper --non-interactive search --exact-match "$package" >/dev/null 2>&1
      ;;
    *)
      return 1
      ;;
  esac
}

install_packages() {
  local manager="$1"
  shift
  local packages=("$@")
  if (( ${#packages[@]} == 0 )); then
    return 0
  fi

  case "$manager" in
    apt)
      run_privileged apt-get update
      run_privileged apt-get install -y --no-install-recommends "${packages[@]}"
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
      echo "No supported package manager detected; install manually: ${packages[*]}" >&2
      return 1
      ;;
  esac
}

resolve_packages() {
  local manager="$1"
  shift
  local resolved=()
  local -A seen=()
  local package
  for package in "$@"; do
    if [[ -z "${seen[$package]+x}" ]] && package_available "$manager" "$package"; then
      resolved+=("$package")
      seen["$package"]=1
    fi
  done
  printf '%s\n' "${resolved[@]}"
}

manager="$(detect_manager)"
missing_commands=()
for program in pipewire pactl wpctl pw-cli pw-dump pw-metadata pw-top; do
  if ! command_exists "$program"; then
    missing_commands+=("$program")
  fi
done
missing_webkit_helpers=()
for program in bwrap xdg-dbus-proxy; do
  if ! command_exists "$program"; then
    missing_webkit_helpers+=("$program")
  fi
done
if [[ "${XDG_SESSION_TYPE:-}" == "wayland" ]] && ! command_exists Xwayland; then
  missing_webkit_helpers+=("Xwayland")
fi
missing_streamer_commands=()
if ! command_exists aseqdump; then
  missing_streamer_commands+=("aseqdump")
fi
missing_libraries=()
if ! library_available 'libusb-1\.0\.so\.0' /usr/lib/libusb-1.0.so.0 /usr/lib64/libusb-1.0.so.0 /usr/lib/x86_64-linux-gnu/libusb-1.0.so.0; then
  missing_libraries+=("libusb-1.0")
fi
if ! library_available 'libEGL\.so\.1' /usr/lib/libEGL.so.1 /usr/lib64/libEGL.so.1 /usr/lib/x86_64-linux-gnu/libEGL.so.1; then
  missing_libraries+=("libEGL")
fi
if ! library_available 'libGL\.so\.1' /usr/lib/libGL.so.1 /usr/lib64/libGL.so.1 /usr/lib/x86_64-linux-gnu/libGL.so.1; then
  missing_libraries+=("libGL")
fi
if ! library_available 'libgbm\.so\.1' /usr/lib/libgbm.so.1 /usr/lib64/libgbm.so.1 /usr/lib/x86_64-linux-gnu/libgbm.so.1; then
  missing_libraries+=("libgbm")
fi
if ! library_available 'libdrm\.so\.2' /usr/lib/libdrm.so.2 /usr/lib64/libdrm.so.2 /usr/lib/x86_64-linux-gnu/libdrm.so.2; then
  missing_libraries+=("libdrm")
fi
if ! library_available 'libwayland-client\.so\.0' /usr/lib/libwayland-client.so.0 /usr/lib64/libwayland-client.so.0 /usr/lib/x86_64-linux-gnu/libwayland-client.so.0; then
  missing_libraries+=("libwayland-client")
fi
if ! library_available 'libwayland-cursor\.so\.0' /usr/lib/libwayland-cursor.so.0 /usr/lib64/libwayland-cursor.so.0 /usr/lib/x86_64-linux-gnu/libwayland-cursor.so.0; then
  missing_libraries+=("libwayland-cursor")
fi
if ! library_available 'libwayland-egl\.so\.1' /usr/lib/libwayland-egl.so.1 /usr/lib64/libwayland-egl.so.1 /usr/lib/x86_64-linux-gnu/libwayland-egl.so.1; then
  missing_libraries+=("libwayland-egl")
fi
if ! library_available 'libwayland-server\.so\.0' /usr/lib/libwayland-server.so.0 /usr/lib64/libwayland-server.so.0 /usr/lib/x86_64-linux-gnu/libwayland-server.so.0; then
  missing_libraries+=("libwayland-server")
fi
streamer_discovery_notes=()
if [[ ! -d /sys/class/hidraw ]]; then
  streamer_discovery_notes+=("hidraw sysfs unavailable")
fi
if [[ ! -r /proc/asound/seq/clients ]]; then
  streamer_discovery_notes+=("ALSA sequencer client list unavailable")
fi

audio_candidates=()
streamer_candidates=()
usb_candidates=()
egl_candidates=()
gl_candidates=()
gbm_candidates=()
drm_candidates=()
wayland_client_candidates=()
wayland_cursor_candidates=()
wayland_egl_candidates=()
wayland_server_candidates=()
bwrap_candidates=()
dbus_proxy_candidates=()
xwayland_candidates=()

case "$manager" in
  apt)
    audio_candidates=(pipewire wireplumber pipewire-pulse pipewire-bin pulseaudio-utils)
    streamer_candidates=(alsa-utils)
    usb_candidates=(libusb-1.0-0)
    egl_candidates=(libegl1)
    gl_candidates=(libgl1)
    gbm_candidates=(libgbm1)
    drm_candidates=(libdrm2)
    wayland_client_candidates=(libwayland-client0)
    wayland_cursor_candidates=(libwayland-cursor0)
    wayland_egl_candidates=(libwayland-egl1)
    wayland_server_candidates=(libwayland-server0)
    bwrap_candidates=(bubblewrap)
    dbus_proxy_candidates=(xdg-dbus-proxy)
    xwayland_candidates=(xwayland)
    ;;
  dnf)
    audio_candidates=(pipewire pipewire-utils wireplumber pipewire-pulseaudio pulseaudio-utils)
    streamer_candidates=(alsa-utils)
    usb_candidates=(libusb1)
    egl_candidates=(mesa-libEGL)
    gl_candidates=(mesa-libGL)
    gbm_candidates=(mesa-libgbm)
    drm_candidates=(libdrm)
    wayland_client_candidates=(libwayland-client)
    wayland_cursor_candidates=(libwayland-cursor)
    wayland_egl_candidates=(libwayland-egl)
    wayland_server_candidates=(libwayland-server)
    bwrap_candidates=(bubblewrap)
    dbus_proxy_candidates=(xdg-dbus-proxy)
    xwayland_candidates=(xorg-x11-server-Xwayland)
    ;;
  pacman)
    audio_candidates=(pipewire wireplumber pipewire-pulse libpulse)
    streamer_candidates=(alsa-utils)
    usb_candidates=(libusb)
    egl_candidates=(libglvnd)
    gl_candidates=(libglvnd)
    gbm_candidates=(mesa)
    drm_candidates=(libdrm)
    wayland_client_candidates=(wayland)
    wayland_cursor_candidates=(wayland)
    wayland_egl_candidates=(wayland)
    wayland_server_candidates=(wayland)
    bwrap_candidates=(bubblewrap)
    dbus_proxy_candidates=(xdg-dbus-proxy)
    xwayland_candidates=(xorg-xwayland)
    ;;
  zypper)
    audio_candidates=(pipewire wireplumber pipewire-pulseaudio pulseaudio-utils)
    streamer_candidates=(alsa)
    usb_candidates=(libusb-1_0-0)
    egl_candidates=(Mesa-libEGL1)
    gl_candidates=(Mesa-libGL1)
    gbm_candidates=(libgbm1)
    drm_candidates=(libdrm2)
    wayland_client_candidates=(libwayland-client0)
    wayland_cursor_candidates=(libwayland-cursor0)
    wayland_egl_candidates=(libwayland-egl1)
    wayland_server_candidates=(libwayland-server0)
    bwrap_candidates=(bubblewrap)
    dbus_proxy_candidates=(xdg-dbus-proxy)
    xwayland_candidates=(xwayland)
    ;;
esac

runtime_candidates=()
if (( ${#missing_commands[@]} > 0 )); then
  runtime_candidates+=("${audio_candidates[@]}")
fi
if (( ${#missing_streamer_commands[@]} > 0 )); then
  runtime_candidates+=("${streamer_candidates[@]}")
fi
for library in "${missing_libraries[@]}"; do
  case "$library" in
    libusb-1.0)
      runtime_candidates+=("${usb_candidates[@]}")
      ;;
    libEGL)
      runtime_candidates+=("${egl_candidates[@]}")
      ;;
    libGL)
      runtime_candidates+=("${gl_candidates[@]}")
      ;;
    libgbm)
      runtime_candidates+=("${gbm_candidates[@]}")
      ;;
    libdrm)
      runtime_candidates+=("${drm_candidates[@]}")
      ;;
    libwayland-client)
      runtime_candidates+=("${wayland_client_candidates[@]}")
      ;;
    libwayland-cursor)
      runtime_candidates+=("${wayland_cursor_candidates[@]}")
      ;;
    libwayland-egl)
      runtime_candidates+=("${wayland_egl_candidates[@]}")
      ;;
    libwayland-server)
      runtime_candidates+=("${wayland_server_candidates[@]}")
      ;;
  esac
done
for helper in "${missing_webkit_helpers[@]}"; do
  case "$helper" in
    bwrap)
      runtime_candidates+=("${bwrap_candidates[@]}")
      ;;
    xdg-dbus-proxy)
      runtime_candidates+=("${dbus_proxy_candidates[@]}")
      ;;
    Xwayland)
      runtime_candidates+=("${xwayland_candidates[@]}")
      ;;
  esac
done

echo "WaveLinux dependency check"
echo "Package manager: $manager"

if (( ${#missing_commands[@]} == 0 )); then
  echo "Runtime tools: ok"
else
  echo "Runtime tools missing: ${missing_commands[*]}"
fi

if (( ${#missing_libraries[@]} == 0 )); then
  echo "Runtime libraries: ok"
else
  echo "Runtime libraries missing: ${missing_libraries[*]}"
fi

if (( ${#missing_webkit_helpers[@]} == 0 )); then
  echo "WebKit/AppImage helpers: ok"
else
  echo "WebKit/AppImage helpers missing: ${missing_webkit_helpers[*]}"
fi

echo "Native effects: bundled"
if (( ${#streamer_discovery_notes[@]} == 0 )); then
  echo "Streamer device discovery: ok"
else
  echo "Streamer device discovery notes: ${streamer_discovery_notes[*]}"
fi
if (( ${#missing_streamer_commands[@]} == 0 )); then
  echo "Streamer device runtime: ok"
else
  echo "Streamer device runtime missing: ${missing_streamer_commands[*]}"
fi

if (( INSTALL == 1 && ( ${#missing_commands[@]} > 0 || ${#missing_streamer_commands[@]} > 0 || ${#missing_libraries[@]} > 0 || ${#missing_webkit_helpers[@]} > 0 ) )); then
  mapfile -t packages < <(resolve_packages "$manager" "${runtime_candidates[@]}")
  if (( ${#packages[@]} > 0 )); then
    echo "Installing runtime packages: ${packages[*]}"
    install_packages "$manager" "${packages[@]}"
    INSTALLED_PACKAGES=1
  else
    echo "No runtime package candidates were available for automatic install." >&2
  fi
fi

if (( INSTALLED_PACKAGES == 1 )); then
  verify_args=()
  (( STRICT == 1 )) && verify_args+=(--strict)
  (( STRICT_RUNTIME == 1 )) && verify_args+=(--strict-runtime)
  echo "Rechecking dependencies after package installation..."
  exec env \
    WAVELINUX_INSTALL_DEPS=0 \
    bash "$0" "${verify_args[@]}"
fi

runtime_missing=0
if (( ${#missing_commands[@]} > 0 || ${#missing_streamer_commands[@]} > 0 || ${#missing_libraries[@]} > 0 || ${#missing_webkit_helpers[@]} > 0 )); then
  runtime_missing=1
fi

if (( STRICT_RUNTIME == 1 && runtime_missing == 1 )); then
  exit 1
fi

if (( STRICT == 1 && runtime_missing == 1 )); then
  exit 1
fi
