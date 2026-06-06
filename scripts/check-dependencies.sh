#!/usr/bin/env bash
set -euo pipefail

INSTALL=0
INSTALL_EFFECTS=0
STRICT=0
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
  cat <<'HELP'
Check WaveLinux runtime and effect dependencies.

Usage:
  bash scripts/check-dependencies.sh [--install] [--install-effects] [--strict]

Environment:
  WAVELINUX_INSTALL_DEPS=1      Install missing runtime dependencies.
  WAVELINUX_INSTALL_EFFECTS=1   Install missing effect packages.
HELP
}

for arg in "$@"; do
  case "$arg" in
    --install)
      INSTALL=1
      ;;
    --install-effects)
      INSTALL_EFFECTS=1
      ;;
    --strict)
      STRICT=1
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

if [[ "${WAVELINUX_INSTALL_EFFECTS:-0}" == "1" ]]; then
  INSTALL_EFFECTS=1
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

sudo_prefix() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    return 0
  fi
  if command_exists sudo; then
    printf '%s\n' sudo
  fi
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

aur_package_available() {
  local package="$1"
  if command_exists paru; then
    paru -Si "$package" >/dev/null 2>&1
  elif command_exists yay; then
    yay -Si "$package" >/dev/null 2>&1
  else
    return 1
  fi
}

install_packages() {
  local manager="$1"
  shift
  local packages=("$@")
  if (( ${#packages[@]} == 0 )); then
    return 0
  fi

  local sudo_cmd
  sudo_cmd="$(sudo_prefix || true)"
  if [[ "${EUID:-$(id -u)}" -ne 0 && -z "$sudo_cmd" ]]; then
    echo "sudo is unavailable; install manually: ${packages[*]}" >&2
    return 0
  fi

  case "$manager" in
    apt)
      $sudo_cmd apt-get update
      $sudo_cmd apt-get install -y "${packages[@]}"
      ;;
    dnf)
      $sudo_cmd dnf install -y "${packages[@]}"
      ;;
    pacman)
      $sudo_cmd pacman -Syu --needed --noconfirm "${packages[@]}"
      ;;
    zypper)
      $sudo_cmd zypper --non-interactive install --no-recommends "${packages[@]}"
      ;;
    *)
      echo "No supported package manager detected; install manually: ${packages[*]}" >&2
      ;;
  esac
}

install_aur_packages() {
  local packages=("$@")
  if (( ${#packages[@]} == 0 )); then
    return 0
  fi
  if command_exists paru; then
    paru -S --needed --noconfirm "${packages[@]}"
  elif command_exists yay; then
    yay -S --needed --noconfirm "${packages[@]}"
  else
    echo "No AUR helper found; install manually: ${packages[*]}" >&2
  fi
}

existing_ladspa_paths() {
  local paths=()
  if [[ -n "${LADSPA_PATH:-}" ]]; then
    IFS=':' read -r -a paths <<< "$LADSPA_PATH"
  fi
  paths+=(
    /usr/lib/ladspa
    /usr/lib64/ladspa
    /usr/local/lib/ladspa
    /usr/local/lib64/ladspa
    /usr/lib/x86_64-linux-gnu/ladspa
    /usr/lib/aarch64-linux-gnu/ladspa
    /usr/lib/arm-linux-gnueabihf/ladspa
    "$ROOT_DIR/crates/app/appimage-extra/usr/wavelinux-runtime/lib/ladspa"
  )
  printf '%s\n' "${paths[@]}" | awk 'NF && !seen[$0]++'
}

ladspa_has_any() {
  local pattern
  while IFS= read -r root; do
    [[ -d "$root" ]] || continue
    for pattern in "$@"; do
      compgen -G "$root/$pattern" >/dev/null 2>&1 && return 0
    done
  done < <(existing_ladspa_paths)
  return 1
}

resolve_packages() {
  local manager="$1"
  shift
  local resolved=()
  local package
  for package in "$@"; do
    if package_available "$manager" "$package"; then
      resolved+=("$package")
    fi
  done
  printf '%s\n' "${resolved[@]}"
}

manager="$(detect_manager)"
missing_commands=()
for program in pipewire pactl wpctl pw-cli pw-dump; do
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
streamer_discovery_notes=()
if [[ ! -d /sys/class/hidraw ]]; then
  streamer_discovery_notes+=("hidraw sysfs unavailable")
fi
if [[ ! -r /proc/asound/seq/clients ]]; then
  streamer_discovery_notes+=("ALSA sequencer client list unavailable")
fi

runtime_candidates=()
effect_candidates=()
aur_effect_candidates=()

case "$manager" in
  apt)
    runtime_candidates=(pipewire wireplumber pipewire-pulse pipewire-bin pulseaudio-utils alsa-utils libwebkit2gtk-4.1-0 libayatana-appindicator3-1 libusb-1.0-0 bubblewrap xdg-dbus-proxy xwayland libegl1 libgl1 libgbm1 libdrm2 gstreamer1.0-plugins-base gstreamer1.0-plugins-good fonts-dejavu-core xdg-desktop-portal xdg-desktop-portal-gtk)
    effect_candidates=(swh-plugins lsp-plugins-ladspa librnnoise-ladspa deepfilternet-ladspa deepfilternet)
    ;;
  dnf)
    runtime_candidates=(pipewire wireplumber pipewire-pulseaudio pulseaudio-utils alsa-utils webkit2gtk4.1 libappindicator-gtk3 libusb1 bubblewrap xdg-dbus-proxy xorg-x11-server-Xwayland mesa-libEGL mesa-libGL mesa-libgbm libdrm gstreamer1-plugins-base gstreamer1-plugins-good google-noto-sans-fonts xdg-desktop-portal xdg-desktop-portal-gtk)
    effect_candidates=(ladspa-swh-plugins lsp-plugins-ladspa rnnoise noise-suppression-for-voice deepfilternet)
    ;;
  pacman)
    runtime_candidates=(pipewire wireplumber pipewire-pulse libpulse alsa-utils webkit2gtk-4.1 gtk3 libayatana-appindicator libusb bubblewrap xdg-dbus-proxy xorg-xwayland mesa libglvnd gstreamer gst-plugins-base-libs gst-plugins-good noto-fonts xdg-desktop-portal xdg-desktop-portal-gtk)
    effect_candidates=(swh-plugins noise-suppression-for-voice)
    aur_effect_candidates=(deepfilternet-plugin-pipewire-bin noise-suppression-for-voice deepfilternet deepfilternet-ladspa)
    ;;
  zypper)
    runtime_candidates=(pipewire wireplumber pipewire-pulseaudio pulseaudio-utils alsa libwebkit2gtk-4_1-0 typelib-1_0-AyatanaAppIndicator3-0_1 libusb-1_0-0 bubblewrap xdg-dbus-proxy xwayland libwebkit2gtk-4_1-0 Mesa-libEGL1 Mesa-libGL1 libgbm1 libdrm2 gstreamer-plugins-base gstreamer-plugins-good google-noto-sans-fonts xdg-desktop-portal xdg-desktop-portal-gtk)
    effect_candidates=(ladspa-swh-plugins lsp-plugins-ladspa rnnoise deepfilternet)
    ;;
esac

ladspa_file_has_marker() {
  local marker="$1"
  shift
  local pattern path
  while IFS= read -r root; do
    [[ -d "$root" ]] || continue
    for pattern in "$@"; do
      for path in "$root"/$pattern; do
        [[ -f "$path" ]] || continue
        grep -a -q "$marker" "$path" && return 0
      done
    done
  done < <(existing_ladspa_paths)
  return 1
}

missing_effects=()
if ! ladspa_file_has_marker 'DeepFilterNet3' 'libdeep_filter_ladspa.so' 'deep_filter_ladspa.so' 'libdeepfilternet_ladspa.so' 'deepfilternet_ladspa.so' 'libdeep_filter_net_ladspa.so' 'deep_filter_net_ladspa.so'; then
  missing_effects+=("DeepFilterNet3")
fi
if ! ladspa_has_any 'librnnoise_ladspa.so' 'rnnoise_ladspa.so'; then
  missing_effects+=("RNNoise")
fi
if ! ladspa_has_any 'sc4_1882.so' 'gate_1410.so' 'fast_lookahead_limiter_1913.so' 'hard_limiter_1413.so'; then
  missing_effects+=("SWH LADSPA dynamics")
fi

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

if (( ${#missing_effects[@]} == 0 )); then
  echo "Effect plugins: ok"
else
  echo "Effect plugins missing: ${missing_effects[*]}"
fi
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
  else
    echo "No runtime package candidates were available for automatic install." >&2
  fi
fi

if (( INSTALL_EFFECTS == 1 && ${#missing_effects[@]} > 0 )); then
  mapfile -t packages < <(resolve_packages "$manager" "${effect_candidates[@]}")
  if (( ${#packages[@]} > 0 )); then
    echo "Installing effect packages: ${packages[*]}"
    install_packages "$manager" "${packages[@]}"
  fi

  if [[ "$manager" == "pacman" ]]; then
    aur_packages=()
    for package in "${aur_effect_candidates[@]}"; do
      if aur_package_available "$package"; then
        aur_packages+=("$package")
      fi
    done
    if (( ${#aur_packages[@]} > 0 )); then
      echo "Installing AUR effect packages: ${aur_packages[*]}"
      install_aur_packages "${aur_packages[@]}"
    fi
  fi
fi

if (( STRICT == 1 && ( ${#missing_commands[@]} > 0 || ${#missing_streamer_commands[@]} > 0 || ${#missing_libraries[@]} > 0 || ${#missing_webkit_helpers[@]} > 0 || ${#missing_effects[@]} > 0 ) )); then
  exit 1
fi
