import base64
import json
import os
import shutil
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

import updates


class UpdatesTests(unittest.TestCase):
    def setUp(self):
        private_key = Ed25519PrivateKey.generate()
        public_key = private_key.public_key()
        self._private_key = private_key
        self._public_key_b64 = base64.b64encode(
            public_key.public_bytes(
                encoding=serialization.Encoding.Raw,
                format=serialization.PublicFormat.Raw,
            )
        ).decode("ascii")
        self._public_key_patch = mock.patch.object(
            updates,
            "RELEASE_SIGNING_PUBLIC_KEY_B64",
            self._public_key_b64,
        )
        self._public_key_patch.start()
        self.addCleanup(self._public_key_patch.stop)

    def _manifest(self, **overrides):
        manifest = {
            "app": "WaveLinux",
            "version": "2.0.5",
            "published_at": "2026-05-12T18:22:00Z",
            "release_url": "https://github.com/DuskyProjects/WaveLinux/releases/tag/v2.0.5",
            "assets": [
                {
                    "name": "WaveLinux-2.0.5-x86_64.AppImage",
                    "kind": "appimage",
                    "arch": "x86_64",
                    "download_url": "https://example.test/WaveLinux-2.0.5-x86_64.AppImage",
                    "sha256": "a" * 64,
                    "size_bytes": 12345,
                }
            ],
        }
        manifest.update(overrides)
        return manifest

    def _signed_manifest_bytes(self, manifest):
        payload = json.dumps(manifest, indent=2, sort_keys=True).encode("utf-8")
        signature = self._private_key.sign(payload)
        return payload, signature

    def test_verify_release_manifest_bytes_accepts_valid_signature(self):
        manifest = self._manifest()
        payload, signature = self._signed_manifest_bytes(manifest)

        verified = updates.verify_release_manifest_bytes(payload, signature)

        self.assertEqual(verified["version"], "2.0.5")
        self.assertEqual(verified["assets"][0]["arch"], "x86_64")

    def test_verify_release_manifest_bytes_rejects_invalid_signature(self):
        manifest = self._manifest()
        payload, signature = self._signed_manifest_bytes(manifest)

        with self.assertRaises(updates.UpdateError) as ctx:
            updates.verify_release_manifest_bytes(payload + b"\n", signature)

        self.assertEqual(ctx.exception.code, "update.signature_invalid")

    def test_select_appimage_asset_prefers_x86_64_entry(self):
        manifest = self._manifest(assets=[
            {
                "name": "WaveLinux-2.0.5-arm64.AppImage",
                "kind": "appimage",
                "arch": "arm64",
                "download_url": "https://example.test/arm64",
                "sha256": "b" * 64,
                "size_bytes": 111,
            },
            {
                "name": "WaveLinux-2.0.5-x86_64.AppImage",
                "kind": "appimage",
                "arch": "x86_64",
                "download_url": "https://example.test/x86_64",
                "sha256": "c" * 64,
                "size_bytes": 222,
            },
        ])

        asset = updates.select_appimage_asset(manifest)

        self.assertEqual(asset["name"], "WaveLinux-2.0.5-x86_64.AppImage")
        self.assertEqual(asset["download_url"], "https://example.test/x86_64")

    def test_verified_release_info_from_release_data_requires_signed_manifest_assets(self):
        with self.assertRaises(updates.UpdateError) as ctx:
            updates.verified_release_info_from_release_data({
                "tag_name": "v2.0.5",
                "html_url": "https://github.com/DuskyProjects/WaveLinux/releases/tag/v2.0.5",
                "assets": [],
            })

        self.assertEqual(ctx.exception.code, "update.manifest_missing")

    def test_verified_release_info_from_release_data_uses_signed_manifest(self):
        manifest = self._manifest()
        payload, signature = self._signed_manifest_bytes(manifest)
        release_data = {
            "tag_name": "v2.0.5",
            "html_url": manifest["release_url"],
            "assets": [
                {
                    "name": updates.RELEASE_MANIFEST_FILENAME,
                    "browser_download_url": "https://example.test/manifest",
                },
                {
                    "name": updates.RELEASE_MANIFEST_SIGNATURE_FILENAME,
                    "browser_download_url": "https://example.test/manifest.sig",
                },
            ],
        }

        def fake_fetch(url):
            if url.endswith(".sig"):
                return signature
            return payload

        with mock.patch.object(updates, "_fetch_bytes", side_effect=fake_fetch):
            info = updates.verified_release_info_from_release_data(release_data)

        self.assertEqual(info.version, "2.0.5")
        self.assertTrue(info.signature_verified)
        self.assertEqual(info.asset_name, "WaveLinux-2.0.5-x86_64.AppImage")

    def test_install_verified_release_rolls_back_on_install_failure(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            installed = updates.installed_appimage_path(home=tmpdir)
            os.makedirs(os.path.dirname(installed), exist_ok=True)
            with open(installed, "wb") as handle:
                handle.write(b"old-build")
            os.chmod(installed, 0o755)

            downloaded = os.path.join(tmpdir, "WaveLinux-2.0.5-x86_64.AppImage")
            with open(downloaded, "wb") as handle:
                handle.write(b"new-build")
            os.chmod(downloaded, 0o755)

            release = updates.VerifiedReleaseInfo(
                version="2.0.5",
                release_url="https://example.test/release",
                asset_name="WaveLinux-2.0.5-x86_64.AppImage",
                asset_url="https://example.test/download",
                sha256=updates.file_sha256(downloaded),
                size_bytes=os.path.getsize(downloaded),
                signature_verified=True,
            )

            def fake_download(_url, target_path, **_kwargs):
                shutil.copy2(downloaded, target_path)
                return os.path.getsize(downloaded)

            def fake_install(source, *, home=None):
                if str(source).endswith(".bak"):
                    shutil.copy2(source, updates.installed_appimage_path(home=home))
                    return SimpleNamespace(
                        appimage_path=updates.installed_appimage_path(home=home),
                        desktop_path="desktop",
                        wrapper_path="wrapper",
                    )
                raise RuntimeError("boom")

            with mock.patch.object(updates, "download_file", side_effect=fake_download):
                with mock.patch.object(updates, "smoke_test_appimage", return_value=None):
                    with mock.patch.object(updates, "install_appimage_file", side_effect=fake_install):
                        with self.assertRaises(updates.UpdateError) as ctx:
                            updates.install_verified_release(release, home=tmpdir)

            self.assertEqual(ctx.exception.code, "update.smoke_test_failed")
            with open(installed, "rb") as handle:
                self.assertEqual(handle.read(), b"old-build")
            with open(installed + ".bak", "rb") as handle:
                self.assertEqual(handle.read(), b"old-build")

    def test_install_verified_release_writes_canonical_appimage_and_backup(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            installed = updates.installed_appimage_path(home=tmpdir)
            os.makedirs(os.path.dirname(installed), exist_ok=True)
            with open(installed, "wb") as handle:
                handle.write(b"old-build")
            os.chmod(installed, 0o755)

            downloaded = os.path.join(tmpdir, "WaveLinux-2.0.5-x86_64.AppImage")
            with open(downloaded, "wb") as handle:
                handle.write(b"new-build")
            os.chmod(downloaded, 0o755)

            release = updates.VerifiedReleaseInfo(
                version="2.0.5",
                release_url="https://example.test/release",
                asset_name="WaveLinux-2.0.5-x86_64.AppImage",
                asset_url="https://example.test/download",
                sha256=updates.file_sha256(downloaded),
                size_bytes=os.path.getsize(downloaded),
                signature_verified=True,
            )

            def fake_download(_url, target_path, **_kwargs):
                shutil.copy2(downloaded, target_path)
                return os.path.getsize(downloaded)

            with mock.patch.object(updates, "download_file", side_effect=fake_download):
                with mock.patch.object(updates, "smoke_test_appimage", return_value=None):
                    result = updates.install_verified_release(release, home=tmpdir)

            self.assertTrue(os.path.exists(result.appimage_path))
            with open(result.appimage_path, "rb") as handle:
                self.assertEqual(handle.read(), b"new-build")
            with open(result.backup_path, "rb") as handle:
                self.assertEqual(handle.read(), b"old-build")


if __name__ == "__main__":
    unittest.main()
