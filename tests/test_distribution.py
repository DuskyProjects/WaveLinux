import os
import sys
import tempfile
import unittest
from unittest import mock

import distribution


class DistributionTests(unittest.TestCase):
    def test_current_appimage_path_prefers_environment(self):
        with mock.patch.dict(os.environ, {"APPIMAGE": "/tmp/WaveLinux.AppImage"}, clear=False):
            path = distribution.current_appimage_path()
        self.assertEqual(path, "/tmp/WaveLinux.AppImage")

    def test_runtime_mode_detects_appimage(self):
        with mock.patch.dict(os.environ, {"APPIMAGE": "/tmp/WaveLinux.AppImage"}, clear=False):
            mode = distribution.runtime_mode()
        self.assertEqual(mode.kind, "appimage")
        self.assertTrue(mode.allows_self_update)

    def test_runtime_mode_detects_source_checkout(self):
        mode = distribution.runtime_mode(
            frozen=False,
            argv=["/home/dusky/Projects/WaveLinux/main.py"],
        )
        self.assertEqual(mode.kind, "source")
        self.assertTrue(mode.allows_self_update)

    def test_runtime_mode_detects_local_bundle_under_home(self):
        mode = distribution.runtime_mode(
            home="/home/tester",
            frozen=True,
            executable="/home/tester/Downloads/WaveLinux/WaveLinux",
        )
        self.assertEqual(mode.kind, "bundle")
        self.assertTrue(mode.allows_self_update)

    def test_runtime_mode_detects_package_managed_binary(self):
        mode = distribution.runtime_mode(
            home="/home/tester",
            frozen=True,
            executable="/usr/bin/wavelinux",
        )
        self.assertEqual(mode.kind, "package")
        self.assertFalse(mode.allows_self_update)

    def test_launch_command_uses_current_source_tree(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            cmd = distribution.launch_command()
        self.assertEqual(cmd[0], sys.executable)
        self.assertTrue(cmd[1].endswith("/main.py"))

    def test_desktop_exec_command_quotes_paths(self):
        with mock.patch.object(distribution, "launch_command", return_value=["python3", "/tmp/My App/main.py"]):
            cmd = distribution.desktop_exec_command()
        self.assertEqual(cmd, 'python3 "/tmp/My App/main.py"')

    def test_installed_appimage_backup_path_uses_canonical_suffix(self):
        path = distribution.installed_appimage_backup_path(home="/home/tester")
        self.assertEqual(path, "/home/tester/.local/bin/WaveLinux.AppImage.bak")

    def test_install_current_appimage_writes_launcher_files(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            fake_appimage = os.path.join(tmpdir, "WaveLinux.AppImage")
            with open(fake_appimage, "wb") as handle:
                handle.write(b"appimage")
            os.chmod(fake_appimage, 0o755)

            with mock.patch.dict(os.environ, {"APPIMAGE": fake_appimage}, clear=False):
                result = distribution.install_current_appimage(home=tmpdir)

            self.assertTrue(os.path.exists(result.appimage_path))
            self.assertTrue(os.path.exists(result.wrapper_path))
            self.assertTrue(os.path.exists(result.desktop_path))
            self.assertTrue(os.path.exists(result.icon_path))

            with open(result.wrapper_path, "r", encoding="utf-8") as handle:
                wrapper = handle.read()
            self.assertIn(result.appimage_path, wrapper)

            with open(result.desktop_path, "r", encoding="utf-8") as handle:
                desktop = handle.read()
            self.assertIn(f"Exec={result.wrapper_path}", desktop)

    def test_install_source_checkout_writes_source_launcher_files(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            source_dir = os.path.join(tmpdir, "WaveLinux")
            os.makedirs(source_dir, exist_ok=True)
            with open(os.path.join(source_dir, "main.py"), "w", encoding="utf-8") as handle:
                handle.write("print('ok')\n")
            with open(os.path.join(source_dir, "icon.png"), "wb") as handle:
                handle.write(b"icon")

            result = distribution.install_source_checkout(source_dir, home=tmpdir)

            self.assertTrue(os.path.exists(result.wrapper_path))
            self.assertTrue(os.path.exists(result.desktop_path))
            self.assertTrue(os.path.exists(result.icon_path))
            with open(result.wrapper_path, "r", encoding="utf-8") as handle:
                wrapper = handle.read()
            self.assertIn(f'SOURCE_DIR="{source_dir}"', wrapper)
            self.assertIn('exec python3 "$SOURCE_DIR/main.py" "$@"', wrapper)
            with open(result.desktop_path, "r", encoding="utf-8") as handle:
                desktop = handle.read()
            self.assertIn(f"Exec={result.wrapper_path}", desktop)

    def test_install_bundle_binary_writes_bundle_launcher_files(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            bundle_dir = os.path.join(tmpdir, "WaveLinux-bundle")
            os.makedirs(bundle_dir, exist_ok=True)
            bundle_path = os.path.join(bundle_dir, "WaveLinux")
            with open(bundle_path, "wb") as handle:
                handle.write(b"bundle")
            os.chmod(bundle_path, 0o755)

            result = distribution.install_bundle_binary(bundle_path, home=tmpdir)

            self.assertTrue(os.path.exists(result.wrapper_path))
            self.assertTrue(os.path.exists(result.desktop_path))
            self.assertTrue(os.path.exists(result.icon_path))
            with open(result.wrapper_path, "r", encoding="utf-8") as handle:
                wrapper = handle.read()
            self.assertIn(f'BUNDLE_EXEC="{bundle_path}"', wrapper)
            self.assertIn('exec "$BUNDLE_EXEC" "$@"', wrapper)
            with open(result.desktop_path, "r", encoding="utf-8") as handle:
                desktop = handle.read()
            self.assertIn(f"Exec={result.wrapper_path}", desktop)

    def test_install_appimage_file_writes_launcher_files(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            download_dir = os.path.join(tmpdir, "downloads")
            os.makedirs(download_dir, exist_ok=True)
            fake_appimage = os.path.join(download_dir, "WaveLinux-2.0.4-x86_64.AppImage")
            with open(fake_appimage, "wb") as handle:
                handle.write(b"downloaded-appimage")
            os.chmod(fake_appimage, 0o755)

            result = distribution.install_appimage_file(fake_appimage, home=tmpdir)

            self.assertTrue(os.path.exists(result.appimage_path))
            self.assertNotEqual(os.path.abspath(result.appimage_path), os.path.abspath(fake_appimage))
            with open(result.wrapper_path, "r", encoding="utf-8") as handle:
                wrapper = handle.read()
            self.assertIn(result.appimage_path, wrapper)

            state = distribution.install_state(home=tmpdir)
            self.assertEqual(
                state.installed_appimage_backup_path,
                distribution.installed_appimage_backup_path(home=tmpdir),
            )
            self.assertFalse(state.installed_appimage_backup_exists)

    def test_install_state_reports_stale_desktop_entries(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            home = tmpdir
            fake_appimage = os.path.join(tmpdir, "WaveLinux.AppImage")
            with open(fake_appimage, "wb") as handle:
                handle.write(b"appimage")
            os.chmod(fake_appimage, 0o755)

            with mock.patch.dict(os.environ, {"APPIMAGE": fake_appimage}, clear=False):
                distribution.install_current_appimage(home=home)

            apps_dir = os.path.join(home, ".local", "share", "applications")
            stale_path = os.path.join(apps_dir, "wavelinux-2.0.3-x86_64.desktop")
            with open(stale_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "[Desktop Entry]\n"
                    "Name=WaveLinux\n"
                    'Exec=\"/opt/shelly/WaveLinux-2.0.3-x86_64.AppImage\"\n'
                    "Type=Application\n"
                )

            with mock.patch.dict(os.environ, {"APPIMAGE": fake_appimage}, clear=False):
                state = distribution.install_state(home=home)

            self.assertTrue(state.installed_appimage_exists)
            self.assertFalse(state.appimage_missing)
            self.assertTrue(state.desktop_exists)
            self.assertEqual(len(state.stale_launcher_entries), 1)
            self.assertTrue(
                any("extra WaveLinux desktop launcher" in warning for warning in state.warnings)
            )

    def test_install_state_reports_wrapper_target(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            bin_dir = os.path.join(tmpdir, ".local", "bin")
            os.makedirs(bin_dir, exist_ok=True)
            wrapper_path = distribution.installed_wrapper_path(home=tmpdir)
            with open(wrapper_path, "w", encoding="utf-8") as handle:
                handle.write("#!/bin/sh\nexec \"/tmp/Other.AppImage\" \"$@\"\n")

            state = distribution.install_state(home=tmpdir)

            self.assertEqual(state.wrapper_target, "/tmp/Other.AppImage")
            self.assertTrue(state.wrapper_mismatch)
            self.assertTrue(state.appimage_missing)
            self.assertIn("Installed wrapper points at an unexpected AppImage path.", state.warnings)

    def test_install_state_accepts_source_wrapper(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            source_dir = os.path.join(tmpdir, "WaveLinux")
            os.makedirs(source_dir, exist_ok=True)
            with open(os.path.join(source_dir, "main.py"), "w", encoding="utf-8") as handle:
                handle.write("print('ok')\n")

            wrapper_path = distribution.installed_wrapper_path(home=tmpdir)
            os.makedirs(os.path.dirname(wrapper_path), exist_ok=True)
            with open(wrapper_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "#!/bin/sh\n"
                    f'SOURCE_DIR="{source_dir}"\n'
                    'exec python3 "$SOURCE_DIR/main.py" "$@"\n'
                )

            desktop_path = distribution.installed_desktop_path(home=tmpdir)
            os.makedirs(os.path.dirname(desktop_path), exist_ok=True)
            with open(desktop_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "[Desktop Entry]\n"
                    "Name=WaveLinux\n"
                    f"Exec={wrapper_path}\n"
                    "Type=Application\n"
                )

            state = distribution.install_state(home=tmpdir)

            self.assertEqual(state.wrapper_mode, "source")
            self.assertEqual(state.wrapper_source_dir, source_dir)
            self.assertFalse(state.wrapper_mismatch)
            self.assertTrue(state.appimage_missing)
            self.assertEqual(state.warnings, ())

    def test_install_state_accepts_bundle_wrapper(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            bundle_dir = os.path.join(tmpdir, "WaveLinux-bundle")
            os.makedirs(bundle_dir, exist_ok=True)
            bundle_path = os.path.join(bundle_dir, "WaveLinux")
            with open(bundle_path, "wb") as handle:
                handle.write(b"bundle")
            os.chmod(bundle_path, 0o755)

            wrapper_path = distribution.installed_wrapper_path(home=tmpdir)
            os.makedirs(os.path.dirname(wrapper_path), exist_ok=True)
            with open(wrapper_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "#!/bin/sh\n"
                    f'BUNDLE_EXEC="{bundle_path}"\n'
                    'exec "$BUNDLE_EXEC" "$@"\n'
                )

            desktop_path = distribution.installed_desktop_path(home=tmpdir)
            os.makedirs(os.path.dirname(desktop_path), exist_ok=True)
            with open(desktop_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "[Desktop Entry]\n"
                    "Name=WaveLinux\n"
                    f"Exec={wrapper_path}\n"
                    "Type=Application\n"
                )

            state = distribution.install_state(home=tmpdir)

            self.assertEqual(state.wrapper_mode, "bundle")
            self.assertEqual(state.wrapper_bundle_exec, bundle_path)
            self.assertFalse(state.wrapper_mismatch)
            self.assertTrue(state.appimage_missing)
            self.assertEqual(state.warnings, ())

    def test_install_state_warns_when_source_wrapper_checkout_is_missing(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            wrapper_path = distribution.installed_wrapper_path(home=tmpdir)
            os.makedirs(os.path.dirname(wrapper_path), exist_ok=True)
            with open(wrapper_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "#!/bin/sh\n"
                    'SOURCE_DIR="/tmp/missing-wavelinux"\n'
                    'exec python3 "$SOURCE_DIR/main.py" "$@"\n'
                )

            state = distribution.install_state(home=tmpdir)

            self.assertEqual(state.wrapper_mode, "source")
            self.assertIn("Installed source wrapper points at a missing WaveLinux checkout.", state.warnings)

    def test_install_state_warns_when_bundle_wrapper_binary_is_missing(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            wrapper_path = distribution.installed_wrapper_path(home=tmpdir)
            os.makedirs(os.path.dirname(wrapper_path), exist_ok=True)
            with open(wrapper_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "#!/bin/sh\n"
                    'BUNDLE_EXEC="/tmp/missing-wavelinux-bundle"\n'
                    'exec "$BUNDLE_EXEC" "$@"\n'
                )

            state = distribution.install_state(home=tmpdir)

            self.assertEqual(state.wrapper_mode, "bundle")
            self.assertIn("Installed bundle launcher points at a missing WaveLinux binary.", state.warnings)

    def test_repair_source_checkout_launchers_rewrites_canonical_files_and_removes_stale(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            source_dir = os.path.join(tmpdir, "WaveLinux")
            os.makedirs(source_dir, exist_ok=True)
            with open(os.path.join(source_dir, "main.py"), "w", encoding="utf-8") as handle:
                handle.write("print('ok')\n")
            with open(os.path.join(source_dir, "icon.png"), "wb") as handle:
                handle.write(b"icon")

            result = distribution.install_source_checkout(source_dir, home=tmpdir)

            stale_path = os.path.join(
                tmpdir,
                ".local",
                "share",
                "applications",
                "wavelinux-legacy.desktop",
            )
            with open(stale_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "[Desktop Entry]\n"
                    "Name=WaveLinux\n"
                    'Exec="/opt/old/WaveLinux.AppImage"\n'
                    "Type=Application\n"
                )

            repaired = distribution.repair_source_checkout_launchers(source_dir, home=tmpdir)

            self.assertEqual(repaired.source_dir, source_dir)
            self.assertIn(stale_path, repaired.removed_entries)
            self.assertFalse(os.path.exists(stale_path))
            with open(result.wrapper_path, "r", encoding="utf-8") as handle:
                wrapper = handle.read()
            self.assertIn(f'SOURCE_DIR="{source_dir}"', wrapper)

    def test_repair_bundle_launchers_rewrites_canonical_files_and_removes_stale(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            bundle_dir = os.path.join(tmpdir, "WaveLinux-bundle")
            os.makedirs(bundle_dir, exist_ok=True)
            bundle_path = os.path.join(bundle_dir, "WaveLinux")
            with open(bundle_path, "wb") as handle:
                handle.write(b"bundle")
            os.chmod(bundle_path, 0o755)

            result = distribution.install_bundle_binary(bundle_path, home=tmpdir)

            stale_path = os.path.join(
                tmpdir,
                ".local",
                "share",
                "applications",
                "wavelinux-legacy.desktop",
            )
            with open(stale_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "[Desktop Entry]\n"
                    "Name=WaveLinux\n"
                    'Exec="/tmp/old/WaveLinux"\n'
                    "Type=Application\n"
                )

            repaired = distribution.repair_bundle_launchers(bundle_path, home=tmpdir)

            self.assertEqual(repaired.bundle_path, bundle_path)
            self.assertIn(stale_path, repaired.removed_entries)
            self.assertFalse(os.path.exists(stale_path))
            with open(result.wrapper_path, "r", encoding="utf-8") as handle:
                wrapper = handle.read()
            self.assertIn(f'BUNDLE_EXEC="{bundle_path}"', wrapper)

    def test_repair_installed_appimage_launchers_rewrites_canonical_files_and_removes_stale(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            appimage_path = distribution.installed_appimage_path(home=tmpdir)
            os.makedirs(os.path.dirname(appimage_path), exist_ok=True)
            with open(appimage_path, "wb") as handle:
                handle.write(b"appimage")
            os.chmod(appimage_path, 0o755)

            wrapper_path = distribution.installed_wrapper_path(home=tmpdir)
            os.makedirs(os.path.dirname(wrapper_path), exist_ok=True)
            with open(wrapper_path, "w", encoding="utf-8") as handle:
                handle.write("#!/bin/sh\nexec \"/tmp/Other.AppImage\" \"$@\"\n")

            desktop_path = distribution.installed_desktop_path(home=tmpdir)
            os.makedirs(os.path.dirname(desktop_path), exist_ok=True)
            with open(desktop_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "[Desktop Entry]\n"
                    "Name=WaveLinux\n"
                    'Exec=\"/tmp/Other.AppImage\"\n'
                    "Type=Application\n"
                )

            stale_path = os.path.join(
                tmpdir,
                ".local",
                "share",
                "applications",
                "wavelinux-legacy.desktop",
            )
            with open(stale_path, "w", encoding="utf-8") as handle:
                handle.write(
                    "[Desktop Entry]\n"
                    "Name=WaveLinux\n"
                    'Exec=\"/opt/shelly/WaveLinux-2.0.3-x86_64.AppImage\"\n'
                    "Type=Application\n"
                )

            result = distribution.repair_installed_appimage_launchers(home=tmpdir)

            self.assertEqual(result.appimage_path, appimage_path)
            self.assertIn(stale_path, result.removed_entries)
            self.assertFalse(os.path.exists(stale_path))

            with open(wrapper_path, "r", encoding="utf-8") as handle:
                wrapper = handle.read()
            self.assertIn(appimage_path, wrapper)

            with open(desktop_path, "r", encoding="utf-8") as handle:
                desktop = handle.read()
            self.assertIn(f"Exec={wrapper_path}", desktop)


if __name__ == "__main__":
    unittest.main()
