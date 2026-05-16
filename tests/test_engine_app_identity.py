import unittest

import engine.app_identity as app_identity
from pipewire_engine import PipeWireEngine


class _SandboxIdentityEngine:
    def _pid_lineage(self, pid):
        _ = pid
        return ["111"]

    def _read_proc_env(self, pid):
        _ = pid
        return {"FLATPAK_ID": "com.spotify.client"}

    def _read_proc_cgroup(self, pid):
        _ = pid
        return ""

    def _read_proc_cmdline(self, pid):
        _ = pid
        return []

    def _canonicalize_app_id(self, app_id):
        return app_identity.canonicalize_app_id(app_id)

    def _candidate_from_raw(self, prefix, raw_value, display_name, score, source):
        return app_identity.candidate_from_raw(
            PipeWireEngine,
            prefix,
            raw_value,
            display_name,
            score,
            source,
        )


class _OverrideEngine:
    def __init__(self):
        self._app_identity_overrides = {"app:foo": "app:bar"}
        self._app_identity_label_overrides = {"app:bar": "Renamed Bar"}

    def display_name_for_app_id(self, app_id):
        return app_identity.display_name_for_app_id(PipeWireEngine, app_id)

    def _override_display_name_for_app_id(self, app_id, fallback=None):
        return app_identity.override_display_name_for_app_id(self, app_id, fallback=fallback)


class AppIdentityModuleTests(unittest.TestCase):
    def test_display_name_for_known_app_id(self):
        self.assertEqual(
            app_identity.display_name_for_app_id(PipeWireEngine, "app:com.spotify.client"),
            "Spotify",
        )

    def test_normalize_identity_override_map_filters_invalid_entries(self):
        normalized = app_identity.normalize_identity_override_map(
            PipeWireEngine,
            {
                "app:spotify": "app:discord",
                "stream:123": "app:discord",
                PipeWireEngine.SYSTEM_SOUNDS_BUCKET: "app:discord",
                "app:spotify": "stream:123",
            },
        )
        self.assertEqual(normalized, {})

        normalized = app_identity.normalize_identity_override_map(
            PipeWireEngine,
            {"app:spotify": "app:discord"},
        )
        self.assertEqual(normalized, {"app:spotify": "app:discord"})

    def test_sandbox_identity_candidate_uses_flatpak_env(self):
        candidate = app_identity.sandbox_identity_candidate(_SandboxIdentityEngine(), "111")

        self.assertEqual(candidate["app_id"], "app:com.spotify.client")
        self.assertEqual(candidate["app_name"], "Spotify")
        self.assertEqual(candidate["source"], "flatpak")

    def test_apply_identity_override_rewrites_target_and_label(self):
        identity = app_identity.apply_identity_override(
            _OverrideEngine(),
            {"app_id": "app:foo", "app_name": "Foo", "source": "manual"},
        )

        self.assertEqual(identity["app_id"], "app:bar")
        self.assertEqual(identity["app_name"], "Renamed Bar")
        self.assertEqual(identity["resolved_app_id"], "app:foo")
        self.assertTrue(identity["override_applied"])


if __name__ == "__main__":
    unittest.main()
