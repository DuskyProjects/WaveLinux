#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT_DIR="${WAVELINUX_INSTALLER_OUTPUT_DIR:-$ROOT_DIR/dist}"

read_version() {
  if command -v node >/dev/null 2>&1; then
    node -e 'console.log(require(process.argv[1]).version)' \
      "$ROOT_DIR/crates/app/tauri.conf.json"
    return
  fi

  sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$ROOT_DIR/crates/app/tauri.conf.json" | head -n1
}

VERSION="$(read_version)"
[[ -n "$VERSION" ]] || {
  echo "Could not determine the WaveLinux version." >&2
  exit 1
}

APPIMAGE_DIR="$ROOT_DIR/target/release/bundle/appimage"
APPIMAGE="${WAVELINUX_INSTALLER_APPIMAGE:-$APPIMAGE_DIR/WaveLinux6_${VERSION}_amd64.AppImage}"
AUDIO_CORE="${WAVELINUX_INSTALLER_AUDIO_CORE:-$ROOT_DIR/target/release/wavelinux6-audio-core}"
PERIPHERAL_PLUGIN="${WAVELINUX_INSTALLER_PERIPHERAL_PLUGIN:-$ROOT_DIR/target/release/wavelinux6-peripheral-plugin}"

required_files=(
  "$APPIMAGE"
  "$AUDIO_CORE"
  "$PERIPHERAL_PLUGIN"
  "$ROOT_DIR/scripts/install-local.sh"
  "$ROOT_DIR/scripts/check-dependencies.sh"
  "$ROOT_DIR/scripts/runtime-dependencies.sh"
  "$ROOT_DIR/scripts/wavelinux-processes.sh"
  "$ROOT_DIR/scripts/verify-install.sh"
  "$ROOT_DIR/scripts/wavelinux-launcher.sh"
  "$ROOT_DIR/scripts/sanitize-runtime-env.sh"
  "$ROOT_DIR/scripts/install-alsa-aliases.sh"
  "$ROOT_DIR/scripts/remove-alsa-aliases.sh"
  "$ROOT_DIR/crates/app/icons/32x32.png"
  "$ROOT_DIR/crates/app/icons/128x128.png"
  "$ROOT_DIR/crates/app/icons/128x128@2x.png"
  "$ROOT_DIR/crates/app/icons/icon.png"
  "$ROOT_DIR/crates/app/icons/icon.svg"
  "$ROOT_DIR/crates/app/tauri.conf.json"
)

missing=0
for file in "${required_files[@]}"; do
  if [[ -z "$file" || ! -f "$file" ]]; then
    echo "Missing required installer input: ${file:-AppImage}" >&2
    missing=1
  fi
done
(( missing == 0 )) || {
  echo "Build the known-good local release first, then run this script again." >&2
  exit 1
}

for binary in "$APPIMAGE" "$AUDIO_CORE" "$PERIPHERAL_PLUGIN"; do
  [[ -x "$binary" ]] || chmod +x "$binary"
done

mkdir -p "$OUTPUT_DIR"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux6-installer.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT
PAYLOAD_ROOT="$WORK_DIR/payload"
ARCHIVE="$WORK_DIR/payload.tar.gz"
OUTPUT="$OUTPUT_DIR/WaveLinux6_${VERSION}_amd64_Installer.sh"
STABLE_OUTPUT="$OUTPUT_DIR/WaveLinux6_amd64_Installer.sh"

install -d \
  "$PAYLOAD_ROOT/target/release/bundle/appimage" \
  "$PAYLOAD_ROOT/target/release" \
  "$PAYLOAD_ROOT/scripts" \
  "$PAYLOAD_ROOT/crates/app/icons"

install -m 0755 "$APPIMAGE" \
  "$PAYLOAD_ROOT/target/release/bundle/appimage/$(basename "$APPIMAGE")"
install -m 0755 "$AUDIO_CORE" "$PAYLOAD_ROOT/target/release/wavelinux6-audio-core"
install -m 0755 "$PERIPHERAL_PLUGIN" \
  "$PAYLOAD_ROOT/target/release/wavelinux6-peripheral-plugin"

for script in \
  install-local.sh \
  check-dependencies.sh \
  runtime-dependencies.sh \
  wavelinux-processes.sh \
  verify-install.sh \
  wavelinux-launcher.sh \
  sanitize-runtime-env.sh \
  install-alsa-aliases.sh \
  remove-alsa-aliases.sh; do
  install -m 0755 "$ROOT_DIR/scripts/$script" "$PAYLOAD_ROOT/scripts/$script"
done

for icon in 32x32.png 128x128.png 128x128@2x.png icon.png icon.svg; do
  install -m 0644 "$ROOT_DIR/crates/app/icons/$icon" \
    "$PAYLOAD_ROOT/crates/app/icons/$icon"
done
install -m 0644 "$ROOT_DIR/crates/app/tauri.conf.json" \
  "$PAYLOAD_ROOT/crates/app/tauri.conf.json"

if [[ -d "$ROOT_DIR/profiles/v1/devices" ]]; then
  install -d "$PAYLOAD_ROOT/profiles/v1/devices"
  find "$ROOT_DIR/profiles/v1/devices" -maxdepth 1 -type f -name '*.json' \
    -exec install -m 0644 {} "$PAYLOAD_ROOT/profiles/v1/devices/" \;
fi

printf '%s\n' \
  "WaveLinux 6 installer payload" \
  "Version: $VERSION" \
  "Created: $(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  "Source commit: $(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || echo unknown)" \
  > "$PAYLOAD_ROOT/BUILD-INFO.txt"

checksum_file="$WORK_DIR/SHA256SUMS"
(
  cd "$PAYLOAD_ROOT"
  find . -type f -print0 \
    | sort -z \
    | xargs -0 sha256sum
) >"$checksum_file"
install -m 0644 "$checksum_file" "$PAYLOAD_ROOT/SHA256SUMS"

tar -C "$PAYLOAD_ROOT" -czf "$ARCHIVE" .

cat > "$OUTPUT" <<'INSTALLER_HEADER'
#!/usr/bin/env bash
set -euo pipefail

PRODUCT="WaveLinux 6"
SKIP_DEPS=0
INSTALL_ALSA=1
PREWARM_PROFILES=1
LAUNCH=1

usage() {
  cat <<'HELP'
WaveLinux 6 self-extracting installer

Usage:
  ./WaveLinux6_*_Installer.sh [options]

Options:
  --skip-deps       Do not install or verify system dependencies.
  --no-alsa         Do not install ALSA compatibility aliases.
  --no-prewarm      Do not prewarm signed hardware profiles.
  --no-launch       Install and verify files without launching WaveLinux.
  -h, --help        Show this help.
HELP
}

for arg in "$@"; do
  case "$arg" in
    --skip-deps) SKIP_DEPS=1 ;;
    --no-alsa) INSTALL_ALSA=0 ;;
    --no-prewarm) PREWARM_PROFILES=0 ;;
    --no-launch) LAUNCH=0 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "Unknown option: $arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "Do not run this installer with sudo." >&2
  echo "Run it as your normal desktop user; it will request permission only for dependencies." >&2
  exit 1
fi

arch="$(uname -m)"
case "$arch" in
  x86_64|amd64) ;;
  *)
    echo "$PRODUCT currently supports x86_64 only; detected $arch." >&2
    exit 1
    ;;
esac

for command in tail tar sha256sum bash; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "Required extraction command is missing: $command" >&2
    exit 1
  }
done

SELF="$(readlink -f "$0" 2>/dev/null || realpath "$0" 2>/dev/null || printf '%s' "$0")"
PAYLOAD_LINE=0
PAYLOAD_MARKER_FOUND=0
while IFS= read -r line; do
  PAYLOAD_LINE=$((PAYLOAD_LINE + 1))
  if [[ "$line" == __WAVELINUX_PAYLOAD_BELOW__ ]]; then
    PAYLOAD_LINE=$((PAYLOAD_LINE + 1))
    PAYLOAD_MARKER_FOUND=1
    break
  fi
done <"$SELF"
if ((PAYLOAD_MARKER_FOUND == 0)); then
  echo "Installer payload marker is missing." >&2
  exit 1
fi

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux6-install.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

echo "Extracting $PRODUCT..."
tail -n +"$PAYLOAD_LINE" "$SELF" | tar -xzf - -C "$WORK_DIR"

(
  cd "$WORK_DIR"
  sha256sum -c SHA256SUMS
)

if (( SKIP_DEPS == 0 )); then
  echo "Checking the WaveLinux host runtime..."
  if bash "$WORK_DIR/scripts/check-dependencies.sh" --strict-runtime; then
    echo "WaveLinux host dependencies are already installed."
  else
    echo "Installing missing WaveLinux host dependencies..."
    bash "$WORK_DIR/scripts/check-dependencies.sh" --install --strict-runtime
  fi
else
  echo "Skipping dependency installation by request."
fi

export WAVELINUX_INSTALL_DEPS=0
export WAVELINUX_INSTALL_ALSA_ALIASES="$INSTALL_ALSA"
export WAVELINUX_PREWARM_HARDWARE_PROFILES="$PREWARM_PROFILES"
export WAVELINUX_INSTALL_LOCAL_PROFILE_SEEDS=1

bash "$WORK_DIR/scripts/install-local.sh"

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
AUDIO_CORE="$BIN_DIR/wavelinux6-audio-core"
LAUNCHER="$BIN_DIR/wavelinux6"

[[ -x "$AUDIO_CORE" ]] || {
  echo "Installation failed: $AUDIO_CORE is missing." >&2
  exit 1
}
[[ -x "$LAUNCHER" ]] || {
  echo "Installation failed: $LAUNCHER is missing." >&2
  exit 1
}

verify_args=(--timeout 30)
(( LAUNCH == 0 )) && verify_args+=(--no-launch)
bash "$WORK_DIR/scripts/verify-install.sh" "${verify_args[@]}"

echo
echo "$PRODUCT was installed successfully."
if (( LAUNCH == 1 )); then
  echo "$PRODUCT is running and its PipeWire graph is ready."
else
  echo "Launch it from your application menu or run:"
  echo "  $LAUNCHER"
fi
exit 0

__WAVELINUX_PAYLOAD_BELOW__
INSTALLER_HEADER

cat "$ARCHIVE" >> "$OUTPUT"
chmod 0755 "$OUTPUT"
install -m 0755 "$OUTPUT" "$STABLE_OUTPUT"

printf 'Created self-extracting installer:\n  %s\n' "$OUTPUT"
printf 'Created stable installer alias:\n  %s\n' "$STABLE_OUTPUT"
read -r output_size _ < <(du -h "$OUTPUT")
printf 'Size: %s\n' "$output_size"
printf '\nRecipients install it with:\n  chmod +x %q\n  ./%q\n' \
  "$(basename "$OUTPUT")" "$(basename "$OUTPUT")"
