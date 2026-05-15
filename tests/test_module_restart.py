import unittest

from app_core import AppContext, BaseFeatureModule, EventBus, ModuleManager


class _SnapshotModule(BaseFeatureModule):
    module_id = "effects"

    def __init__(self, state_store):
        super().__init__(state_store)
        self.state_store = state_store
        self.restored = None

    def on_start(self, ctx):
        self.state_store.append("start")

    def on_stop(self, reason):
        self.state_store.append(f"stop:{reason}")

    def snapshot(self):
        return super().snapshot().__class__(module_id=self.module_id, state={"value": 42})

    def restore(self, snapshot):
        self.restored = dict(snapshot.state)
        self.state_store.append(f"restore:{self.restored['value']}")


class ModuleRestartTests(unittest.TestCase):
    def test_restart_module_snapshots_and_restores(self):
        state_store = []
        ctx = AppContext(
            runtime=None,
            engine=None,
            config_store=None,
            event_bus=EventBus(),
            module_manager=None,
            diagnostics=None,
            main_window=None,
        )
        manager = ModuleManager(ctx)
        ctx.module_manager = manager
        manager.register(_SnapshotModule(state_store))
        manager.start_all()

        manager.restart_module("effects", "test")

        self.assertEqual(state_store, ["start", "stop:test", "start", "restore:42"])
        self.assertEqual(manager.module_health("effects").state, "running")


if __name__ == "__main__":
    unittest.main()
