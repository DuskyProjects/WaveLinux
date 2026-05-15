"""Metering module wrapper."""

from .base import WindowFeatureModule


class MeteringModule(WindowFeatureModule):
    module_id = "metering"
    dependencies = ("runtime", "mixer_ui")

    def on_stop(self, reason: str) -> None:
        self.window._stop_all_meters()
        super().on_stop(reason)

    def restore(self, snapshot) -> None:
        super().restore(snapshot)
        self.window._restart_metering_module()

