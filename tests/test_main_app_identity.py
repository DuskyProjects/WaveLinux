import os
import unittest
from types import SimpleNamespace
from unittest import mock

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt6.QtWidgets import QApplication

from main import AppRoutingRow, WaveLinuxWindow
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


class _FakeLabel:
    def __init__(self):
        self._text = ""

    def setText(self, text):
        self._text = str(text)

    def text(self):
        return self._text


class _FakeRuntime:
    def __init__(self):
        self.sync_calls = []
        self.refresh_calls = []
        self.ensure_output_mix_calls = []
        self.ensure_virtual_channel_calls = []
        self.remove_virtual_channel_calls = []
        self.selected_mic_calls = []
        self.mix_route_calls = []

    def sync_persistent_state(self, **kwargs):
        self.sync_calls.append(kwargs)

    def refresh_now(self, reason):
        self.refresh_calls.append(reason)

    def ensure_output_mix_sync(self, mix_name):
        self.ensure_output_mix_calls.append(mix_name)

    def ensure_virtual_channel_sync(self, name):
        self.ensure_virtual_channel_calls.append(name)
        return name

    def remove_virtual_channel_sync(self, sink_name):
        self.remove_virtual_channel_calls.append(sink_name)

    def set_selected_mic(self, node_name):
        self.selected_mic_calls.append(node_name)

    def set_mix_hardware_route(self, mix_name, sink_name):
        self.mix_route_calls.append((mix_name, sink_name))


class _FakeEngine:
    def __init__(self):
        self.override_calls = []

    def set_app_identity_overrides(self, overrides, labels):
        self.override_calls.append((dict(overrides), dict(labels)))

    def get_default_sink(self):
        return "alsa_output.default"

    def display_name_for_sink(self, sink_name):
        return sink_name

    def get_sink_input_volume(self, _index):
        return 1.0


class WaveLinuxMainAppIdentityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._app = QApplication.instance() or QApplication([])

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
        win.app_identity_overrides = {}
        win.app_label_overrides = {}
        win.app_prune_days = 14
        win.forgotten_apps = set()
        win._desired_mix_hw = {"Monitor": None, "Stream": None}
        win.mon_out_combo = _DummyCombo(None, [None, "alsa_output.default"])
        win.str_out_combo = _DummyCombo(None, [None, "alsa_output.stream"])
        win._runtime_view_state = SimpleNamespace(app_views=[])
        win.engine = _FakeEngine()
        win.runtime = _FakeRuntime()
        win.status_lbl = _FakeLabel()
        win._onboarding_completed = True
        win._selected_setup_template = ""
        win.save_config = lambda: setattr(win, "_saved", True)
        win._refresh = lambda: setattr(win, "_refreshed", True)
        win.settings_dialog = None
        return win

    def test_config_roundtrip_persists_identity_overrides_and_labels(self):
        win = self._window()
        win.app_identity_overrides = {
            "binary:chromium": "custom:ferdium",
            "stream:99": "custom:bad",
        }
        win.app_label_overrides = {
            "custom:ferdium": "Ferdium",
            "stream:99": "Bad",
        }
        win.app_display_names = {
            "custom:ferdium": "Ferdium",
            "binary:chromium": "Chromium",
        }

        payload = win._serialize_config()

        self.assertEqual(
            payload["app_identity_overrides"],
            {"binary:chromium": "custom:ferdium"},
        )
        self.assertEqual(
            payload["app_label_overrides"],
            {"custom:ferdium": "Ferdium"},
        )

        restored = self._window()
        restored._apply_config_dict(payload)

        self.assertEqual(
            restored.app_identity_overrides,
            {"binary:chromium": "custom:ferdium"},
        )
        self.assertEqual(
            restored.app_label_overrides,
            {"custom:ferdium": "Ferdium"},
        )
        self.assertEqual(restored.app_display_names["custom:ferdium"], "Ferdium")

    def test_pin_app_identity_migrates_state_and_scenes_to_custom_target(self):
        win = self._window()
        source_app_id = "binary:chromium"
        win.app_routing = {source_app_id: "wavelinux_browser"}
        win.app_volumes = {source_app_id: 0.4}
        win.app_last_seen = {source_app_id: 123}
        win.app_display_names = {source_app_id: "Chromium"}
        win.forgotten_apps = {source_app_id}
        win.scenes = {
            "Desk": {
                "app_routing": {source_app_id: "wavelinux_browser"},
                "app_volumes": {source_app_id: 0.2},
            }
        }
        app_view = SimpleNamespace(
            app_id=source_app_id,
            app_name="Chromium",
            resolved_app_id=source_app_id,
            resolved_app_name="Chromium",
            identity_source="binary",
            override_applied=False,
            manual_override_active=False,
            reset_source_app_id="",
        )

        with mock.patch("main.QInputDialog.getText", return_value=("Ferdium", True)):
            changed = win._pin_app_identity(app_view)

        self.assertTrue(changed)
        self.assertEqual(
            win.app_identity_overrides,
            {"binary:chromium": "custom:ferdium"},
        )
        self.assertEqual(win.app_label_overrides, {"custom:ferdium": "Ferdium"})
        self.assertEqual(win.app_routing, {"custom:ferdium": "wavelinux_browser"})
        self.assertEqual(win.app_volumes, {"custom:ferdium": 0.4})
        self.assertEqual(win.app_last_seen, {"custom:ferdium": 123})
        self.assertEqual(win.forgotten_apps, {"custom:ferdium"})
        self.assertEqual(win.scenes["Desk"]["app_routing"], {"custom:ferdium": "wavelinux_browser"})
        self.assertEqual(win.scenes["Desk"]["app_volumes"], {"custom:ferdium": 0.2})
        self.assertEqual(win.runtime.refresh_calls, ["app-identity-change"])
        self.assertEqual(
            win.runtime.sync_calls[-1]["app_routing"],
            {"custom:ferdium": "wavelinux_browser"},
        )
        self.assertEqual(win.status_lbl.text(), "Pinned app identity: Ferdium")

    def test_merge_app_identity_preserves_existing_target_route_and_volume(self):
        win = self._window()
        source_app_id = "binary:chromium"
        target_app_id = "app:com.slack.Slack"
        win.app_routing = {
            source_app_id: "wavelinux_browser",
            target_app_id: "wavelinux_voice_chat",
        }
        win.app_volumes = {
            source_app_id: 0.25,
            target_app_id: 0.8,
        }
        win.app_last_seen = {
            source_app_id: 10,
            target_app_id: 20,
        }
        win.app_display_names = {
            source_app_id: "Chromium",
            target_app_id: "Slack",
        }
        win.scenes = {
            "Desk": {
                "app_routing": {
                    source_app_id: "wavelinux_browser",
                    target_app_id: "wavelinux_voice_chat",
                },
                "app_volumes": {
                    source_app_id: 0.25,
                    target_app_id: 0.8,
                },
            }
        }
        app_view = SimpleNamespace(
            app_id=source_app_id,
            app_name="Chromium",
            resolved_app_id=source_app_id,
            resolved_app_name="Chromium",
            identity_source="binary",
            override_applied=False,
            manual_override_active=False,
            reset_source_app_id="",
        )

        with mock.patch(
            "main.QInputDialog.getItem",
            return_value=("Slack [app:com.slack.Slack]", True),
        ):
            changed = win._merge_app_identity(app_view)

        self.assertTrue(changed)
        self.assertEqual(
            win.app_identity_overrides,
            {"binary:chromium": "app:com.slack.Slack"},
        )
        self.assertEqual(
            win.app_routing,
            {"app:com.slack.Slack": "wavelinux_voice_chat"},
        )
        self.assertEqual(
            win.app_volumes,
            {"app:com.slack.Slack": 0.8},
        )
        self.assertEqual(win.app_last_seen, {"app:com.slack.Slack": 20})
        self.assertEqual(
            win.scenes["Desk"]["app_routing"],
            {"app:com.slack.Slack": "wavelinux_voice_chat"},
        )
        self.assertEqual(
            win.scenes["Desk"]["app_volumes"],
            {"app:com.slack.Slack": 0.8},
        )

    def test_reset_app_identity_override_restores_source_and_drops_orphan_custom_label(self):
        win = self._window()
        source_app_id = "binary:chromium"
        custom_app_id = "custom:ferdium"
        win.app_identity_overrides = {source_app_id: custom_app_id}
        win.app_label_overrides = {custom_app_id: "Ferdium"}
        win.app_routing = {custom_app_id: "wavelinux_browser"}
        win.app_volumes = {custom_app_id: 0.4}
        win.app_last_seen = {custom_app_id: 123}
        win.app_display_names = {custom_app_id: "Ferdium"}
        win.forgotten_apps = {custom_app_id}
        win.scenes = {
            "Desk": {
                "app_routing": {custom_app_id: "wavelinux_browser"},
                "app_volumes": {custom_app_id: 0.2},
            }
        }
        app_view = SimpleNamespace(
            app_id=custom_app_id,
            app_name="Ferdium",
            resolved_app_id=source_app_id,
            resolved_app_name="Chromium",
            identity_source="remembered",
            override_applied=True,
            manual_override_active=True,
            reset_source_app_id=source_app_id,
        )

        changed = win._reset_app_identity_override(app_view)

        self.assertTrue(changed)
        self.assertEqual(win.app_identity_overrides, {})
        self.assertEqual(win.app_label_overrides, {})
        self.assertEqual(win.app_routing, {source_app_id: "wavelinux_browser"})
        self.assertEqual(win.app_volumes, {source_app_id: 0.4})
        self.assertEqual(win.app_last_seen, {source_app_id: 123})
        self.assertEqual(win.forgotten_apps, {source_app_id})
        self.assertEqual(win.scenes["Desk"]["app_routing"], {source_app_id: "wavelinux_browser"})
        self.assertEqual(win.scenes["Desk"]["app_volumes"], {source_app_id: 0.2})
        self.assertEqual(win.app_display_names[source_app_id], "Chromium")
        self.assertNotIn(custom_app_id, win.app_display_names)

    def test_pin_app_identity_rejects_transient_stream_identity(self):
        win = self._window()
        app_view = SimpleNamespace(
            app_id="stream:42",
            app_name="War Thunder",
            resolved_app_id="stream:42",
            resolved_app_name="War Thunder",
            identity_source="fallback",
            override_applied=False,
            manual_override_active=False,
            reset_source_app_id="",
        )

        with mock.patch("main.QMessageBox.information") as info:
            changed = win._pin_app_identity(app_view)

        self.assertFalse(changed)
        info.assert_called_once()
        self.assertEqual(win.app_identity_overrides, {})

    def test_app_routing_row_manage_button_visibility_matches_identity_type(self):
        engine = _FakeEngine()
        sinks = [{"name": "alsa_output.default", "display_name": "Default Output"}]

        app_row = AppRoutingRow("binary:chromium", "Chromium", engine, sinks)
        app_row.update_state(
            "Chromium",
            [],
            sinks,
            None,
            resolved_app_id="binary:chromium",
            resolved_app_name="Chromium",
            identity_source="binary",
        )
        self.assertFalse(app_row.manage_btn.isHidden())
        self.assertIn("Canonical app ID: binary:chromium", app_row.name_lbl.toolTip())

        system_row = AppRoutingRow(
            PipeWireEngine.SYSTEM_SOUNDS_BUCKET,
            PipeWireEngine.SYSTEM_SOUNDS_BUCKET,
            engine,
            sinks,
        )
        system_row.update_state(
            PipeWireEngine.SYSTEM_SOUNDS_BUCKET,
            [],
            sinks,
            None,
            resolved_app_id=PipeWireEngine.SYSTEM_SOUNDS_BUCKET,
            resolved_app_name=PipeWireEngine.SYSTEM_SOUNDS_BUCKET,
            identity_source="system-sounds",
        )
        self.assertTrue(system_row.manage_btn.isHidden())
        self.assertTrue(system_row.forget_btn.isHidden())


if __name__ == "__main__":
    unittest.main()
