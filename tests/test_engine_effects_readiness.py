import unittest

from engine.effects_pipeline import (
    describe_channel_fx_runtime,
    verify_channel_fx_runtime,
)


class _FakeProc:
    def __init__(self, alive=True):
        self._alive = bool(alive)

    def poll(self):
        return None if self._alive else 1


class _FakeEngine:
    def __init__(self):
        self.channel_fx = {
            "mic": {
                "mode": "proxy",
                "effects": ["rnnoise"],
                "params": {"rnnoise": {"VAD Threshold (%)": 75.0}},
                "procs": ["proc-a"],
                "loopbacks": ["301", "302"],
                "source": "output.wavelinux.fx.mic.source",
                "active_chain_source": "wavelinux.rnnoise.mic.source",
                "active_chain_sink": "wavelinux.rnnoise.mic.capture",
                "capture_target": "alsa_input.usb_test",
                "proxy_sink_name": "wavelinux.fx.mic.sink",
                "proxy_sink_module_id": "501",
                "proxy_source_name": "output.wavelinux.fx.mic.source",
                "proxy_source_module_id": "502",
            }
        }
        self.rnnoise_processes = {
            "proc-a": _FakeProc(True),
        }
        self.default_source = "output.wavelinux.fx.mic.source"
        self.visible_sources = {
            "output.wavelinux.fx.mic.source",
            "wavelinux.rnnoise.mic.source",
        }
        self.live_modules = {"301", "302", "501", "502"}

    def get_default_source(self):
        return self.default_source

    def resolve_source_name(self, source_name, snap=None):
        source_name = str(source_name or "").strip()
        return source_name if source_name in self.visible_sources else ""

    def _module_is_alive(self, module_id, short_text=None):
        return str(module_id) in self.live_modules

    def _source_names_match(self, left, right):
        return str(left or "").strip() == str(right or "").strip()


class EffectsReadinessTests(unittest.TestCase):
    def test_describe_channel_fx_runtime_reports_live_proxy_details(self):
        engine = _FakeEngine()

        runtime = describe_channel_fx_runtime(
            engine,
            "mic",
            fx_status={"state": "active", "generation": 4},
        )

        self.assertEqual(runtime.node_name, "mic")
        self.assertEqual(runtime.mode, "proxy")
        self.assertEqual(runtime.source, "output.wavelinux.fx.mic.source")
        self.assertTrue(runtime.proxy_sink_alive)
        self.assertTrue(runtime.proxy_source_alive)
        self.assertEqual(runtime.live_loopbacks, {"301": True, "302": True})
        self.assertEqual(runtime.live_processes, {"proc-a": True})

    def test_verify_channel_fx_runtime_accepts_live_selected_mic_chain(self):
        engine = _FakeEngine()

        result = verify_channel_fx_runtime(
            engine,
            "mic",
            expected_default=True,
            fx_status={"state": "active", "generation": 4},
        )

        self.assertTrue(result.ready)
        self.assertEqual(result.reason_codes(), [])

    def test_verify_channel_fx_runtime_flags_default_source_mismatch(self):
        engine = _FakeEngine()
        engine.default_source = "alsa_input.usb_test"

        result = verify_channel_fx_runtime(
            engine,
            "mic",
            expected_default=True,
            fx_status={"state": "active", "generation": 4},
        )

        self.assertFalse(result.ready)
        self.assertIn("default_source_mismatch", result.reason_codes())

    def test_verify_channel_fx_runtime_flags_dead_passthrough_feed(self):
        engine = _FakeEngine()
        engine.channel_fx["mic"]["mode"] = "proxy_passthrough"
        engine.live_modules.discard("301")

        result = verify_channel_fx_runtime(
            engine,
            "mic",
            expected_default=False,
            fx_status={"state": "active", "generation": 4},
        )

        self.assertFalse(result.ready)
        self.assertIn("fx_passthrough_feed_dead", result.reason_codes())

    def test_verify_channel_fx_runtime_flags_idle_status_even_when_artifacts_exist(self):
        engine = _FakeEngine()

        result = verify_channel_fx_runtime(
            engine,
            "mic",
            expected_default=False,
            fx_status={"state": "idle", "generation": 4},
        )

        self.assertFalse(result.ready)
        self.assertIn("fx_status_not_active", result.reason_codes())


if __name__ == "__main__":
    unittest.main()
