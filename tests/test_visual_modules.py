import unittest

from app_core import AppContext, EventBus
from modules.app_routing_module import AppRoutingModule
from modules.mixer_ui_module import MixerUiModule


class _FakeBar:
    def __init__(self, value=0):
        self._value = int(value)

    def value(self):
        return self._value

    def setValue(self, value):
        self._value = int(value)


class _FakeScroll:
    def __init__(self, horizontal=0, vertical=0):
        self._horizontal = _FakeBar(horizontal)
        self._vertical = _FakeBar(vertical)

    def horizontalScrollBar(self):
        return self._horizontal

    def verticalScrollBar(self):
        return self._vertical


class _FakeStrip:
    def __init__(self):
        self.flushes = 0

    def flush_pending_state(self):
        self.flushes += 1


class _FakeRow:
    def __init__(self):
        self.flushes = 0

    def flush_pending_state(self):
        self.flushes += 1


class _FakeWindow:
    def __init__(self):
        self.enabled_changes = []
        self.inputs_scroll = _FakeScroll(horizontal=17, vertical=3)
        self.routing_scroll = _FakeScroll(vertical=29)
        self.channel_widgets = {"one": _FakeStrip(), "two": _FakeStrip()}
        self.app_widgets = {"app": _FakeRow()}
        self._runtime_view_state = object()
        self.refreshes = 0

    def _set_feature_module_enabled(self, module_id, enabled, *, reason=""):
        self.enabled_changes.append((module_id, bool(enabled), reason))

    def _refresh_runtime_view(self):
        self.refreshes += 1


class VisualModuleTests(unittest.TestCase):
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

    def test_mixer_ui_module_restores_scroll_and_flushes_pending_state(self):
        win = _FakeWindow()
        module = MixerUiModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        win.inputs_scroll.horizontalScrollBar().setValue(0)
        win.inputs_scroll.verticalScrollBar().setValue(0)
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertEqual(win.channel_widgets["one"].flushes, 1)
        self.assertEqual(win.channel_widgets["two"].flushes, 1)
        self.assertEqual(win.inputs_scroll.horizontalScrollBar().value(), 17)
        self.assertEqual(win.inputs_scroll.verticalScrollBar().value(), 3)
        self.assertEqual(win.refreshes, 1)

    def test_app_routing_module_restores_scroll_and_flushes_pending_state(self):
        win = _FakeWindow()
        module = AppRoutingModule(win)
        ctx = self._ctx()

        module.start(ctx)
        snapshot = module.snapshot()
        win.routing_scroll.verticalScrollBar().setValue(0)
        module.stop("restart")
        module.start(ctx)
        module.restore(snapshot)

        self.assertEqual(win.app_widgets["app"].flushes, 1)
        self.assertEqual(win.routing_scroll.verticalScrollBar().value(), 29)
        self.assertEqual(win.refreshes, 1)


if __name__ == "__main__":
    unittest.main()
