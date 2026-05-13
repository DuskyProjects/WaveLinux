"""Verified GitHub release discovery and AppImage install helpers."""

from __future__ import annotations

from dataclasses import dataclass
import base64
import hashlib
import json
import os
import queue
import shutil
import subprocess
import tempfile
import threading
import urllib.error
import urllib.request

from distribution import (
    install_appimage_file,
    installed_appimage_backup_path,
    installed_appimage_path,
)

GITHUB_OWNER = "DuskyProjects"
GITHUB_REPO = "WaveLinux"
GITHUB_LATEST_RELEASE_API_URL = (
    f"https://api.github.com/repos/{GITHUB_OWNER}/{GITHUB_REPO}/releases/latest"
)
GITHUB_RELEASES_URL = f"https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases"
UPDATE_USER_AGENT = "WaveLinux-Updater"
RELEASE_MANIFEST_FILENAME = "wavelinux-release-manifest.json"
RELEASE_MANIFEST_SIGNATURE_FILENAME = "wavelinux-release-manifest.sig"
RELEASE_SIGNING_PUBLIC_KEY_B64 = "a6wczeBBFFfIu0JaZERGhwskfbkgRNBM1BFjBJR/k4w="
UPDATE_RELEASE_API_URL_ENV = "WAVELINUX_UPDATE_RELEASE_API_URL"
UPDATE_RELEASES_URL_ENV = "WAVELINUX_UPDATE_RELEASES_URL"

try:
    from cryptography.exceptions import InvalidSignature
    from cryptography.hazmat.primitives.asymmetric.ed25519 import (
        Ed25519PublicKey,
    )
except Exception as exc:  # pragma: no cover - exercised via runtime fallback
    InvalidSignature = None
    Ed25519PublicKey = None
    _CRYPTO_IMPORT_ERROR = exc
else:
    _CRYPTO_IMPORT_ERROR = None


@dataclass(frozen=True)
class VerifiedReleaseInfo:
    version: str
    release_url: str
    asset_name: str
    asset_url: str
    sha256: str
    size_bytes: int
    signature_verified: bool


@dataclass(frozen=True)
class UpdateInstallResult:
    appimage_path: str
    backup_path: str
    desktop_path: str
    wrapper_path: str
    smoke_test_passed: bool


@dataclass(frozen=True)
class UpdateRollbackResult:
    appimage_path: str
    backup_path: str
    desktop_path: str
    wrapper_path: str
    restored_version: str = ""


class UpdateError(RuntimeError):
    """Structured updater failure with a stable machine-readable code."""

    def __init__(self, code: str, message: str, *, release_url: str = ""):
        super().__init__(message)
        self.code = str(code or "update.failed")
        self.release_url = str(release_url or release_page_url())

    def as_payload(self) -> dict[str, str]:
        return {
            "code": self.code,
            "message": str(self),
            "release_url": self.release_url,
        }


def _request(url: str) -> urllib.request.Request:
    return urllib.request.Request(
        url,
        headers={
            "User-Agent": UPDATE_USER_AGENT,
            "Accept": "application/vnd.github+json",
        },
    )


def release_page_url(*, environ=None) -> str:
    env = os.environ if environ is None else environ
    value = str(env.get(UPDATE_RELEASES_URL_ENV) or "").strip()
    return value or GITHUB_RELEASES_URL


def latest_release_api_url(*, environ=None) -> str:
    env = os.environ if environ is None else environ
    value = str(env.get(UPDATE_RELEASE_API_URL_ENV) or "").strip()
    return value or GITHUB_LATEST_RELEASE_API_URL


def _fetch_json(url: str) -> dict:
    with urllib.request.urlopen(_request(url), timeout=10) as resp:
        return json.loads(resp.read().decode("utf-8"))


def _fetch_bytes(url: str) -> bytes:
    with urllib.request.urlopen(_request(url), timeout=15) as resp:
        return resp.read()


def _public_key():
    if Ed25519PublicKey is None:
        raise UpdateError(
            "update.signature_invalid",
            "Cryptographic signature verification is unavailable in this build. "
            f"Missing dependency: {_CRYPTO_IMPORT_ERROR}",
        )
    return Ed25519PublicKey.from_public_bytes(
        base64.b64decode(RELEASE_SIGNING_PUBLIC_KEY_B64)
    )


def verify_release_manifest_bytes(
    manifest_bytes: bytes,
    signature_bytes: bytes,
) -> dict:
    if not manifest_bytes:
        raise UpdateError(
            "update.manifest_missing",
            "The latest GitHub release published an empty release manifest.",
        )
    try:
        _public_key().verify(signature_bytes, manifest_bytes)
    except InvalidSignature:
        raise UpdateError(
            "update.signature_invalid",
            "The latest GitHub release manifest failed signature verification.",
        ) from None
    except ValueError as exc:
        raise UpdateError(
            "update.signature_invalid",
            f"The latest GitHub release manifest could not be verified: {exc}",
        ) from exc
    try:
        manifest = json.loads(manifest_bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise UpdateError(
            "update.signature_invalid",
            f"The signed release manifest is not valid JSON: {exc}",
        ) from exc
    _validate_manifest_shape(manifest)
    return manifest


def _validate_manifest_shape(manifest: dict):
    if not isinstance(manifest, dict):
        raise UpdateError(
            "update.signature_invalid",
            "The signed release manifest is not a JSON object.",
        )
    if str(manifest.get("app") or "").strip() != "WaveLinux":
        raise UpdateError(
            "update.signature_invalid",
            "The signed release manifest is not for WaveLinux.",
        )
    if not str(manifest.get("version") or "").strip():
        raise UpdateError(
            "update.signature_invalid",
            "The signed release manifest does not include a version.",
        )
    if not isinstance(manifest.get("assets"), list):
        raise UpdateError(
            "update.signature_invalid",
            "The signed release manifest does not include an assets list.",
        )


def _normalize_asset(asset: dict) -> dict | None:
    if not isinstance(asset, dict):
        return None
    name = str(asset.get("name") or "").strip()
    kind = str(asset.get("kind") or "").strip().lower()
    arch = str(asset.get("arch") or "").strip().lower()
    download_url = str(asset.get("download_url") or "").strip()
    sha256 = str(asset.get("sha256") or "").strip().lower()
    size_bytes = asset.get("size_bytes")
    if not all((name, kind, arch, download_url, sha256)):
        return None
    try:
        size_bytes = int(size_bytes)
    except (TypeError, ValueError):
        return None
    if size_bytes < 0:
        return None
    if len(sha256) != 64 or any(ch not in "0123456789abcdef" for ch in sha256):
        return None
    return {
        "name": name,
        "kind": kind,
        "arch": arch,
        "download_url": download_url,
        "sha256": sha256,
        "size_bytes": size_bytes,
    }


def select_appimage_asset(manifest: dict, *, arch: str = "x86_64") -> dict:
    candidates = []
    for raw_asset in manifest.get("assets", []) or []:
        asset = _normalize_asset(raw_asset)
        if asset is None:
            continue
        if asset["kind"] != "appimage" or asset["arch"] != arch:
            continue
        score = (
            1 if asset["name"].startswith("WaveLinux-") else 0,
            1 if asset["name"].endswith(".AppImage") else 0,
            asset["size_bytes"],
        )
        candidates.append((score, asset))
    if not candidates:
        raise UpdateError(
            "update.asset_missing",
            "The signed release manifest does not contain an x86_64 AppImage asset.",
            release_url=str(manifest.get("release_url") or release_page_url()),
        )
    return max(candidates, key=lambda item: item[0])[1]


def find_release_asset_download_url(release_data: dict, asset_name: str) -> str:
    for asset in release_data.get("assets", []) or []:
        if str(asset.get("name") or "").strip() != asset_name:
            continue
        return str(asset.get("browser_download_url") or "").strip()
    return ""


def latest_release_data() -> dict:
    return _fetch_json(latest_release_api_url())


def verified_release_info_from_release_data(release_data: dict) -> VerifiedReleaseInfo:
    release_url = str(release_data.get("html_url") or release_page_url()).strip() or release_page_url()
    manifest_url = find_release_asset_download_url(
        release_data,
        RELEASE_MANIFEST_FILENAME,
    )
    signature_url = find_release_asset_download_url(
        release_data,
        RELEASE_MANIFEST_SIGNATURE_FILENAME,
    )
    if not manifest_url or not signature_url:
        raise UpdateError(
            "update.manifest_missing",
            "The latest GitHub release does not publish a signed WaveLinux release manifest yet.",
            release_url=release_url,
        )
    manifest = verify_release_manifest_bytes(
        _fetch_bytes(manifest_url),
        _fetch_bytes(signature_url),
    )
    asset = select_appimage_asset(manifest)
    return VerifiedReleaseInfo(
        version=str(manifest.get("version") or "").strip(),
        release_url=str(manifest.get("release_url") or release_url).strip() or release_url,
        asset_name=asset["name"],
        asset_url=asset["download_url"],
        sha256=asset["sha256"],
        size_bytes=int(asset["size_bytes"]),
        signature_verified=True,
    )


def latest_verified_release_info() -> VerifiedReleaseInfo:
    return verified_release_info_from_release_data(latest_release_data())


def file_sha256(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_file_sha256(path: str, expected_hex: str) -> None:
    actual = file_sha256(path)
    if actual != expected_hex.lower():
        raise UpdateError(
            "update.checksum_mismatch",
            f"Downloaded AppImage checksum mismatch. Expected {expected_hex.lower()}, got {actual}.",
        )


def _appimage_run_env() -> dict[str, str]:
    env = os.environ.copy()
    env["APPIMAGE_EXTRACT_AND_RUN"] = "1"
    return env


def smoke_test_appimage(path: str) -> None:
    commands = (
        [path, "--version"],
        [path, "--self-test"],
    )
    for cmd in commands:
        try:
            result = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                timeout=15,
                check=False,
                env=_appimage_run_env(),
            )
        except Exception as exc:
            raise UpdateError(
                "update.smoke_test_failed",
                f"Downloaded AppImage failed to launch for smoke validation: {exc}",
            ) from exc
        if result.returncode != 0:
            stderr = (result.stderr or result.stdout or "").strip()
            raise UpdateError(
                "update.smoke_test_failed",
                "Downloaded AppImage failed smoke validation"
                + (f": {stderr[:200]}" if stderr else "."),
            )


def _read_appimage_version(path: str) -> str:
    try:
        result = subprocess.run(
            [path, "--version"],
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
            env=_appimage_run_env(),
        )
    except Exception:
        return ""
    if result.returncode != 0:
        return ""
    return str(result.stdout or "").strip().splitlines()[0] if str(result.stdout or "").strip() else ""


def _copy_backup(source: str, backup_path: str):
    tmp_backup = backup_path + ".tmp"
    try:
        shutil.copy2(source, tmp_backup)
        os.replace(tmp_backup, backup_path)
    finally:
        if os.path.exists(tmp_backup):
            try:
                os.remove(tmp_backup)
            except OSError:
                pass


def _restore_backup(backup_path: str, *, home=None):
    if not backup_path or not os.path.exists(backup_path):
        return None
    return install_appimage_file(backup_path, home=home)


def download_file(
    url: str,
    target_path: str,
    *,
    label: str = "",
    progress_callback=None,
    cancel_event: threading.Event | None = None,
) -> int:
    with urllib.request.urlopen(_request(url), timeout=30) as resp, open(target_path, "wb") as handle:
        total_header = resp.headers.get("Content-Length")
        total = int(total_header) if total_header and total_header.isdigit() else 0
        downloaded = 0
        if progress_callback is not None:
            progress_callback("progress", label, downloaded, total)
        while True:
            if cancel_event is not None and cancel_event.is_set():
                raise UpdateError("update.cancelled", "Update cancelled.")
            chunk = resp.read(1024 * 256)
            if not chunk:
                break
            handle.write(chunk)
            downloaded += len(chunk)
            if progress_callback is not None:
                progress_callback("progress", label, downloaded, total)
    return downloaded


def install_verified_release(
    release_info: VerifiedReleaseInfo,
    *,
    home=None,
    progress_callback=None,
    cancel_event: threading.Event | None = None,
) -> UpdateInstallResult:
    temp_path = ""
    installed_path = installed_appimage_path(home=home)
    backup_path = installed_appimage_backup_path(home=home)
    backup_created = False
    try:
        with tempfile.NamedTemporaryFile(
            prefix="wavelinux-update-",
            suffix=".AppImage",
            delete=False,
        ) as handle:
            temp_path = handle.name
        if progress_callback is not None:
            progress_callback("status", "Downloading verified AppImage…")
        download_file(
            release_info.asset_url,
            temp_path,
            label=release_info.asset_name,
            progress_callback=progress_callback,
            cancel_event=cancel_event,
        )
        os.chmod(temp_path, 0o755)
        if progress_callback is not None:
            progress_callback("status", "Verifying AppImage checksum…")
        verify_file_sha256(temp_path, release_info.sha256)
        if progress_callback is not None:
            progress_callback("status", "Running AppImage smoke checks…")
        smoke_test_appimage(temp_path)
        if os.path.exists(installed_path):
            _copy_backup(installed_path, backup_path)
            backup_created = True
        if progress_callback is not None:
            progress_callback("status", "Installing verified AppImage…")
        result = install_appimage_file(temp_path, home=home)
        return UpdateInstallResult(
            appimage_path=result.appimage_path,
            backup_path=backup_path if backup_created else "",
            desktop_path=result.desktop_path,
            wrapper_path=result.wrapper_path,
            smoke_test_passed=True,
        )
    except UpdateError:
        if backup_created:
            _restore_backup(backup_path, home=home)
        raise
    except urllib.error.HTTPError as exc:
        if backup_created:
            _restore_backup(backup_path, home=home)
        raise UpdateError(
            "update.asset_missing",
            f"HTTP {exc.code}: {exc.reason}",
            release_url=release_info.release_url,
        ) from exc
    except urllib.error.URLError as exc:
        if backup_created:
            _restore_backup(backup_path, home=home)
        raise UpdateError(
            "update.asset_missing",
            f"Network error: {exc.reason}",
            release_url=release_info.release_url,
        ) from exc
    except Exception as exc:
        if backup_created:
            _restore_backup(backup_path, home=home)
        raise UpdateError(
            "update.smoke_test_failed",
            f"Verified AppImage install failed: {exc}",
            release_url=release_info.release_url,
        ) from exc
    finally:
        if temp_path and os.path.exists(temp_path):
            try:
                os.remove(temp_path)
            except OSError:
                pass


def restore_previous_install(*, home=None) -> UpdateRollbackResult:
    backup_path = installed_appimage_backup_path(home=home)
    if not os.path.exists(backup_path):
        raise UpdateError(
            "update.rollback_failed",
            f"No previous AppImage backup exists at {backup_path}.",
        )
    try:
        smoke_test_appimage(backup_path)
        result = install_appimage_file(backup_path, home=home)
    except UpdateError as exc:
        raise UpdateError(
            "update.rollback_failed",
            str(exc),
            release_url=exc.release_url,
        ) from exc
    except Exception as exc:
        raise UpdateError(
            "update.rollback_failed",
            f"Could not restore the previous AppImage backup: {exc}",
        ) from exc
    return UpdateRollbackResult(
        appimage_path=result.appimage_path,
        backup_path=backup_path,
        desktop_path=result.desktop_path,
        wrapper_path=result.wrapper_path,
        restored_version=_read_appimage_version(backup_path),
    )


class UpdateChecker:
    """Background verified-release checker using queue polling."""

    _RELEASES_URL = GITHUB_RELEASES_URL

    def __init__(self):
        self._q = queue.SimpleQueue()
        self._cancel = threading.Event()

    def check(self):
        self._cancel.clear()
        threading.Thread(target=self._do_check, daemon=True).start()

    def cancel(self):
        self._cancel.set()

    def poll(self):
        try:
            return self._q.get_nowait()
        except queue.Empty:
            return None

    def _do_check(self):
        try:
            info = latest_verified_release_info()
            if self._cancel.is_set():
                return
            self._q.put(("result", info))
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                self._q.put((
                    "error",
                    UpdateError(
                        "update.manifest_missing",
                        "No GitHub releases have been published yet.",
                    ).as_payload(),
                ))
            else:
                self._q.put((
                    "error",
                    UpdateError(
                        "update.asset_missing",
                        f"HTTP {exc.code}: {exc.reason}",
                    ).as_payload(),
                ))
        except urllib.error.URLError as exc:
            self._q.put((
                "error",
                UpdateError(
                    "update.asset_missing",
                    f"Network error: {exc.reason}",
                ).as_payload(),
            ))
        except UpdateError as exc:
            self._q.put(("error", exc.as_payload()))
        except Exception as exc:
            self._q.put((
                "error",
                UpdateError("update.asset_missing", f"Check failed: {exc}").as_payload(),
            ))


class AppImageUpdateInstaller:
    """Download the latest verified GitHub AppImage and install it locally."""

    def __init__(self):
        self._q = queue.SimpleQueue()
        self._cancel = threading.Event()

    def install(self, *, release_info: VerifiedReleaseInfo | None = None):
        self._cancel.clear()
        threading.Thread(target=self._do_install, args=(release_info,), daemon=True).start()

    def cancel(self):
        self._cancel.set()

    def poll(self):
        try:
            return self._q.get_nowait()
        except queue.Empty:
            return None

    def _emit_progress(self, kind, *payload):
        self._q.put((kind, *payload))

    def _do_install(self, info: VerifiedReleaseInfo | None):
        try:
            release_info = info or latest_verified_release_info()
            if self._cancel.is_set():
                return
            result = install_verified_release(
                release_info,
                progress_callback=self._emit_progress,
                cancel_event=self._cancel,
            )
            self._q.put(("installed", result, release_info))
        except UpdateError as exc:
            self._q.put(("error", exc.as_payload()))
        except Exception as exc:
            self._q.put((
                "error",
                UpdateError("update.smoke_test_failed", str(exc)).as_payload(),
            ))
