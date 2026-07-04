#!/usr/bin/env bash
set -euo pipefail

# Tauri caches linuxdeploy as an AppImage. Some cached linuxdeploy builds carry
# an older strip binary that fails on current ELF sections such as .relr.dyn.
# Extract linuxdeploy, replace only that embedded strip with the host tool, and
# rerun the same GTK/GStreamer plugin pass against the existing AppDir.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPIMAGE_DIR="$ROOT_DIR/target/release/bundle/appimage"
PRODUCT_NAME="$(node -e 'console.log(require(process.argv[1]).productName || "WaveLinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"
MAIN_BINARY_NAME="$(node -e 'console.log(require(process.argv[1]).mainBinaryName || "wavelinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"
APP_IDENTIFIER="$(node -e 'console.log(require(process.argv[1]).identifier || "io.github.duskyprojects.WaveLinux5")' "$ROOT_DIR/crates/app/tauri.conf.json")"
APPDIR="$APPIMAGE_DIR/${PRODUCT_NAME}.AppDir"
LINUXDEPLOY="${LINUXDEPLOY:-$HOME/.cache/tauri/linuxdeploy-x86_64.AppImage}"
GTK_PLUGIN="${LINUXDEPLOY_PLUGIN_GTK:-$HOME/.cache/tauri/linuxdeploy-plugin-gtk.sh}"
GSTREAMER_PLUGIN="${LINUXDEPLOY_PLUGIN_GSTREAMER:-$HOME/.cache/tauri/linuxdeploy-plugin-gstreamer.sh}"
HOST_STRIP="${STRIP:-$(command -v strip || true)}"

if [[ ! -d "$APPDIR" ]]; then
  echo "Missing AppDir: $APPDIR" >&2
  exit 1
fi

if [[ ! -x "$LINUXDEPLOY" ]]; then
  echo "Missing linuxdeploy AppImage: $LINUXDEPLOY" >&2
  exit 1
fi

if [[ ! -x "$GTK_PLUGIN" ]]; then
  echo "Missing linuxdeploy GTK plugin: $GTK_PLUGIN" >&2
  exit 1
fi

if [[ ! -x "$GSTREAMER_PLUGIN" ]]; then
  echo "Missing linuxdeploy GStreamer plugin: $GSTREAMER_PLUGIN" >&2
  exit 1
fi

if [[ -z "$HOST_STRIP" || ! -x "$HOST_STRIP" ]]; then
  echo "Missing host strip binary" >&2
  exit 1
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux-linuxdeploy.XXXXXX")"
cleanup() {
  rm -rf "$tmp"
}
trap cleanup EXIT

(
  cd "$tmp"
  "$LINUXDEPLOY" --appimage-extract >/dev/null
)

extracted="$tmp/squashfs-root"
if [[ ! -x "$extracted/AppRun" || ! -f "$extracted/usr/bin/strip" ]]; then
  echo "linuxdeploy extraction did not produce the expected tools" >&2
  exit 1
fi

cp "$HOST_STRIP" "$extracted/usr/bin/strip"

plugin_dir="$tmp/plugins"
mkdir -p "$plugin_dir"
ln -s "$GTK_PLUGIN" "$plugin_dir/linuxdeploy-plugin-gtk"
ln -s "$GSTREAMER_PLUGIN" "$plugin_dir/linuxdeploy-plugin-gstreamer"

remove_generated_gtk_module_links() {
  local root basename target
  for root in \
    /usr/lib/gtk-3.0/3.0.0/immodules \
    /usr/lib/gtk-3.0/3.0.0/printbackends \
    /usr/lib/gdk-pixbuf-2.0/2.10.0/loaders; do
    [[ -d "$root" ]] || continue
    while IFS= read -r -d '' target; do
      basename="$(basename "$target")"
      if [[ -L "$APPDIR/usr/lib/$basename" ]]; then
        rm -f "$APPDIR/usr/lib/$basename"
      fi
    done < <(find "$root" -maxdepth 1 -type f -name '*.so' -print0)
  done
}

remove_obsolete_runtime_artifacts() {
  find "$APPDIR/usr/wavelinux-runtime/lib/ladspa" \
    -maxdepth 1 \
    -type f \
    \( -iname '*deep_filter*' -o -iname '*deepfilter*' \) \
    -delete 2>/dev/null || true
}

remove_generated_gtk_module_links
remove_obsolete_runtime_artifacts

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
    echo "Run the Tauri build step before rebuilding the AppImage." >&2
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

ensure_appdir_identity

echo "Rebuilding AppImage with host strip: $HOST_STRIP"
(
  cd "$APPIMAGE_DIR"
  PATH="$plugin_dir:$PATH" "$extracted/AppRun" \
    --verbosity 1 \
    --appdir "$APPDIR" \
    --plugin gtk \
    --plugin gstreamer \
    --output appimage
)
