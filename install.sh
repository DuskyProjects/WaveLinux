#!/usr/bin/env bash
set -euo pipefail

REPOSITORY="DuskyProjects/WaveLinux"
DEFAULT_TAG="v6.0.0-alpha.1"
REQUESTED_TAG="${WAVELINUX_RELEASE_TAG:-}"
REQUESTED_FORMAT="auto"
DRY_RUN=0

usage() {
  cat <<'HELP'
WaveLinux 6 distro-aware installer

Usage:
  ./install.sh [options]

Options:
  --tag TAG                 Install a specific release tag.
  --format auto|deb|rpm|appimage
                            Override automatic package selection.
  --dry-run                 Show the selected release and package without installing.
  -h, --help                Show this help.

Environment:
  WAVELINUX_RELEASE_TAG     Same as --tag.
HELP
}

while (($#)); do
  case "$1" in
    --tag)
      [[ $# -ge 2 ]] || { echo "--tag requires a value" >&2; exit 2; }
      REQUESTED_TAG="$2"
      shift 2
      ;;
    --format)
      [[ $# -ge 2 ]] || { echo "--format requires a value" >&2; exit 2; }
      REQUESTED_FORMAT="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$REQUESTED_FORMAT" in
  auto|deb|rpm|appimage) ;;
  *)
    echo "Unsupported format: $REQUESTED_FORMAT" >&2
    exit 2
    ;;
esac

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "Run this installer as your normal desktop user, not with sudo." >&2
  echo "It requests administrator permission only when the package manager needs it." >&2
  exit 1
fi

case "$(uname -m)" in
  x86_64|amd64) ;;
  *)
    echo "WaveLinux 6 currently supports x86_64 only; detected $(uname -m)." >&2
    exit 1
    ;;
esac

http_download() {
  local url="$1"
  local destination="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fL --retry 3 --retry-delay 2 --output "$destination" "$url"
  elif command -v wget >/dev/null 2>&1; then
    wget -O "$destination" "$url"
  else
    echo "Install curl or wget, then run this installer again." >&2
    return 1
  fi
}

run_privileged() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  elif command -v pkexec >/dev/null 2>&1; then
    pkexec "$@"
  else
    echo "Neither sudo nor pkexec is available. Run this command manually:" >&2
    printf '  %q' "$@" >&2
    printf '\n' >&2
    return 1
  fi
}

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux6-network-install.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

if [[ -z "$REQUESTED_TAG" ]]; then
  release_list="$WORK_DIR/releases.json"
  if http_download \
    "https://api.github.com/repos/$REPOSITORY/releases?per_page=20" \
    "$release_list"; then
    REQUESTED_TAG="$(
      sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' \
        "$release_list" | head -n1
    )"
  fi
fi
REQUESTED_TAG="${REQUESTED_TAG:-$DEFAULT_TAG}"

release_json="$WORK_DIR/release.json"
http_download \
  "https://api.github.com/repos/$REPOSITORY/releases/tags/$REQUESTED_TAG" \
  "$release_json"

mapfile -t release_urls < <(
  sed -n 's/.*"browser_download_url":[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$release_json"
)

if ((${#release_urls[@]} == 0)); then
  echo "Release $REQUESTED_TAG has no downloadable assets." >&2
  exit 1
fi

OS_ID="unknown"
OS_LIKE=""
OS_NAME="Linux"
if [[ -r /etc/os-release ]]; then
  # shellcheck source=/dev/null
  source /etc/os-release
  OS_ID="${ID:-unknown}"
  OS_LIKE="${ID_LIKE:-}"
  OS_NAME="${PRETTY_NAME:-${NAME:-Linux}}"
fi
identity=" ${OS_ID,,} ${OS_LIKE,,} "

select_format() {
  if [[ "$REQUESTED_FORMAT" != "auto" ]]; then
    printf '%s\n' "$REQUESTED_FORMAT"
    return
  fi

  if command -v apt-get >/dev/null 2>&1 \
    && [[ "$identity" =~ (debian|ubuntu|linuxmint|pop|neon|elementary|zorin) ]]; then
    printf '%s\n' deb
  elif command -v dnf >/dev/null 2>&1 \
    && [[ "$identity" =~ (fedora|rhel|centos|rocky|almalinux) ]]; then
    printf '%s\n' rpm
  elif command -v zypper >/dev/null 2>&1 \
    && [[ "$identity" =~ (suse|opensuse) ]]; then
    printf '%s\n' rpm
  elif command -v pacman >/dev/null 2>&1 \
    && [[ "$identity" =~ (arch|cachyos|manjaro|endeavouros|garuda) ]]; then
    printf '%s\n' appimage
  elif command -v apt-get >/dev/null 2>&1; then
    printf '%s\n' deb
  elif command -v dnf >/dev/null 2>&1 || command -v zypper >/dev/null 2>&1; then
    printf '%s\n' rpm
  else
    printf '%s\n' appimage
  fi
}

FORMAT="$(select_format)"

asset_url_for_format() {
  local format="$1"
  local url
  for url in "${release_urls[@]}"; do
    case "$format:$url" in
      deb:*.deb|rpm:*.rpm|appimage:*.AppImage)
        printf '%s\n' "$url"
        return 0
        ;;
    esac
  done
  return 1
}

ASSET_URL="$(asset_url_for_format "$FORMAT" || true)"
if [[ -z "$ASSET_URL" ]]; then
  echo "Release $REQUESTED_TAG does not contain a $FORMAT package." >&2
  echo "Available assets:" >&2
  printf '  %s\n' "${release_urls[@]##*/}" >&2
  exit 1
fi
ASSET_NAME="${ASSET_URL##*/}"
ASSET_PATH="$WORK_DIR/$ASSET_NAME"
CHECKSUM_URL=""
for url in "${release_urls[@]}"; do
  if [[ "$url" == */SHA256SUMS ]]; then
    CHECKSUM_URL="$url"
    break
  fi
done

printf 'Detected system: %s\n' "$OS_NAME"
printf 'Selected release: %s\n' "$REQUESTED_TAG"
printf 'Selected package: %s\n' "$ASSET_NAME"

if ((DRY_RUN == 1)); then
  exit 0
fi

http_download "$ASSET_URL" "$ASSET_PATH"

if [[ -n "$CHECKSUM_URL" ]]; then
  checksum_file="$WORK_DIR/SHA256SUMS"
  http_download "$CHECKSUM_URL" "$checksum_file"
  expected_checksum="$(
    awk -v filename="$ASSET_NAME" '
      $2 == filename || $2 == "*" filename { print $1; exit }
    ' "$checksum_file"
  )"
  [[ -n "$expected_checksum" ]] || {
    echo "SHA256SUMS does not contain $ASSET_NAME." >&2
    exit 1
  }
  actual_checksum="$(sha256sum "$ASSET_PATH" | awk '{print $1}')"
  [[ "$actual_checksum" == "$expected_checksum" ]] || {
    echo "Checksum verification failed for $ASSET_NAME." >&2
    exit 1
  }
  echo "Checksum verified."
else
  echo "Release is missing SHA256SUMS; refusing an unverified installation." >&2
  exit 1
fi

install_deb() {
  echo "Installing Debian package and dependencies..."
  run_privileged apt-get update
  run_privileged apt-get install -y "$ASSET_PATH"
}

install_rpm() {
  if command -v dnf >/dev/null 2>&1; then
    echo "Installing RPM package and dependencies with dnf..."
    run_privileged dnf install -y "$ASSET_PATH"
  elif command -v zypper >/dev/null 2>&1; then
    echo "Installing RPM package and dependencies with zypper..."
    run_privileged zypper --non-interactive install --allow-unsigned-rpm "$ASSET_PATH"
  elif command -v yum >/dev/null 2>&1; then
    echo "Installing RPM package and dependencies with yum..."
    run_privileged yum localinstall -y "$ASSET_PATH"
  else
    echo "An RPM package was selected, but dnf, zypper, and yum are unavailable." >&2
    exit 1
  fi
}

arch_portal_package() {
  local desktop="${XDG_CURRENT_DESKTOP:-${DESKTOP_SESSION:-}}"
  desktop="${desktop,,}"
  case "$desktop" in
    *kde*|*plasma*) printf '%s\n' xdg-desktop-portal-kde ;;
    *gnome*) printf '%s\n' xdg-desktop-portal-gnome ;;
    *hyprland*) printf '%s\n' xdg-desktop-portal-hyprland ;;
    *) printf '%s\n' xdg-desktop-portal-gtk ;;
  esac
}

install_arch_appimage_dependencies() {
  local portal
  portal="$(arch_portal_package)"
  echo "Installing Arch/CachyOS AppImage dependencies..."
  run_privileged pacman -Syu --needed --noconfirm \
    pipewire \
    wireplumber \
    pipewire-pulse \
    libpulse \
    alsa-utils \
    bubblewrap \
    xdg-dbus-proxy \
    xorg-xwayland \
    mesa \
    libglvnd \
    wayland \
    libdrm \
    libusb \
    xdg-desktop-portal \
    "$portal"
}

install_appimage() {
  if command -v pacman >/dev/null 2>&1; then
    install_arch_appimage_dependencies
  else
    echo "No supported native package manager was detected; installing the AppImage." >&2
    echo "You may need to install PipeWire, WirePlumber, pipewire-pulse, ALSA utilities," >&2
    echo "Wayland/Xwayland, bubblewrap, xdg-dbus-proxy, and a desktop portal manually." >&2
  fi

  chmod 0755 "$ASSET_PATH"
  local data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
  local bin_home="${XDG_BIN_HOME:-$HOME/.local/bin}"
  local support_dir="$data_home/wavelinux6"
  local applications_dir="$data_home/applications"
  local icon_dir="$data_home/icons/hicolor/512x512/apps"
  local installed_appimage="$support_dir/$ASSET_NAME"
  local launcher="$bin_home/wavelinux6"
  local audio_core="$bin_home/wavelinux6-audio-core"
  local peripheral_plugin="$bin_home/wavelinux6-peripheral-plugin"
  local extract_dir="$WORK_DIR/appimage-extract"

  install -d "$support_dir" "$bin_home" "$applications_dir" "$icon_dir" "$extract_dir"
  rm -f "$support_dir"/WaveLinux6_*_amd64.AppImage
  install -m 0755 "$ASSET_PATH" "$installed_appimage"

  (
    cd "$extract_dir"
    "$installed_appimage" --appimage-extract >/dev/null
  )

  local appdir="$extract_dir/squashfs-root"
  local bundled_audio_core="$appdir/usr/wavelinux-runtime/bin/wavelinux6-audio-core"
  local bundled_peripheral="$appdir/usr/wavelinux-runtime/bin/wavelinux6-peripheral-plugin"
  [[ -x "$bundled_audio_core" ]] || {
    echo "The AppImage does not contain wavelinux6-audio-core." >&2
    exit 1
  }
  [[ -x "$bundled_peripheral" ]] || {
    echo "The AppImage does not contain wavelinux6-peripheral-plugin." >&2
    exit 1
  }
  install -m 0755 "$bundled_audio_core" "$audio_core"
  install -m 0755 "$bundled_peripheral" "$peripheral_plugin"

  cat > "$launcher" <<LAUNCHER
#!/usr/bin/env bash
set -euo pipefail
export WAVELINUX_XDG_APP_NAME="WaveLinux6"
export WAVELINUX_GRAPH_PREFIX="wavelinux6"
export WAVELINUX_GRAPH_PROPERTY_PREFIX="wavelinux6"
export WAVELINUX_APP_DISPLAY_NAME="WaveLinux 6"
export WAVELINUX_AUDIO_RUNTIME="\${WAVELINUX_AUDIO_RUNTIME:-dsp_auto}"
export WAVELINUX_DSP_PROVIDER="\${WAVELINUX_DSP_PROVIDER:-auto}"
export WAVELINUX_DSP_HELPER="\${WAVELINUX_DSP_HELPER:-$audio_core}"
export WAVELINUX_PERIPHERAL_PLUGIN="\${WAVELINUX_PERIPHERAL_PLUGIN:-$peripheral_plugin}"
if command -v pipewire >/dev/null 2>&1; then
  export WAVELINUX_FILTER_CHAIN_PIPEWIRE="\${WAVELINUX_FILTER_CHAIN_PIPEWIRE:-\$(command -v pipewire)}"
fi
unset CEF_PATH CEF_ROOT GIO_EXTRA_MODULES GIO_MODULE_DIR GI_TYPELIB_PATH \
  GST_PLUGIN_PATH GST_PLUGIN_PATH_1_0 GST_PLUGIN_SCANNER GST_PLUGIN_SCANNER_1_0 \
  GST_PLUGIN_SYSTEM_PATH GST_PLUGIN_SYSTEM_PATH_1_0 GTK_PATH LD_AUDIT \
  LD_LIBRARY_PATH LD_PRELOAD LIBRARY_PATH WEBKIT_EXEC_PATH 2>/dev/null || true
export WEBKIT_DISABLE_DMABUF_RENDERER="\${WEBKIT_DISABLE_DMABUF_RENDERER:-1}"
export WEBKIT_DISABLE_COMPOSITING_MODE="\${WEBKIT_DISABLE_COMPOSITING_MODE:-1}"
exec "$installed_appimage" "\$@"
LAUNCHER
  chmod 0755 "$launcher"

  local icon_source=""
  icon_source="$(
    find "$appdir/usr/share/icons" -type f \
      \( -iname '*wavelinux*.png' -o -iname '*wavelinux*.svg' \) \
      2>/dev/null | sort | tail -n1
  )"
  if [[ -n "$icon_source" ]]; then
    install -m 0644 "$icon_source" "$icon_dir/wavelinux6.${icon_source##*.}"
  fi

  cat > "$applications_dir/wavelinux6.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=WaveLinux 6
Comment=Linux creator audio mixer
Exec=$launcher
Icon=wavelinux6
Terminal=false
Categories=Audio;AudioVideo;Mixer;
StartupWMClass=io.github.duskyprojects.WaveLinux6
DESKTOP
  chmod 0644 "$applications_dir/wavelinux6.desktop"

  command -v update-desktop-database >/dev/null 2>&1 \
    && update-desktop-database "$applications_dir" >/dev/null 2>&1 || true
  command -v gtk-update-icon-cache >/dev/null 2>&1 \
    && gtk-update-icon-cache -q "$data_home/icons/hicolor" >/dev/null 2>&1 || true
  command -v systemctl >/dev/null 2>&1 \
    && systemctl --user start pipewire.socket pipewire-pulse.socket wireplumber.service \
      >/dev/null 2>&1 || true

  "$audio_core" --probe-binary
  echo "Installed AppImage: $installed_appimage"
  echo "Installed launcher: $launcher"
}

case "$FORMAT" in
  deb) install_deb ;;
  rpm) install_rpm ;;
  appimage) install_appimage ;;
esac

echo
echo "WaveLinux 6 installation completed."
echo "Launch it from the application menu or run: wavelinux6"
