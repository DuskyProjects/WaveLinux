"""Settings dialog module wrapper."""

from app_core import ModuleSnapshot

from .base import WindowFeatureModule


class SettingsUiModule(WindowFeatureModule):
    module_id = "settings_ui"

    def snapshot(self) -> ModuleSnapshot:
        dialog = getattr(self.window, "settings_dialog", None)
        geometry = None
        if dialog is not None and hasattr(dialog, "saveGeometry"):
            try:
                geometry = bytes(dialog.saveGeometry())
            except Exception:
                geometry = None
        return ModuleSnapshot(
            module_id=self.module_id,
            state={
                "visible": bool(self.window._settings_dialog_visible()),
                "active_tab": self.window._active_settings_tab_name(),
                "geometry": geometry,
            },
        )

    def on_stop(self, reason: str) -> None:
        self.window._stress_close_settings()
        super().on_stop(reason)

    def restore(self, snapshot: ModuleSnapshot) -> None:
        super().restore(snapshot)
        state = dict(getattr(snapshot, "state", {}) or {})
        geometry = state.get("geometry")
        dialog = getattr(self.window, "settings_dialog", None)
        if geometry and dialog is not None and hasattr(dialog, "restoreGeometry"):
            try:
                dialog.restoreGeometry(geometry)
            except Exception:
                pass
        if not state.get("visible"):
            return
        result = self.window._stress_open_settings_tab(state.get("active_tab"))
        if not result.get("visible"):
            self.mark_failed("Could not reopen Settings after module restart.")
