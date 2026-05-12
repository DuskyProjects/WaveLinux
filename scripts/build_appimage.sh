#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="${ROOT_DIR}/build/appimage"
DIST_DIR="${ROOT_DIR}/dist"
APPDIR="${BUILD_DIR}/AppDir"
PYINSTALLER="${PYINSTALLER:-pyinstaller}"
APPIMAGETOOL="${APPIMAGETOOL:-${BUILD_DIR}/appimagetool-x86_64.AppImage}"
APPIMAGETOOL_URL="${APPIMAGETOOL_URL:-https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage}"
WAVELINUX_VERSION="${WAVELINUX_VERSION:-$(awk -F'\"' '/^APP_VERSION = / { print $2; exit }' "${ROOT_DIR}/main.py")}"
DESKTOP_ID="io.github.duskyprojects.WaveLinux.desktop"
if [ -z "${WAVELINUX_VERSION}" ]; then
    echo "Could not determine APP_VERSION from main.py" >&2
    exit 1
fi
OUTPUT_APPIMAGE="${DIST_DIR}/WaveLinux-${WAVELINUX_VERSION}-x86_64.AppImage"

bundle_optional_fx() {
    local target_dir="$1"
    local -a plugin_names=(
        "librnnoise_ladspa.so"
        "sc4m_1916.so"
        "gate_1410.so"
    )
    local -a roots=(
        "/usr/lib/ladspa"
        "/usr/lib64/ladspa"
        "/usr/local/lib/ladspa"
        "/usr/local/lib64/ladspa"
        "/usr/lib/x86_64-linux-gnu/ladspa"
        "/usr/lib/aarch64-linux-gnu/ladspa"
    )
    local plugin root

    mkdir -p "${target_dir}"
    for plugin in "${plugin_names[@]}"; do
        for root in "${roots[@]}"; do
            if [ -f "${root}/${plugin}" ]; then
                cp -a "${root}/${plugin}" "${target_dir}/"
                break
            fi
        done
    done
}

download_appimagetool() {
    mkdir -p "${BUILD_DIR}"
    if [ ! -x "${APPIMAGETOOL}" ]; then
        curl -L "${APPIMAGETOOL_URL}" -o "${APPIMAGETOOL}"
        chmod +x "${APPIMAGETOOL}"
    fi
}

rm -rf "${ROOT_DIR}/build" "${ROOT_DIR}/dist/WaveLinux" "${APPDIR}"
mkdir -p "${DIST_DIR}" "${BUILD_DIR}"

cd "${ROOT_DIR}"
"${PYINSTALLER}" --noconfirm --clean WaveLinux.spec

mkdir -p "${APPDIR}/usr/lib/wavelinux"
cp -a "${DIST_DIR}/WaveLinux/." "${APPDIR}/usr/lib/wavelinux/"
install -Dm755 "${ROOT_DIR}/packaging/appimage/AppRun" "${APPDIR}/AppRun"
install -Dm644 "${ROOT_DIR}/packaging/appimage/wavelinux.desktop" "${APPDIR}/${DESKTOP_ID}"
install -Dm644 "${ROOT_DIR}/packaging/appimage/wavelinux.desktop" \
    "${APPDIR}/usr/share/applications/${DESKTOP_ID}"
install -Dm644 "${ROOT_DIR}/icon.png" "${APPDIR}/wavelinux.png"
install -Dm644 "${ROOT_DIR}/packaging/appimage/wavelinux.appdata.xml" \
    "${APPDIR}/usr/share/metainfo/io.github.duskyprojects.WaveLinux.appdata.xml"

bundle_optional_fx "${APPDIR}/usr/lib/ladspa"
download_appimagetool

ARCH=x86_64 "${APPIMAGETOOL}" --appimage-extract-and-run "${APPDIR}" "${OUTPUT_APPIMAGE}"

(cd "${DIST_DIR}" && sha256sum "$(basename "${OUTPUT_APPIMAGE}")" > sha256sums.txt)

echo "Built ${OUTPUT_APPIMAGE}"
