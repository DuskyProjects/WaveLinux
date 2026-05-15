"""Stress control module wrapper."""

from .base import WindowFeatureModule


class StressControlModule(WindowFeatureModule):
    module_id = "stress_control"

    def on_start(self, ctx) -> None:
        super().on_start(ctx)
        self.window._setup_stress_control()

    def on_stop(self, reason: str) -> None:
        self.window._stop_stress_control()
        super().on_stop(reason)
