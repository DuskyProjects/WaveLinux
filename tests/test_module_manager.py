import unittest

from app_core import AppContext, BaseFeatureModule, EventBus, HealthBus, ModuleManager


class _RecorderModule(BaseFeatureModule):
    def __init__(self, module_id, order, *, dependencies=(), disableable=True, restartable=True):
        super().__init__(module_id, order, dependencies, disableable, restartable)
        self.module_id = module_id
        self.dependencies = tuple(dependencies)
        self.disableable = disableable
        self.restartable = restartable
        self.order = order

    def on_start(self, ctx):
        self.order.append(("start", self.module_id))

    def on_stop(self, reason):
        self.order.append(("stop", self.module_id, reason))


class _SnapshotRecorderModule(_RecorderModule):
    def snapshot(self):
        return super().snapshot().__class__(module_id=self.module_id, state={"value": 7})

    def restore(self, snapshot):
        self.order.append(("restore", self.module_id, dict(snapshot.state)))


class ModuleManagerTests(unittest.TestCase):
    def _ctx(self):
        return AppContext(
            runtime=None,
            engine=None,
            config_store=None,
            event_bus=EventBus(),
            module_manager=None,
            diagnostics=None,
            main_window=None,
        )

    def test_start_and_stop_follow_dependency_order(self):
        order = []
        ctx = self._ctx()
        manager = ModuleManager(ctx, health_bus=HealthBus())
        ctx.module_manager = manager
        manager.register(_RecorderModule("runtime", order))
        manager.register(_RecorderModule("metering", order, dependencies=("runtime",)))
        manager.start_all()
        manager.stop_all("shutdown")

        self.assertEqual(
            order,
            [
                ("start", "runtime"),
                ("start", "metering"),
                ("stop", "metering", "shutdown"),
                ("stop", "runtime", "shutdown"),
            ],
        )

    def test_disable_and_enable_module_updates_health(self):
        order = []
        ctx = self._ctx()
        manager = ModuleManager(ctx, health_bus=HealthBus())
        ctx.module_manager = manager
        manager.register(_RecorderModule("updates", order))
        manager.start_all()

        manager.disable_module("updates", "diagnostic")
        self.assertEqual(manager.module_health("updates").state, "disabled")

        manager.enable_module("updates")
        self.assertEqual(manager.module_health("updates").state, "running")

    def test_disable_and_enable_restores_snapshot(self):
        order = []
        ctx = self._ctx()
        manager = ModuleManager(ctx, health_bus=HealthBus())
        ctx.module_manager = manager
        manager.register(_SnapshotRecorderModule("settings_ui", order))
        manager.start_all()

        manager.disable_module("settings_ui", "diagnostic")
        manager.enable_module("settings_ui")

        self.assertEqual(
            order,
            [
                ("start", "settings_ui"),
                ("stop", "settings_ui", "diagnostic"),
                ("start", "settings_ui"),
                ("restore", "settings_ui", {"value": 7}),
            ],
        )


if __name__ == "__main__":
    unittest.main()
