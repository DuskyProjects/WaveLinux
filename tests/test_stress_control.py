import os
import tempfile
import unittest

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PyQt6.QtCore import QObject

from stress_control import StressControlServer


class _FakeRuntime:
    def __init__(self):
        self.set_routes = []
        self.exported = []

    def export_diagnostics(self, reason=""):
        self.exported.append(reason)
        return "/tmp/wavelinux-diag.zip"

    def set_app_route(self, app_id, sink_name):
        self.set_routes.append((app_id, sink_name))


class _FakeWindow(QObject):
    def __init__(self):
        super().__init__()
        self.runtime = _FakeRuntime()
        self.refresh_reasons = []
        self.quit_requests = 0
        self.module_actions = []

    def _stress_runtime_summary(self):
        return {"ready": True, "apps": []}

    def _stress_health_summary(self):
        return [{"code": "ok"}]

    def _stress_list_modules(self):
        return [{"module_id": "effects", "state": "running"}]

    def _stress_get_module_health(self, module_id):
        return {"module_id": module_id, "state": "running"}

    def _stress_disable_module(self, module_id, reason=""):
        self.module_actions.append(("disable", module_id, reason))
        return {"module_id": module_id, "state": "disabled"}

    def _stress_enable_module(self, module_id):
        self.module_actions.append(("enable", module_id))
        return {"module_id": module_id, "state": "running"}

    def _stress_restart_module(self, module_id, reason=""):
        self.module_actions.append(("restart", module_id, reason))
        return {"module_id": module_id, "state": "running"}

    def _stress_set_monitor_output(self, sink_name, persist=True, include_summary=False):
        return {"sink_name": sink_name, "persist": persist, "include_summary": include_summary}

    def _stress_set_stream_output(self, sink_name, persist=True, include_summary=False):
        return {"sink_name": sink_name, "persist": persist, "include_summary": include_summary}

    def _stress_set_selected_mic(self, source_name, persist=True, include_summary=False):
        return {
            "source_name": source_name,
            "persist": persist,
            "include_summary": include_summary,
        }

    def _stress_open_settings_tab(self, tab_name):
        return {"active_tab": tab_name, "visible": True}

    def _stress_close_settings(self):
        return {"visible": False}

    def _stress_list_known_sinks(self):
        return [{"name": "sink"}]

    def _stress_list_known_sources(self):
        return [{"name": "source"}]

    def _request_runtime_refresh(self, reason):
        self.refresh_reasons.append(reason)

    def _request_quit_app(self):
        self.quit_requests += 1


class StressControlServerTests(unittest.TestCase):
    def test_dispatch_ping_and_lists(self):
        window = _FakeWindow()
        with tempfile.TemporaryDirectory() as temp_dir:
            server = StressControlServer(window, socket_path=os.path.join(temp_dir, "stress.sock"))
            self.assertTrue(server._dispatch_command("ping", {})["pong"])
            self.assertEqual(server._dispatch_command("list_known_sinks", {}), [{"name": "sink"}])
            self.assertEqual(server._dispatch_command("list_known_sources", {}), [{"name": "source"}])

    def test_dispatch_set_app_route_requests_refresh(self):
        window = _FakeWindow()
        with tempfile.TemporaryDirectory() as temp_dir:
            server = StressControlServer(window, socket_path=os.path.join(temp_dir, "stress.sock"))
            result = server._dispatch_command(
                "set_app_route",
                {"app_id": "StressMusic", "sink_name": "wavelinux_music", "refresh": True},
            )
            self.assertTrue(result["ready"])
            self.assertEqual(window.runtime.set_routes, [("StressMusic", "wavelinux_music")])
            self.assertEqual(window.refresh_reasons, ["stress-set-app-route"])

    def test_dispatch_export_diagnostics(self):
        window = _FakeWindow()
        with tempfile.TemporaryDirectory() as temp_dir:
            server = StressControlServer(window, socket_path=os.path.join(temp_dir, "stress.sock"))
            result = server._dispatch_command("export_diagnostics", {"reason": "stress:test"})
            self.assertEqual(result["path"], "/tmp/wavelinux-diag.zip")
            self.assertEqual(window.runtime.exported, ["stress:test"])

    def test_dispatch_selected_mic_can_request_lightweight_response(self):
        window = _FakeWindow()
        with tempfile.TemporaryDirectory() as temp_dir:
            server = StressControlServer(window, socket_path=os.path.join(temp_dir, "stress.sock"))
            result = server._dispatch_command(
                "set_selected_mic",
                {"source_name": "alsa_input.test", "persist": False, "include_summary": False},
            )
            self.assertEqual(
                result,
                {
                    "source_name": "alsa_input.test",
                    "persist": False,
                    "include_summary": False,
                },
            )

    def test_dispatch_module_commands(self):
        window = _FakeWindow()
        with tempfile.TemporaryDirectory() as temp_dir:
            server = StressControlServer(window, socket_path=os.path.join(temp_dir, "stress.sock"))
            self.assertEqual(server._dispatch_command("list_modules", {}), [{"module_id": "effects", "state": "running"}])
            self.assertEqual(
                server._dispatch_command("get_module_health", {"module_id": "effects"}),
                {"module_id": "effects", "state": "running"},
            )
            self.assertEqual(
                server._dispatch_command("disable_module", {"module_id": "effects", "reason": "test"}),
                {"module_id": "effects", "state": "disabled"},
            )
            self.assertEqual(
                server._dispatch_command("enable_module", {"module_id": "effects"}),
                {"module_id": "effects", "state": "running"},
            )
            self.assertEqual(
                server._dispatch_command("restart_module", {"module_id": "effects", "reason": "test"}),
                {"module_id": "effects", "state": "running"},
            )
            self.assertEqual(
                window.module_actions,
                [
                    ("disable", "effects", "test"),
                    ("enable", "effects"),
                    ("restart", "effects", "test"),
                ],
            )


if __name__ == "__main__":
    unittest.main()
