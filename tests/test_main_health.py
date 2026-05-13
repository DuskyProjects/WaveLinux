import time
import unittest
from types import SimpleNamespace

from audio_runtime.models import OperationStatus
from health import RecoveryStatus
from main import WaveLinuxWindow


class _FakeTimer:
    def __init__(self, active=True, remaining_ms=1500):
        self._active = active
        self._remaining_ms = remaining_ms

    def isActive(self):
        return self._active

    def remainingTime(self):
        return self._remaining_ms


class _FakeRuntime:
    def __init__(self):
        self.statuses = {}
        self.diagnostics = SimpleNamespace(root_dir="/tmp/wavelinux-diag")

    def fx_status_for(self, node_name):
        return self.statuses.get(node_name, OperationStatus(node_name=node_name))


class WaveLinuxHealthTests(unittest.TestCase):
    def _window(self):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        win.runtime = _FakeRuntime()
        win._runtime_view_state = SimpleNamespace(health={}, mic_inputs=[], virtual_channels=[])
        win._auto_recovery_state = {}
        win._recent_recovery_status = {}
        win._last_update_issue = None
        win._channel_label = lambda node_name: node_name
        win._current_runtime_mode = lambda: SimpleNamespace(
            kind="appimage",
            running_path="/tmp/WaveLinux.AppImage",
            allows_self_update=True,
            update_channel="appimage",
        )
        return win

    def test_collect_health_issues_includes_runtime_install_update_and_fx(self):
        win = self._window()
        win.runtime.statuses["mic"] = OperationStatus(
            node_name="mic",
            state="degraded",
            generation=2,
            message="FX rebuild failed; diagnostics: /tmp/fx.json",
            diagnostics_path="/tmp/fx.json",
        )
        win._runtime_view_state = SimpleNamespace(
            health={"mic": "fx_effects_mismatch"},
            mic_inputs=[],
            virtual_channels=[],
        )
        win._auto_recovery_state["mic"] = {
            "attempts": 1,
            "timer": _FakeTimer(active=True, remaining_ms=1200),
            "last_delay_ms": 1500,
            "generation": 2,
            "exhausted": False,
        }
        win._last_update_issue = {
            "code": "update.manifest_missing",
            "message": "Signed manifest not published.",
            "release_url": "https://example.test/releases",
        }

        issues = win._collect_health_issues(
            preflight={
                "deps": {"pactl": True, "parec": False},
                "issue_details": [
                    {
                        "code": "runtime.missing_tool",
                        "detail": "Missing required audio/runtime tools: parec",
                        "context": {"tools": ["parec"]},
                    }
                ],
            },
            state=SimpleNamespace(
                running_appimage_path=None,
                appimage_missing=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=True,
                wrapper_mode="unknown",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec=None,
                wrapper_source_dir=None,
                wrapper_target="/opt/old.AppImage",
                desktop_exists=True,
                desktop_mismatch=True,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(SimpleNamespace(path="/tmp/stale.desktop"),),
            ),
        )

        codes = {issue.code for issue in issues}

        self.assertIn("runtime.missing_tool", codes)
        self.assertIn("install.appimage_missing", codes)
        self.assertIn("install.wrapper_mismatch", codes)
        self.assertIn("install.desktop_stale", codes)
        self.assertIn("update.manifest_missing", codes)
        self.assertIn("fx.channel_degraded", codes)
        fx_issue = next(issue for issue in issues if issue.code == "fx.channel_degraded")
        self.assertEqual(fx_issue.primary_action, "Recover channel")
        self.assertEqual(fx_issue.secondary_action, "Open diagnostics")

    def test_collect_health_issues_includes_recovered_card(self):
        win = self._window()
        win._recent_recovery_status["mic"] = {
            "at": time.time(),
            "status": RecoveryStatus(
                node_name="mic",
                state="recovered",
                retry_count=2,
                diagnostics_path="/tmp/fx.json",
            ),
        }

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path=None,
                appimage_missing=False,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=False,
                wrapper_mode="appimage",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec=None,
                wrapper_source_dir=None,
                wrapper_target="/tmp/WaveLinux.AppImage",
                desktop_exists=True,
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
            ),
        )

        recovered = next(issue for issue in issues if issue.code == "fx.channel_recovered")
        self.assertEqual(recovered.severity, "info")
        self.assertIn("Automatic recovery completed", recovered.detail)

    def test_collect_health_issues_skips_missing_appimage_for_package_mode(self):
        win = self._window()
        win._current_runtime_mode = lambda: SimpleNamespace(
            kind="package",
            running_path="/usr/bin/wavelinux",
            allows_self_update=False,
            update_channel="package-manager",
        )

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path=None,
                appimage_missing=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=False,
                wrapper_mode="unknown",
                wrapper_exists=False,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec=None,
                wrapper_source_dir=None,
                wrapper_target=None,
                desktop_exists=False,
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
            ),
        )

        self.assertNotIn("install.appimage_missing", {issue.code for issue in issues})

    def test_collect_health_issues_flags_missing_source_checkout_wrapper(self):
        win = self._window()
        win._current_runtime_mode = lambda: SimpleNamespace(
            kind="source",
            running_path="/home/tester/Projects/WaveLinux/main.py",
            allows_self_update=True,
            update_channel="appimage",
        )

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path=None,
                appimage_missing=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=False,
                wrapper_mode="source",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec=None,
                wrapper_source_dir="/tmp/missing-checkout",
                wrapper_target="python3",
                desktop_exists=True,
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
                warnings=("Installed source wrapper points at a missing WaveLinux checkout.",),
            ),
        )

        codes = {issue.code for issue in issues}
        self.assertIn("install.wrapper_mismatch", codes)
        self.assertNotIn("install.appimage_missing", codes)

    def test_collect_health_issues_flags_missing_bundle_wrapper(self):
        win = self._window()
        win._current_runtime_mode = lambda: SimpleNamespace(
            kind="bundle",
            running_path="/home/tester/Downloads/WaveLinux/WaveLinux",
            allows_self_update=True,
            update_channel="appimage",
        )

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path=None,
                appimage_missing=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=False,
                wrapper_mode="bundle",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec="/tmp/missing-bundle",
                wrapper_source_dir=None,
                wrapper_target="/tmp/missing-bundle",
                desktop_exists=True,
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
                warnings=("Installed bundle launcher points at a missing WaveLinux binary.",),
            ),
        )

        codes = {issue.code for issue in issues}
        self.assertIn("install.wrapper_mismatch", codes)
        self.assertNotIn("install.appimage_missing", codes)

    def test_collect_health_issues_flags_different_source_checkout(self):
        win = self._window()
        win._current_runtime_mode = lambda: SimpleNamespace(
            kind="source",
            running_path="/home/tester/Projects/WaveLinux-dev/main.py",
            allows_self_update=True,
            update_channel="appimage",
        )

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path=None,
                appimage_missing=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_exists=False,
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=False,
                wrapper_mode="source",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec=None,
                wrapper_source_dir="/home/tester/Projects/WaveLinux-stable",
                wrapper_target="python3",
                desktop_exists=True,
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
                warnings=(),
            ),
        )

        mismatch = next(issue for issue in issues if issue.code == "install.runtime_target_mismatch")
        self.assertEqual(mismatch.severity, "info")
        self.assertIn("different checkout", mismatch.title.lower())
        self.assertNotIn("install.appimage_missing", {issue.code for issue in issues})

    def test_collect_health_issues_flags_different_bundle_binary(self):
        win = self._window()
        win._current_runtime_mode = lambda: SimpleNamespace(
            kind="bundle",
            running_path="/home/tester/Downloads/WaveLinux-dev/WaveLinux",
            allows_self_update=True,
            update_channel="appimage",
        )

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path=None,
                appimage_missing=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_exists=False,
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=False,
                wrapper_mode="bundle",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec="/home/tester/Downloads/WaveLinux-stable/WaveLinux",
                wrapper_source_dir=None,
                wrapper_target="/home/tester/Downloads/WaveLinux-stable/WaveLinux",
                desktop_exists=True,
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
                warnings=(),
            ),
        )

        mismatch = next(issue for issue in issues if issue.code == "install.runtime_target_mismatch")
        self.assertEqual(mismatch.severity, "info")
        self.assertIn("different binary", mismatch.title.lower())

    def test_collect_health_issues_flags_different_running_appimage(self):
        win = self._window()

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path="/tmp/WaveLinux-2.0.5-x86_64.AppImage",
                appimage_missing=False,
                installed_appimage_exists=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=False,
                wrapper_mode="appimage",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec=None,
                wrapper_source_dir=None,
                wrapper_target="/tmp/WaveLinux.AppImage",
                desktop_exists=True,
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
                warnings=(),
            ),
        )

        mismatch = next(issue for issue in issues if issue.code == "install.runtime_target_mismatch")
        self.assertEqual(mismatch.severity, "info")
        self.assertIn("different file", mismatch.title.lower())

    def test_collect_health_issues_includes_backup_available_card(self):
        win = self._window()

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path="/tmp/WaveLinux.AppImage",
                appimage_missing=False,
                installed_appimage_exists=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=True,
                wrapper_mismatch=False,
                wrapper_mode="appimage",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec=None,
                wrapper_source_dir=None,
                wrapper_target="/tmp/WaveLinux.AppImage",
                desktop_exists=True,
                desktop_exec_target="/tmp/wavelinux",
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
                warnings=(),
            ),
        )

        backup_issue = next(issue for issue in issues if issue.code == "update.backup_available")
        self.assertEqual(backup_issue.severity, "info")
        self.assertEqual(backup_issue.primary_action, "Restore Previous AppImage")

    def test_collect_health_issues_surfaces_rollback_failure(self):
        win = self._window()
        win._last_update_issue = {
            "code": "update.rollback_failed",
            "message": "Could not restore the previous AppImage backup.",
            "release_url": "https://example.test/releases",
        }

        issues = win._collect_health_issues(
            preflight={"deps": {}, "issue_details": []},
            state=SimpleNamespace(
                running_appimage_path="/tmp/WaveLinux.AppImage",
                appimage_missing=False,
                installed_appimage_exists=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                installed_appimage_backup_path="/tmp/WaveLinux.AppImage.bak",
                installed_appimage_backup_exists=False,
                wrapper_mismatch=False,
                wrapper_mode="appimage",
                wrapper_exists=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_bundle_exec=None,
                wrapper_source_dir=None,
                wrapper_target="/tmp/WaveLinux.AppImage",
                desktop_exists=True,
                desktop_exec_target="/tmp/wavelinux",
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
                warnings=(),
            ),
        )

        rollback_issue = next(issue for issue in issues if issue.code == "update.rollback_failed")
        self.assertEqual(rollback_issue.severity, "error")


if __name__ == "__main__":
    unittest.main()
