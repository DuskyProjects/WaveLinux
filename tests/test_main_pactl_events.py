import os
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from main import WaveLinuxWindow


class _FakeProc:
    def __init__(self, payload):
        self._payload = payload

    def readAllStandardOutput(self):
        return self._payload


class _FakeTimer:
    def __init__(self):
        self.start_calls = 0

    def start(self):
        self.start_calls += 1


class _FakeRuntime:
    def __init__(self):
        self.refresh_calls = []

    def refresh_now(self, reason=""):
        self.refresh_calls.append(reason)


class WaveLinuxPactlEventTests(unittest.TestCase):
    def _window(self, payload=b""):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        win._event_proc = _FakeProc(payload)
        win._event_refresh_timer = _FakeTimer()
        win._hotplug_refresh_timer = _FakeTimer()
        win.runtime = _FakeRuntime()
        return win

    def test_request_runtime_refresh_delegates_to_runtime(self):
        win = self._window()

        win._request_runtime_refresh("periodic-refresh")

        self.assertEqual(win.runtime.refresh_calls, ["periodic-refresh"])

    def test_pactl_sink_event_starts_immediate_and_settle_refresh(self):
        win = self._window(b"Event 'new' on sink #376\n")

        win._on_pactl_event()

        self.assertEqual(win._event_refresh_timer.start_calls, 1)
        self.assertEqual(win._hotplug_refresh_timer.start_calls, 1)

    def test_pactl_sink_input_event_starts_immediate_refresh_only(self):
        win = self._window(b"Event 'change' on sink-input #12\n")

        win._on_pactl_event()

        self.assertEqual(win._event_refresh_timer.start_calls, 1)
        self.assertEqual(win._hotplug_refresh_timer.start_calls, 0)

    def test_pactl_source_output_event_is_ignored(self):
        win = self._window(b"Event 'change' on source-output #8\n")

        win._on_pactl_event()

        self.assertEqual(win._event_refresh_timer.start_calls, 0)
        self.assertEqual(win._hotplug_refresh_timer.start_calls, 0)


if __name__ == "__main__":
    unittest.main()
