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


if __name__ == "__main__":
    unittest.main()
