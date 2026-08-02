#!/usr/bin/env bash
set -euo pipefail

# Tauri caches linuxdeploy as an AppImage. Some cached linuxdeploy builds carry
# an older strip binary that fails on current ELF sections such as .relr.dyn.
# Extract linuxdeploy, replace only that embedded strip with the host tool, and
# rerun the same GTK/GStreamer plugin pass against the existing AppDir.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APPIMAGE_DIR="$ROOT_DIR/target/release/bundle/appimage"
PRODUCT_NAME="$(node -e 'console.log(require(process.argv[1]).productName || "WaveLinux6")' "$ROOT_DIR/crates/app/tauri.conf.json")"
MAIN_BINARY_NAME="$(node -e 'console.log(require(process.argv[1]).mainBinaryName || "wavelinux6")' "$ROOT_DIR/crates/app/tauri.conf.json")"
APP_IDENTIFIER="$(node -e 'console.log(require(process.argv[1]).identifier || "io.github.duskyprojects.WaveLinux6")' "$ROOT_DIR/crates/app/tauri.conf.json")"
DESKTOP_FILE="$APP_IDENTIFIER.desktop"
APPSTREAM_FILE="$APP_IDENTIFIER.appdata.xml"
APPDIR="$APPIMAGE_DIR/${PRODUCT_NAME}.AppDir"
LINUXDEPLOY="${LINUXDEPLOY:-$HOME/.cache/tauri/linuxdeploy-x86_64.AppImage}"
GTK_PLUGIN="${LINUXDEPLOY_PLUGIN_GTK:-$HOME/.cache/tauri/linuxdeploy-plugin-gtk.sh}"
GSTREAMER_PLUGIN="${LINUXDEPLOY_PLUGIN_GSTREAMER:-$HOME/.cache/tauri/linuxdeploy-plugin-gstreamer.sh}"
BUNDLE_GSTREAMER="${WAVELINUX_BUNDLE_GSTREAMER_PLUGIN:-0}"
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

if [[ "$BUNDLE_GSTREAMER" == "1" && ! -x "$GSTREAMER_PLUGIN" ]]; then
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
if [[ "$BUNDLE_GSTREAMER" == "1" ]]; then
  ln -s "$GSTREAMER_PLUGIN" "$plugin_dir/linuxdeploy-plugin-gstreamer"
fi

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

remove_partial_gstreamer_plugin_tree() {
  if [[ "$BUNDLE_GSTREAMER" == "1" ]]; then
    return 0
  fi
  rm -rf \
    "$APPDIR/usr/lib/gstreamer-1.0" \
    "$APPDIR/usr/lib/gstreamer1.0"
}

remove_generated_gtk_module_links
remove_obsolete_runtime_artifacts
remove_partial_gstreamer_plugin_tree

ensure_appdir_identity() {
  rm -f \
    "$APPDIR/WaveLinux.desktop" \
    "$APPDIR/WaveLinux5.desktop" \
    "$APPDIR/$PRODUCT_NAME.desktop" \
    "$APPDIR/wavelinux.png" \
    "$APPDIR/wavelinux5.png" \
    "$APPDIR/usr/bin/wavelinux" \
    "$APPDIR/usr/bin/wavelinux5" \
    "$APPDIR/usr/wavelinux-runtime/bin/wavelinux5-dsp-helper" \
    "$APPDIR/usr/share/applications/WaveLinux.desktop" \
    "$APPDIR/usr/share/applications/WaveLinux5.desktop" \
    "$APPDIR/usr/share/applications/$PRODUCT_NAME.desktop" \
    "$APPDIR/usr/share/applications/wavelinux5.desktop" \
    "$APPDIR/usr/share/metainfo/WaveLinux6.appdata.xml" \
    "$APPDIR/usr/share/metainfo/io.github.duskyprojects.WaveLinux6.metainfo.xml"
  find "$APPDIR/usr/share/icons" -type f \
    \( -name 'wavelinux.*' -o -name 'wavelinux5.*' \) -delete 2>/dev/null || true
  find "$APPDIR" -depth -iname 'wavelinux5*' -delete 2>/dev/null || true

  install -d \
    "$APPDIR/usr/bin" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/metainfo" \
    "$APPDIR/usr/share/icons/hicolor/32x32/apps" \
    "$APPDIR/usr/share/icons/hicolor/128x128/apps" \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
    "$APPDIR/usr/share/icons/hicolor/512x512/apps"

  if [[ ! -x "$APPDIR/usr/bin/$MAIN_BINARY_NAME" ]]; then
    echo "Missing Tauri-packaged AppDir binary: $APPDIR/usr/bin/$MAIN_BINARY_NAME" >&2
    echo "Run the Tauri build step before rebuilding the AppImage." >&2
    exit 1
  fi
  if find "$APPDIR" -iname 'wavelinux5*' -print -quit | grep -q .; then
    echo "Stale WaveLinux5 identity remained in AppDir" >&2
    exit 1
  fi

  cat >"$APPDIR/$DESKTOP_FILE" <<DESKTOP
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
  if [[ ! "$APPDIR/$DESKTOP_FILE" -ef "$APPDIR/usr/share/applications/$DESKTOP_FILE" ]]; then
    install -m 0644 "$APPDIR/$DESKTOP_FILE" "$APPDIR/usr/share/applications/$DESKTOP_FILE"
  fi

  install -m 0644 "$ROOT_DIR/crates/app/icons/32x32.png" "$APPDIR/usr/share/icons/hicolor/32x32/apps/$MAIN_BINARY_NAME.png"
  install -m 0644 "$ROOT_DIR/crates/app/icons/128x128.png" "$APPDIR/usr/share/icons/hicolor/128x128/apps/$MAIN_BINARY_NAME.png"
  install -m 0644 "$ROOT_DIR/crates/app/icons/128x128@2x.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/$MAIN_BINARY_NAME.png"
  install -m 0644 "$ROOT_DIR/crates/app/icons/icon.png" "$APPDIR/usr/share/icons/hicolor/512x512/apps/$MAIN_BINARY_NAME.png"
  install -m 0644 "$ROOT_DIR/crates/app/icons/128x128@2x.png" "$APPDIR/$MAIN_BINARY_NAME.png"
  install -m 0644 \
    "$ROOT_DIR/crates/app/appimage-extra/usr/share/metainfo/$APPSTREAM_FILE" \
    "$APPDIR/usr/share/metainfo/$APPSTREAM_FILE"
}

ensure_appdir_identity

echo "Rebuilding AppImage with host strip: $HOST_STRIP"
plugin_args=(--plugin gtk)
if [[ "$BUNDLE_GSTREAMER" == "1" ]]; then
  plugin_args+=(--plugin gstreamer)
fi
(
  cd "$APPIMAGE_DIR"
  PATH="$plugin_dir:$PATH" "$extracted/AppRun" \
    --verbosity 1 \
    --appdir "$APPDIR" \
    "${plugin_args[@]}" \
    --output appimage
)
