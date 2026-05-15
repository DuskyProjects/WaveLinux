"""Device policy module wrapper."""

from __future__ import annotations

from app_core import ModuleSnapshot

from .base import WindowFeatureModule


class DevicePolicyModule(WindowFeatureModule):
    module_id = "device_policy"
    dependencies = ("runtime",)

    _STATE_KEYS = (
        "_desired_mix_hw",
        "_preferred_monitor_hw_id",
        "_preferred_monitor_hw_name",
        "_preferred_selected_mic_id",
        "_preferred_selected_mic_name",
        "_restorable_monitor_hw_id",
        "_restorable_monitor_hw_name",
        "_restorable_selected_mic_id",
        "_restorable_selected_mic_name",
        "_active_monitor_fallback",
        "_active_mic_fallback",
    )

    def on_start(self, ctx) -> None:
        super().on_start(ctx)
        view = getattr(self.window, "_runtime_view_state", None)
        if view is not None:
            self.window._reconcile_device_policy(view)

    def snapshot(self) -> ModuleSnapshot:
        state = {}
        for key in self._STATE_KEYS:
            state[key] = self._deepcopy_state(getattr(self.window, key, None))
        return ModuleSnapshot(module_id=self.module_id, state=state)

    def on_stop(self, reason: str) -> None:
        for timer_name in (
            "_device_settle_refresh_timer",
            "_bluetooth_refresh_timer",
            "_monitor_route_reassert_timer",
            "_monitor_route_bluetooth_reassert_timer",
            "_mic_cutover_refresh_timer",
        ):
            timer = getattr(self.window, timer_name, None)
            if timer is not None and hasattr(timer, "stop"):
                timer.stop()
        super().on_stop(reason)

    def restore(self, snapshot: ModuleSnapshot) -> None:
        super().restore(snapshot)
        state = dict(getattr(snapshot, "state", {}) or {})
        for key in self._STATE_KEYS:
            if key in state:
                setattr(self.window, key, self._deepcopy_state(state.get(key)))
        view = getattr(self.window, "_runtime_view_state", None)
        if view is not None:
            self.window._reconcile_device_policy(view)
