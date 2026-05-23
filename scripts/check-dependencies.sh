#!/usr/bin/env bash
set -euo pipefail

INSTALL=0
INSTALL_EFFECTS=0
STRICT=0

usage() {
  cat <<'HELP'
Check WaveLinux runtime and optional effect dependencies.

Usage:
  bash scripts/check-dependencies.sh [--install] [--install-effects] [--strict]

Environment:
  WAVELINUX_INSTALL_DEPS=1      Install missing runtime dependencies.
  WAVELINUX_INSTALL_EFFECTS=1   Install missing optional effect packages.
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
      $sudo_cmd pacman -S --needed --noconfirm "${packages[@]}"
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
    /usr/lib/x86_64-linux-gnu/ladspa
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

runtime_candidates=()
effect_candidates=()
aur_effect_candidates=()

case "$manager" in
  apt)
    runtime_candidates=(pipewire wireplumber pipewire-pulse pipewire-bin pulseaudio-utils libwebkit2gtk-4.1-0 libayatana-appindicator3-1)
    effect_candidates=(swh-plugins lsp-plugins-ladspa librnnoise-ladspa deepfilternet-ladspa deepfilternet)
    ;;
  dnf)
    runtime_candidates=(pipewire wireplumber pipewire-pulseaudio pulseaudio-utils webkit2gtk4.1 libappindicator-gtk3)
    effect_candidates=(ladspa-swh-plugins lsp-plugins-ladspa rnnoise noise-suppression-for-voice deepfilternet)
    ;;
  pacman)
    runtime_candidates=(pipewire wireplumber pipewire-pulse libpulse webkit2gtk-4.1 gtk3 libayatana-appindicator)
    effect_candidates=(swh-plugins lsp-plugins rnnoise)
    aur_effect_candidates=(noise-suppression-for-voice deepfilternet deepfilternet-ladspa)
    ;;
  zypper)
    runtime_candidates=(pipewire wireplumber pipewire-pulseaudio pulseaudio-utils libwebkit2gtk-4_1-0 typelib-1_0-AyatanaAppIndicator3-0_1)
    effect_candidates=(ladspa-swh-plugins lsp-plugins-ladspa rnnoise deepfilternet)
    ;;
esac

missing_effects=()
if ! ladspa_has_any 'libdeep_filter_ladspa.so' 'deep_filter_ladspa.so' 'libdeepfilternet_ladspa.so' 'deepfilternet_ladspa.so' 'libdeep_filter_net_ladspa.so' 'deep_filter_net_ladspa.so'; then
  missing_effects+=("DeepFilterNet")
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

if (( ${#missing_effects[@]} == 0 )); then
  echo "Optional effects: ok"
else
  echo "Optional effects missing: ${missing_effects[*]}"
fi

if (( INSTALL == 1 && ${#missing_commands[@]} > 0 )); then
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
    echo "Installing optional effect packages: ${packages[*]}"
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
      echo "Installing optional AUR effect packages: ${aur_packages[*]}"
      install_aur_packages "${aur_packages[@]}"
    fi
  fi
fi

if (( STRICT == 1 && ( ${#missing_commands[@]} > 0 || ${#missing_effects[@]} > 0 ) )); then
  exit 1
fi
