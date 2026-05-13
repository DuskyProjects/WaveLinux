import unittest
from types import SimpleNamespace
from unittest import mock

from audio_runtime.models import OperationStatus
import main
from main import WaveLinuxWindow


class _FakeLabel:
    def __init__(self):
        self._text = ""

    def setText(self, text):
        self._text = str(text)

    def text(self):
        return self._text


class _FakeSignal:
    def __init__(self):
        self._callbacks = []

    def connect(self, callback):
        self._callbacks.append(callback)

    def emit(self):
        for callback in list(self._callbacks):
            callback()


class _FakeTimer:
    def __init__(self):
        self.timeout = _FakeSignal()
        self.delay_ms = 0
        self._active = False

    def setSingleShot(self, _flag):
        return

    def start(self, delay_ms):
        self.delay_ms = int(delay_ms)
        self._active = True

    def stop(self):
        self._active = False

    def isActive(self):
        return self._active

    def fire(self):
        self._active = False
        self.timeout.emit()


class _FakeRuntime:
    def __init__(self):
        self.statuses = {}
        self.recover_calls = []
        self.export_calls = []

    def fx_status_for(self, node_name):
        return self.statuses.get(node_name, OperationStatus(node_name=node_name))

    def recover_channel(self, node_name):
        self.recover_calls.append(node_name)

    def export_diagnostics(self, reason="manual-export"):
        self.export_calls.append(reason)
        return "/tmp/exported-runtime-diagnostics.json"


class WaveLinuxMainAutoRecoveryTests(unittest.TestCase):
    def _window(self):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        win._auto_recovery_state = {}
        win.runtime = _FakeRuntime()
        win.status_lbl = _FakeLabel()
        win._runtime_view_state = None
        win.channel_widgets = {}
        win._make_auto_recovery_timer = lambda: _FakeTimer()
        win._channel_label = lambda node_name: node_name
        return win

    def test_degraded_status_schedules_first_auto_recovery_attempt(self):
        win = self._window()
        status = OperationStatus(node_name="mic", state="degraded", generation=2, message="boom")
        win.runtime.statuses["mic"] = status

        win._on_runtime_fx_status(status)

        entry = win._auto_recovery_state["mic"]
        self.assertEqual(entry["generation"], 2)
        self.assertEqual(entry["attempts"], 1)
        self.assertEqual(entry["last_delay_ms"], 1500)
        self.assertTrue(entry["timer"].isActive())
        self.assertIn("retrying automatically", win.status_lbl.text())

    def test_auto_recovery_timer_runs_recover_channel(self):
        win = self._window()
        status = OperationStatus(node_name="mic", state="degraded", generation=3, message="boom")
        win.runtime.statuses["mic"] = status
        win._on_runtime_fx_status(status)

        timer = win._auto_recovery_state["mic"]["timer"]
        timer.fire()

        self.assertEqual(win.runtime.recover_calls, ["mic"])
        self.assertIn("Attempting automatic recovery", win.status_lbl.text())

    def test_repeated_degraded_status_uses_second_backoff_attempt(self):
        win = self._window()
        status = OperationStatus(node_name="mic", state="degraded", generation=4, message="boom")
        win.runtime.statuses["mic"] = status
        win._on_runtime_fx_status(status)
        win._auto_recovery_state["mic"]["timer"].fire()

        win._on_runtime_fx_status(status)

        entry = win._auto_recovery_state["mic"]
        self.assertEqual(entry["attempts"], 2)
        self.assertEqual(entry["last_delay_ms"], 5000)
        self.assertTrue(entry["timer"].isActive())

    def test_auto_recovery_marks_channel_exhausted_after_final_attempt(self):
        win = self._window()
        status = OperationStatus(node_name="mic", state="degraded", generation=4, message="boom")
        win.runtime.statuses["mic"] = status
        win._on_runtime_fx_status(status)
        win._auto_recovery_state["mic"]["timer"].fire()
        win._on_runtime_fx_status(status)
        win._auto_recovery_state["mic"]["timer"].fire()

        win._on_runtime_fx_status(status)

        self.assertTrue(win._auto_recovery_state["mic"]["exhausted"])
        self.assertIn("exhausted", win.status_lbl.text())

    def test_active_status_clears_auto_recovery_state(self):
        win = self._window()
        degraded = OperationStatus(node_name="mic", state="degraded", generation=5, message="boom")
        win.runtime.statuses["mic"] = degraded
        win._on_runtime_fx_status(degraded)

        active = OperationStatus(node_name="mic", state="active", generation=5, message="ok")
        win.runtime.statuses["mic"] = active
        win._on_runtime_fx_status(active)

        self.assertNotIn("mic", win._auto_recovery_state)

    def test_manual_recover_channel_clears_retry_state(self):
        win = self._window()
        degraded = OperationStatus(node_name="mic", state="degraded", generation=6, message="boom")
        win.runtime.statuses["mic"] = degraded
        win._on_runtime_fx_status(degraded)

        win.recover_channel("mic")

        self.assertNotIn("mic", win._auto_recovery_state)
        self.assertEqual(win.runtime.recover_calls, ["mic"])

    def test_channel_runtime_issue_surfaces_health_and_diagnostics(self):
        win = self._window()
        status = OperationStatus(
            node_name="mic",
            state="degraded",
            generation=7,
            message="FX rebuild failed; diagnostics: /tmp/fx.json",
            diagnostics_path="/tmp/fx.json",
        )
        win.runtime.statuses["mic"] = status
        win._runtime_view_state = SimpleNamespace(health={"mic": "fx_effects_mismatch"})
        win._on_runtime_fx_status(status)

        issue = win.channel_runtime_issue("mic")

        self.assertTrue(issue["degraded"])
        self.assertEqual(issue["diagnostics_path"], "/tmp/fx.json")
        self.assertIn("active FX chain does not match", issue["tooltip"])
        self.assertIn("Automatic recovery scheduled", issue["tooltip"])

    def test_open_channel_diagnostics_uses_existing_bundle_path(self):
        win = self._window()
        status = OperationStatus(
            node_name="mic",
            state="degraded",
            diagnostics_path="/tmp/fx.json",
        )
        win.runtime.statuses["mic"] = status
        with mock.patch.object(main.os.path, "exists", return_value=True):
            with mock.patch.object(main.QDesktopServices, "openUrl", return_value=True) as open_url:
                win.open_channel_diagnostics("mic")

        self.assertEqual(win.runtime.export_calls, [])
        open_url.assert_called_once()
        self.assertIn("Opened diagnostics for mic", win.status_lbl.text())

    def test_open_channel_diagnostics_exports_when_status_has_no_bundle(self):
        win = self._window()
        win.runtime.statuses["mic"] = OperationStatus(node_name="mic", state="degraded")
        with mock.patch.object(main.os.path, "exists", return_value=True):
            with mock.patch.object(main.QDesktopServices, "openUrl", return_value=True):
                win.open_channel_diagnostics("mic")

        self.assertEqual(win.runtime.export_calls, ["channel-diagnostics:mic"])


if __name__ == "__main__":
    unittest.main()
