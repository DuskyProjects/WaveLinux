import unittest
from types import SimpleNamespace
from unittest import mock

from main import WaveLinuxWindow


class _FakeWidget:
    def __init__(self):
        self.visible = True
        self.enabled = True
        self.text = ""
        self.tooltip = ""

    def setVisible(self, value):
        self.visible = bool(value)

    def setEnabled(self, value):
        self.enabled = bool(value)

    def setText(self, value):
        self.text = str(value)

    def setToolTip(self, value):
        self.tooltip = str(value)


class _FakeLabel(_FakeWidget):
    pass


class WaveLinuxUpdateTabTests(unittest.TestCase):
    def _window(self, *, mode_kind, allows_self_update):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        mode = SimpleNamespace(
            kind=mode_kind,
            running_path="/tmp/WaveLinux.AppImage" if mode_kind == "appimage" else "/tmp/WaveLinux/main.py",
            allows_self_update=allows_self_update,
            update_channel="appimage" if allows_self_update else "package-manager",
        )
        win._runtime_mode_detail = lambda: (mode, "desc", "guidance")
        win._install_runtime_btn = _FakeWidget()
        win._rollback_update_btn = _FakeWidget()
        win._download_update_btn = _FakeWidget()
        win._update_policy_lbl = _FakeLabel()
        win._install_state_lbl = _FakeLabel()
        win._install_warning_lbl = _FakeLabel()
        win._update_note_lbl = _FakeLabel()
        win._repair_launcher_btn = _FakeWidget()
        win._pending_update_tag = None
        win._pending_update_asset_url = ""
        return win

    def test_refresh_update_tab_enables_rollback_when_backup_exists(self):
        win = self._window(mode_kind="appimage", allows_self_update=True)
        state = SimpleNamespace(
            running_appimage_path="/tmp/WaveLinux.AppImage",
            installed_appimage_exists=True,
            installed_appimage_path="/tmp/WaveLinux.AppImage",
            installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
            installed_appimage_backup_exists=True,
            wrapper_mode="appimage",
            wrapper_source_dir=None,
            wrapper_bundle_exec=None,
            wrapper_exists=True,
            wrapper_mismatch=False,
            wrapper_path="/tmp/wavelinux",
            desktop_exists=True,
            desktop_exec_target="/tmp/wavelinux",
            desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
            stale_launcher_entries=(),
            warnings=(),
        )

        with mock.patch("main.install_state", return_value=state):
            with mock.patch("main.is_running_in_appimage", return_value=True):
                win._refresh_update_tab()

        self.assertTrue(win._rollback_update_btn.visible)
        self.assertTrue(win._rollback_update_btn.enabled)
        self.assertIn("Backup AppImage: /tmp/WaveLinux.AppImage.bak", win._install_state_lbl.text)
        self.assertIn("Launcher targets active runtime: yes", win._install_state_lbl.text)

    def test_refresh_update_tab_hides_rollback_for_package_mode(self):
        win = self._window(mode_kind="package", allows_self_update=False)
        state = SimpleNamespace(
            running_appimage_path=None,
            installed_appimage_exists=True,
            installed_appimage_path="/tmp/WaveLinux.AppImage",
            installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
            installed_appimage_backup_exists=True,
            wrapper_mode="unknown",
            wrapper_source_dir=None,
            wrapper_bundle_exec=None,
            wrapper_exists=False,
            wrapper_mismatch=False,
            wrapper_path="/tmp/wavelinux",
            desktop_exists=False,
            desktop_exec_target=None,
            desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
            stale_launcher_entries=(),
            warnings=(),
        )

        with mock.patch("main.install_state", return_value=state):
            with mock.patch("main.is_running_in_appimage", return_value=False):
                win._refresh_update_tab()

        self.assertFalse(win._rollback_update_btn.visible)
        self.assertFalse(win._download_update_btn.enabled)
        self.assertIn("Backup AppImage: /tmp/WaveLinux.AppImage.bak", win._install_state_lbl.text)


if __name__ == "__main__":
    unittest.main()
