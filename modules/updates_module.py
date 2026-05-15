"""Updates module wrapper."""

from __future__ import annotations

from app_core import ModuleSnapshot

from .base import WindowFeatureModule


class UpdatesModule(WindowFeatureModule):
    module_id = "updates"
    dependencies = ("settings_ui",)

    def snapshot(self) -> ModuleSnapshot:
        return ModuleSnapshot(
            module_id=self.module_id,
            state={
                "pending_update_tag": getattr(self.window, "_pending_update_tag", None),
                "pending_verified_release": getattr(self.window, "_pending_verified_release", None),
                "pending_update_url": getattr(self.window, "_pending_update_url", ""),
                "pending_update_asset_url": getattr(self.window, "_pending_update_asset_url", ""),
                "pending_update_asset_name": getattr(self.window, "_pending_update_asset_name", ""),
                "last_update_check_at": getattr(self.window, "_last_update_check_at", None),
                "last_update_issue": self._deepcopy_state(
                    getattr(self.window, "_last_update_issue", None)
                ),
                "last_update_attempt_result": getattr(
                    self.window,
                    "_last_update_attempt_result",
                    "",
                ),
            },
        )

    def on_stop(self, reason: str) -> None:
        updater = getattr(self.window, "_updater", None)
        if updater is not None and hasattr(updater, "cancel"):
            updater.cancel()
        installer = getattr(self.window, "_update_installer", None)
        if installer is not None and hasattr(installer, "cancel"):
            installer.cancel()
        for attr in ("_update_poll_timer", "_update_install_poll_timer"):
            timer = getattr(self.window, attr, None)
            if timer is not None and hasattr(timer, "stop"):
                timer.stop()
        super().on_stop(reason)

    def restore(self, snapshot: ModuleSnapshot) -> None:
        super().restore(snapshot)
        state = dict(getattr(snapshot, "state", {}) or {})
        self.window._pending_update_tag = state.get("pending_update_tag")
        self.window._pending_verified_release = state.get("pending_verified_release")
        self.window._pending_update_url = state.get("pending_update_url", "")
        self.window._pending_update_asset_url = state.get("pending_update_asset_url", "")
        self.window._pending_update_asset_name = state.get("pending_update_asset_name", "")
        self.window._last_update_check_at = state.get("last_update_check_at")
        self.window._last_update_issue = self._deepcopy_state(state.get("last_update_issue"))
        self.window._last_update_attempt_result = state.get("last_update_attempt_result", "")
        if self._settings_tab_is_active("Updates"):
            self.window._refresh_update_tab()
        else:
            self._mark_settings_tab_stale("Updates")
