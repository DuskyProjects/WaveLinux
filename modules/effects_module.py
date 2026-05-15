"""Effects module wrapper with stateful restart."""

from __future__ import annotations

from app_core import ModuleSnapshot

from .base import WindowFeatureModule


class EffectsModule(WindowFeatureModule):
    module_id = "effects"
    dependencies = ("runtime",)

    def snapshot(self) -> ModuleSnapshot:
        return ModuleSnapshot(
            module_id=self.module_id,
            state={
                "active_effects": self._deepcopy_state(getattr(self.window, "active_effects", {})),
                "effect_params": self._deepcopy_state(getattr(self.window, "effect_params", {})),
            },
        )

    def on_stop(self, reason: str) -> None:
        self.window._disable_effects_module_runtime(reason=reason)
        super().on_stop(reason)

    def restore(self, snapshot: ModuleSnapshot) -> None:
        super().restore(snapshot)
        state = dict(getattr(snapshot, "state", {}) or {})
        self.window.active_effects = self._deepcopy_state(state.get("active_effects", {}))
        self.window.effect_params = self._deepcopy_state(state.get("effect_params", {}))
        self.window._enable_effects_module_runtime()

