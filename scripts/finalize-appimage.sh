#!/usr/bin/env bash
set -euo pipefail

# Final AppImage pass: remove host-bound PipeWire client artifacts from the
# AppDir, rebuild the AppImage, verify the sealed artifact, and optionally
# recreate/sign updater artifacts.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPIMAGE_DIR="$ROOT_DIR/target/release/bundle/appimage"
PRODUCT_NAME="$(node -e 'console.log(require(process.argv[1]).productName || "WaveLinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"
MAIN_BINARY_NAME="$(node -e 'console.log(require(process.argv[1]).mainBinaryName || "wavelinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"
APP_IDENTIFIER="$(node -e 'console.log(require(process.argv[1]).identifier || "io.github.duskyprojects.WaveLinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"
PACKAGE_VERSION="$(node -e 'console.log(require(process.argv[1]).version)' "$ROOT_DIR/package.json")"
APPDIR="$APPIMAGE_DIR/${PRODUCT_NAME}.AppDir"
PLUGIN="${LINUXDEPLOY_PLUGIN_APPIMAGE:-$HOME/.cache/tauri/linuxdeploy-plugin-appimage.AppImage}"
CREATE_UPDATER=0

usage() {
  cat <<'HELP'
Sanitize, rebuild, and optionally sign WaveLinux AppImage artifacts.

Usage:
  bash scripts/finalize-appimage.sh [--updater]

Options:
  --updater  Recreate the .AppImage.tar.gz updater archive and sign refreshed
             AppImage artifacts with the Tauri signing environment.
HELP
}

while (($#)); do
  case "$1" in
    --updater)
      CREATE_UPDATER=1
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

if [[ ! -d "$APPDIR" ]]; then
  echo "Missing AppDir: $APPDIR" >&2
  exit 1
fi

if [[ ! -x "$PLUGIN" ]]; then
  echo "Missing linuxdeploy AppImage plugin: $PLUGIN" >&2
  exit 1
fi

appimage="$APPIMAGE_DIR/${PRODUCT_NAME}_${PACKAGE_VERSION}_amd64.AppImage"

ensure_appdir_identity() {
  rm -f \
    "$APPDIR/WaveLinux.desktop" \
    "$APPDIR/wavelinux.png" \
    "$APPDIR/usr/bin/wavelinux" \
    "$APPDIR/usr/share/applications/WaveLinux.desktop"
  find "$APPDIR/usr/share/icons" -type f -name 'wavelinux.*' -delete 2>/dev/null || true

  install -d \
    "$APPDIR/usr/bin" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/icons/hicolor/32x32/apps" \
    "$APPDIR/usr/share/icons/hicolor/128x128/apps" \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
    "$APPDIR/usr/share/icons/hicolor/512x512/apps"

  if [[ ! -x "$APPDIR/usr/bin/$MAIN_BINARY_NAME" ]]; then
    echo "Missing Tauri-packaged AppDir binary: $APPDIR/usr/bin/$MAIN_BINARY_NAME" >&2
    echo "Run the Tauri build step before finalizing the AppImage." >&2
    exit 1
  fi

  cat >"$APPDIR/$PRODUCT_NAME.desktop" <<DESKTOP
[Desktop Entry]
Categories=AudioVideo;Audio;Music;
Comment=Linux creator audio mixer
Exec=$MAIN_BINARY_NAME
StartupWMClass=$APP_IDENTIFIER
Icon=$MAIN_BINARY_NAME
Name=$PRODUCT_NAME
Terminal=false
Type=Application
DESKTOP
  if [[ ! "$APPDIR/$PRODUCT_NAME.desktop" -ef "$APPDIR/usr/share/applications/$PRODUCT_NAME.desktop" ]]; then
    install -m 0644 "$APPDIR/$PRODUCT_NAME.desktop" "$APPDIR/usr/share/applications/$PRODUCT_NAME.desktop"
  fi

  install -m 0644 "$ROOT_DIR/crates/app/icons/32x32.png" "$APPDIR/usr/share/icons/hicolor/32x32/apps/$MAIN_BINARY_NAME.png"
  install -m 0644 "$ROOT_DIR/crates/app/icons/128x128.png" "$APPDIR/usr/share/icons/hicolor/128x128/apps/$MAIN_BINARY_NAME.png"
  install -m 0644 "$ROOT_DIR/crates/app/icons/128x128@2x.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/$MAIN_BINARY_NAME.png"
  install -m 0644 "$ROOT_DIR/crates/app/icons/icon.png" "$APPDIR/usr/share/icons/hicolor/512x512/apps/$MAIN_BINARY_NAME.png"
  install -m 0644 "$ROOT_DIR/crates/app/icons/128x128@2x.png" "$APPDIR/$MAIN_BINARY_NAME.png"
}

remove_obsolete_runtime_artifacts() {
  find "$APPDIR/usr/wavelinux-runtime/lib/ladspa" \
    -maxdepth 1 \
    -type f \
    \( -iname '*deep_filter*' -o -iname '*deepfilter*' \) \
    -delete 2>/dev/null || true
}

install_appimage_apprun() {
  install -d "$APPDIR/usr/wavelinux-runtime/bin"
  install -m 0755 "$ROOT_DIR/scripts/check-dependencies.sh" "$APPDIR/usr/wavelinux-runtime/bin/check-dependencies.sh"
  if [[ ! -x "$APPDIR/AppRun.wrapped" ]]; then
    echo "Missing linuxdeploy AppRun payload: $APPDIR/AppRun.wrapped" >&2
    echo "Run the Tauri AppImage build step before finalizing the AppImage." >&2
    exit 1
  fi
  install -m 0755 "$ROOT_DIR/scripts/appimage-apprun.sh" "$APPDIR/AppRun"
}

ensure_appdir_identity
remove_obsolete_runtime_artifacts
install_appimage_apprun
"$ROOT_DIR/scripts/sanitize-appimage-pipewire.sh" --sanitize "$APPDIR"

echo "Rebuilding sanitized AppImage: $appimage"
rm -f "$appimage" "$appimage.sig" "$appimage.tar.gz" "$appimage.tar.gz.sig"
marker="$(mktemp "$APPIMAGE_DIR/.finalize-appimage.XXXXXX")"
(
  cd "$APPIMAGE_DIR"
  ARCH=x86_64 "$PLUGIN" --appdir="$APPDIR"
)
generated="$({ find "$APPIMAGE_DIR" -maxdepth 1 -type f -name '*.AppImage' -newer "$marker" -print 2>/dev/null || true; } | sort -V | tail -n1)"
rm -f "$marker"
if [[ ! -f "$appimage" ]]; then
  if [[ -z "$generated" || ! -f "$generated" ]]; then
    echo "linuxdeploy plugin did not produce an AppImage in $APPIMAGE_DIR" >&2
    exit 1
  fi
  mv -f "$generated" "$appimage"
elif [[ -n "$generated" && "$generated" != "$appimage" ]]; then
  rm -f "$generated"
fi

"$ROOT_DIR/scripts/sanitize-appimage-pipewire.sh" --check "$APPDIR" "$appimage"

sign_artifact() {
  local artifact="$1"
  if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" && -z "${TAURI_SIGNING_PRIVATE_KEY_PATH:-}" ]]; then
    return 0
  fi

  "$ROOT_DIR/node_modules/.bin/tauri" signer sign "$artifact" >/dev/null
  echo "Signed $artifact"
}

if ((CREATE_UPDATER == 1)); then
  tarball="$appimage.tar.gz"
  echo "Rebuilding sanitized updater archive: $tarball"
  (
    cd "$APPIMAGE_DIR"
    tar -czf "$(basename "$tarball")" "$(basename "$appimage")"
  )
  sign_artifact "$appimage"
  sign_artifact "$tarball"
fi
