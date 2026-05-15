"""Mixer UI module wrapper."""

from __future__ import annotations

from app_core import ModuleSnapshot

from .base import WindowFeatureModule


class MixerUiModule(WindowFeatureModule):
    module_id = "mixer_ui"
    dependencies = ("runtime",)

    def snapshot(self) -> ModuleSnapshot:
        scroll = getattr(self.window, "inputs_scroll", None)
        horizontal = 0
        vertical = 0
        if scroll is not None:
            hbar = getattr(scroll, "horizontalScrollBar", lambda: None)()
            vbar = getattr(scroll, "verticalScrollBar", lambda: None)()
            if hbar is not None and hasattr(hbar, "value"):
                horizontal = int(hbar.value())
            if vbar is not None and hasattr(vbar, "value"):
                vertical = int(vbar.value())
        return ModuleSnapshot(
            module_id=self.module_id,
            state={
                "horizontal_scroll": horizontal,
                "vertical_scroll": vertical,
            },
        )

    def on_stop(self, reason: str) -> None:
        for strip in list(getattr(self.window, "channel_widgets", {}).values()):
            flusher = getattr(strip, "flush_pending_state", None)
            if flusher is not None:
                flusher()
        super().on_stop(reason)

    def restore(self, snapshot: ModuleSnapshot) -> None:
        super().restore(snapshot)
        if getattr(self.window, "_runtime_view_state", None) is not None:
            self.window._refresh_runtime_view()
        state = dict(getattr(snapshot, "state", {}) or {})
        scroll = getattr(self.window, "inputs_scroll", None)
        if scroll is None:
            return
        hbar = getattr(scroll, "horizontalScrollBar", lambda: None)()
        vbar = getattr(scroll, "verticalScrollBar", lambda: None)()
        if hbar is not None and hasattr(hbar, "setValue"):
            hbar.setValue(int(state.get("horizontal_scroll", 0) or 0))
        if vbar is not None and hasattr(vbar, "setValue"):
            vbar.setValue(int(state.get("vertical_scroll", 0) or 0))
