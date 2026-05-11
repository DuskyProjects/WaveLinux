import unittest

from pipewire_engine import OutputMix, PipeWireEngine


class PipeWireEngineRuntimeStabilityTests(unittest.TestCase):
    def _engine(self):
        engine = PipeWireEngine.__new__(PipeWireEngine)
        engine.output_mixes = {}
        engine.loopback_modules = {}
        engine.submix_loopbacks = {}
        engine.submix_sources = {}
        engine.submix_state_cache = {}
        engine.channel_fx = {}
        engine.rnnoise_processes = {}
        return engine

    def test_route_mix_to_hardware_keeps_old_route_if_new_route_fails(self):
        engine = self._engine()
        mix = OutputMix("Monitor", sink_name="wavelinux_mix_monitor")
        mix.hardware_output = "old_sink"
        engine.output_mixes["Monitor"] = mix
        engine.loopback_modules = {"Monitor->old_sink": "11"}

        calls = []

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:3] == ["pactl", "load-module", "module-loopback"]:
                return None
            return ""

        engine._run = fake_run
        engine._sink_visible = lambda sink_name: True
        engine._find_loopback_for = lambda source_token, sink_token: None
        engine._sink_input_for_module = lambda module_id: None
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine.invalidate_snapshot = lambda: None

        success = engine.route_mix_to_hardware("Monitor", "new_sink")

        self.assertFalse(success)
        self.assertEqual(engine.loopback_modules, {"Monitor->old_sink": "11"})
        self.assertFalse(
            any(cmd[:2] == ["pactl", "unload-module"] for cmd in calls)
        )

    def test_route_input_to_submix_keeps_old_loopback_if_replacement_fails(self):
        engine = self._engine()
        engine.output_mixes["Monitor"] = OutputMix("Monitor", sink_name="wavelinux_mix_monitor")
        engine.submix_loopbacks = {"55->Monitor": "111"}
        engine.submix_sources = {"55->Monitor": "old.source"}
        engine.submix_state_cache = {"55->Monitor": {"vol": 0.5, "mute": True}}

        calls = []

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:3] == ["pactl", "load-module", "module-loopback"]:
                return None
            return ""

        engine._run = fake_run
        engine.get_channel_fx_source = lambda node_name: "new.source"
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._find_loopback_for = lambda source_token, sink_token, snap=None: None
        engine.invalidate_snapshot = lambda: None

        success = engine.route_input_to_submix(
            "55",
            "mic",
            "Audio/Source",
            "Monitor",
        )

        self.assertTrue(success)
        self.assertEqual(engine.submix_loopbacks["55->Monitor"], "111")
        self.assertEqual(engine.submix_sources["55->Monitor"], "old.source")
        self.assertFalse(
            any(cmd[:2] == ["pactl", "unload-module"] for cmd in calls)
        )

    def test_set_channel_fx_does_not_move_internal_submix_loopbacks(self):
        engine = self._engine()
        engine.submix_sources = {
            "55->Monitor": "mic",
            "55->Stream": "mic",
        }
        engine.submix_loopbacks = {
            "55->Monitor": "201",
            "55->Stream": "202",
        }

        exclude_modules_seen = []
        default_source_calls = []

        engine._ordered_chain = lambda effects: effects
        engine.effect_available = lambda effect_id: True
        engine._safe_channel_key = lambda node_name: "mic"
        engine._build_unified_chain_config = (
            lambda safe_key, ordered, params_map, stamp: (
                "/tmp/wavelinux-fx.conf",
                "wavelinux.fx.mic.input",
                "wavelinux.fx.mic.source",
                ordered,
            )
        )
        engine._fx_log_path = lambda safe_key, suffix: "/tmp/wavelinux-fx.log"
        engine._spawn_fx = lambda config_path, log_path, proc_key: True
        engine._wait_load_loopback = lambda source, sink: "300"
        engine.get_default_source = lambda: "mic"
        engine.set_default_source = lambda source_name: default_source_calls.append(source_name)
        engine._reapply_submix_state_cache = lambda: None
        engine.invalidate_snapshot = lambda: None

        def fake_move_source_outputs(from_source, to_source, exclude_modules=None):
            exclude_modules_seen.append(list(exclude_modules or []))

        engine._move_source_outputs = fake_move_source_outputs

        source = engine.set_channel_fx("mic", "mic", ["rnnoise"], {})

        self.assertEqual(source, "wavelinux.fx.mic.source")
        self.assertEqual(default_source_calls, ["wavelinux.fx.mic.source"])
        self.assertEqual(engine.submix_sources["55->Monitor"], "mic")
        self.assertEqual(engine.submix_sources["55->Stream"], "mic")
        self.assertEqual(len(exclude_modules_seen), 1)
        self.assertCountEqual(exclude_modules_seen[0], ["300", "201", "202"])


if __name__ == "__main__":
    unittest.main()
