import unittest

from app_core import AppContext, EventBus
from modules.device_policy_module import DevicePolicyModule
from modules.effects_module import EffectsModule


class _FakeTimer:
    def __init__(self):
        self.stopped = False

    def stop(self):
        self.stopped = True


class _FakeWindow:
    def __init__(self):
        self.enabled_changes = []
        self.active_effects = {"mic": ["rnnoise"]}
        self.effect_params = {"mic": {"rnnoise": {"VAD Threshold (%)": 75.0}}}
        self._runtime_view_state = object()
        self._desired_mix_hw = {"Monitor": "bluez_output.test.1", "Stream": None}
        self._preferred_monitor_hw_id = "bt:ac:80"
        self._preferred_monitor_hw_name = "Headphones"
        self._preferred_selected_mic_id = "usb:dji"
        self._preferred_selected_mic_name = "Wireless Mic Rx"
        self._restorable_monitor_hw_id = "alsa:speakers"
        self._restorable_monitor_hw_name = "Speakers"
        self._restorable_selected_mic_id = "alsa:internal"
        self._restorable_selected_mic_name = "Internal Mic"
        self._active_monitor_fallback = True
        self._active_mic_fallback = False
        self._device_settle_refresh_timer = _FakeTimer()
        self._bluetooth_refresh_timer = _FakeTimer()
        self._monitor_route_reassert_timer = _FakeTimer()
        self._monitor_route_bluetooth_reassert_timer = _FakeTimer()
        self._mic_cutover_refresh_timer = _FakeTimer()
        self.reconciles = []
        self.disabled_effects = []
        self.enabled_effects = 0

    def _set_feature_module_enabled(self, module_id, enabled, *, reason=""):
        self.enabled_changes.append((module_id, bool(enabled), reason))

    def _reconcile_device_policy(self, view):
        self.reconciles.append(view)

    def _disable_effects_module_runtime(self, *, reason=""):
        self.disabled_effects.append(reason)

    def _enable_effects_module_runtime(self):
        self.enabled_effects += 1


class RuntimeFeatureModuleTests(unittest.TestCase):
    def _ctx(self):
        return AppContext(
            runtime=None,
            engine=None,
            config_store=None,
            event_bus=EventBus(),
            module_manager=None,
            diagnostics=None,
            main_window=None,
        )

    def test_effects_module_restores_effect_state(self):
        win = _FakeWindow()
        module = EffectsModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        win.active_effects = {}
        win.effect_params = {}
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertEqual(win.disabled_effects, ["restart"])
        self.assertEqual(win.active_effects, {"mic": ["rnnoise"]})
        self.assertEqual(
            win.effect_params,
            {"mic": {"rnnoise": {"VAD Threshold (%)": 75.0}}},
        )
        self.assertEqual(win.enabled_effects, 1)

    def test_device_policy_module_stops_timers_and_restores_policy_state(self):
        win = _FakeWindow()
        module = DevicePolicyModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        win._desired_mix_hw = {"Monitor": "alsa_output.speakers", "Stream": None}
        win._preferred_monitor_hw_id = ""
        win._active_monitor_fallback = False
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertTrue(win._device_settle_refresh_timer.stopped)
        self.assertTrue(win._bluetooth_refresh_timer.stopped)
        self.assertTrue(win._monitor_route_reassert_timer.stopped)
        self.assertTrue(win._monitor_route_bluetooth_reassert_timer.stopped)
        self.assertTrue(win._mic_cutover_refresh_timer.stopped)
        self.assertEqual(win._desired_mix_hw, {"Monitor": "bluez_output.test.1", "Stream": None})
        self.assertEqual(win._preferred_monitor_hw_id, "bt:ac:80")
        self.assertTrue(win._active_monitor_fallback)
        self.assertEqual(len(win.reconciles), 3)


if __name__ == "__main__":
    unittest.main()
