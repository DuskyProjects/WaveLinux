"""App routing module wrapper."""

from __future__ import annotations

from app_core import ModuleSnapshot

from .base import WindowFeatureModule


class AppRoutingModule(WindowFeatureModule):
    module_id = "app_routing"
    dependencies = ("runtime", "settings_ui")

    def snapshot(self) -> ModuleSnapshot:
        scroll = getattr(self.window, "routing_scroll", None)
        vertical = 0
        if scroll is not None:
            vbar = getattr(scroll, "verticalScrollBar", lambda: None)()
            if vbar is not None and hasattr(vbar, "value"):
                vertical = int(vbar.value())
        return ModuleSnapshot(
            module_id=self.module_id,
            state={"vertical_scroll": vertical},
        )

    def on_stop(self, reason: str) -> None:
        for row in list(getattr(self.window, "app_widgets", {}).values()):
            flusher = getattr(row, "flush_pending_state", None)
            if flusher is not None:
                flusher()
        super().on_stop(reason)

    def restore(self, snapshot: ModuleSnapshot) -> None:
        super().restore(snapshot)
        if getattr(self.window, "_runtime_view_state", None) is not None:
            self.window._refresh_runtime_view()
        state = dict(getattr(snapshot, "state", {}) or {})
        scroll = getattr(self.window, "routing_scroll", None)
        if scroll is None:
            return
        vbar = getattr(scroll, "verticalScrollBar", lambda: None)()
        if vbar is not None and hasattr(vbar, "setValue"):
            vbar.setValue(int(state.get("vertical_scroll", 0) or 0))
