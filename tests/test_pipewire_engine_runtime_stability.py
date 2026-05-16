import unittest

from pipewire_engine import OutputMix, PipeWireEngine
from pipewire_engine import AudioNode


class _AliveProc:
    def poll(self):
        return None

    def send_signal(self, signum):
        return None

    def wait(self, timeout=None):
        return 0

    def kill(self):
        return None


class PipeWireEngineRuntimeStabilityTests(unittest.TestCase):
    def _engine(self):
        engine = PipeWireEngine.__new__(PipeWireEngine)
        engine.output_mixes = {}
        engine.loopback_modules = {}
        engine.submix_loopbacks = {}
        engine.submix_sources = {}
        engine.submix_state_cache = {}
        engine._pending_submix_state_reapply = set()
        engine.virtual_sink_modules = {}
        engine.channel_fx = {}
        engine.rnnoise_processes = {}
        engine._sink_visible = lambda sink_name: True
        return engine

    def _spawn_fx_success(self, engine):
        def _spawn(config_path, log_path, proc_key):
            engine.rnnoise_processes[proc_key] = _AliveProc()
            return True

        return _spawn

    def test_rnnoise_params_include_grace_controls(self):
        params = PipeWireEngine.get_effect_params("rnnoise")

        self.assertEqual(
            [entry[0] for entry in params],
            [
                "VAD Threshold (%)",
                "VAD Grace Period (ms)",
                "Retroactive VAD Grace (ms)",
            ],
        )

    def test_resolved_rnnoise_params_fill_grace_defaults(self):
        engine = self._engine()

        resolved = engine._resolved_params("rnnoise", {"VAD Threshold (%)": 75.0})

        self.assertEqual(resolved["VAD Threshold (%)"], 75.0)
        self.assertEqual(resolved["VAD Grace Period (ms)"], 200.0)
        self.assertEqual(resolved["Retroactive VAD Grace (ms)"], 0.0)

    def test_ensure_fx_proxy_creates_mono_sink_and_source(self):
        engine = self._engine()
        run_calls = []

        def fake_run(cmd, *args, **kwargs):
            run_calls.append(cmd)
            if cmd[:3] == ["pactl", "load-module", "module-null-sink"]:
                return "501"
            if cmd[:3] == ["pactl", "load-module", "module-virtual-source"]:
                return "502"
            return ""

        engine._run = fake_run
        engine._find_module_by_arg = lambda arg: None
        engine.invalidate_snapshot = lambda: None
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: source_name
        engine.resolve_source_name = lambda source_name: source_name

        proxy = engine._ensure_fx_proxy("mic")

        self.assertIsNotNone(proxy)
        self.assertIn("channels=1", run_calls[0])
        self.assertIn("channel_map=mono", run_calls[0])
        self.assertIn("channels=1", run_calls[3])
        self.assertIn("channel_map=mono", run_calls[3])

    def test_source_names_match_treats_output_alias_as_same_source(self):
        engine = self._engine()
        engine._source_id_to_name = lambda: {
            "1": "output.wavelinux.fx.mic.source",
        }

        self.assertTrue(
            engine._source_names_match(
                "wavelinux.fx.mic.source",
                "output.wavelinux.fx.mic.source",
            )
        )

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

    def test_route_mix_to_hardware_falls_back_when_requested_sink_missing(self):
        engine = self._engine()
        mix = OutputMix("Monitor", sink_name="wavelinux_mix_monitor")
        engine.output_mixes["Monitor"] = mix

        calls = []

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:3] == ["pactl", "load-module", "module-loopback"]:
                self.assertIn("sink=alsa_output.speakers", cmd)
                return "22"
            return ""

        engine._run = fake_run
        engine.invalidate_snapshot = lambda: None
        engine.get_default_sink = lambda: "alsa_output.speakers"
        engine.get_all_sinks = lambda snap=None: [
            {"index": "1", "name": "alsa_output.speakers"}
        ]
        engine._find_loopback_for = lambda source_token, sink_token, snap=None: None
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._sink_input_for_module = lambda module_id: "55" if str(module_id) == "22" else None

        success = engine.route_mix_to_hardware(
            "Monitor",
            "bluez_output.AA_BB_CC_DD_EE_FF.1",
        )

        self.assertTrue(success)
        self.assertEqual(mix.hardware_output, "alsa_output.speakers")
        self.assertEqual(
            engine.loopback_modules,
            {"Monitor->alsa_output.speakers": "22"},
        )

    def test_route_mix_to_hardware_keeps_stream_explicit_when_target_missing(self):
        engine = self._engine()
        mix = OutputMix("Stream", sink_name="wavelinux_mix_stream")
        engine.output_mixes["Stream"] = mix

        calls = []

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:3] == ["pactl", "load-module", "module-loopback"]:
                return "22"
            return ""

        engine._run = fake_run
        engine.invalidate_snapshot = lambda: None
        engine.get_default_sink = lambda: "alsa_output.speakers"
        engine.get_all_sinks = lambda snap=None: [
            {"index": "1", "name": "alsa_output.speakers"}
        ]
        engine._find_loopback_for = lambda source_token, sink_token, snap=None: None
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._sink_input_for_module = lambda module_id: "55" if str(module_id) == "22" else None

        success = engine.route_mix_to_hardware(
            "Stream",
            "bluez_output.AA_BB_CC_DD_EE_FF.1",
        )

        self.assertFalse(success)
        self.assertEqual(engine.loopback_modules, {})
        self.assertFalse(
            any(cmd[:3] == ["pactl", "load-module", "module-loopback"] for cmd in calls)
        )

    def test_resolve_hardware_source_name_matches_stable_usb_identity_after_rename(self):
        engine = self._engine()
        renamed_mic = AudioNode(
            55,
            "alsa_input.usb-renamed",
            "USB Mic",
            "Audio/Source",
            props={"device.serial": "ABC123"},
        )
        engine.get_hardware_inputs = lambda snap=None: [renamed_mic]

        stable_id = engine.stable_source_id(renamed_mic)
        resolved = engine.resolve_hardware_source_name("usb:abc123")

        self.assertEqual(stable_id, "usb:abc123")
        self.assertEqual(resolved, "alsa_input.usb-renamed")

    def test_display_name_for_source_prefers_device_label_over_profile_text(self):
        engine = self._engine()
        dji_mic = AudioNode(
            145,
            "alsa_input.usb-dji",
            "Wireless Mic Rx Digital Stereo (IEC958)",
            "Audio/Source",
            props={
                "device.description": "Wireless Mic Rx",
                "node.nick": "Wireless Mic Rx",
                "device.profile.description": "Digital Stereo (IEC958)",
            },
        )

        self.assertEqual(engine.display_name_for_source(dji_mic), "Wireless Mic Rx")

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

    def test_route_input_to_submix_unloads_stale_raw_loopback_when_fx_route_takes_over(self):
        engine = self._engine()
        engine.output_mixes["Monitor"] = OutputMix("Monitor", sink_name="wavelinux_mix_monitor")

        calls = []

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:3] == ["pactl", "load-module", "module-loopback"]:
                return "222"
            return ""

        def fake_find_loopback(source_token, sink_token, snap=None):
            if source_token == "mic" and sink_token == "wavelinux_mix_monitor":
                return "111"
            return None

        engine._run = fake_run
        engine.get_channel_fx_source = lambda node_name, snap=None: "wavelinux.fx.mic.source"
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._find_loopback_for = fake_find_loopback
        engine.invalidate_snapshot = lambda: None

        success = engine.route_input_to_submix(
            "55",
            "mic",
            "Audio/Source",
            "Monitor",
        )

        self.assertTrue(success)
        self.assertEqual(engine.submix_loopbacks["55->Monitor"], "222")
        self.assertEqual(engine.submix_sources["55->Monitor"], "wavelinux.fx.mic.source")
        self.assertIn(["pactl", "unload-module", "111"], calls)

    def test_route_input_to_submix_marks_new_route_for_state_reapply_even_after_initial_apply(self):
        engine = self._engine()
        engine.output_mixes["Stream"] = OutputMix("Stream", sink_name="wavelinux_mix_stream")
        engine._run = lambda cmd, *args, **kwargs: "222" if cmd[:3] == ["pactl", "load-module", "module-loopback"] else ""
        engine.get_channel_fx_source = lambda node_name, snap=None: None
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._find_loopback_for = lambda source_token, sink_token, snap=None: None
        engine.invalidate_snapshot = lambda: None
        apply_calls = []
        engine._apply_loopback_state = lambda module_id, state: apply_calls.append((module_id, dict(state))) or True

        success = engine.route_input_to_submix(
            "55",
            "wavelinux_music",
            "Audio/Sink",
            "Stream",
            initial_state={"vol": 0.33, "mute": True},
        )

        self.assertTrue(success)
        self.assertEqual(apply_calls, [("222", {"vol": 0.33, "mute": True})])
        self.assertIn("55->Stream", engine._pending_submix_state_reapply)
        self.assertEqual(
            engine.submix_state_cache["55->Stream"],
            {"vol": 0.33, "mute": True},
        )

    def test_set_submix_state_caches_pending_restore_when_sink_input_missing(self):
        engine = self._engine()
        engine.get_submix_sink_input = lambda node_id, mix_name: None
        engine._run = lambda cmd, *args, **kwargs: "ok"

        vol_ok = engine.set_submix_volume("55", "Stream", 0.33)
        mute_ok = engine.set_submix_mute("55", "Stream", True)

        self.assertFalse(vol_ok)
        self.assertFalse(mute_ok)
        self.assertEqual(
            engine.submix_state_cache["55->Stream"],
            {"vol": 0.33, "mute": True},
        )
        self.assertIn("55->Stream", engine._pending_submix_state_reapply)

    def test_create_snapshot_reapplies_pending_submix_state(self):
        engine = self._engine()
        engine.submix_loopbacks = {"55->Stream": "401"}
        engine.submix_state_cache = {"55->Stream": {"vol": 0.33, "mute": True}}
        engine._pending_submix_state_reapply = {"55->Stream"}
        run_calls = []

        def fake_run(cmd, *args, **kwargs):
            run_calls.append(cmd)
            return ""

        engine._run = fake_run
        engine._sink_input_for_module = lambda module_id: "88" if str(module_id) == "401" else None
        engine._parse_nodes = lambda: []
        engine._parse_short_sinks = lambda: []
        engine.reap_dead_processes = lambda: None

        engine.create_snapshot()

        self.assertIn(
            ["pactl", "set-sink-input-volume", "88", "33%"],
            run_calls,
        )
        self.assertIn(
            ["pactl", "set-sink-input-mute", "88", "1"],
            run_calls,
        )
        self.assertNotIn("55->Stream", engine._pending_submix_state_reapply)

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
        engine._spawn_fx = self._spawn_fx_success(engine)
        engine.resolve_source_name = lambda source_name, snap=None: str(source_name or "").strip()
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: source_name
        engine._wait_load_loopback = lambda source, sink, **kwargs: "300"
        engine._create_submix_replacement = lambda source_name, mix_name, initial_state=None: (
            "401" if mix_name == "Monitor" else "402"
        )
        engine._ensure_fx_proxy = lambda safe_key: {
            "sink_name": "wavelinux.fx.mic.sink",
            "sink_module_id": "501",
            "source_name": "wavelinux.fx.mic.source",
            "source_module_id": "502",
        }
        engine._load_loopback_module = lambda source_name, sink_name, latency_msec=20, **kwargs: "301"
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
        engine._spawn_fx = self._spawn_fx_success(engine)
        engine.resolve_source_name = lambda source_name, snap=None: str(source_name or "").strip()
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: source_name
        engine._wait_load_loopback = lambda source, sink, **kwargs: "300"
        engine._snapshot_submix_bindings = lambda source_name: {}
        engine._ensure_fx_proxy = lambda safe_key: {
            "sink_name": "wavelinux.fx.mic.sink",
            "sink_module_id": "501",
            "source_name": "wavelinux.fx.mic.source",
            "source_module_id": "502",
        }
        engine._load_loopback_module = lambda source_name, sink_name, latency_msec=20, **kwargs: "301"
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
        loopback_ids = iter(["300", "301"])
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
        engine._spawn_fx = self._spawn_fx_success(engine)
        engine.resolve_source_name = lambda source_name, snap=None: str(source_name or "").strip()
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: source_name
        engine._wait_load_loopback = lambda source, sink, **kwargs: next(loopback_ids)
        engine._ensure_fx_proxy = lambda safe_key: {
            "sink_name": "wavelinux.fx.mic.sink",
            "sink_module_id": "501",
            "source_name": "wavelinux.fx.mic.source",
            "source_module_id": "502",
        }
        engine._load_loopback_module = lambda source_name, sink_name, latency_msec=20, **kwargs: "301"
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

    def test_apply_channel_fx_waits_for_proxy_feed_loopback(self):
        engine = self._engine()
        current_default = {"value": "mic"}
        wait_calls = []
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
        engine._spawn_fx = self._spawn_fx_success(engine)
        engine.resolve_source_name = lambda source_name, snap=None: str(source_name or "").strip()
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: source_name

        def fake_wait_load_loopback(source_name, sink_name, **kwargs):
            wait_calls.append(
                (
                    source_name,
                    sink_name,
                    kwargs.get("channels"),
                    kwargs.get("channel_map"),
                )
            )
            return "300" if len(wait_calls) == 1 else "301"

        engine._wait_load_loopback = fake_wait_load_loopback
        engine._ensure_fx_proxy = lambda safe_key: {
            "sink_name": "wavelinux.fx.mic.sink",
            "sink_module_id": "501",
            "source_name": "wavelinux.fx.mic.source",
            "source_module_id": "502",
        }
        engine._load_loopback_module = lambda *args, **kwargs: self.fail(
            "proxy feed should use _wait_load_loopback so the sink-input gets normalized"
        )
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
        self.assertEqual(
            wait_calls,
            [
                ("wavelinux.fx.chain.source", "wavelinux.fx.mic.sink", 1, "mono"),
                ("mic", "wavelinux.fx.chain.input", 1, "mono"),
            ],
        )

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
        engine._spawn_fx = self._spawn_fx_success(engine)
        engine.resolve_source_name = lambda source_name, snap=None: str(source_name or "").strip()
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: source_name
        engine._wait_load_loopback = lambda source, sink, **kwargs: "300"
        engine._load_loopback_module = lambda source_name, sink_name, latency_msec=20: "301"
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine.get_default_source = lambda: "wavelinux.fx.mic.source"
        engine.set_default_source = lambda source_name: (
            current_default.__setitem__("value", source_name),
            True,
        )[-1]
        engine.stop_rnnoise = lambda key="default": True
        engine._run = lambda cmd, *args, **kwargs: ""
        engine._move_known_source_outputs = lambda *args, **kwargs: move_calls.append(args) or True

        result = engine.apply_channel_fx_transaction("mic", "mic", ["gate"], {})

        self.assertTrue(result["success"])
        self.assertEqual(result["active_source"], "wavelinux.fx.mic.source")
        self.assertEqual(move_calls, [])

    def test_apply_channel_fx_recreates_stale_proxy_before_live_update(self):
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
            "proxy_source_request_name": "wavelinux.fx.mic.source",
            "proxy_source_module_id": "502",
            "capture_target": "mic",
            "safe_key": "mic",
            "prev_default": "mic",
        }
        ensure_calls = []
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
        engine._spawn_fx = self._spawn_fx_success(engine)
        engine.resolve_source_name = lambda source_name, snap=None: str(source_name or "").strip()
        engine._wait_source_visible = lambda source_name, attempts=20, delay=0.05: source_name
        engine._wait_load_loopback = lambda source, sink, **kwargs: "300"
        engine._module_is_alive = lambda module_id, short_text=None: str(module_id) not in {"501", "502"}
        engine.resolve_source_name = lambda source_name: source_name
        engine.get_default_source = lambda: "wavelinux.fx.mic.source"
        engine.set_default_source = lambda source_name: True
        engine.stop_rnnoise = lambda key="default": True
        engine._run = lambda cmd, *args, **kwargs: ""
        engine._move_known_source_outputs = lambda *args, **kwargs: move_calls.append(args) or True

        def fake_ensure_fx_proxy(safe_key):
            ensure_calls.append(safe_key)
            return {
                "sink_name": "wavelinux.fx.mic.sink",
                "sink_module_id": "701",
                "source_name": "wavelinux.fx.mic.source",
                "source_request_name": "wavelinux.fx.mic.source",
                "source_module_id": "702",
            }

        engine._ensure_fx_proxy = fake_ensure_fx_proxy

        result = engine.apply_channel_fx_transaction("mic", "mic", ["gate"], {})

        self.assertTrue(result["success"])
        self.assertEqual(result["active_source"], "wavelinux.fx.mic.source")
        self.assertEqual(ensure_calls, ["mic"])
        self.assertEqual(move_calls, [])
        self.assertEqual(engine.channel_fx["mic"]["proxy_sink_module_id"], "701")
        self.assertEqual(engine.channel_fx["mic"]["proxy_source_module_id"], "702")

    def test_set_default_source_resolves_virtual_source_alias(self):
        engine = self._engine()
        run_calls = []
        engine._source_id_to_name = lambda: {
            "1": "output.wavelinux.fx.mic.source",
        }
        engine._run = lambda cmd, *args, **kwargs: (
            run_calls.append(cmd),
            "output.wavelinux.fx.mic.source"
            if cmd[:2] == ["pactl", "get-default-source"]
            else "ok",
        )[-1]

        success = engine.set_default_source("wavelinux.fx.mic.source")

        self.assertTrue(success)
        self.assertEqual(
            run_calls[0],
            ["pactl", "set-default-source", "output.wavelinux.fx.mic.source"],
        )

    def test_get_hardware_inputs_reads_source_volume_from_snapshot(self):
        engine = self._engine()
        node = AudioNode(55, "mic", "Mic", "Audio/Source")
        snap = type("Snap", (), {
            "nodes": [node],
            "sources_text": "\n".join([
                "Source #55",
                "\tName: mic",
                "\tMute: yes",
                "\tVolume: front-left: 36045 /  55% / -15.63 dB,   front-right: 36045 /  55% / -15.63 dB",
            ]),
            "_source_state_by_name": None,
        })()
        engine.get_all_nodes = lambda snap=None: [node]
        engine._is_internal_node_name = lambda name: False

        inputs = engine.get_hardware_inputs(snap=snap)

        self.assertEqual(len(inputs), 1)
        self.assertAlmostEqual(inputs[0].volume, 0.55)
        self.assertTrue(inputs[0].muted)

    def test_get_hardware_inputs_excludes_internal_wavelinux_sources(self):
        engine = self._engine()
        real_mic = AudioNode(55, "alsa_input.real_mic", "Mic", "Audio/Source")
        monitor_src = AudioNode(56, "output.wavelinux_src_monitor", "WaveLinux-Monitor", "Audio/Source")
        fx_src = AudioNode(57, "output.wavelinux.fx.real_mic.source", "_WaveLinux-FX-Source", "Audio/Source")
        inline_src = AudioNode(58, "wavelinux.fx.real_mic.source", "_WaveLinux internal: chain output", "Audio/Source")
        snap = type("Snap", (), {
            "nodes": [real_mic, monitor_src, fx_src, inline_src],
            "sources_text": "",
            "_source_state_by_name": {},
        })()
        engine.get_all_nodes = lambda snap=None: [real_mic, monitor_src, fx_src, inline_src]

        inputs = engine.get_hardware_inputs(snap=snap)

        self.assertEqual([node.name for node in inputs], ["alsa_input.real_mic"])

    def test_get_hardware_inputs_includes_stress_synthetic_sources(self):
        engine = self._engine()
        synthetic = AudioNode(
            59,
            "output.wavelinux_stress_fx_a.source",
            "_WaveLinux-Stress-Synthetic-Source",
            "Audio/Source",
        )
        snap = type("Snap", (), {
            "nodes": [synthetic],
            "sources_text": "",
            "_source_state_by_name": {},
        })()
        engine.get_all_nodes = lambda snap=None: [synthetic]

        inputs = engine.get_hardware_inputs(snap=snap)

        self.assertEqual([node.name for node in inputs], ["output.wavelinux_stress_fx_a.source"])

    def test_full_audio_reset_rehomes_app_streams_and_restores_physical_defaults(self):
        engine = self._engine()
        engine.output_mixes["Monitor"] = OutputMix("Monitor", sink_name="wavelinux_mix_monitor")
        engine.output_mixes["Monitor"].hardware_output = "bluez_output.AA_BB_CC_DD_EE_FF.1"
        engine.channel_fx["mic"] = {"prev_default": "alsa_input.real_mic"}

        calls = []
        modules_text = "\n".join([
            "201\tmodule-loopback\tsource=wavelinux_music.monitor sink=wavelinux_mix_monitor",
            "202\tmodule-null-sink\tsink_name=wavelinux_music",
        ])

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:4] == ["pactl", "list", "short", "modules"]:
                return modules_text
            return "ok"

        engine._run = fake_run
        engine.create_snapshot = lambda force=True: object()
        engine.get_default_sink = lambda: "wavelinux_music"
        engine.get_default_source = lambda: "output.wavelinux.fx.real_mic.source"
        engine.resolve_hardware_sink_name = (
            lambda sink_name, snap=None:
            "bluez_output.AA_BB_CC_DD_EE_FF.1"
            if sink_name == "bluez_output.AA_BB_CC_DD_EE_FF.1"
            else None
        )
        engine.resolve_source_name = lambda source_name: source_name
        engine.get_hardware_outputs = lambda snap=None: [
            AudioNode(1, "bluez_output.AA_BB_CC_DD_EE_FF.1", "Headphones", "Audio/Sink")
        ]
        engine.get_sink_inputs = lambda snap=None: [
            {"index": "91", "sink": "wavelinux_music"},
            {"index": "92", "sink": "bluez_output.AA_BB_CC_DD_EE_FF.1"},
        ]
        engine.get_hardware_inputs = lambda snap=None: [
            AudioNode(55, "alsa_input.real_mic", "Mic", "Audio/Source")
        ]

        engine.full_audio_reset()

        self.assertIn(
            ["pactl", "move-sink-input", "91", "bluez_output.AA_BB_CC_DD_EE_FF.1"],
            calls,
        )
        self.assertNotIn(
            ["pactl", "move-sink-input", "92", "bluez_output.AA_BB_CC_DD_EE_FF.1"],
            calls,
        )
        self.assertIn(
            ["pactl", "set-default-sink", "bluez_output.AA_BB_CC_DD_EE_FF.1"],
            calls,
        )
        self.assertIn(
            ["pactl", "set-default-source", "alsa_input.real_mic"],
            calls,
        )
        self.assertLess(
            calls.index(["pactl", "move-sink-input", "91", "bluez_output.AA_BB_CC_DD_EE_FF.1"]),
            calls.index(["pactl", "unload-module", "202"]),
        )

    def test_full_audio_reset_keeps_existing_hardware_defaults(self):
        engine = self._engine()
        calls = []

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:4] == ["pactl", "list", "short", "modules"]:
                return ""
            return "ok"

        engine._run = fake_run
        engine.create_snapshot = lambda force=True: object()
        engine.get_default_sink = lambda: "bluez_output.AA_BB_CC_DD_EE_FF.1"
        engine.get_default_source = lambda: "alsa_input.real_mic"
        engine.resolve_hardware_sink_name = lambda sink_name, snap=None: sink_name
        engine.resolve_source_name = lambda source_name: source_name
        engine.get_hardware_outputs = lambda snap=None: []
        engine.get_hardware_inputs = lambda snap=None: []
        engine.get_sink_inputs = lambda snap=None: [
            {"index": "91", "sink": "bluez_output.AA_BB_CC_DD_EE_FF.1"},
        ]

        engine.full_audio_reset()

        self.assertFalse(any(cmd[:3] == ["pactl", "move-sink-input", "91"] for cmd in calls))
        self.assertFalse(any(cmd[:3] == ["pactl", "set-default-sink", "bluez_output.AA_BB_CC_DD_EE_FF.1"] for cmd in calls))
        self.assertFalse(any(cmd[:3] == ["pactl", "set-default-source", "alsa_input.real_mic"] for cmd in calls))

    def test_full_audio_reset_unloads_virtual_sources(self):
        engine = self._engine()
        calls = []
        modules_text = "\n".join([
            "201\tmodule-loopback\tsource=wavelinux_music.monitor sink=wavelinux_mix_monitor",
            "202\tmodule-virtual-source\tsource_name=wavelinux_src_monitor master=wavelinux_mix_monitor.monitor",
            "203\tmodule-null-sink\tsink_name=wavelinux_mix_monitor",
        ])

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:4] == ["pactl", "list", "short", "modules"]:
                return modules_text
            return "ok"

        engine._run = fake_run
        engine.create_snapshot = lambda force=True: object()
        engine._restore_physical_defaults_before_reset = lambda snap=None: None
        engine.loopback_modules = {"Monitor->sink": "201"}
        engine.virtual_sink_modules = {"wavelinux_mix_monitor": "203"}
        engine.output_mixes = {"Monitor": OutputMix("Monitor", sink_name="wavelinux_mix_monitor")}

        engine.full_audio_reset()

        unloads = [cmd for cmd in calls if cmd[:2] == ["pactl", "unload-module"]]
        self.assertIn(["pactl", "unload-module", "201"], unloads)
        self.assertIn(["pactl", "unload-module", "202"], unloads)
        self.assertIn(["pactl", "unload-module", "203"], unloads)
        self.assertEqual(engine.loopback_modules, {})
        self.assertEqual(engine.virtual_sink_modules, {})
        self.assertEqual(engine.output_mixes, {})

    def test_full_audio_reset_tears_down_fx_state_and_clears_runtime_caches(self):
        engine = self._engine()
        calls = []
        engine.channel_fx = {"mic": {"source": "output.wavelinux.fx.mic.source"}}
        engine.rnnoise_processes = {"chain_mic": object()}
        engine.submix_loopbacks = {"55->Monitor": "201"}
        engine.submix_sources = {"55->Monitor": "output.wavelinux.fx.mic.source"}
        engine.submix_state_cache = {"55->Monitor": {"vol": 1.0, "mute": False}}
        engine._pending_submix_state_reapply = {"55->Monitor"}

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:4] == ["pactl", "list", "short", "modules"]:
                return ""
            if cmd[:3] == ["pactl", "list", "modules"]:
                return "\n".join([
                    "Module #301",
                    "\tArgument: source_name=output.wavelinux.fx.mic.source",
                ])
            return "ok"

        cleared = []
        stopped = []
        engine._run = fake_run
        engine.create_snapshot = lambda force=True: object()
        engine._restore_physical_defaults_before_reset = lambda snap=None: None
        engine.clear_channel_fx = lambda node_name: cleared.append(node_name)
        engine.stop_rnnoise = lambda key='default': stopped.append(key)
        engine.invalidate_snapshot = lambda: calls.append(["invalidate_snapshot"])
        engine.unlock_bluetooth_autoswitch = lambda: calls.append(["unlock_bluetooth_autoswitch"])

        engine.full_audio_reset()

        self.assertEqual(cleared, ["mic"])
        self.assertEqual(stopped, ["chain_mic"])
        self.assertEqual(engine.channel_fx, {})
        self.assertEqual(engine.submix_loopbacks, {})
        self.assertEqual(engine.submix_sources, {})
        self.assertEqual(engine.submix_state_cache, {})
        self.assertEqual(engine._pending_submix_state_reapply, set())
        self.assertIn(["unlock_bluetooth_autoswitch"], calls)
        self.assertIn(["invalidate_snapshot"], calls)
        self.assertIn(["pactl", "unload-module", "301"], calls)

    def test_full_audio_reset_reaps_orphan_fx_processes(self):
        engine = self._engine()
        calls = []

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            if cmd[:4] == ["pactl", "list", "short", "modules"]:
                return ""
            if cmd[:3] == ["pactl", "list", "modules"]:
                return ""
            return "ok"

        reaped = []
        engine._run = fake_run
        engine.create_snapshot = lambda force=True: object()
        engine._restore_physical_defaults_before_reset = lambda snap=None: None
        engine.clear_channel_fx = lambda node_name: None
        engine.stop_rnnoise = lambda key='default': None
        engine.invalidate_snapshot = lambda: calls.append(["invalidate_snapshot"])
        engine.unlock_bluetooth_autoswitch = lambda: calls.append(["unlock_bluetooth_autoswitch"])
        engine._reap_orphan_fx_processes = lambda: reaped.append(True)

        engine.full_audio_reset()

        self.assertEqual(reaped, [True])
        self.assertIn(["invalidate_snapshot"], calls)

    def test_set_source_volume_by_name_resolves_virtual_source_alias(self):
        engine = self._engine()
        run_calls = []
        engine._source_id_to_name = lambda: {
            "1": "output.wavelinux.fx.mic.source",
        }
        engine._run = lambda cmd, *args, **kwargs: (run_calls.append(cmd), "ok")[-1]

        engine.set_source_volume_by_name("wavelinux.fx.mic.source", 0.62)

        self.assertEqual(
            run_calls[0],
            ["pactl", "set-source-volume", "output.wavelinux.fx.mic.source", "62%"],
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

    def test_clear_proxy_channel_fx_rebinds_mono_proxy_feed(self):
        engine = self._engine()
        engine.channel_fx["mic"] = {
            "mode": "proxy",
            "source": "output.wavelinux.fx.mic.source",
            "capture_target": "alsa_input.mic",
            "proxy_sink_name": "wavelinux.fx.mic.sink",
            "proxy_source_name": "output.wavelinux.fx.mic.source",
            "proxy_source_module_id": "501",
            "proxy_sink_module_id": "502",
            "loopbacks": ["601", "602"],
            "procs": ["chain_mic"],
            "prev_default": "alsa_input.mic",
        }
        load_calls = []
        engine._wait_load_loopback = (
            lambda source_name, sink_name, latency_msec=20, **kwargs: (
                load_calls.append((source_name, sink_name, kwargs.get("channels"), kwargs.get("channel_map"))),
                "777",
            )[-1]
        )
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._snapshot_submix_bindings = lambda source_name: {}
        engine.snapshot_external_source_outputs = lambda source_name, exclude_modules=None: []
        current_default = {"value": "output.wavelinux.fx.mic.source"}
        engine.get_default_source = lambda: current_default["value"]
        engine.set_default_source = lambda source_name: (
            current_default.__setitem__("value", source_name),
            True,
        )[-1]
        engine._teardown_fx_plumbing = lambda info: None
        engine._destroy_fx_proxy = lambda info: None
        engine.invalidate_snapshot = lambda: None
        engine._run = lambda cmd, *args, **kwargs: ""

        result = engine.clear_channel_fx_transaction("mic")

        self.assertTrue(result["success"])
        self.assertEqual(
            load_calls,
            [("alsa_input.mic", "wavelinux.fx.mic.sink", 1, "mono")],
        )

    def test_clear_proxy_channel_fx_can_keep_passthrough_proxy_for_selected_mic(self):
        engine = self._engine()
        engine.channel_fx["mic"] = {
            "mode": "proxy",
            "effects": ["rnnoise"],
            "params": {"rnnoise": {"VAD Threshold (%)": 50.0}},
            "source": "output.wavelinux.fx.mic.source",
            "capture_target": "alsa_input.mic",
            "proxy_sink_name": "wavelinux.fx.mic.sink",
            "proxy_source_name": "output.wavelinux.fx.mic.source",
            "proxy_source_request_name": "wavelinux.fx.mic.source",
            "proxy_source_module_id": "501",
            "proxy_sink_module_id": "502",
            "loopbacks": ["601", "602"],
            "procs": ["chain_mic"],
            "safe_key": "mic",
            "prev_default": "alsa_input.mic",
        }
        teardown_calls = []
        load_calls = []
        engine._wait_load_loopback = (
            lambda source_name, sink_name, latency_msec=20, **kwargs: (
                load_calls.append((source_name, sink_name, kwargs.get("channels"), kwargs.get("channel_map"))),
                "777",
            )[-1]
        )
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine._teardown_fx_plumbing = lambda info: teardown_calls.append(info)
        engine.invalidate_snapshot = lambda: None

        result = engine.clear_channel_fx_transaction("mic", keep_proxy=True)

        self.assertTrue(result["success"])
        self.assertEqual(result["active_source"], "output.wavelinux.fx.mic.source")
        self.assertEqual(engine.channel_fx["mic"]["mode"], "proxy_passthrough")
        self.assertEqual(engine.channel_fx["mic"]["effects"], [])
        self.assertEqual(engine.channel_fx["mic"]["loopbacks"], ["777"])
        self.assertEqual(
            load_calls,
            [("alsa_input.mic", "wavelinux.fx.mic.sink", 1, "mono")],
        )
        self.assertEqual(
            teardown_calls,
            [{"loopbacks": ["601", "602"], "procs": ["chain_mic"]}],
        )

    def test_get_channel_fx_source_clears_proxy_when_active_chain_source_disappears(self):
        engine = self._engine()
        cleared = []
        engine.channel_fx["mic"] = {
            "mode": "proxy",
            "effects": ["rnnoise"],
            "params": {},
            "procs": [],
            "loopbacks": ["601", "602"],
            "source": "output.wavelinux.fx.mic.source",
            "active_chain_source": "wavelinux.fx.chain.source",
            "proxy_sink_name": "wavelinux.fx.mic.sink",
            "proxy_source_name": "output.wavelinux.fx.mic.source",
            "proxy_source_module_id": "501",
            "proxy_sink_module_id": "502",
        }
        engine._module_is_alive = lambda module_id, short_text=None: True
        engine.resolve_source_name = lambda source_name, snap=None: (
            None if source_name == "wavelinux.fx.chain.source" else source_name
        )
        engine.clear_channel_fx = lambda node_name: cleared.append(node_name)

        source = engine.get_channel_fx_source("mic")

        self.assertIsNone(source)
        self.assertEqual(cleared, ["mic"])

    def test_get_channel_fx_source_clears_proxy_when_proxy_module_dies(self):
        engine = self._engine()
        cleared = []
        engine.channel_fx["mic"] = {
            "mode": "proxy_passthrough",
            "effects": [],
            "params": {},
            "procs": [],
            "loopbacks": ["601"],
            "source": "output.wavelinux.fx.mic.source",
            "proxy_sink_name": "wavelinux.fx.mic.sink",
            "proxy_source_name": "output.wavelinux.fx.mic.source",
            "proxy_source_module_id": "501",
            "proxy_sink_module_id": "502",
        }
        engine._module_is_alive = lambda module_id, short_text=None: str(module_id) != "502"
        engine.resolve_source_name = lambda source_name, snap=None: source_name
        engine.clear_channel_fx = lambda node_name: cleared.append(node_name)

        source = engine.get_channel_fx_source("mic")

        self.assertIsNone(source)
        self.assertEqual(cleared, ["mic"])


if __name__ == "__main__":
    unittest.main()
