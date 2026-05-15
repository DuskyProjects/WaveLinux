import unittest

from app_core import AppContext, EventBus
from modules.health_module import HealthModule
from modules.scenes_module import ScenesModule
from modules.settings_ui_module import SettingsUiModule
from modules.updates_module import UpdatesModule


class _FakeDialog:
    def __init__(self):
        self._geometry = b"dialog-geometry"
        self.restored_geometry = None

    def saveGeometry(self):
        return self._geometry

    def restoreGeometry(self, geometry):
        self.restored_geometry = bytes(geometry)
        return True


class _FakeTimer:
    def __init__(self):
        self.stopped = False

    def stop(self):
        self.stopped = True


class _FakeCancelable:
    def __init__(self):
        self.cancelled = False

    def cancel(self):
        self.cancelled = True


class _FakeWindow:
    def __init__(self):
        self._visible = True
        self._active_tab = "Updates"
        self._scene_name = "Streaming"
        self.settings_dialog = _FakeDialog()
        self._startup_preflight = {"deps": {"pipewire": True}}
        self._pending_update_tag = "2.0.12"
        self._pending_verified_release = object()
        self._pending_update_url = "https://example.test/releases"
        self._pending_update_asset_url = "https://example.test/WaveLinux.AppImage"
        self._pending_update_asset_name = "WaveLinux.AppImage"
        self._last_update_check_at = 1234
        self._last_update_issue = {"code": "update.asset_missing"}
        self._last_update_attempt_result = "failed"
        self._updater = _FakeCancelable()
        self._update_installer = _FakeCancelable()
        self._update_poll_timer = _FakeTimer()
        self._update_install_poll_timer = _FakeTimer()
        self.enabled_changes = []
        self.opened_tabs = []
        self.closed_settings = 0
        self.stale_tabs = []
        self.refreshed_updates = []
        self.refreshed_health = []
        self.refreshed_scenes = []

    def _set_feature_module_enabled(self, module_id, enabled, *, reason=""):
        self.enabled_changes.append((module_id, bool(enabled), reason))

    def _settings_dialog_visible(self):
        return self._visible

    def _active_settings_tab_name(self):
        return self._active_tab

    def _stress_open_settings_tab(self, tab_name):
        self._visible = True
        if tab_name:
            self._active_tab = str(tab_name)
        self.opened_tabs.append(self._active_tab)
        return {"visible": True, "active_tab": self._active_tab}

    def _stress_close_settings(self):
        self._visible = False
        self.closed_settings += 1
        return {"visible": False}

    def _mark_settings_tab_stale(self, tab_name):
        self.stale_tabs.append(str(tab_name))

    def _refresh_update_tab(self):
        self.refreshed_updates.append(True)

    def _refresh_system_tab(self, *, preflight=None):
        self.refreshed_health.append(preflight)

    def _selected_scene_name(self):
        return self._scene_name

    def _refresh_scenes_tab(self, selected_name=None):
        self.refreshed_scenes.append(selected_name)


class SettingsFeatureModuleTests(unittest.TestCase):
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

    def test_settings_ui_restore_reopens_same_tab_and_geometry(self):
        win = _FakeWindow()
        win._active_tab = "Health"
        module = SettingsUiModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertEqual(win.closed_settings, 1)
        self.assertEqual(win.opened_tabs, ["Health"])
        self.assertEqual(win.settings_dialog.restored_geometry, b"dialog-geometry")

    def test_updates_module_stop_cancels_workers_and_restore_refreshes_active_tab(self):
        win = _FakeWindow()
        win._active_tab = "Updates"
        module = UpdatesModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        win._pending_update_tag = None
        win._last_update_issue = None
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertTrue(win._updater.cancelled)
        self.assertTrue(win._update_installer.cancelled)
        self.assertTrue(win._update_poll_timer.stopped)
        self.assertTrue(win._update_install_poll_timer.stopped)
        self.assertEqual(win._pending_update_tag, "2.0.12")
        self.assertEqual(win._last_update_issue, {"code": "update.asset_missing"})
        self.assertEqual(win.refreshed_updates, [True])

    def test_updates_module_restore_marks_tab_stale_when_not_active(self):
        win = _FakeWindow()
        win._active_tab = "Apps"
        module = UpdatesModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertEqual(win.stale_tabs, ["Updates"])

    def test_health_module_restore_refreshes_active_tab(self):
        win = _FakeWindow()
        win._active_tab = "Health"
        module = HealthModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        win._startup_preflight = {}
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertEqual(win.refreshed_health, [{"deps": {"pipewire": True}}])

    def test_scenes_module_restore_refreshes_selected_scene_when_active(self):
        win = _FakeWindow()
        win._active_tab = "Scenes"
        module = ScenesModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        win._scene_name = "Desk"
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertEqual(win.refreshed_scenes, ["Streaming"])


if __name__ == "__main__":
    unittest.main()
