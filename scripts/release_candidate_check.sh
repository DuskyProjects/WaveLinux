#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
MANIFEST_PATH="${DIST_DIR}/wavelinux-release-manifest.json"
SIGNATURE_PATH="${DIST_DIR}/wavelinux-release-manifest.sig"
DEFAULT_REPO="DuskyProjects/WaveLinux"

log() {
    printf '\n[%s] %s\n' "$(date +%H:%M:%S)" "$*"
}

prefer_repo_python() {
    local candidate
    for candidate in \
        "${ROOT_DIR}/.venv/bin/python" \
        "${ROOT_DIR}/.venv-build/bin/python" \
        "${ROOT_DIR}/.build-venv/bin/python"
    do
        if [[ -x "${candidate}" ]]; then
            export PATH="$(dirname -- "${candidate}"):${PATH}"
            export PYTHON_BIN="${PYTHON_BIN:-${candidate}}"
            return 0
        fi
    done
    return 0
}

resolve_repo_slug() {
    local remote_url repo
    remote_url="$(git -C "${ROOT_DIR}" config --get remote.origin.url || true)"
    case "${remote_url}" in
        git@github.com:*.git)
            repo="${remote_url#git@github.com:}"
            printf '%s\n' "${repo%.git}"
            ;;
        https://github.com/*)
            repo="${remote_url#https://github.com/}"
            printf '%s\n' "${repo%.git}"
            ;;
        *)
            printf '%s\n' "${DEFAULT_REPO}"
            ;;
    esac
}

find_built_appimage() {
    local candidate appimage
    appimage=""
    shopt -s nullglob
    for candidate in "${DIST_DIR}"/WaveLinux-*-x86_64.AppImage; do
        appimage="${candidate}"
    done
    shopt -u nullglob
    if [[ -z "${appimage}" ]]; then
        printf 'No built AppImage found under %s\n' "${DIST_DIR}" >&2
        return 1
    fi
    printf '%s\n' "${appimage}"
}

prefer_repo_python

log "Running unit suite"
cd "${ROOT_DIR}"
python -m unittest discover -s tests

log "Building AppImage"
"${ROOT_DIR}/scripts/build_appimage.sh"

APPIMAGE_PATH="$(find_built_appimage)"
APPIMAGE_NAME="$(basename -- "${APPIMAGE_PATH}")"
APP_VERSION="${APPIMAGE_NAME#WaveLinux-}"
APP_VERSION="${APP_VERSION%-x86_64.AppImage}"
REPO_SLUG="${WAVELINUX_RELEASE_REPO:-$(resolve_repo_slug)}"

log "Smoke testing AppImage"
APPIMAGE_EXTRACT_AND_RUN=1 "${APPIMAGE_PATH}" --version
APPIMAGE_EXTRACT_AND_RUN=1 "${APPIMAGE_PATH}" --self-test

log "Building release manifest"
python "${ROOT_DIR}/scripts/build_release_manifest.py" \
    --appimage "${APPIMAGE_PATH}" \
    --version "${APP_VERSION}" \
    --repo "${REPO_SLUG}" \
    --tag "v${APP_VERSION}" \
    --output "${MANIFEST_PATH}"

if [[ -n "${WAVELINUX_RELEASE_ED25519_PRIVATE_KEY_B64:-}" ]]; then
    log "Signing release manifest"
    python "${ROOT_DIR}/scripts/sign_release_manifest.py" \
        "${MANIFEST_PATH}" \
        "${SIGNATURE_PATH}"

    log "Verifying signed release manifest"
    python "${ROOT_DIR}/scripts/verify_release_manifest.py" \
        "${MANIFEST_PATH}" \
        "${SIGNATURE_PATH}" \
        "${APPIMAGE_PATH}"
else
    log "Skipping manifest signing and verification because WAVELINUX_RELEASE_ED25519_PRIVATE_KEY_B64 is not set"
fi

cat <<EOF

Release-candidate checks complete.
Built AppImage: ${APPIMAGE_PATH}
Manifest: ${MANIFEST_PATH}

Next manual gate:
python3 tools/stress/run_stress_suite.py \\
  --profile tools/stress/profile.current-machine.json \\
  --mode maximum
EOF
