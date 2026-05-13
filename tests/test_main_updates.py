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
        self.style = ""

    def setVisible(self, value):
        self.visible = bool(value)

    def setEnabled(self, value):
        self.enabled = bool(value)

    def setText(self, value):
        self.text = str(value)

    def setToolTip(self, value):
        self.tooltip = str(value)

    def setStyleSheet(self, value):
        self.style = str(value)


class _FakeLabel(_FakeWidget):
    pass


class _FakeProgress(_FakeWidget):
    def __init__(self):
        super().__init__()
        self.range = (0, 100)
        self.value = 0
        self.format = ""

    def setRange(self, low, high):
        self.range = (int(low), int(high))

    def setValue(self, value):
        self.value = int(value)

    def setFormat(self, value):
        self.format = str(value)


class _FakeTimer:
    def __init__(self):
        self.started = False
        self.interval = None

    def setInterval(self, value):
        self.interval = int(value)

    class _Signal:
        def connect(self, _handler):
            return None

    @property
    def timeout(self):
        return self._Signal()

    def stop(self):
        self.started = False

    def start(self):
        self.started = True


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
        win._check_update_btn = _FakeWidget()
        win._update_status_lbl = _FakeLabel()
        win._update_progress = _FakeProgress()
        win._update_install_poll_timer = _FakeTimer()
        win._pending_update_tag = None
        win._pending_update_asset_url = ""
        win._pending_verified_release = None
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

    def test_download_and_install_update_rechecks_latest_instead_of_using_stale_cached_release(self):
        win = self._window(mode_kind="appimage", allows_self_update=True)
        win._pending_verified_release = SimpleNamespace(version="2.0.8")

        created = []

        class _FakeInstaller:
            def __init__(self):
                self.calls = []
                created.append(self)

            def cancel(self):
                return None

            def install(self, *, release_info=None):
                self.calls.append(release_info)

            def poll(self):
                return None

        with mock.patch("main.AppImageUpdateInstaller", _FakeInstaller):
            win._download_and_install_update()

        self.assertEqual(len(created), 1)
        self.assertEqual(created[0].calls, [None])
        self.assertEqual(
            win._update_status_lbl.text,
            "Checking latest verified AppImage release…",
        )
        self.assertEqual(win._update_progress.format, "Checking latest release…")
        self.assertTrue(win._update_install_poll_timer.started)


if __name__ == "__main__":
    unittest.main()
