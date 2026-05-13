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

    def test_launch_command_uses_current_source_tree(self):
        with mock.patch.dict(os.environ, {}, clear=False):
            cmd = distribution.launch_command()
        self.assertEqual(cmd[0], sys.executable)
        self.assertTrue(cmd[1].endswith("/main.py"))

    def test_desktop_exec_command_quotes_paths(self):
        with mock.patch.object(distribution, "launch_command", return_value=["python3", "/tmp/My App/main.py"]):
            cmd = distribution.desktop_exec_command()
        self.assertEqual(cmd, 'python3 "/tmp/My App/main.py"')

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
            self.assertIn("Installed wrapper points at an unexpected AppImage path.", state.warnings)

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
