"""Scenes feature module wrapper."""

from __future__ import annotations

from app_core import ModuleSnapshot

from .base import WindowFeatureModule


class ScenesModule(WindowFeatureModule):
    module_id = "scenes"
    dependencies = ("runtime", "settings_ui")

    def snapshot(self) -> ModuleSnapshot:
        return ModuleSnapshot(
            module_id=self.module_id,
            state={
                "selected_name": str(self.window._selected_scene_name() or "").strip(),
            },
        )

    def restore(self, snapshot: ModuleSnapshot) -> None:
        super().restore(snapshot)
        state = dict(getattr(snapshot, "state", {}) or {})
        selected_name = str(state.get("selected_name") or "").strip() or None
        if self._settings_tab_is_active("Scenes"):
            self.window._refresh_scenes_tab(selected_name=selected_name)
        else:
            self._mark_settings_tab_stale("Scenes")
