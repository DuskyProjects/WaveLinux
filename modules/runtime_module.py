"""Core runtime module wrapper."""

from __future__ import annotations

from app_core import ModuleHealth

from .base import WindowFeatureModule


class RuntimeModule(WindowFeatureModule):
    module_id = "runtime"
    disableable = False
    restartable = False

    def health(self) -> ModuleHealth:
        if bool(self.window.__dict__.get("_runtime_stopped", False)):
            return ModuleHealth(
                module_id=self.module_id,
                state="failed",
                summary="Audio runtime is stopped.",
                issues=["runtime_stopped"],
                restartable=False,
            )
        return super().health()

