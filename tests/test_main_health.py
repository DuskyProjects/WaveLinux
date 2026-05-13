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
                appimage_missing=True,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                wrapper_mismatch=True,
                wrapper_path="/tmp/wavelinux",
                wrapper_target="/opt/old.AppImage",
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
                appimage_missing=False,
                installed_appimage_path="/tmp/WaveLinux.AppImage",
                wrapper_mismatch=False,
                wrapper_path="/tmp/wavelinux",
                wrapper_target="/tmp/WaveLinux.AppImage",
                desktop_mismatch=False,
                desktop_path="/tmp/io.github.duskyprojects.WaveLinux.desktop",
                stale_launcher_entries=(),
            ),
        )

        recovered = next(issue for issue in issues if issue.code == "fx.channel_recovered")
        self.assertEqual(recovered.severity, "info")
        self.assertIn("Automatic recovery completed", recovered.detail)


if __name__ == "__main__":
    unittest.main()
