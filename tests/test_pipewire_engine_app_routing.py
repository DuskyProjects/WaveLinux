import unittest
from unittest.mock import patch

from pipewire_engine import PipeWireEngine


class PipeWireEngineAppRoutingTests(unittest.TestCase):
    def _engine(self):
        return PipeWireEngine.__new__(PipeWireEngine)

    def test_display_name_for_stream_app_id_is_human_readable(self):
        self.assertEqual(
            PipeWireEngine.display_name_for_app_id("stream:42"),
            "Audio Stream #42",
        )

    def test_is_persistent_app_id_rejects_transient_stream_routes(self):
        self.assertFalse(PipeWireEngine.is_persistent_app_id("stream:42"))
        self.assertFalse(PipeWireEngine.is_persistent_app_id("Media Stream #42"))
        self.assertFalse(PipeWireEngine.is_persistent_app_id("Audio Stream #42"))
        self.assertTrue(PipeWireEngine.is_persistent_app_id("app:io.ferdium.ferdium"))

    def test_sandbox_identity_candidate_prefers_flatpak_app_id(self):
        engine = self._engine()
        with patch.object(PipeWireEngine, "_pid_lineage", return_value=["321"]), \
             patch.object(PipeWireEngine, "_read_proc_env", return_value={"FLATPAK_ID": "io.ferdium.ferdium"}):
            candidate = engine._sandbox_identity_candidate("321")

        self.assertIsNotNone(candidate)
        self.assertEqual(candidate["app_id"], "app:io.ferdium.ferdium")
        self.assertEqual(candidate["app_name"], "Ferdium")

    def test_resolve_app_identity_prefers_specific_application_name_over_generic_browser_binary(self):
        engine = self._engine()
        current = {
            "node.id": "77",
            "application.name": "Ferdium",
            "application.process.binary": "chrome",
        }
        chrome = PipeWireEngine._candidate_from_raw("binary", "chrome", "Chrome", 88, "binary")
        with patch.object(PipeWireEngine, "_gio_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_sandbox_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_path_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_window_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_binary_identity_candidates", return_value=[chrome]):
            identity = engine._resolve_app_identity(current)

        self.assertEqual(identity["app_id"], "name:ferdium")
        self.assertEqual(identity["app_name"], "Ferdium")

    def test_resolve_app_identity_uses_window_title_for_generic_wrapper_when_needed(self):
        engine = self._engine()
        current = {
            "node.id": "77",
            "application.name": "chrome",
            "application.process.binary": "chrome",
            "window.title": "War Thunder",
        }
        chrome = PipeWireEngine._candidate_from_raw("binary", "chrome", "Chrome", 88, "binary")
        with patch.object(PipeWireEngine, "_gio_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_sandbox_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_path_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_window_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_binary_identity_candidates", return_value=[chrome]):
            identity = engine._resolve_app_identity(current)

        self.assertEqual(identity["app_id"], "stream:77")
        self.assertEqual(identity["app_name"], "War Thunder")

    def test_resolve_app_identity_ignores_browser_tab_titles(self):
        engine = self._engine()
        current = {
            "node.id": "88",
            "application.name": "chrome",
            "application.process.binary": "chrome",
            "window.title": "YouTube - Brave",
        }
        chrome = PipeWireEngine._candidate_from_raw("binary", "chrome", "Chrome", 88, "binary")
        with patch.object(PipeWireEngine, "_gio_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_sandbox_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_path_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_window_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_binary_identity_candidates", return_value=[chrome]):
            identity = engine._resolve_app_identity(current)

        self.assertEqual(identity["app_id"], "binary:chrome")
        self.assertEqual(identity["app_name"], "Chrome")

    def test_resolve_app_identity_prefers_wrapper_launcher_over_generic_chromium_shell(self):
        engine = self._engine()
        current = {
            "node.id": "91",
            "application.name": "chromium",
            "application.process.binary": "chromium",
            "application.process.id": "321",
        }
        chromium = PipeWireEngine._candidate_from_raw(
            "binary", "chromium", "Chromium", 104, "binary"
        )
        with patch.object(PipeWireEngine, "_gio_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_sandbox_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_path_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_window_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_binary_identity_candidates", return_value=[chromium]), \
             patch.object(PipeWireEngine, "_pid_lineage", return_value=["321"]), \
             patch.object(PipeWireEngine, "_read_proc_cmdline", return_value=["/opt/FooChat/foochat", "--type=renderer"]), \
             patch.object(PipeWireEngine, "_desktop_app_index", return_value={"foochat": "Foo Chat"}):
            identity = engine._resolve_app_identity(current)

        self.assertEqual(identity["app_id"], "binary:foochat")
        self.assertEqual(identity["app_name"], "Foo Chat")

    def test_resolve_app_identity_prefers_specific_wrapper_candidate_over_generic_browser_shell(self):
        engine = self._engine()
        current = {
            "node.id": "92",
            "application.name": "chromium",
            "application.process.binary": "chromium",
        }
        chromium = PipeWireEngine._candidate_from_raw(
            "binary", "chromium", "Chromium", 104, "binary"
        )
        wrapper = PipeWireEngine._candidate_from_raw(
            "path", "foochat", "Foo Chat", 94, "exe-path"
        )
        with patch.object(PipeWireEngine, "_gio_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_sandbox_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_path_identity_candidate", return_value=wrapper), \
             patch.object(PipeWireEngine, "_window_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_binary_identity_candidates", return_value=[chromium]), \
             patch.object(PipeWireEngine, "_cmdline_identity_candidates", return_value=[]):
            identity = engine._resolve_app_identity(current)

        self.assertEqual(identity["app_id"], "path:foochat")
        self.assertEqual(identity["app_name"], "Foo Chat")

    def test_resolve_app_identity_prefers_launcher_candidate_over_generic_electron_shell(self):
        engine = self._engine()
        current = {
            "node.id": "93",
            "application.name": "electron",
            "application.process.binary": "electron",
        }
        electron = PipeWireEngine._candidate_from_raw(
            "binary", "electron", "Electron", 104, "binary"
        )
        launcher = PipeWireEngine._candidate_from_raw(
            "desktop", "io.foochat.desktop", "Foo Chat", 92, "gio-desktop"
        )
        with patch.object(PipeWireEngine, "_gio_identity_candidate", return_value=launcher), \
             patch.object(PipeWireEngine, "_sandbox_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_path_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_window_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_binary_identity_candidates", return_value=[electron]), \
             patch.object(PipeWireEngine, "_cmdline_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_text_identity_candidates", return_value=[]):
            identity = engine._resolve_app_identity(current)

        self.assertEqual(identity["app_id"], "desktop:io.foochat.desktop")
        self.assertEqual(identity["app_name"], "Foo Chat")

    def test_resolve_app_identity_prefers_flatpak_wrapper_over_generic_chromium_shell(self):
        engine = self._engine()
        current = {
            "node.id": "94",
            "application.name": "chromium",
            "application.process.binary": "chromium",
        }
        chromium = PipeWireEngine._candidate_from_raw(
            "binary", "chromium", "Chromium", 104, "binary"
        )
        flatpak = PipeWireEngine._candidate_from_raw(
            "app", "io.ferdium.ferdium", "Ferdium", 101, "flatpak"
        )
        with patch.object(PipeWireEngine, "_gio_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_sandbox_identity_candidate", return_value=flatpak), \
             patch.object(PipeWireEngine, "_path_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_window_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_binary_identity_candidates", return_value=[chromium]), \
             patch.object(PipeWireEngine, "_cmdline_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_text_identity_candidates", return_value=[]):
            identity = engine._resolve_app_identity(current)

        self.assertEqual(identity["app_id"], "app:io.ferdium.ferdium")
        self.assertEqual(identity["app_name"], "Ferdium")

    def test_resolve_app_identity_prefers_snap_wrapper_over_generic_electron_shell(self):
        engine = self._engine()
        current = {
            "node.id": "95",
            "application.name": "electron",
            "application.process.binary": "electron",
        }
        electron = PipeWireEngine._candidate_from_raw(
            "binary", "electron", "Electron", 104, "binary"
        )
        snap = PipeWireEngine._candidate_from_raw(
            "snap", "slack", "Slack", 101, "snap-env"
        )
        with patch.object(PipeWireEngine, "_gio_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_sandbox_identity_candidate", return_value=snap), \
             patch.object(PipeWireEngine, "_path_identity_candidate", return_value=None), \
             patch.object(PipeWireEngine, "_window_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_binary_identity_candidates", return_value=[electron]), \
             patch.object(PipeWireEngine, "_cmdline_identity_candidates", return_value=[]), \
             patch.object(PipeWireEngine, "_text_identity_candidates", return_value=[]):
            identity = engine._resolve_app_identity(current)

        self.assertEqual(identity["app_id"], "snap:slack")
        self.assertEqual(identity["app_name"], "Slack")

    def test_sandbox_identity_candidate_prefers_snap_name(self):
        engine = self._engine()
        with patch.object(PipeWireEngine, "_pid_lineage", return_value=["321"]), \
             patch.object(PipeWireEngine, "_read_proc_env", return_value={"SNAP_NAME": "slack"}), \
             patch.object(PipeWireEngine, "_read_proc_cgroup", return_value=""):
            candidate = engine._sandbox_identity_candidate("321")

        self.assertIsNotNone(candidate)
        self.assertEqual(candidate["app_id"], "snap:slack")
        self.assertEqual(candidate["app_name"], "Slack")

    def test_apply_identity_override_rewrites_to_custom_target(self):
        engine = self._engine()
        engine.set_app_identity_overrides(
            {"binary:chromium": "custom:ferdium"},
            {"custom:ferdium": "Ferdium"},
        )

        identity = engine._apply_identity_override({
            "app_id": "binary:chromium",
            "app_name": "Chromium",
            "source": "binary",
        })

        self.assertEqual(identity["app_id"], "custom:ferdium")
        self.assertEqual(identity["app_name"], "Ferdium")
        self.assertEqual(identity["resolved_app_id"], "binary:chromium")
        self.assertEqual(identity["resolved_app_name"], "Chromium")
        self.assertTrue(identity["override_applied"])

    def test_apply_identity_override_keeps_canonical_id_when_only_label_is_overridden(self):
        engine = self._engine()
        engine.set_app_identity_overrides(
            {},
            {"app:com.slack.slack": "Work Slack"},
        )

        identity = engine._apply_identity_override({
            "app_id": "app:com.slack.slack",
            "app_name": "Slack",
            "source": "flatpak",
        })

        self.assertEqual(identity["app_id"], "app:com.slack.slack")
        self.assertEqual(identity["app_name"], "Work Slack")
        self.assertEqual(identity["resolved_app_id"], "app:com.slack.slack")
        self.assertFalse(identity["override_applied"])


if __name__ == "__main__":
    unittest.main()
