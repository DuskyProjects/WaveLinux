import json
import tempfile
import unittest

from main import WaveLinuxWindow
from pipewire_engine import PipeWireEngine


class _DummyCombo:
    def __init__(self, value=None, items=None):
        self._value = value
        self._items = list(items or [])

    def currentData(self):
        return self._value

    def blockSignals(self, _flag):
        return

    def findData(self, value):
        try:
            return self._items.index(value)
        except ValueError:
            return -1

    def setCurrentIndex(self, index):
        if 0 <= index < len(self._items):
            self._value = self._items[index]


class _DummyLabel:
    def __init__(self):
        self._text = ""

    def setText(self, text):
        self._text = str(text)

    def text(self):
        return self._text


class _FakeRuntime:
    def __init__(self):
        self.ensure_virtual_channel_calls = []
        self.remove_virtual_channel_calls = []
        self.ensure_output_mix_calls = []
        self.set_selected_mic_calls = []
        self.set_mix_hardware_calls = []
        self.sync_calls = []

    def ensure_virtual_channel_sync(self, name):
        self.ensure_virtual_channel_calls.append(name)
        return name

    def remove_virtual_channel_sync(self, sink_name):
        self.remove_virtual_channel_calls.append(sink_name)

    def ensure_output_mix_sync(self, mix_name):
        self.ensure_output_mix_calls.append(mix_name)

    def set_selected_mic(self, node_name):
        self.set_selected_mic_calls.append(node_name)

    def set_mix_hardware_route(self, mix_name, sink_name):
        self.set_mix_hardware_calls.append((mix_name, sink_name))

    def sync_persistent_state(self, **kwargs):
        self.sync_calls.append(kwargs)


class _FakeEngine:
    def get_default_sink(self):
        return "alsa_output.default"

    def get_default_source(self):
        return "usb_mic"


class WaveLinuxMainScenesTests(unittest.TestCase):
    def _window(self):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        win.selected_mic = "mic"
        win.submix_state = {}
        win.active_effects = {}
        win.effect_params = {}
        win.app_routing = {}
        win.app_volumes = {}
        win.virtual_channels = []
        win.scenes = {}
        win.hidden_nodes = set()
        win.channel_order = []
        win.app_last_seen = {}
        win.app_display_names = {}
        win.app_prune_days = 14
        win.forgotten_apps = set()
        win._desired_mix_hw = {"Monitor": None, "Stream": None}
        win.mon_out_combo = _DummyCombo(None, [None, "alsa_output.default", "alsa_output.headphones"])
        win.str_out_combo = _DummyCombo(None, [None, "alsa_output.stream"])
        win.engine = _FakeEngine()
        win.runtime = _FakeRuntime()
        win.status_lbl = _DummyLabel()
        win.schedule_save = lambda: setattr(win, "_scheduled_save", True)
        win._refresh_hidden_list = lambda: None
        win._refresh_advanced_tab = lambda: None
        win._refresh_scenes_tab = lambda *args, **kwargs: None
        win._refresh = lambda: None
        return win

    def test_capture_scene_snapshot_includes_routing_levels_and_fx(self):
        win = self._window()
        win.virtual_channels = ["Music", "Voice Chat"]
        win.channel_order = ["wavelinux_music", "wavelinux_voice_chat"]
        win.submix_state = {
            "wavelinux_music_Monitor": {"vol": 0.4, "mute": False},
            "wavelinux_music_linked": True,
        }
        win.app_routing = {"com.test.player": "wavelinux_music"}
        win.app_volumes = {"com.test.player": 0.35}
        win.active_effects = {"mic": ["rnnoise", "limiter"]}
        win.effect_params = {"mic": {"rnnoise": {"wet": 0.8}}}
        win._desired_mix_hw["Monitor"] = "alsa_output.headphones"

        snapshot = win._capture_scene_snapshot()

        self.assertEqual(snapshot["selected_mic"], "mic")
        self.assertEqual(snapshot["monitor_hw"], "alsa_output.headphones")
        self.assertEqual(snapshot["virtual_channels"], ["Music", "Voice Chat"])
        self.assertEqual(snapshot["channel_order"], ["wavelinux_music", "wavelinux_voice_chat"])
        self.assertEqual(snapshot["submixes"]["wavelinux_music_Monitor"]["vol"], 0.4)
        self.assertTrue(snapshot["submixes"]["wavelinux_music_linked"])
        self.assertEqual(snapshot["app_routing"], {"com.test.player": "wavelinux_music"})
        self.assertEqual(snapshot["app_volumes"], {"com.test.player": 0.35})
        self.assertEqual(snapshot["active_effects"]["mic"], ["rnnoise", "limiter"])
        self.assertEqual(snapshot["effect_params"]["mic"]["rnnoise"]["wet"], 0.8)

    def test_apply_scene_snapshot_updates_runtime_state_and_preserves_extra_virtuals(self):
        win = self._window()
        win.virtual_channels = ["Aux"]
        win.channel_order = ["wavelinux_aux"]
        win.submix_state = {"wavelinux_aux_Monitor": {"vol": 0.2, "mute": False}}
        win.active_effects = {"wavelinux_aux": ["eq"]}
        win.effect_params = {"wavelinux_aux": {"eq": {"gain": 1.0}}}
        snapshot = {
            "selected_mic": "usb_mic",
            "monitor_hw": "alsa_output.headphones",
            "stream_hw": "alsa_output.stream",
            "virtual_channels": ["Music", "Voice Chat"],
            "channel_order": ["wavelinux_music", "wavelinux_voice_chat"],
            "submixes": {
                "wavelinux_music_Monitor": {"vol": 0.7, "mute": True},
                "wavelinux_voice_chat_Stream": {"vol": 0.5, "mute": False},
            },
            "app_routing": {"com.test.player": "wavelinux_music"},
            "app_volumes": {"com.test.player": 0.25},
            "active_effects": {"usb_mic": ["rnnoise"]},
            "effect_params": {"usb_mic": {"rnnoise": {"wet": 0.9}}},
        }

        applied = win._apply_scene_snapshot(snapshot, scene_name="Streaming")

        self.assertTrue(applied)
        self.assertEqual(win.selected_mic, "usb_mic")
        self.assertEqual(win.virtual_channels, ["Music", "Voice Chat", "Aux"])
        self.assertEqual(
            win.channel_order,
            ["wavelinux_music", "wavelinux_voice_chat", "wavelinux_aux"],
        )
        self.assertEqual(win.submix_state["wavelinux_music_Monitor"]["vol"], 0.7)
        self.assertEqual(win.submix_state["wavelinux_aux_Monitor"]["vol"], 0.2)
        self.assertEqual(win.active_effects["usb_mic"], ["rnnoise"])
        self.assertEqual(win.effect_params["usb_mic"]["rnnoise"]["wet"], 0.9)
        self.assertEqual(win.app_routing, {"com.test.player": "wavelinux_music"})
        self.assertEqual(win.app_volumes, {"com.test.player": 0.25})
        self.assertEqual(
            win.runtime.ensure_virtual_channel_calls,
            ["Music", "Voice Chat"],
        )
        self.assertEqual(win.runtime.set_selected_mic_calls, ["usb_mic"])
        self.assertEqual(
            win.runtime.set_mix_hardware_calls,
            [
                ("Monitor", "alsa_output.headphones"),
                ("Stream", "alsa_output.stream"),
            ],
        )
        self.assertEqual(len(win.runtime.sync_calls), 1)
        self.assertEqual(win.runtime.sync_calls[0]["app_routing"], {"com.test.player": "wavelinux_music"})
        self.assertEqual(win.runtime.sync_calls[0]["app_volumes"], {"com.test.player": 0.25})
        self.assertTrue(getattr(win, "_scheduled_save", False))
        self.assertEqual(win.status_lbl.text(), "Applied Streaming")

    def test_save_config_persists_scenes(self):
        win = self._window()
        win.scenes = {
            "Streaming": {
                "saved_at": 123,
                "selected_mic": "usb_mic",
                "monitor_hw": "alsa_output.headphones",
                "stream_hw": None,
                "virtual_channels": ["Music"],
                "channel_order": ["wavelinux_music"],
                "submixes": {"wavelinux_music_Monitor": {"vol": 0.6, "mute": False}},
                "app_routing": {"com.test.player": "wavelinux_music"},
                "app_volumes": {"com.test.player": 0.4},
                "active_effects": {"usb_mic": ["rnnoise"]},
                "effect_params": {"usb_mic": {"rnnoise": {"wet": 0.9}}},
            }
        }
        with tempfile.TemporaryDirectory() as tmpdir:
            win.config_path = f"{tmpdir}/config.json"

            win.save_config()

            with open(win.config_path, "r") as fh:
                conf = json.load(fh)
        self.assertIn("scenes", conf)
        self.assertIn("Streaming", conf["scenes"])
        self.assertEqual(conf["scenes"]["Streaming"]["selected_mic"], "usb_mic")
        self.assertEqual(conf["scenes"]["Streaming"]["app_volumes"]["com.test.player"], 0.4)

    def test_apply_config_dict_replaces_virtual_channels_and_restores_scene_library(self):
        win = self._window()
        win.virtual_channels = ["Old Virtual"]
        payload = {
            "monitor_hw": "alsa_output.headphones",
            "stream_hw": "alsa_output.stream",
            "channels": ["Music"],
            "scenes": {
                "Desk": {
                    "selected_mic": "usb_mic",
                    "virtual_channels": ["Music"],
                    "channel_order": ["wavelinux_music"],
                    "submixes": {"wavelinux_music_Monitor": {"vol": 0.8, "mute": False}},
                    "app_routing": {"com.test.player": "wavelinux_music"},
                    "app_volumes": {"com.test.player": 0.45},
                    "active_effects": {"usb_mic": ["rnnoise"]},
                    "effect_params": {"usb_mic": {"rnnoise": {"wet": 0.6}}},
                }
            },
            "selected_mic": "usb_mic",
            "submixes": {"wavelinux_music_Monitor": {"vol": 0.8, "mute": False}},
            "app_routing": {"com.test.player": "wavelinux_music"},
            "app_volumes": {"com.test.player": 0.45},
            "channel_order": ["wavelinux_music"],
            "active_effects": {"usb_mic": ["rnnoise"]},
            "effect_params": {"usb_mic": {"rnnoise": {"wet": 0.6}}},
        }

        win._apply_config_dict(payload, remove_missing_virtuals=True)

        self.assertEqual(win.virtual_channels, ["Music"])
        self.assertIn("Desk", win.scenes)
        self.assertEqual(win.selected_mic, "usb_mic")
        self.assertEqual(win.app_volumes, {"com.test.player": 0.45})
        self.assertEqual(win._desired_mix_hw["Monitor"], "alsa_output.headphones")
        self.assertEqual(win._desired_mix_hw["Stream"], "alsa_output.stream")
        self.assertEqual(win.runtime.ensure_output_mix_calls, ["Monitor", "Stream"])
        self.assertEqual(win.runtime.ensure_virtual_channel_calls, ["Music"])
        self.assertEqual(
            win.runtime.remove_virtual_channel_calls,
            [f"wavelinux_{PipeWireEngine._sanitize_channel_name('Old Virtual')[1]}"],
        )
        self.assertEqual(win.runtime.set_selected_mic_calls, ["usb_mic"])
        self.assertEqual(
            win.runtime.set_mix_hardware_calls,
            [
                ("Monitor", "alsa_output.headphones"),
                ("Stream", "alsa_output.stream"),
            ],
        )
        self.assertEqual(len(win.runtime.sync_calls), 1)
        self.assertEqual(win.runtime.sync_calls[0]["app_volumes"], {"com.test.player": 0.45})

    def test_apply_quick_start_template_sets_channels_mic_and_default_fx(self):
        win = self._window()
        win.selected_mic = None
        win._runtime_view_state = None

        applied = win._apply_quick_start_template("streaming_obs")

        self.assertTrue(applied)
        self.assertEqual(
            win.virtual_channels,
            ["Game", "Music", "Browser", "Voice Chat", "Alerts"],
        )
        self.assertEqual(
            win.channel_order,
            [
                "wavelinux_game",
                "wavelinux_music",
                "wavelinux_browser",
                "wavelinux_voice_chat",
                "wavelinux_alerts",
            ],
        )
        self.assertEqual(win.selected_mic, "usb_mic")
        self.assertEqual(
            win.active_effects["usb_mic"],
            ["rnnoise", "compressor", "limiter"],
        )
        self.assertEqual(win.runtime.set_selected_mic_calls, ["usb_mic"])
        self.assertEqual(
            win.runtime.ensure_virtual_channel_calls,
            ["Game", "Music", "Browser", "Voice Chat", "Alerts"],
        )
        self.assertEqual(
            win.runtime.set_mix_hardware_calls,
            [("Monitor", "alsa_output.default")],
        )
        self.assertTrue(win._onboarding_completed)
        self.assertEqual(win._selected_setup_template, "streaming_obs")


if __name__ == "__main__":
    unittest.main()
