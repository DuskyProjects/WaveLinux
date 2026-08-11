#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${1:-$ROOT_DIR/target/release/bundle}"
ARTIFACT_DIR="$(cd "$ARTIFACT_DIR" && pwd)"
VERSION="$(node -e 'console.log(require(process.argv[1]).version)' "$ROOT_DIR/crates/app/tauri.conf.json")"

bash "$ROOT_DIR/scripts/check-packaging-metadata.sh"

find_one() {
  local pattern="$1"
  local matches=()
  mapfile -t matches < <(find "$ARTIFACT_DIR" -type f -name "$pattern" | sort)
  if (( ${#matches[@]} != 1 )); then
    echo "Expected one $pattern under $ARTIFACT_DIR, found ${#matches[@]}" >&2
    printf '%s\n' "${matches[@]}" >&2
    return 1
  fi
  printf '%s\n' "${matches[0]}"
}

appimage="$(find_one "WaveLinux6_${VERSION}_amd64.AppImage")"
deb="$(find_one "WaveLinux6_${VERSION}_amd64.deb")"
rpm="$(find_one "WaveLinux6-${VERSION}-1.x86_64.rpm")"

bash "$ROOT_DIR/scripts/sanitize-appimage-pipewire.sh" --check "$appimage"
bash "$ROOT_DIR/scripts/sanitize-appimage-wayland.sh" --check "$appimage"
extract_dir="$(mktemp -d "${TMPDIR:-/tmp}/wavelinux-package-check.XXXXXX")"
trap 'rm -rf "$extract_dir"' EXIT
(
  cd "$extract_dir"
  "$appimage" --appimage-extract >/dev/null
)
test -x "$extract_dir/squashfs-root/usr/wavelinux-runtime/bin/wavelinux6-audio-core" || {
  echo "AppImage omitted wavelinux6-audio-core" >&2
  exit 1
}
test -x "$extract_dir/squashfs-root/usr/wavelinux-runtime/bin/wavelinux6-peripheral-plugin" || {
  echo "AppImage omitted wavelinux6-peripheral-plugin" >&2
  exit 1
}
for script in check-dependencies.sh runtime-dependencies.sh; do
  test -x "$extract_dir/squashfs-root/usr/wavelinux-runtime/bin/$script" || {
    echo "AppImage omitted executable runtime helper: $script" >&2
    exit 1
  }
done
if find "$extract_dir/squashfs-root" -type f \
  \( -name bwrap -o -name xdg-dbus-proxy \) -print -quit | grep -q .; then
  echo "AppImage bundled a host-only WebKit sandbox executable" >&2
  exit 1
fi
grep -Fq 'WAVELINUX_ASSUME_RUNTIME_DEPS=1' "$extract_dir/squashfs-root/AppRun" || {
  echo "AppImage does not mark its successful dependency preflight" >&2
  exit 1
}
grep -Fq 'WAVELINUX_DSP_HELPER=' "$extract_dir/squashfs-root/AppRun" || {
  echo "AppImage does not explicitly select wavelinux6-audio-core" >&2
  exit 1
}
test -x "$extract_dir/squashfs-root/usr/bin/wavelinux6" || {
  echo "AppImage omitted the wavelinux6 application binary" >&2
  exit 1
}
test -r "$extract_dir/squashfs-root/usr/wavelinux-runtime/etc/fonts/fonts.conf" || {
  echo "AppImage omitted its matching Fontconfig configuration" >&2
  exit 1
}
if ! find "$extract_dir/squashfs-root/usr/wavelinux-runtime/etc/fonts/conf.d" -maxdepth 1 -type f -print -quit | grep -q .; then
  echo "AppImage omitted its matching Fontconfig rules" >&2
  exit 1
fi
if ! find "$extract_dir/squashfs-root/usr/wavelinux-runtime/etc/fonts/conf.avail" -maxdepth 1 -type f -print -quit | grep -q .; then
  echo "AppImage omitted its matching Fontconfig available-rule baseline" >&2
  exit 1
fi
grep -q 'FONTCONFIG_FILE=' "$extract_dir/squashfs-root/AppRun" || {
  echo "AppImage does not activate its matching Fontconfig configuration" >&2
  exit 1
}
grep -q 'FONTCONFIG_SYSROOT=' "$extract_dir/squashfs-root/AppRun" || {
  echo "AppImage does not isolate Fontconfig rules behind its AppDir sysroot" >&2
  exit 1
}
bash "$ROOT_DIR/scripts/check-glibc-baseline.sh" \
  "$extract_dir/squashfs-root/usr/bin/wavelinux6" \
  "$extract_dir/squashfs-root/usr/wavelinux-runtime/bin/wavelinux6-audio-core" \
  "$extract_dir/squashfs-root/usr/wavelinux-runtime/bin/wavelinux6-peripheral-plugin"

deb_extract="$extract_dir/deb"
mkdir -p "$deb_extract"
if command -v dpkg-deb >/dev/null 2>&1; then
  deb_version="$(dpkg-deb -f "$deb" Version)"
  dpkg-deb -x "$deb" "$deb_extract"
else
  if ! command -v ar >/dev/null 2>&1 || ! command -v bsdtar >/dev/null 2>&1; then
    echo "dpkg-deb or both ar and bsdtar are required to inspect the Debian package" >&2
    exit 1
  fi
  deb_archive_dir="$extract_dir/deb-archive"
  mkdir -p "$deb_archive_dir"
  (
    cd "$deb_archive_dir"
    ar x "$deb"
  )
  control_archive="$(find "$deb_archive_dir" -maxdepth 1 -type f -name 'control.tar.*' -print -quit)"
  data_archive="$(find "$deb_archive_dir" -maxdepth 1 -type f -name 'data.tar.*' -print -quit)"
  [[ -n "$control_archive" && -n "$data_archive" ]] || {
    echo "Debian package is missing its control or data archive" >&2
    exit 1
  }
  deb_control="$(bsdtar -xOf "$control_archive" control 2>/dev/null || bsdtar -xOf "$control_archive" ./control)"
  deb_version="$(sed -n 's/^Version:[[:space:]]*//p' <<<"$deb_control" | head -n1)"
  bsdtar -xf "$data_archive" -C "$deb_extract"
fi
[[ "$deb_version" == "$VERSION" ]] || {
  echo "Debian package version mismatch: expected $VERSION, found ${deb_version:-missing}" >&2
  exit 1
}
test -x "$deb_extract/usr/bin/wavelinux6" || {
  echo "Debian package omitted /usr/bin/wavelinux6" >&2
  exit 1
}
test -x "$deb_extract/usr/bin/wavelinux6-audio-core" || {
  echo "Debian package omitted /usr/bin/wavelinux6-audio-core" >&2
  exit 1
}
test -x "$deb_extract/usr/bin/wavelinux6-peripheral-plugin" || {
  echo "Debian package omitted /usr/bin/wavelinux6-peripheral-plugin" >&2
  exit 1
}
for script in check-dependencies.sh runtime-dependencies.sh verify-install.sh wavelinux-processes.sh; do
  test -x "$deb_extract/usr/lib/wavelinux6/$script" || {
    echo "Debian package omitted executable /usr/lib/wavelinux6/$script" >&2
    exit 1
  }
done
test -r "$deb_extract/usr/share/metainfo/io.github.duskyprojects.WaveLinux6.appdata.xml" || {
  echo "Debian package omitted AppStream metadata" >&2
  exit 1
}
if ! find "$deb_extract/usr/share/applications" -maxdepth 1 -type f -name '*.desktop' -print -quit | grep -q .; then
  echo "Debian package omitted its desktop entry" >&2
  exit 1
fi
if ! find "$deb_extract/usr/share/icons" -type f -iname '*wavelinux6*' -print -quit | grep -q .; then
  echo "Debian package omitted its application icons" >&2
  exit 1
fi

if command -v rpm >/dev/null 2>&1; then
  rpm_version="$(rpm -qp --queryformat '%{VERSION}' "$rpm")"
  [[ "$rpm_version" == "$VERSION" ]] || {
    echo "RPM package version mismatch: expected $VERSION, found $rpm_version" >&2
    exit 1
  }
  rpm_files="$(rpm -qpl "$rpm")"
else
  command -v bsdtar >/dev/null 2>&1 || {
    echo "rpm or bsdtar is required to inspect the RPM package" >&2
    exit 1
  }
  rpm_files="$(bsdtar -tf "$rpm" | sed 's#^\./#/#; /^usr\//s#^#/#')"
fi
grep -Fxq '/usr/bin/wavelinux6' <<<"$rpm_files" || {
  echo "RPM package omitted /usr/bin/wavelinux6" >&2
  exit 1
}
grep -Fxq '/usr/bin/wavelinux6-audio-core' <<<"$rpm_files" || {
  echo "RPM package omitted /usr/bin/wavelinux6-audio-core" >&2
  exit 1
}
grep -Fxq '/usr/bin/wavelinux6-peripheral-plugin' <<<"$rpm_files" || {
  echo "RPM package omitted /usr/bin/wavelinux6-peripheral-plugin" >&2
  exit 1
}
for script in check-dependencies.sh runtime-dependencies.sh verify-install.sh wavelinux-processes.sh; do
  grep -Fxq "/usr/lib/wavelinux6/$script" <<<"$rpm_files" || {
    echo "RPM package omitted /usr/lib/wavelinux6/$script" >&2
    exit 1
  }
done
grep -Fxq '/usr/share/metainfo/io.github.duskyprojects.WaveLinux6.appdata.xml' <<<"$rpm_files" || {
  echo "RPM package omitted AppStream metadata" >&2
  exit 1
}
grep -Eq '^/usr/share/applications/.*[.]desktop$' <<<"$rpm_files" || {
  echo "RPM package omitted its desktop entry" >&2
  exit 1
}
grep -Eq '^/usr/share/icons/.*/wavelinux6[.](png|svg)$' <<<"$rpm_files" || {
  echo "RPM package omitted its application icons" >&2
  exit 1
}

echo "Package contents verified for WaveLinux $VERSION"
