import json
import tempfile
import unittest

import os
os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt6.QtWidgets import QApplication, QComboBox
from types import SimpleNamespace

from main import ChannelStrip, WaveLinuxWindow


class _DummyCombo:
    def __init__(self, value=None):
        self._value = value

    def currentData(self):
        return self._value


class _FakeRuntime:
    def __init__(self):
        self.calls = []
        self.reset_calls = 0
        self.selected_mic_calls = []
        self.mix_route_calls = []
        self.mix_route_sync_calls = []

    def sync_persistent_state(self, **kwargs):
        self.calls.append(kwargs)

    def full_audio_reset_sync(self):
        self.reset_calls += 1

    def set_selected_mic(self, node_name):
        self.selected_mic_calls.append(node_name)

    def set_mix_hardware_route(self, mix_name, sink_name):
        self.mix_route_calls.append((mix_name, sink_name))

    def set_mix_hardware_route_sync(self, mix_name, sink_name):
        self.mix_route_sync_calls.append((mix_name, sink_name))


class WaveLinuxMainMixPersistenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._app = QApplication.instance() or QApplication([])

    def _window(self):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        win.selected_mic = None
        win._mic_selection_initialized = False
        win.submix_state = {}
        win.active_effects = {}
        win.effect_params = {}
        win.app_routing = {}
        win.app_volumes = {}
        win.virtual_channels = []
        win.hidden_nodes = set()
        win.channel_order = []
        win.app_last_seen = {}
        win.app_display_names = {}
        win.app_prune_days = 14
        win.forgotten_apps = set()
        win._desired_mix_hw = {"Monitor": None, "Stream": None}
        win.mon_out_combo = _DummyCombo(None)
        win.str_out_combo = _DummyCombo(None)
        win._runtime_pid_path = ""
        return win

    def test_sync_runtime_persistent_state_uses_desired_mix_routes_when_combo_empty(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win._desired_mix_hw["Monitor"] = "bluez_output.AA_BB_CC_DD_EE_FF.1"
        win._desired_mix_hw["Stream"] = "alsa_output.speakers"

        win._sync_runtime_persistent_state()

        self.assertEqual(len(win.runtime.calls), 1)
        call = win.runtime.calls[0]
        self.assertEqual(call["monitor_hw"], "bluez_output.AA_BB_CC_DD_EE_FF.1")
        self.assertEqual(call["stream_hw"], "alsa_output.speakers")
        self.assertEqual(call["app_volumes"], {})

    def test_save_config_persists_desired_mix_routes_when_combo_empty(self):
        win = self._window()
        win._desired_mix_hw["Monitor"] = "bluez_output.AA_BB_CC_DD_EE_FF.1"
        win._desired_mix_hw["Stream"] = "alsa_output.speakers"
        with tempfile.TemporaryDirectory() as tmpdir:
            win.config_path = f"{tmpdir}/config.json"

            win.save_config()

            with open(win.config_path, "r") as fh:
                conf = json.load(fh)
        self.assertEqual(conf["monitor_hw"], "bluez_output.AA_BB_CC_DD_EE_FF.1")
        self.assertEqual(conf["stream_hw"], "alsa_output.speakers")

    def test_recover_unclean_runtime_state_resets_when_pid_is_stale(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        with tempfile.TemporaryDirectory() as tmpdir:
            win._runtime_pid_path = f"{tmpdir}/runtime.pid"
            with open(win._runtime_pid_path, "w") as fh:
                fh.write("424242")
            win._pid_is_alive = lambda pid: False

            win._recover_unclean_runtime_state()

        self.assertEqual(win.runtime.reset_calls, 1)

    def test_recover_unclean_runtime_state_skips_live_pid(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        with tempfile.TemporaryDirectory() as tmpdir:
            win._runtime_pid_path = f"{tmpdir}/runtime.pid"
            with open(win._runtime_pid_path, "w") as fh:
                fh.write("424242")
            win._pid_is_alive = lambda pid: True

            win._recover_unclean_runtime_state()

        self.assertEqual(win.runtime.reset_calls, 0)

    def test_sync_mic_picker_autoselects_once_during_initial_resolution(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.engine = type("Engine", (), {"get_default_source": lambda self: "mic_b"})()
        win.schedule_save = lambda: setattr(win, "_saved", True)
        win.mic_in_combo = QComboBox()
        mics = [
            SimpleNamespace(name="mic_a", description="Mic A"),
            SimpleNamespace(name="mic_b", description="Mic B"),
        ]

        win._sync_mic_picker(mics, default_src="mic_b")

        self.assertEqual(win.selected_mic, "mic_b")
        self.assertTrue(win._mic_selection_initialized)
        self.assertEqual(win.runtime.selected_mic_calls, ["mic_b"])

    def test_sync_mic_picker_does_not_promote_new_hotplug_after_initial_lock(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.engine = type("Engine", (), {"get_default_source": lambda self: "usb_mic"})()
        win.schedule_save = lambda: setattr(win, "_saved", True)
        win.selected_mic = "mic_a"
        win._mic_selection_initialized = True
        win.mic_in_combo = QComboBox()
        mics = [
            SimpleNamespace(name="usb_mic", description="USB Mic"),
        ]

        win._sync_mic_picker(mics, default_src="usb_mic")

        self.assertEqual(win.selected_mic, "mic_a")
        self.assertEqual(win.runtime.selected_mic_calls, [])
        self.assertFalse(win.__dict__.get("_saved", False))

    def test_sync_mic_picker_keeps_saved_selection_during_startup_if_missing(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.engine = type("Engine", (), {"get_default_source": lambda self: "usb_mic"})()
        win.schedule_save = lambda: setattr(win, "_saved", True)
        win.selected_mic = "mic_a"
        win._mic_selection_initialized = False
        win.mic_in_combo = QComboBox()
        mics = [
            SimpleNamespace(name="usb_mic", description="USB Mic"),
        ]

        win._sync_mic_picker(mics, default_src="usb_mic")

        self.assertEqual(win.selected_mic, "mic_a")
        self.assertTrue(win._mic_selection_initialized)
        self.assertEqual(win.runtime.selected_mic_calls, [])
        self.assertFalse(win.__dict__.get("_saved", False))

    def test_set_mix_output_target_uses_sync_runtime_when_requested(self):
        win = self._window()
        win.runtime = _FakeRuntime()

        win._set_mix_output_target(
            "Monitor",
            "bluez_output.headset",
            persist=False,
            update_combo=False,
            sync_runtime=True,
        )

        self.assertEqual(win.runtime.mix_route_sync_calls, [("Monitor", "bluez_output.headset")])
        self.assertEqual(win.runtime.mix_route_calls, [])

    def test_channel_strip_centers_vertical_sliders_with_mute_buttons(self):
        strip = ChannelStrip("1", "mic", "Digital Microphone", "Microphone", "🎤", engine=None)

        strip.apply_scale(200, 140)
        strip.show()
        self._app.processEvents()

        self.assertLessEqual(
            abs(strip.mon_slider.geometry().center().x() - strip.mon_mute.geometry().center().x()),
            1,
        )
        self.assertLessEqual(
            abs(strip.str_slider.geometry().center().x() - strip.str_mute.geometry().center().x()),
            1,
        )


if __name__ == "__main__":
    unittest.main()
