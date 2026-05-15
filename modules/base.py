"""Internal helpers for window-backed feature modules."""

from __future__ import annotations

from copy import deepcopy

from app_core import BaseFeatureModule, ModuleSnapshot


class WindowFeatureModule(BaseFeatureModule):
    feature_name = ""

    def __init__(self, window):
        super().__init__(window)
        self.window = window

    def on_start(self, ctx) -> None:
        _ = ctx
        self.window._set_feature_module_enabled(self.module_id, True)

    def on_stop(self, reason: str) -> None:
        self.window._set_feature_module_enabled(self.module_id, False, reason=reason)

    def snapshot(self) -> ModuleSnapshot:
        return ModuleSnapshot(module_id=self.module_id, state={})

    def restore(self, snapshot: ModuleSnapshot) -> None:
        _ = snapshot
        self.window._set_feature_module_enabled(self.module_id, True, reason="restore")

    def _deepcopy_state(self, value):
        return deepcopy(value)

    def _settings_tab_is_active(self, tab_name: str) -> bool:
        if not bool(self.window._settings_dialog_visible()):
            return False
        active_tab = str(self.window._active_settings_tab_name() or "").strip().lower()
        return active_tab == str(tab_name or "").strip().lower()

    def _mark_settings_tab_stale(self, tab_name: str) -> None:
        marker = getattr(self.window, "_mark_settings_tab_stale", None)
        if marker is not None:
            marker(tab_name)
