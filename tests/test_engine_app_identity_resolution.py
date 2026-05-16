import unittest

import engine.app_identity_resolution as app_identity_resolution
from pipewire_engine import PipeWireEngine


class _FallbackEngine:
    def name_matches_host(self, value):
        _ = value
        return False

    def _sanitize_app_label(self, value):
        return value

    def _make_app_route_key(self, prefix, value):
        return f"{prefix}:{value}"


class _ProcessEngine:
    SYSTEM_SOUNDS_BUCKET = PipeWireEngine.SYSTEM_SOUNDS_BUCKET

    def __init__(self):
        self.resolved = {
            "app_id": "app:spotify",
            "app_name": "Spotify",
            "resolved_app_id": "app:spotify",
            "resolved_app_name": "Spotify",
            "source": "application.name",
            "override_applied": False,
        }

    def _resolve_app_identity(self, current):
        _ = current
        return dict(self.resolved)

    def _app_icon_candidates(self, current, **kwargs):
        _ = current, kwargs
        return ["spotify"]


class AppIdentityResolutionModuleTests(unittest.TestCase):
    def test_stream_fallback_identity_uses_first_non_host_label(self):
        identity = app_identity_resolution.stream_fallback_identity(
            _FallbackEngine(),
            {
                "node.id": "12",
                "application.name": "Music Player",
            },
        )

        self.assertEqual(identity["app_id"], "stream:12")
        self.assertEqual(identity["app_name"], "Music Player")

    def test_process_sink_input_skips_internal_nodes(self):
        entries = []
        app_identity_resolution.process_sink_input(
            _ProcessEngine(),
            {
                "sink_id": "4",
                "node.name": "wavelinux.fx.mic.source",
                "media.name": "Mic FX",
            },
            entries,
            {"4": "alsa_output.speakers"},
        )

        self.assertEqual(entries, [])


if __name__ == "__main__":
    unittest.main()
