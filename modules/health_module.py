"""Health module wrapper."""

from __future__ import annotations

from app_core import ModuleSnapshot

from .base import WindowFeatureModule


class HealthModule(WindowFeatureModule):
    module_id = "health"
    dependencies = ("runtime", "settings_ui")

    def snapshot(self) -> ModuleSnapshot:
        return ModuleSnapshot(
            module_id=self.module_id,
            state={
                "preflight": self._deepcopy_state(
                    getattr(self.window, "_startup_preflight", None)
                ),
            },
        )

    def restore(self, snapshot: ModuleSnapshot) -> None:
        super().restore(snapshot)
        state = dict(getattr(snapshot, "state", {}) or {})
        preflight = state.get("preflight")
        if preflight is not None:
            self.window._startup_preflight = self._deepcopy_state(preflight)
        if self._settings_tab_is_active("Health"):
            self.window._refresh_system_tab(preflight=self.window._startup_preflight)
        else:
            self._mark_settings_tab_stale("Health")
