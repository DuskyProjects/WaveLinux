import unittest

from pipewire_engine import OutputMix, PipeWireEngine
from pipewire_engine import AudioNode


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

    def test_route_mix_to_hardware_resolves_rotated_bluetooth_sink_name(self):
        engine = self._engine()
        mix = OutputMix("Monitor", sink_name="wavelinux_mix_monitor")
        mix.hardware_output = "alsa_output.speakers"
        engine.output_mixes["Monitor"] = mix
        engine.loopback_modules = {"Monitor->alsa_output.speakers": "11"}

        calls = []

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:3] == ["pactl", "load-module", "module-loopback"]:
                self.assertIn("sink=bluez_output.AA_BB_CC_DD_EE_FF.2", cmd)
                return "22"
            return ""

        engine._run = fake_run
        engine.invalidate_snapshot = lambda: None
        engine.get_all_sinks = lambda snap=None: [
            {"index": "1", "name": "bluez_output.AA_BB_CC_DD_EE_FF.2"}
        ]
        engine._find_loopback_for = lambda source_token, sink_token, snap=None: None
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._sink_input_for_module = lambda module_id: "55" if str(module_id) == "22" else None

        success = engine.route_mix_to_hardware(
            "Monitor",
            "bluez_output.AA_BB_CC_DD_EE_FF.1",
        )

        self.assertTrue(success)
        self.assertEqual(
            mix.hardware_output,
            "bluez_output.AA_BB_CC_DD_EE_FF.2",
        )
        self.assertEqual(
            engine.loopback_modules,
            {"Monitor->bluez_output.AA_BB_CC_DD_EE_FF.2": "22"},
        )
        self.assertTrue(
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
        engine.get_channel_fx_source = lambda node_name, snap=None: "new.source"
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
        current_default = {"value": "mic"}

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
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: True
        engine._wait_load_loopback = lambda source, sink: "300"
        engine._create_submix_replacement = lambda source_name, mix_name, initial_state=None: (
            "401" if mix_name == "Monitor" else "402"
        )
        engine._ensure_fx_proxy = lambda safe_key: {
            "sink_name": "wavelinux.fx.mic.sink",
            "sink_module_id": "501",
            "source_name": "wavelinux.fx.mic.source",
            "source_module_id": "502",
        }
        engine._load_loopback_module = lambda source_name, sink_name, latency_msec=20: "301"
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine.get_default_source = lambda: current_default["value"]
        engine.set_default_source = lambda source_name: (
            default_source_calls.append(source_name),
            current_default.__setitem__("value", source_name),
            True,
        )[-1]
        engine._reapply_submix_state_cache = lambda: None
        engine.invalidate_snapshot = lambda: None

        def fake_snapshot_external_source_outputs(source_name, exclude_modules=None):
            exclude_modules_seen.append(list(exclude_modules or []))
            return []

        engine.snapshot_external_source_outputs = fake_snapshot_external_source_outputs
        engine._move_known_source_outputs = lambda *args, **kwargs: True

        source = engine.set_channel_fx("mic", "mic", ["rnnoise"], {})

        self.assertEqual(source, "wavelinux.fx.mic.source")
        self.assertEqual(default_source_calls, ["wavelinux.fx.mic.source"])
        self.assertEqual(engine.submix_sources["55->Monitor"], "wavelinux.fx.mic.source")
        self.assertEqual(engine.submix_sources["55->Stream"], "wavelinux.fx.mic.source")
        self.assertEqual(len(exclude_modules_seen), 1)
        self.assertCountEqual(exclude_modules_seen[0], ["201", "202"])

    def test_apply_channel_fx_rolls_back_to_old_chain_if_cutover_fails(self):
        engine = self._engine()
        old_info = {
            "effects": ["rnnoise"],
            "params": {},
            "procs": ["old-proc"],
            "loopbacks": ["111"],
            "source": "wavelinux.fx.old.source",
            "capture_target": "mic",
            "safe_key": "mic",
            "prev_default": "mic",
        }
        engine.channel_fx["mic"] = old_info

        current_default = {"value": "wavelinux.fx.old.source"}
        default_source_calls = []
        move_calls = []
        stop_calls = []

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
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: True
        engine._wait_load_loopback = lambda source, sink: "300"
        engine._snapshot_submix_bindings = lambda source_name: {}
        engine._ensure_fx_proxy = lambda safe_key: {
            "sink_name": "wavelinux.fx.mic.sink",
            "sink_module_id": "501",
            "source_name": "wavelinux.fx.mic.source",
            "source_module_id": "502",
        }
        engine._load_loopback_module = lambda source_name, sink_name, latency_msec=20: "301"
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine.snapshot_external_source_outputs = lambda source_name, exclude_modules=None: ["900"]
        engine.invalidate_snapshot = lambda: None
        engine.get_default_source = lambda: current_default["value"]

        def fake_set_default_source(source_name):
            default_source_calls.append(source_name)
            current_default["value"] = source_name
            return True

        def fake_move_known_source_outputs(source_output_ids, from_source, to_source,
                                           attempts=20, delay=0.05):
            move_calls.append((list(source_output_ids), from_source, to_source))
            return to_source == "wavelinux.fx.old.source"

        engine.set_default_source = fake_set_default_source
        engine._move_known_source_outputs = fake_move_known_source_outputs
        engine.stop_rnnoise = lambda key="default": stop_calls.append(key)
        engine._run = lambda cmd, *args, **kwargs: ""

        result = engine.apply_channel_fx_transaction("mic", "mic", ["rnnoise"], {})

        self.assertFalse(result["success"])
        self.assertTrue(result["rolled_back"])
        self.assertEqual(result["kept_source"], "wavelinux.fx.old.source")
        self.assertIs(engine.channel_fx["mic"], old_info)
        self.assertEqual(
            default_source_calls,
            ["wavelinux.fx.mic.source", "wavelinux.fx.old.source"],
        )
        self.assertEqual(
            move_calls,
            [
                (["900"], "wavelinux.fx.old.source", "wavelinux.fx.mic.source"),
                (["900"], "wavelinux.fx.mic.source", "wavelinux.fx.old.source"),
            ],
        )
        self.assertEqual(len(stop_calls), 1)

    def test_apply_channel_fx_exposes_stable_proxy_source(self):
        engine = self._engine()
        current_default = {"value": "mic"}
        engine._ordered_chain = lambda effects: effects
        engine.effect_available = lambda effect_id: True
        engine.invalidate_snapshot = lambda: None
        engine._safe_channel_key = lambda node_name: "mic"
        engine._build_unified_chain_config = (
            lambda safe_key, ordered, params_map, stamp: (
                "/tmp/wavelinux-fx.conf",
                "wavelinux.fx.chain.input",
                "wavelinux.fx.chain.source",
                ordered,
            )
        )
        engine._fx_log_path = lambda safe_key, suffix: "/tmp/wavelinux-fx.log"
        engine._spawn_fx = lambda config_path, log_path, proc_key: True
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: True
        engine._wait_load_loopback = lambda source, sink: "300"
        engine._ensure_fx_proxy = lambda safe_key: {
            "sink_name": "wavelinux.fx.mic.sink",
            "sink_module_id": "501",
            "source_name": "wavelinux.fx.mic.source",
            "source_module_id": "502",
        }
        engine._load_loopback_module = lambda source_name, sink_name, latency_msec=20: "301"
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._snapshot_submix_bindings = lambda source_name: {}
        engine.snapshot_external_source_outputs = lambda source_name, exclude_modules=None: []
        engine._move_known_source_outputs = lambda *args, **kwargs: True
        engine.get_default_source = lambda: current_default["value"]
        engine.set_default_source = lambda source_name: (
            current_default.__setitem__("value", source_name),
            True,
        )[-1]

        result = engine.apply_channel_fx_transaction("mic", "mic", ["rnnoise"], {})

        self.assertTrue(result["success"])
        self.assertEqual(result["active_source"], "wavelinux.fx.mic.source")
        self.assertEqual(engine.channel_fx["mic"]["mode"], "proxy")
        self.assertEqual(engine.channel_fx["mic"]["source"], "wavelinux.fx.mic.source")
        self.assertEqual(engine.channel_fx["mic"]["active_chain_source"], "wavelinux.fx.chain.source")
        self.assertEqual(engine.channel_fx["mic"]["loopbacks"], ["300", "301"])

    def test_apply_channel_fx_reuses_existing_proxy_source_on_live_update(self):
        engine = self._engine()
        engine.channel_fx["mic"] = {
            "mode": "proxy",
            "effects": ["rnnoise"],
            "params": {},
            "procs": ["old-proc"],
            "loopbacks": ["111", "112"],
            "source": "wavelinux.fx.mic.source",
            "proxy_sink_name": "wavelinux.fx.mic.sink",
            "proxy_sink_module_id": "501",
            "proxy_source_name": "wavelinux.fx.mic.source",
            "proxy_source_module_id": "502",
            "capture_target": "mic",
            "safe_key": "mic",
            "prev_default": "mic",
        }
        move_calls = []

        engine._ordered_chain = lambda effects: effects
        engine.effect_available = lambda effect_id: True
        engine.invalidate_snapshot = lambda: None
        engine._safe_channel_key = lambda node_name: "mic"
        engine._build_unified_chain_config = (
            lambda safe_key, ordered, params_map, stamp: (
                "/tmp/wavelinux-fx.conf",
                "wavelinux.fx.chain.input",
                "wavelinux.fx.chain.source",
                ordered,
            )
        )
        engine._fx_log_path = lambda safe_key, suffix: "/tmp/wavelinux-fx.log"
        engine._spawn_fx = lambda config_path, log_path, proc_key: True
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: True
        engine._wait_load_loopback = lambda source, sink: "300"
        engine._load_loopback_module = lambda source_name, sink_name, latency_msec=20: "301"
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine.get_default_source = lambda: "wavelinux.fx.mic.source"
        engine.set_default_source = lambda source_name: True
        engine.stop_rnnoise = lambda key="default": True
        engine._run = lambda cmd, *args, **kwargs: ""
        engine._move_known_source_outputs = lambda *args, **kwargs: move_calls.append(args) or True

        result = engine.apply_channel_fx_transaction("mic", "mic", ["gate"], {})

        self.assertTrue(result["success"])
        self.assertEqual(result["active_source"], "wavelinux.fx.mic.source")
        self.assertEqual(move_calls, [])

    def test_set_default_source_resolves_virtual_source_alias(self):
        engine = self._engine()
        run_calls = []
        engine._source_id_to_name = lambda: {
            "1": "output.wavelinux.fx.mic.source",
        }
        engine._run = lambda cmd, *args, **kwargs: (run_calls.append(cmd), "ok")[-1]

        success = engine.set_default_source("wavelinux.fx.mic.source")

        self.assertTrue(success)
        self.assertEqual(
            run_calls[0],
            ["pactl", "set-default-source", "output.wavelinux.fx.mic.source"],
        )

    def test_clear_channel_fx_clears_inline_filter_graph(self):
        engine = self._engine()
        node = AudioNode(55, "mic", "Mic", "Audio/Source")
        run_calls = []
        engine.channel_fx["mic"] = {
            "mode": "inline",
            "effects": ["rnnoise"],
            "params": {},
            "source": "mic",
            "capture_target": "mic",
            "node_id": "55",
        }
        engine.create_snapshot = lambda force=False: object()
        engine.get_hardware_inputs = lambda snap=None: [node]
        engine.invalidate_snapshot = lambda: None
        engine._run = lambda cmd, *args, **kwargs: (run_calls.append(cmd), "ok")[-1]

        result = engine.clear_channel_fx_transaction("mic")

        self.assertTrue(result["success"])
        self.assertNotIn("mic", engine.channel_fx)
        self.assertEqual(run_calls[0][:4], ["pw-cli", "s", "55", "Props"])
        self.assertIn('"audioconvert.filter-graph" ""', run_calls[0][4])


if __name__ == "__main__":
    unittest.main()
