import json
import tempfile
import unittest

from main import WaveLinuxWindow


class _DummyCombo:
    def __init__(self, value=None):
        self._value = value

    def currentData(self):
        return self._value


class _FakeRuntime:
    def __init__(self):
        self.calls = []
        self.reset_calls = 0

    def sync_persistent_state(self, **kwargs):
        self.calls.append(kwargs)

    def full_audio_reset_sync(self):
        self.reset_calls += 1


class WaveLinuxMainMixPersistenceTests(unittest.TestCase):
    def _window(self):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        win.selected_mic = None
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


if __name__ == "__main__":
    unittest.main()
