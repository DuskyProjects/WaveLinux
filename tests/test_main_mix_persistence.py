import os
import json
import tempfile
import time
import unittest
from unittest import mock

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt6.QtCore import QProcess
from PyQt6.QtWidgets import QApplication, QComboBox, QDialog, QWidget
from types import SimpleNamespace

from main import ChannelStrip, FXSelectionDialog, MixerStripMetrics, WaveLinuxWindow


class _DummyCombo:
    def __init__(self, value=None):
        self._value = value

    def currentData(self):
        return self._value


class _FakeTimer:
    def __init__(self):
        self.start_calls = 0
        self.active = False
        self.interval = None

    def start(self, interval=None):
        if interval is not None:
            self.interval = int(interval)
        self.start_calls += 1
        self.active = True

    def stop(self):
        self.active = False

    def isActive(self):
        return self.active

    def setInterval(self, value):
        self.interval = int(value)


class _FakeDialog:
    def __init__(self, visible=True):
        self._visible = visible
        self.show_calls = 0
        self.raise_calls = 0

    def isVisible(self):
        return self._visible

    def show(self):
        self.show_calls += 1
        self._visible = True

    def raise_(self):
        self.raise_calls += 1


class _FakeEvent:
    def __init__(self):
        self.accepted = False
        self.ignored = False

    def accept(self):
        self.accepted = True

    def ignore(self):
        self.ignored = True


class _FakeTabs:
    def __init__(self, names, index=0):
        self._names = list(names)
        self._index = index

    def currentIndex(self):
        return self._index

    def tabText(self, index):
        return self._names[index]


class _FakeRuntime:
    def __init__(self):
        self.calls = []
        self.reset_calls = 0
        self.selected_mic_calls = []
        self.mix_route_calls = []
        self.mix_route_sync_calls = []
        self.submix_calls = []
        self.source_volume_calls = []
        self.mix_volume_calls = []
        self.ensure_output_mix_calls = []
        self.ensure_virtual_channel_calls = []
        self.remove_virtual_channel_calls = []
        self.refresh_calls = []

    def sync_persistent_state(self, **kwargs):
        self.calls.append(kwargs)

    def full_audio_reset_sync(self):
        self.reset_calls += 1

    def set_selected_mic(self, node_name):
        self.selected_mic_calls.append(node_name)

    def set_mix_hardware_route(self, mix_name, sink_name):
        self.mix_route_calls.append((mix_name, sink_name))

    def set_mix_hardware_route_sync(self, mix_name, sink_name, *, refresh=True):
        self.mix_route_sync_calls.append((mix_name, sink_name, refresh))

    def set_submix_state(self, node_id, mix_name, volume, mute, node_name=""):
        self.submix_calls.append((node_id, mix_name, volume, mute, node_name))

    def set_source_volume(self, node_name, volume):
        self.source_volume_calls.append((node_name, volume))

    def set_mix_volume(self, mix_name, volume):
        self.mix_volume_calls.append((mix_name, volume))

    def ensure_output_mix_sync(self, mix_name, *, refresh=True):
        self.ensure_output_mix_calls.append(mix_name)

    def ensure_virtual_channel_sync(self, name, *, refresh=True):
        self.ensure_virtual_channel_calls.append(name)

    def remove_virtual_channel_sync(self, sink_name, *, refresh=True):
        self.remove_virtual_channel_calls.append(sink_name)

    def refresh_now(self, reason=""):
        self.refresh_calls.append(reason)


class _FakeSignal:
    def __init__(self):
        self._callbacks = []

    def connect(self, callback):
        self._callbacks.append(callback)

    def disconnect(self, callback):
        self._callbacks = [cb for cb in self._callbacks if cb != callback]

    def emit(self, *args, **kwargs):
        for callback in list(self._callbacks):
            callback(*args, **kwargs)


class _FakeFxEngine:
    def get_available_effects(self):
        return [{
            "id": "limiter",
            "name": "Limiter",
            "desc": "Protects against clipping",
            "icon": "✨",
        }]

    def effect_available(self, effect_id):
        return effect_id == "limiter"

    def get_effect_params(self, effect_id):
        return []

    def get_effect_help(self, effect_id):
        return ""

    def get_effect_presets(self, effect_id):
        return []


class _FakeFxRuntime:
    def __init__(self):
        self.fx_status_changed = _FakeSignal()
        self.set_calls = []
        self.clear_calls = []

    def set_channel_fx(self, node_name, capture_target, effects, params_map):
        self.set_calls.append((node_name, capture_target, list(effects), dict(params_map or {})))
        return 1

    def clear_channel_fx(self, node_name):
        self.clear_calls.append(node_name)
        return 1

    def fx_status_for(self, node_name):
        return SimpleNamespace(state="idle", message="", generation=0)


class WaveLinuxMainMixPersistenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._app = QApplication.instance() or QApplication([])

    def _window(self):
        win = WaveLinuxWindow.__new__(WaveLinuxWindow)
        win._shutting_down = False
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
        win._desired_mix_volumes = {"Monitor": 1.0, "Stream": 1.0}
        win.mon_out_combo = _DummyCombo(None)
        win.str_out_combo = _DummyCombo(None)
        win._runtime_pid_path = ""
        win.channel_widgets = {}
        win.app_widgets = {}
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

    def test_sync_runtime_persistent_state_can_request_immediate_reconcile(self):
        win = self._window()
        win.runtime = _FakeRuntime()

        win._sync_runtime_persistent_state(immediate=True)

        self.assertEqual(len(win.runtime.calls), 1)
        self.assertTrue(win.runtime.calls[0]["apply_now"])

    def test_startup_audio_ready_requires_fx_default_source_cutover(self):
        win = self._window()
        win.selected_mic = "alsa_input.usb_mic"
        win.active_effects = {"alsa_input.usb_mic": ["limiter"]}
        win.engine = type(
            "Engine",
            (),
            {"get_channel_fx_source": lambda self, node_name: "output.wavelinux.fx.usb_mic.source"},
        )()

        win._runtime_view_state = SimpleNamespace(
            mic_inputs=[SimpleNamespace(name="alsa_input.usb_mic")],
            default_source="alsa_input.usb_mic",
            health={"alsa_input.usb_mic": "default_source_mismatch"},
        )
        self.assertFalse(win._startup_audio_ready())

        win._runtime_view_state = SimpleNamespace(
            mic_inputs=[SimpleNamespace(name="alsa_input.usb_mic")],
            default_source="output.wavelinux.fx.usb_mic.source",
            health={},
        )
        self.assertTrue(win._startup_audio_ready())

    def test_startup_audio_ready_accepts_raw_selected_mic_without_fx(self):
        win = self._window()
        win.selected_mic = "alsa_input.internal"
        win.active_effects = {}
        win._runtime_view_state = SimpleNamespace(
            mic_inputs=[SimpleNamespace(name="alsa_input.internal")],
            default_source="alsa_input.internal",
            health={},
        )

        self.assertTrue(win._startup_audio_ready())

    def test_normalize_effect_request_keeps_rnnoise_for_internal_alsa_mics(self):
        win = self._window()

        wanted, normalized = win._normalize_effect_request_for_node(
            "alsa_input.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Mic1__source",
            ["rnnoise", "limiter"],
            {"rnnoise": {"VAD Threshold (%)": 75.0}},
        )

        self.assertEqual(wanted, ["rnnoise", "limiter"])
        self.assertEqual(normalized["rnnoise"]["VAD Threshold (%)"], 75.0)

    def test_normalize_effect_request_keeps_rnnoise_for_usb_mics(self):
        win = self._window()

        wanted, normalized = win._normalize_effect_request_for_node(
            "alsa_input.usb-DJI_Technology_Co.__Ltd._Wireless_Mic_Rx_XSP12345678B-01.iec958-stereo",
            ["rnnoise", "limiter"],
            {"rnnoise": {"VAD Threshold (%)": 75.0}},
        )

        self.assertEqual(wanted, ["rnnoise", "limiter"])
        self.assertEqual(normalized["rnnoise"]["VAD Threshold (%)"], 75.0)

    def test_normalize_loaded_effect_state_keeps_internal_rnnoise(self):
        win = self._window()
        internal = "alsa_input.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Mic1__source"
        usb = "alsa_input.usb-DJI_Technology_Co.__Ltd._Wireless_Mic_Rx_XSP12345678B-01.iec958-stereo"
        win.active_effects = {
            internal: ["rnnoise", "limiter"],
            usb: ["rnnoise", "limiter"],
        }
        win.effect_params = {
            internal: {"rnnoise": {"VAD Threshold (%)": 75.0}},
            usb: {"rnnoise": {"VAD Threshold (%)": 75.0}},
        }

        win._normalize_loaded_effect_state()

        self.assertEqual(win.active_effects[internal], ["rnnoise", "limiter"])
        self.assertEqual(win.effect_params[internal]["rnnoise"]["VAD Threshold (%)"], 75.0)
        self.assertEqual(win.active_effects[usb], ["rnnoise", "limiter"])
        self.assertEqual(win.effect_params[usb]["rnnoise"]["VAD Threshold (%)"], 75.0)

    def test_channel_strip_on_peak_skips_duplicate_values(self):
        strip = ChannelStrip(
            "0",
            "wavelinux_music",
            "Music",
            "Virtual",
            "🎵",
            engine=None,
        )
        self.addCleanup(strip.deleteLater)

        with mock.patch.object(strip.peak_bar, "setValue") as set_value:
            strip.on_peak(0.5)
            strip.on_peak(0.5)
            strip.on_peak(0.6)

        self.assertEqual(set_value.call_args_list, [mock.call(500), mock.call(600)])

    def test_fx_dialog_done_closes_even_when_runtime_inflight(self):
        parent = QWidget()
        parent.active_effects = {}
        parent.effect_params = {}
        parent.schedule_save = lambda: None
        runtime = _FakeFxRuntime()
        dialog = FXSelectionDialog(
            "1",
            "alsa_input.internal",
            "alsa_input.internal",
            _FakeFxEngine(),
            runtime=runtime,
            parent=parent,
        )
        self.addCleanup(dialog.deleteLater)
        dialog._runtime_inflight = True

        with mock.patch.object(dialog, "accept") as accept:
            dialog._on_done()

        accept.assert_called_once_with()

    def test_fx_dialog_commit_live_patch_queues_changes_while_runtime_inflight(self):
        parent = QWidget()
        parent.active_effects = {}
        parent.effect_params = {}
        parent.schedule_save = lambda: None
        runtime = _FakeFxRuntime()
        dialog = FXSelectionDialog(
            "1",
            "alsa_input.internal",
            "alsa_input.internal",
            _FakeFxEngine(),
            runtime=runtime,
            parent=parent,
        )
        self.addCleanup(dialog.deleteLater)
        dialog._runtime_inflight = True
        dialog._toggle_btns["limiter"].setChecked(True)

        dialog._commit_live_patch()

        self.assertEqual(
            runtime.set_calls,
            [("alsa_input.internal", "alsa_input.internal", ["limiter"], {})],
        )

    def test_save_config_persists_desired_mix_routes_when_combo_empty(self):
        win = self._window()
        win._desired_mix_hw["Monitor"] = "bluez_output.AA_BB_CC_DD_EE_FF.1"
        win._desired_mix_hw["Stream"] = "alsa_output.speakers"
        win._desired_mix_volumes["Monitor"] = 0.73
        win._desired_mix_volumes["Stream"] = 0.41
        with tempfile.TemporaryDirectory() as tmpdir:
            win.config_path = f"{tmpdir}/config.json"

            win.save_config()

            with open(win.config_path, "r") as fh:
                conf = json.load(fh)
        self.assertEqual(conf["monitor_hw"], "bluez_output.AA_BB_CC_DD_EE_FF.1")
        self.assertEqual(conf["stream_hw"], "alsa_output.speakers")
        self.assertEqual(conf["monitor_mix_volume"], 0.73)
        self.assertEqual(conf["stream_mix_volume"], 0.41)

    def test_save_config_flushes_pending_stream_strip_volume_before_serializing(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        strip = ChannelStrip(
            "11",
            "wavelinux_music",
            "Music",
            "Virtual",
            "🎵",
            engine=None,
        )
        strip._main_win = win
        win.channel_widgets = {"11": strip}

        strip.str_slider.setValue(37)

        with tempfile.TemporaryDirectory() as tmpdir:
            win.config_path = f"{tmpdir}/config.json"

            win.save_config()

            with open(win.config_path, "r") as fh:
                conf = json.load(fh)

        self.assertEqual(
            conf["submixes"]["wavelinux_music_Stream"],
            {"vol": 0.37, "mute": False},
        )
        self.assertIn(("11", "Stream", 0.37, False, "wavelinux_music"), win.runtime.submix_calls)

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

    def test_sync_mic_picker_falls_back_when_saved_selection_missing_during_initial_resolution(self):
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

        self.assertEqual(win.selected_mic, "usb_mic")
        self.assertTrue(win._mic_selection_initialized)
        self.assertEqual(win.runtime.selected_mic_calls, ["usb_mic"])
        self.assertTrue(win.__dict__.get("_saved", False))

    def test_sync_mic_picker_prefers_runtime_label_for_hardware_mics(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.mic_in_combo = QComboBox()
        mics = [
            SimpleNamespace(
                name="alsa_input.usb_dji",
                description="Wireless Mic Rx Digital Stereo (IEC958)",
                label="Wireless Mic Rx",
            ),
        ]

        win._sync_mic_picker(mics, default_src="alsa_input.usb_dji")

        self.assertEqual(win.mic_in_combo.itemText(0), "Wireless Mic Rx")

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

        self.assertEqual(
            win.runtime.mix_route_sync_calls,
            [("Monitor", "bluez_output.headset", True)],
        )
        self.assertEqual(win.runtime.mix_route_calls, [])

    def test_sorted_input_nodes_keeps_mics_on_far_left(self):
        win = self._window()
        win.channel_order = ["wavelinux_browser", "usb_mic", "wavelinux_music"]
        mic = SimpleNamespace(name="usb_mic", is_mic=True)
        browser = SimpleNamespace(name="wavelinux_browser", is_mic=False)
        music = SimpleNamespace(name="wavelinux_music", is_mic=False)

        ordered = win._sorted_input_nodes([mic], [browser, music])

        self.assertEqual(
            [node.name for node in ordered],
            ["usb_mic", "wavelinux_browser", "wavelinux_music"],
        )

    def test_on_mix_out_change_uses_sync_runtime_for_monitor(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: setattr(win, "_saved", True)

        win._on_mix_out_change("Monitor", "bluez_output.headset")

        self.assertEqual(
            win.runtime.mix_route_sync_calls,
            [("Monitor", "bluez_output.headset", False)],
        )
        self.assertEqual(win.runtime.mix_route_calls, [])
        self.assertTrue(win.__dict__.get("_saved", False))

    def test_on_mix_out_change_schedules_followup_refreshes_for_bluetooth_monitor(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: None
        win._device_settle_refresh_timer = _FakeTimer()
        win._bluetooth_refresh_timer = _FakeTimer()
        win._monitor_route_reassert_timer = _FakeTimer()
        win._monitor_route_bluetooth_reassert_timer = _FakeTimer()
        win._stable_sink_id_for_name = lambda sink_name: "bt:aa_bb_cc_dd_ee_ff"

        win._on_mix_out_change("Monitor", "bluez_output.headset")

        self.assertEqual(win._device_settle_refresh_timer.start_calls, 1)
        self.assertEqual(win._bluetooth_refresh_timer.start_calls, 1)
        self.assertEqual(win._monitor_route_reassert_timer.start_calls, 1)
        self.assertEqual(win._monitor_route_bluetooth_reassert_timer.start_calls, 1)

    def test_refresh_active_settings_tab_only_refreshes_selected_tab(self):
        win = self._window()
        calls = []
        win._startup_preflight = {"deps": {}, "issue_details": []}
        win._settings_tabs = _FakeTabs(["Apps", "Health", "Updates"], index=1)
        win._settings_tab_names = ("Apps", "Health", "Updates")
        win._refresh_system_tab = lambda *args, **kwargs: calls.append("health")
        win._refresh_update_tab = lambda *args, **kwargs: calls.append("updates")
        win._refresh_advanced_tab = lambda *args, **kwargs: calls.append("advanced")

        win._refresh_active_settings_tab(force=True)

        self.assertEqual(calls, ["health"])

    def test_apply_scheduled_runtime_view_refresh_refreshes_only_active_settings_tab(self):
        win = self._window()
        win.settings_dialog = _FakeDialog(visible=True)
        win._settings_tabs = _FakeTabs(["Apps", "Advanced", "Health"], index=1)
        win._settings_tab_names = ("Apps", "Advanced", "Health")
        calls = []
        win._schedule_active_settings_tab_refresh = lambda *args, **kwargs: calls.append("settings")
        win._refresh_runtime_view = lambda: calls.append("runtime")
        win._any_slider_dragging = lambda: False
        win.tray = None

        win._apply_scheduled_runtime_view_refresh()

        self.assertEqual(calls, ["settings", "runtime"])

    def test_close_event_hides_to_tray_when_tray_visible(self):
        win = self._window()
        win._quit_in_progress = False
        win.tray = SimpleNamespace(isVisible=lambda: True)
        calls = []
        win._request_quit_app = lambda: calls.append("quit")
        win.hide = lambda: calls.append("hide")
        event = _FakeEvent()

        win.closeEvent(event)

        self.assertEqual(calls, ["hide"])
        self.assertFalse(event.accepted)
        self.assertTrue(event.ignored)

    def test_request_quit_closes_top_level_dialogs(self):
        win = self._window()
        win._quit_in_progress = False
        win._shutting_down = False
        win._suppress_pactl_events_for = lambda *_: None
        win.status_lbl = SimpleNamespace(setText=lambda *_: None)
        tray_calls = []
        win.tray = SimpleNamespace(hide=lambda: tray_calls.append("tray-hide"))
        win.setEnabled = lambda enabled: tray_calls.append(("enabled", enabled))
        dialog = QDialog()
        self.addCleanup(dialog.deleteLater)
        dialog.show()
        self._app.processEvents()

        with mock.patch("main.QApplication.instance", return_value=SimpleNamespace(topLevelWidgets=lambda: [win, dialog])):
            with mock.patch("main.QTimer.singleShot") as single_shot:
                win._request_quit_app()

        self.assertFalse(dialog.isVisible())
        self.assertEqual(tray_calls, ["tray-hide", ("enabled", False)])
        single_shot.assert_called_once()

    def test_apply_scheduled_runtime_view_refresh_defers_full_refresh_while_pending_ops_exist(self):
        win = self._window()
        win.settings_dialog = _FakeDialog(visible=True)
        win._settings_tabs = _FakeTabs(["Apps"], index=0)
        win._settings_tab_names = ("Apps",)
        win._runtime_view_state = SimpleNamespace(
            pending_operations={"fx:selected": "building"},
            health={},
        )
        timer = _FakeTimer()
        win._runtime_view_refresh_timer = timer
        calls = []
        win._schedule_active_settings_tab_refresh = lambda *args, **kwargs: calls.append("settings")
        win._refresh_runtime_view = lambda: calls.append("runtime")
        win._any_slider_dragging = lambda: False
        win.tray = None

        win._apply_scheduled_runtime_view_refresh()

        self.assertEqual(calls, ["settings"])
        self.assertEqual(timer.start_calls, 1)
        self.assertEqual(timer.interval, 180)

    def test_on_runtime_fx_status_marks_health_stale_without_refreshing_offscreen_health_tab(self):
        win = self._window()
        win.settings_dialog = _FakeDialog(visible=True)
        win._settings_tabs = _FakeTabs(["Apps", "Health"], index=0)
        win._settings_tab_names = ("Apps", "Health")
        win._settings_tab_last_refresh_at = {"Health": time.monotonic()}
        win.status_lbl = SimpleNamespace(setText=lambda *_: None)
        win._refresh_channel_runtime_status = lambda *_: None
        win._schedule_auto_recovery = lambda *_: None
        win._cancel_auto_recovery_timer = lambda *_: None
        win._clear_auto_recovery_state = lambda *_: None
        win._request_runtime_refresh = lambda *_: None
        calls = []
        win._schedule_active_settings_tab_refresh = lambda *args, **kwargs: calls.append("schedule")

        win._on_runtime_fx_status(SimpleNamespace(node_name="mic", state="degraded", message="broken"))

        self.assertEqual(calls, [])
        self.assertNotIn("Health", win._settings_tab_last_refresh_at)

    def test_apply_scheduled_runtime_view_refresh_defers_full_refresh_during_mic_settle(self):
        win = self._window()
        win.settings_dialog = _FakeDialog(visible=True)
        win._settings_tabs = _FakeTabs(["Apps", "Health"], index=0)
        win._settings_tab_names = ("Apps", "Health")
        win._last_selected_mic_change_at = time.monotonic()
        timer = _FakeTimer()
        win._runtime_view_refresh_timer = timer
        calls = []
        win._schedule_active_settings_tab_refresh = lambda *args, **kwargs: calls.append("settings")
        win._apply_lightweight_runtime_view_refresh = lambda: calls.append("light")
        win._refresh_runtime_view = lambda: calls.append("runtime")
        win._any_slider_dragging = lambda: False
        win._runtime_view_state = SimpleNamespace(pending_operations={}, health={})
        win.tray = None

        win._apply_scheduled_runtime_view_refresh()

        self.assertEqual(calls, ["settings", "light"])
        self.assertEqual(timer.start_calls, 1)
        self.assertEqual(timer.interval, 120)

    def test_apply_scheduled_runtime_view_refresh_defers_health_tab_during_mic_settle(self):
        win = self._window()
        win.settings_dialog = _FakeDialog(visible=True)
        win._settings_tabs = _FakeTabs(["Apps", "Health"], index=1)
        win._settings_tab_names = ("Apps", "Health")
        win._last_selected_mic_change_at = time.monotonic()
        timer = _FakeTimer()
        win._runtime_view_refresh_timer = timer
        calls = []
        win._schedule_active_settings_tab_refresh = lambda *args, **kwargs: calls.append("settings")
        win._apply_lightweight_runtime_view_refresh = lambda: calls.append("light")
        win._refresh_runtime_view = lambda: calls.append("runtime")
        win._any_slider_dragging = lambda: False
        win._runtime_view_state = SimpleNamespace(pending_operations={}, health={})
        win.tray = None

        win._apply_scheduled_runtime_view_refresh()

        self.assertEqual(calls, ["settings", "light"])
        self.assertEqual(timer.start_calls, 1)
        self.assertEqual(timer.interval, 120)

    def test_on_runtime_view_state_skips_device_policy_reconcile_during_selected_mic_transition(self):
        win = self._window()
        win.status_lbl = SimpleNamespace(text=lambda: "", setText=lambda *_: None)
        win._selected_mic_change_settling = lambda **kwargs: True
        win.selected_mic = "alsa_input.internal"
        calls = []
        win._reconcile_device_policy = lambda *_: calls.append("reconcile")
        win._schedule_runtime_view_refresh = lambda: calls.append("refresh")
        win._selected_mic_needs_followup_refresh = lambda *_: False
        view = SimpleNamespace(
            health={"alsa_input.internal": "desired_fx_missing"},
            pending_operations={"fx:alsa_input.internal": "building"},
        )

        win._on_runtime_view_state(view)

        self.assertEqual(calls, ["refresh"])

    def test_on_runtime_fx_status_avoids_runtime_refresh_during_selected_mic_settle(self):
        win = self._window()
        win.status_lbl = SimpleNamespace(setText=lambda *_: None)
        win._selected_mic_change_settling = lambda **kwargs: True
        win._refresh_channel_runtime_status = lambda *_: None
        win._schedule_runtime_view_refresh = lambda: setattr(win, "_scheduled", True)
        win._request_runtime_refresh = lambda *_: setattr(win, "_requested", True)
        win._clear_auto_recovery_state = lambda *_: None
        win._cancel_auto_recovery_timer = lambda *_: None
        win._schedule_auto_recovery = lambda *_: None

        win._on_runtime_fx_status(SimpleNamespace(node_name="mic", state="active", message="ok"))

        self.assertTrue(win.__dict__.get("_scheduled", False))
        self.assertFalse(win.__dict__.get("_requested", False))

    def test_on_mic_input_change_requests_runtime_refresh_without_forcing_full_refresh(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: None
        win.mic_in_combo = QComboBox()
        win.mic_in_combo.addItem("Mic A", "mic_a")
        win.mic_in_combo.addItem("Mic B", "mic_b")
        win.selected_mic = "mic_a"
        refresh_reasons = []
        win._request_runtime_refresh = lambda reason="": refresh_reasons.append(reason)
        win._refresh = lambda: setattr(win, "_full_refresh_called", True)

        win._on_mic_input_change(1)

        self.assertEqual(win.selected_mic, "mic_b")
        self.assertEqual(win.runtime.selected_mic_calls, ["mic_b"])
        self.assertEqual(refresh_reasons, ["selected-mic-change"])
        self.assertFalse(win.__dict__.get("_full_refresh_called", False))

    def test_stress_set_selected_mic_requests_runtime_refresh_without_forcing_full_refresh(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: None
        refresh_reasons = []
        win._request_runtime_refresh = lambda reason="": refresh_reasons.append(reason)
        win._refresh = lambda: setattr(win, "_full_refresh_called", True)

        summary = win._stress_set_selected_mic("mic_b", persist=False)

        self.assertEqual(summary, {"selected_mic": "mic_b", "requested": True})
        self.assertEqual(win.selected_mic, "mic_b")
        self.assertEqual(win.runtime.selected_mic_calls, ["mic_b"])
        self.assertEqual(refresh_reasons, ["selected-mic-change"])
        self.assertFalse(win.__dict__.get("_full_refresh_called", False))

    def test_refresh_advanced_tab_does_not_refresh_update_tab(self):
        win = self._window()
        win.prune_spin = SimpleNamespace(
            blockSignals=lambda *_: None,
            setValue=lambda *_: None,
        )
        win.autostart_check = SimpleNamespace(
            blockSignals=lambda *_: None,
            setChecked=lambda *_: None,
        )
        win.restore_forgotten_btn = SimpleNamespace(
            setEnabled=lambda *_: None,
            setText=lambda *_: None,
        )
        win.recover_degraded_btn = SimpleNamespace(
            setEnabled=lambda *_: None,
            setText=lambda *_: None,
        )
        win.is_autostart_enabled = lambda: False
        win._runtime_degraded_channels = lambda: []
        calls = []
        win._refresh_update_tab = lambda: calls.append("updates")

        win._refresh_advanced_tab()

        self.assertEqual(calls, [])

    def test_schedule_active_settings_tab_refresh_starts_timer_when_stale(self):
        win = self._window()
        win.settings_dialog = _FakeDialog(visible=True)
        win._settings_tabs = _FakeTabs(["Health"], index=0)
        win._settings_tab_names = ("Health",)
        win._settings_tab_refresh_timer = _FakeTimer()
        win._settings_tab_last_refresh_at = {}

        win._schedule_active_settings_tab_refresh(force=False)

        self.assertEqual(win._pending_settings_tab_refresh, "Health")
        self.assertEqual(win._settings_tab_refresh_timer.start_calls, 1)

    def test_schedule_active_settings_tab_refresh_skips_fresh_cache(self):
        win = self._window()
        win.settings_dialog = _FakeDialog(visible=True)
        win._settings_tabs = _FakeTabs(["Health"], index=0)
        win._settings_tab_names = ("Health",)
        win._settings_tab_refresh_timer = _FakeTimer()
        win._settings_tab_last_refresh_at = {"Health": time.monotonic()}

        win._schedule_active_settings_tab_refresh(force=False)

        self.assertEqual(win._settings_tab_refresh_timer.start_calls, 0)

    def test_apply_scheduled_settings_tab_refresh_refreshes_only_matching_active_tab(self):
        win = self._window()
        win.settings_dialog = _FakeDialog(visible=True)
        win._settings_tabs = _FakeTabs(["Health", "Updates"], index=0)
        win._settings_tab_names = ("Health", "Updates")
        win._startup_preflight = {"deps": {}, "issue_details": []}
        win._settings_tab_last_refresh_at = {}
        win._pending_settings_tab_refresh = "Health"
        calls = []
        win._refresh_system_tab = lambda *args, **kwargs: calls.append("health")

        win._apply_scheduled_settings_tab_refresh()

        self.assertEqual(calls, ["health"])
        self.assertEqual(win._pending_settings_tab_refresh, "")

    def test_on_mix_out_change_schedules_only_settle_refresh_for_non_bluetooth_monitor(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: None
        win._device_settle_refresh_timer = _FakeTimer()
        win._bluetooth_refresh_timer = _FakeTimer()
        win._monitor_route_reassert_timer = _FakeTimer()
        win._monitor_route_bluetooth_reassert_timer = _FakeTimer()
        win._stable_sink_id_for_name = lambda sink_name: "id:alsa_output.speakers"

        win._on_mix_out_change("Monitor", "alsa_output.speakers")

        self.assertEqual(win._device_settle_refresh_timer.start_calls, 1)
        self.assertEqual(win._bluetooth_refresh_timer.start_calls, 0)
        self.assertEqual(win._monitor_route_reassert_timer.start_calls, 1)
        self.assertEqual(win._monitor_route_bluetooth_reassert_timer.start_calls, 0)

    def test_reassert_persistent_state_after_monitor_switch_requests_refresh_only(self):
        win = self._window()
        calls = []
        win._request_runtime_refresh = lambda reason="": calls.append(("refresh", reason))

        win._reassert_persistent_state_after_monitor_switch("monitor-route-reassert")

        self.assertEqual(
            calls,
            [
                ("refresh", "monitor-route-reassert"),
            ],
        )

    def test_handle_bluetooth_settle_refresh_restarts_event_subscriber_and_reasserts_profile(self):
        win = self._window()
        calls = []
        win._bluetooth_profile_reassert_retries = 2
        win._bluetooth_refresh_timer = _FakeTimer()
        win._restart_event_subscriber_if_needed = lambda: calls.append("restart")
        win._reassert_bluetooth_playback_profile = lambda: (calls.append("reassert"), (True, True))[1]
        win._request_runtime_refresh = lambda reason="": calls.append(("refresh", reason))

        win._handle_bluetooth_settle_refresh()

        self.assertEqual(
            calls,
            [
                "restart",
                "reassert",
                ("refresh", "bluetooth-settle"),
            ],
        )
        self.assertEqual(win._bluetooth_refresh_timer.start_calls, 1)
        self.assertEqual(win._bluetooth_profile_reassert_retries, 1)

    def test_on_event_proc_error_schedules_audio_server_recovery(self):
        win = self._window()
        win._event_proc_restart_timer = _FakeTimer()
        win._event_refresh_timer = _FakeTimer()
        win._bluetooth_refresh_timer = _FakeTimer()

        win._on_event_proc_error("boom")

        self.assertEqual(win._event_proc_restart_timer.start_calls, 1)
        self.assertEqual(win._event_refresh_timer.start_calls, 1)
        self.assertEqual(win._bluetooth_refresh_timer.start_calls, 1)
        self.assertEqual(win._bluetooth_profile_reassert_retries, 6)

    def test_restart_event_subscriber_if_needed_starts_new_subscriber_when_not_running(self):
        win = self._window()
        calls = []
        win._event_proc = SimpleNamespace(
            state=lambda: QProcess.ProcessState.NotRunning,
            deleteLater=lambda: calls.append("delete"),
        )
        win._start_event_subscriber = lambda: calls.append("start")

        win._restart_event_subscriber_if_needed()

        self.assertEqual(calls, ["delete", "start"])

    def test_reassert_bluetooth_playback_profile_switches_bad_profile_for_non_bluetooth_mic(self):
        win = self._window()
        calls = []
        win.selected_mic = "alsa_input.usb_dji"
        win.engine = SimpleNamespace(
            lock_bluetooth_to_a2dp=lambda: calls.append("lock"),
            list_cards=lambda: [
                {
                    "name": "bluez_card.AA_BB_CC_DD_EE_FF",
                    "active_profile": "headset-head-unit",
                    "profiles": [
                        {"name": "headset-head-unit", "available": True},
                        {"name": "a2dp-sink", "available": True},
                    ],
                }
            ],
            set_card_profile=lambda card_name, profile_name: calls.append(
                ("set_profile", card_name, profile_name)
            ) or True,
        )

        changed, retry_needed = win._reassert_bluetooth_playback_profile()

        self.assertTrue(changed)
        self.assertTrue(retry_needed)
        self.assertEqual(
            calls,
            [
                "lock",
                ("set_profile", "bluez_card.AA_BB_CC_DD_EE_FF", "a2dp-sink"),
            ],
        )

    def test_reassert_bluetooth_playback_profile_does_not_force_a2dp_for_bluetooth_mic(self):
        win = self._window()
        calls = []
        win.selected_mic = "bluez_input.AA:BB:CC:DD:EE:FF"
        win.engine = SimpleNamespace(
            lock_bluetooth_to_a2dp=lambda: calls.append("lock"),
            list_cards=lambda: [
                {
                    "name": "bluez_card.AA_BB_CC_DD_EE_FF",
                    "active_profile": "headset-head-unit",
                    "profiles": [
                        {"name": "headset-head-unit", "available": True},
                        {"name": "a2dp-sink", "available": True},
                    ],
                }
            ],
            set_card_profile=lambda card_name, profile_name: calls.append(
                ("set_profile", card_name, profile_name)
            ) or True,
        )

        changed, retry_needed = win._reassert_bluetooth_playback_profile()

        self.assertFalse(changed)
        self.assertFalse(retry_needed)
        self.assertEqual(calls, ["lock"])

    def test_reassert_bluetooth_playback_profile_schedules_reconnect_when_only_bad_profiles_exist(self):
        win = self._window()
        calls = []
        win.selected_mic = "alsa_input.usb_dji"
        win._schedule_bluetooth_reconnect = lambda card_name: calls.append(
            ("reconnect", card_name)
        ) or True
        win.engine = SimpleNamespace(
            lock_bluetooth_to_a2dp=lambda: calls.append("lock"),
            list_cards=lambda: [
                {
                    "name": "bluez_card.AA_BB_CC_DD_EE_FF",
                    "active_profile": "headset-head-unit",
                    "profiles": [
                        {"name": "headset-head-unit", "available": True},
                        {"name": "off", "available": True},
                    ],
                }
            ],
            set_card_profile=lambda card_name, profile_name: calls.append(
                ("set_profile", card_name, profile_name)
            ) or True,
        )

        changed, retry_needed = win._reassert_bluetooth_playback_profile()

        self.assertFalse(changed)
        self.assertTrue(retry_needed)
        self.assertEqual(
            calls,
            [
                "lock",
                ("reconnect", "bluez_card.AA_BB_CC_DD_EE_FF"),
            ],
        )

    def test_reassert_bluetooth_playback_profile_reconnects_degraded_card_during_recovery(self):
        win = self._window()
        calls = []
        win.selected_mic = "alsa_input.usb_dji"
        win._bluetooth_profile_reassert_retries = 3
        win._schedule_bluetooth_reconnect = lambda card_name: calls.append(
            ("reconnect", card_name)
        ) or True
        win.engine = SimpleNamespace(
            lock_bluetooth_to_a2dp=lambda: calls.append("lock"),
            list_cards=lambda: [
                {
                    "name": "bluez_card.AA_BB_CC_DD_EE_FF",
                    "active_profile": "headset-head-unit",
                    "profiles": [
                        {"name": "headset-head-unit", "available": True},
                        {"name": "a2dp-sink", "available": True},
                    ],
                }
            ],
            set_card_profile=lambda card_name, profile_name: calls.append(
                ("set_profile", card_name, profile_name)
            ) or True,
        )

        changed, retry_needed = win._reassert_bluetooth_playback_profile()

        self.assertTrue(changed)
        self.assertTrue(retry_needed)
        self.assertEqual(
            calls,
            [
                "lock",
                ("reconnect", "bluez_card.AA_BB_CC_DD_EE_FF"),
                ("set_profile", "bluez_card.AA_BB_CC_DD_EE_FF", "a2dp-sink"),
            ],
        )

    def test_reassert_bluetooth_playback_profile_reconnects_missing_known_bluetooth_monitor(self):
        win = self._window()
        calls = []
        win.selected_mic = "alsa_input.usb_dji"
        win._bluetooth_profile_reassert_retries = 3
        win._desired_mix_hw["Monitor"] = "bluez_output.AA_BB_CC_DD_EE_FF.1"
        win.engine = SimpleNamespace(
            lock_bluetooth_to_a2dp=lambda: calls.append("lock"),
            list_cards=lambda: [],
            set_card_profile=lambda *args, **kwargs: calls.append(("set_profile", args, kwargs)) or True,
        )
        win._schedule_bluetooth_reconnect_mac = (
            lambda mac, **kwargs: calls.append(("reconnect_mac", mac, kwargs)) or True
        )

        changed, retry_needed = win._reassert_bluetooth_playback_profile()

        self.assertFalse(changed)
        self.assertTrue(retry_needed)
        self.assertEqual(
            calls,
            [
                "lock",
                (
                    "reconnect_mac",
                    "AA:BB:CC:DD:EE:FF",
                    {"disconnect_first": False, "settle_delay_ms": 250},
                ),
            ],
        )

    def test_schedule_audio_server_recovery_reconnects_known_bluetooth_monitor_immediately(self):
        win = self._window()
        calls = []
        win.selected_mic = "alsa_input.usb_dji"
        win._desired_mix_hw["Monitor"] = "bluez_output.AA_BB_CC_DD_EE_FF.1"
        win._event_proc_restart_timer = _FakeTimer()
        win._event_refresh_timer = _FakeTimer()
        win._bluetooth_refresh_timer = _FakeTimer()
        win._schedule_bluetooth_reconnect_mac = (
            lambda mac, **kwargs: calls.append(("reconnect_mac", mac, kwargs)) or True
        )

        win._schedule_audio_server_recovery()

        self.assertEqual(win._bluetooth_profile_reassert_retries, 6)
        self.assertEqual(win._event_proc_restart_timer.start_calls, 1)
        self.assertEqual(win._event_refresh_timer.start_calls, 1)
        self.assertEqual(win._bluetooth_refresh_timer.start_calls, 1)
        self.assertEqual(win._bluetooth_refresh_timer.interval, 600)
        self.assertEqual(
            calls,
            [
                (
                    "reconnect_mac",
                    "AA:BB:CC:DD:EE:FF",
                    {"disconnect_first": False, "settle_delay_ms": 250},
                )
            ],
        )

    def test_handle_bluetooth_settle_refresh_retries_quickly_during_recovery(self):
        win = self._window()
        calls = []
        win._bluetooth_profile_reassert_retries = 3
        win._bluetooth_refresh_timer = _FakeTimer()
        win._restart_event_subscriber_if_needed = lambda: calls.append("restart_subscriber")
        win._reassert_bluetooth_playback_profile = lambda: (False, True)
        win._request_runtime_refresh = lambda reason: calls.append(("refresh", reason))

        win._handle_bluetooth_settle_refresh()

        self.assertEqual(win._bluetooth_profile_reassert_retries, 2)
        self.assertEqual(win._bluetooth_refresh_timer.start_calls, 1)
        self.assertEqual(win._bluetooth_refresh_timer.interval, 600)
        self.assertEqual(
            calls,
            [
                "restart_subscriber",
                ("refresh", "bluetooth-settle"),
            ],
        )

    def test_prime_bluetooth_playback_profile_reasserts_and_starts_timer(self):
        win = self._window()
        calls = []
        win.selected_mic = "alsa_input.usb_dji"
        win._bluetooth_profile_reassert_retries = 0
        win._bluetooth_refresh_timer = _FakeTimer()
        win._request_runtime_refresh = lambda reason: calls.append(("refresh", reason))
        win.engine = SimpleNamespace(
            list_cards=lambda: [
                {
                    "name": "bluez_card.AA_BB_CC_DD_EE_FF",
                    "active_profile": "headset-head-unit",
                    "profiles": [
                        {"name": "headset-head-unit", "available": True},
                        {"name": "a2dp-sink", "available": True},
                    ],
                }
            ],
            lock_bluetooth_to_a2dp=lambda: calls.append("lock"),
            set_card_profile=lambda card_name, profile_name: calls.append(
                ("set_profile", card_name, profile_name)
            ) or True,
        )

        changed = win._prime_bluetooth_playback_profile()

        self.assertTrue(changed)
        self.assertEqual(win._bluetooth_profile_reassert_retries, 4)
        self.assertEqual(win._bluetooth_refresh_timer.start_calls, 1)
        self.assertEqual(
            calls,
            [
                "lock",
                ("set_profile", "bluez_card.AA_BB_CC_DD_EE_FF", "a2dp-sink"),
                ("refresh", "startup-bt-profile"),
            ],
        )

    def test_prime_bluetooth_playback_profile_skips_bluetooth_mic(self):
        win = self._window()
        win.selected_mic = "bluez_input.AA:BB:CC:DD:EE:FF"
        win._bluetooth_refresh_timer = _FakeTimer()
        win.engine = SimpleNamespace(list_cards=lambda: [{"name": "bluez_card.AA_BB_CC_DD_EE_FF"}])

        changed = win._prime_bluetooth_playback_profile()

        self.assertFalse(changed)
        self.assertEqual(win._bluetooth_refresh_timer.start_calls, 0)

    def test_apply_config_prefers_live_default_sink_for_monitor_output(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: None
        win.engine = type(
            "Engine",
            (),
            {
                "get_default_sink": lambda self: "alsa_output.live_default",
                "get_default_source": lambda self: "mic_b",
            },
        )()
        win._set_engine_identity_overrides = lambda: None
        win._prune_stale_apps = lambda: None

        win._apply_config_dict(
            {
                "monitor_hw": "bluez_output.saved_headset",
                "stream_hw": "alsa_output.stream_saved",
                "monitor_mix_volume": 0.67,
                "stream_mix_volume": 0.28,
            }
        )

        self.assertEqual(win._desired_mix_hw["Monitor"], "alsa_output.live_default")
        self.assertEqual(win._desired_mix_hw["Stream"], "alsa_output.stream_saved")
        self.assertEqual(win._desired_mix_volumes["Monitor"], 0.67)
        self.assertEqual(win._desired_mix_volumes["Stream"], 0.28)
        self.assertEqual(
            win.runtime.mix_route_sync_calls,
            [
                ("Monitor", "alsa_output.live_default", False),
                ("Stream", "alsa_output.stream_saved", False),
            ],
        )
        self.assertEqual(win.runtime.calls[0]["monitor_mix_volume"], 0.67)
        self.assertEqual(win.runtime.calls[0]["stream_mix_volume"], 0.28)

    def test_apply_config_prefers_live_default_source_for_selected_mic(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: None
        win.engine = type(
            "Engine",
            (),
            {
                "get_default_sink": lambda self: "alsa_output.live_default",
                "get_default_source": lambda self: "alsa_input.live_default",
                "resolve_hardware_sink_name": lambda self, sink_name: sink_name,
                "resolve_hardware_source_name": lambda self, source_name: source_name,
                "stable_sink_id": lambda self, sink_name: f"name:{str(sink_name).replace('.', '_')}",
                "stable_source_id": lambda self, source_name: f"name:{str(source_name).replace('.', '_')}",
            },
        )()
        win._set_engine_identity_overrides = lambda: None
        win._prune_stale_apps = lambda: None

        win._apply_config_dict(
            {
                "monitor_hw": "bluez_output.saved_headset",
                "selected_mic": "alsa_input.saved_usb",
            }
        )

        self.assertEqual(win.selected_mic, "alsa_input.live_default")
        self.assertEqual(win.runtime.selected_mic_calls, ["alsa_input.live_default"])
        self.assertEqual(win.runtime.calls[0]["selected_mic"], "alsa_input.live_default")

    def test_apply_config_materializes_virtual_channels_before_immediate_reconcile(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: None
        win.engine = type(
            "Engine",
            (),
            {
                "get_default_sink": lambda self: "alsa_output.live_default",
                "get_default_source": lambda self: "alsa_input.live_default",
                "resolve_hardware_sink_name": lambda self, sink_name: sink_name,
                "resolve_hardware_source_name": lambda self, source_name: source_name,
                "stable_sink_id": lambda self, sink_name: f"name:{str(sink_name).replace('.', '_')}",
                "stable_source_id": lambda self, source_name: f"name:{str(source_name).replace('.', '_')}",
            },
        )()
        win._set_engine_identity_overrides = lambda: None
        win._prune_stale_apps = lambda: None

        win._apply_config_dict(
            {
                "channels": ["Music", "Browser"],
            }
        )

        self.assertEqual(win.runtime.ensure_virtual_channel_calls, ["Music", "Browser"])
        self.assertEqual(len(win.runtime.calls), 1)
        self.assertTrue(win.runtime.calls[0]["apply_now"])
        self.assertEqual(win.runtime.refresh_calls, ["post-config-virtual-sync"])

    def test_reconcile_device_policy_falls_back_monitor_when_active_target_disappears(self):
        win = self._window()
        win.engine = type(
            "Engine",
            (),
            {
                "stable_sink_id": lambda self, sink_name: f"id:{sink_name}",
                "stable_source_id": lambda self, source_name: f"id:{source_name}",
            },
        )()
        win._desired_mix_hw["Monitor"] = "bluez_output.headset"
        win.selected_mic = "alsa_input.internal"
        monitor_calls = []
        win._set_mix_output_target = lambda mix_name, sink_name, **kwargs: (
            monitor_calls.append((mix_name, sink_name, kwargs)),
            win._desired_mix_hw.__setitem__(mix_name, sink_name),
        )[-1]
        win._set_selected_mic_target = lambda *args, **kwargs: None
        view = SimpleNamespace(
            default_sink="alsa_output.speakers",
            default_source="alsa_input.internal",
            sinks=[
                SimpleNamespace(
                    name="alsa_output.speakers",
                    display_name="Speakers",
                    is_internal=False,
                    stable_id="id:alsa_output.speakers",
                )
            ],
            mic_inputs=[
                SimpleNamespace(
                    name="alsa_input.internal",
                    label="Internal Mic",
                    description="Internal Mic",
                    stable_id="id:alsa_input.internal",
                )
            ],
            mixes={},
        )

        changed = win._reconcile_device_policy(view)

        self.assertTrue(changed)
        self.assertTrue(win._active_monitor_fallback)
        self.assertEqual(win._desired_mix_hw["Monitor"], "alsa_output.speakers")
        self.assertEqual(win._restorable_monitor_hw_name, "bluez_output.headset")
        self.assertEqual(monitor_calls[0][0:2], ("Monitor", "alsa_output.speakers"))

    def test_reconcile_device_policy_marks_preferred_monitor_restorable_when_it_returns(self):
        win = self._window()
        win.engine = type(
            "Engine",
            (),
            {"stable_sink_id": lambda self, sink_name: "bt:aa_bb_cc_dd_ee_ff"},
        )()
        win._desired_mix_hw["Monitor"] = "alsa_output.speakers"
        win.selected_mic = "alsa_input.internal"
        win._preferred_monitor_hw_id = "bt:aa_bb_cc_dd_ee_ff"
        win._preferred_monitor_hw_name = "bluez_output.AA_BB_CC_DD_EE_FF.1"
        win._active_monitor_fallback = True
        view = SimpleNamespace(
            default_sink="alsa_output.speakers",
            default_source="alsa_input.internal",
            sinks=[
                SimpleNamespace(
                    name="alsa_output.speakers",
                    display_name="Speakers",
                    is_internal=False,
                    stable_id="id:alsa_output.speakers",
                ),
                SimpleNamespace(
                    name="bluez_output.AA_BB_CC_DD_EE_FF.2",
                    display_name="Headset",
                    is_internal=False,
                    stable_id="bt:aa_bb_cc_dd_ee_ff",
                ),
            ],
            mic_inputs=[
                SimpleNamespace(
                    name="alsa_input.internal",
                    label="Internal Mic",
                    description="Internal Mic",
                    stable_id="id:alsa_input.internal",
                )
            ],
            mixes={},
        )

        changed = win._reconcile_device_policy(view)

        self.assertFalse(changed)
        self.assertTrue(win._active_monitor_fallback)
        self.assertEqual(win._restorable_monitor_hw_name, "bluez_output.AA_BB_CC_DD_EE_FF.2")

    def test_reconcile_device_policy_falls_back_microphone_when_active_source_disappears(self):
        win = self._window()
        win.engine = type(
            "Engine",
            (),
            {
                "stable_sink_id": lambda self, sink_name: f"id:{sink_name}",
                "stable_source_id": lambda self, source_name: f"id:{source_name}",
            },
        )()
        win._desired_mix_hw["Monitor"] = "alsa_output.speakers"
        win.selected_mic = "alsa_input.usb_missing"
        mic_calls = []
        win._set_mix_output_target = lambda *args, **kwargs: None
        win._set_selected_mic_target = lambda mic_name, **kwargs: (
            mic_calls.append((mic_name, kwargs)),
            setattr(win, "selected_mic", mic_name),
        )[-1]
        view = SimpleNamespace(
            default_sink="alsa_output.speakers",
            default_source="alsa_input.internal",
            sinks=[
                SimpleNamespace(
                    name="alsa_output.speakers",
                    display_name="Speakers",
                    is_internal=False,
                    stable_id="id:alsa_output.speakers",
                )
            ],
            mic_inputs=[
                SimpleNamespace(
                    name="alsa_input.internal",
                    label="Internal Mic",
                    description="Internal Mic",
                    stable_id="id:alsa_input.internal",
                )
            ],
            mixes={},
        )

        changed = win._reconcile_device_policy(view)

        self.assertTrue(changed)
        self.assertTrue(win._active_mic_fallback)
        self.assertEqual(win.selected_mic, "alsa_input.internal")
        self.assertEqual(win._restorable_selected_mic_name, "alsa_input.usb_missing")
        self.assertEqual(mic_calls[0][0], "alsa_input.internal")

    def test_reconcile_device_policy_does_not_autopromote_new_devices_when_active_ones_still_exist(self):
        win = self._window()
        win.engine = type(
            "Engine",
            (),
            {
                "stable_sink_id": lambda self, sink_name: f"id:{sink_name}",
                "stable_source_id": lambda self, source_name: f"id:{source_name}",
            },
        )()
        win._desired_mix_hw["Monitor"] = "alsa_output.speakers"
        win.selected_mic = "alsa_input.internal"
        win._preferred_monitor_hw_id = "id:alsa_output.speakers"
        win._preferred_monitor_hw_name = "alsa_output.speakers"
        win._preferred_selected_mic_id = "id:alsa_input.internal"
        win._preferred_selected_mic_name = "alsa_input.internal"
        monitor_calls = []
        mic_calls = []
        win._set_mix_output_target = lambda *args, **kwargs: monitor_calls.append((args, kwargs))
        win._set_selected_mic_target = lambda *args, **kwargs: mic_calls.append((args, kwargs))
        view = SimpleNamespace(
            default_sink="alsa_output.speakers",
            default_source="alsa_input.internal",
            sinks=[
                SimpleNamespace(
                    name="alsa_output.speakers",
                    display_name="Speakers",
                    is_internal=False,
                    stable_id="id:alsa_output.speakers",
                ),
                SimpleNamespace(
                    name="bluez_output.headset",
                    display_name="Headset",
                    is_internal=False,
                    stable_id="id:bluez_output.headset",
                ),
            ],
            mic_inputs=[
                SimpleNamespace(
                    name="alsa_input.internal",
                    label="Internal Mic",
                    description="Internal Mic",
                    stable_id="id:alsa_input.internal",
                ),
                SimpleNamespace(
                    name="alsa_input.usb_mic",
                    label="USB Mic",
                    description="USB Mic",
                    stable_id="id:alsa_input.usb_mic",
                ),
            ],
            mixes={},
        )

        changed = win._reconcile_device_policy(view)

        self.assertFalse(changed)
        self.assertEqual(win._desired_mix_hw["Monitor"], "alsa_output.speakers")
        self.assertEqual(win.selected_mic, "alsa_input.internal")
        self.assertEqual(monitor_calls, [])
        self.assertEqual(mic_calls, [])

    def test_on_master_vol_change_updates_persisted_mix_volume(self):
        win = self._window()
        win.runtime = _FakeRuntime()
        win.schedule_save = lambda: setattr(win, "_saved", True)
        win._pending_master_vol = {}
        win._master_commit_timer = SimpleNamespace(start=lambda: None)

        win._on_master_vol_change("Monitor", 62)
        win._commit_master_vols()

        self.assertEqual(win._desired_mix_volumes["Monitor"], 0.62)
        self.assertEqual(win.runtime.mix_volume_calls, [("Monitor", 0.62)])
        self.assertTrue(win.__dict__.get("_saved", False))

    def test_channel_strip_centers_vertical_sliders_with_mute_buttons(self):
        strip = ChannelStrip("1", "mic", "Digital Microphone", "Microphone", "🎤", engine=None)

        strip.apply_scale(
            MixerStripMetrics(
                strip_width=200,
                slider_height=140,
                strip_height=0,
                outer_margin=6,
                inner_spacing=4,
                fader_spacing=10,
                peak_height=5,
                link_button_size=24,
                mute_button_size=28,
                mic_gain_height=20,
                use_horizontal_scroll=False,
            )
        )
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
