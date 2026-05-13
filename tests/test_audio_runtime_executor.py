import unittest
from contextlib import contextmanager
from types import SimpleNamespace

from audio_runtime.executor import RuntimeExecutor
from audio_runtime.models import (
    Action,
    ChannelSpec,
    DesiredState,
    FxSpec,
    ObservedState,
    RuntimeChannelView,
)
from pipewire_engine import AudioNode


class DummyDiagnostics:
    def export_failure(self, *args, **kwargs):
        return "/tmp/runtime-failure.json"

    def snapshot(self, *args, **kwargs):
        return None


class FakeAdapter:
    def __init__(self, engine):
        self.engine = engine

    @contextmanager
    def session(self):
        yield self.engine


class FakeEngine:
    def __init__(self):
        self.output_mixes = {
            "Monitor": SimpleNamespace(sink_name="wavelinux_mix_monitor"),
            "Stream": SimpleNamespace(sink_name="wavelinux_mix_stream"),
        }
        self.route_calls = []
        self.volume_calls = []
        self.mute_calls = []
        self.default_source_calls = []
        self.cleared = False
        self.submix_loopbacks = {}
        self.submix_sources = {}
        self.channel_fx = {
            "mic": {
                "effects": ["rnnoise"],
                "params": {},
            }
        }
        self.fx_apply_result = {
            "success": True,
            "active_source": "wavelinux.fx.mic.source",
            "message": "FX chain active",
        }
        self.fx_clear_result = {
            "success": True,
            "message": "FX chain cleared",
        }

    def create_snapshot(self, force=False):
        return object()

    def get_hardware_inputs(self, snap=None):
        return [AudioNode(55, "mic", "Mic", "Audio/Source")]

    def get_virtual_sinks(self, snap=None):
        return []

    def get_all_sinks(self, snap=None):
        return []

    def get_sink_inputs(self, snap=None):
        return []

    def snapshot_sink_inputs_by_owner(self, snap=None):
        return {}

    def get_default_source(self):
        return "mic"

    def get_channel_fx_source(self, node_name, snap=None):
        return "wavelinux.fx.mic.source"

    def get_channel_effects(self, node_name):
        return ["rnnoise"]

    def get_live_mix_hardware_route(self, mix_name, snap=None):
        return getattr(self.output_mixes.get(mix_name), "hardware_output", None)

    def get_sink_volume_by_name(self, sink_name, snap=None):
        return 1.0, False

    @staticmethod
    def friendly_name(name):
        return name

    def apply_channel_fx_transaction(self, node_name, capture_target, effects, params_map=None):
        self.last_fx = (node_name, capture_target, list(effects), dict(params_map))
        return dict(self.fx_apply_result)

    def clear_channel_fx_transaction(self, node_name):
        self.cleared = True
        return dict(self.fx_clear_result)

    def route_input_to_submix(self, node_id, node_name, media_class, mix_name,
                              snap=None, initial_state=None):
        self.route_calls.append(
            (node_id, node_name, media_class, mix_name, dict(initial_state or {}))
        )
        return True

    def set_submix_volume(self, node_id, mix_name, volume):
        self.volume_calls.append((node_id, mix_name, volume))

    def set_submix_mute(self, node_id, mix_name, mute):
        self.mute_calls.append((node_id, mix_name, mute))

    def set_default_source(self, source_name):
        self.default_source_calls.append(source_name)


class RuntimeExecutorTests(unittest.TestCase):
    def _desired(self):
        return DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=2),
                    submix_state={
                        "Monitor": {"vol": 0.4, "mute": True},
                        "Stream": {"vol": 0.9, "mute": False},
                    },
                )
            },
        )

    def test_display_name_for_virtual_sink_uses_plain_channel_name(self):
        engine = SimpleNamespace(display_name_for_sink=lambda sink_name, snap=None: sink_name)

        self.assertEqual(
            RuntimeExecutor._display_name_for_sink(engine, "wavelinux_voice_chat", snap=None),
            "Voice Chat",
        )

    def test_apply_channel_fx_uses_transaction_result(self):
        engine = FakeEngine()
        executor = RuntimeExecutor(FakeAdapter(engine), DummyDiagnostics())
        desired = self._desired()

        executor._apply_channel_fx(
            {
                "node_name": "mic",
                "capture_target": "mic",
                "fx_spec": desired.channels["mic"].fx,
            },
            desired,
            status_callback=None,
        )

        self.assertEqual(
            engine.last_fx,
            ("mic", "mic", ["rnnoise"], {}),
        )
        self.assertEqual(engine.route_calls, [])
        self.assertEqual(engine.volume_calls, [])
        self.assertEqual(engine.mute_calls, [])

    def test_apply_channel_fx_emits_degraded_on_failed_transaction(self):
        engine = FakeEngine()
        engine.fx_apply_result = {
            "success": False,
            "failure_stage": "source_output_move",
            "message": "source-output move to candidate source failed",
        }
        executor = RuntimeExecutor(FakeAdapter(engine), DummyDiagnostics())
        desired = self._desired()
        statuses = []

        executor.observe = lambda desired_state: ObservedState()
        executor._apply_channel_fx(
            {
                "node_name": "mic",
                "capture_target": "mic",
                "fx_spec": desired.channels["mic"].fx,
            },
            desired,
            status_callback=statuses.append,
        )

        self.assertEqual(statuses[-1].state, "degraded")
        self.assertIn("source_output_move", statuses[-1].message)
        self.assertIn("source-output move to candidate source failed", statuses[-1].message)
        self.assertEqual(statuses[-1].diagnostics_path, "/tmp/runtime-failure.json")

    def test_clear_channel_fx_uses_transaction_result(self):
        engine = FakeEngine()
        executor = RuntimeExecutor(FakeAdapter(engine), DummyDiagnostics())
        desired = self._desired()

        executor._clear_channel_fx(
            {"node_name": "mic", "generation": 3},
            desired,
            status_callback=None,
        )

        self.assertTrue(engine.cleared)
        self.assertEqual(engine.route_calls, [])

    def test_set_default_source_calls_engine(self):
        engine = FakeEngine()
        executor = RuntimeExecutor(FakeAdapter(engine), DummyDiagnostics())

        executor._set_default_source({"source_name": "wavelinux.fx.mic.source"})

        self.assertEqual(engine.default_source_calls, ["wavelinux.fx.mic.source"])

    def test_check_invariants_flags_effect_mismatch(self):
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    fx=FxSpec(effects=["rnnoise", "eq"], generation=2),
                )
            },
        )
        observed = ObservedState(
            present_node_names={"mic"},
            default_source="wavelinux.fx.mic.source",
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            fx_params_by_channel={"mic": {}},
            source_names={"wavelinux.fx.mic.source"},
        )

        health = RuntimeExecutor._check_invariants(desired, observed)

        self.assertEqual(health["mic"], "fx_effects_mismatch")

    def test_check_invariants_flags_param_mismatch(self):
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    fx=FxSpec(
                        effects=["rnnoise"],
                        params_map={"rnnoise": {"wet": 0.8}},
                        generation=2,
                    ),
                )
            },
        )
        observed = ObservedState(
            present_node_names={"mic"},
            default_source="wavelinux.fx.mic.source",
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            fx_params_by_channel={"mic": {"rnnoise": {"wet": 0.4}}},
            source_names={"wavelinux.fx.mic.source"},
        )

        health = RuntimeExecutor._check_invariants(desired, observed)

        self.assertEqual(health["mic"], "fx_params_mismatch")

    def test_execute_without_actions_reuses_observed_state(self):
        executor = RuntimeExecutor(FakeAdapter(FakeEngine()), DummyDiagnostics())
        desired = DesiredState()
        observed = ObservedState(present_node_names={"mic"})

        def fail_observe(_desired):
            raise AssertionError("observe should not be called for no-op execution")

        executor.observe = fail_observe

        result = executor.execute([], desired_state=desired, observed_state=observed)

        self.assertIs(result, observed)

    def test_execute_set_submix_state_skips_reobserve_and_updates_view_optimistically(self):
        executor = RuntimeExecutor(FakeAdapter(FakeEngine()), DummyDiagnostics())
        desired = DesiredState(selected_mic="mic")
        observed = ObservedState(
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="Mic",
                    media_class="Audio/Source",
                    label="Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                    monitor_volume=1.0,
                    monitor_mute=False,
                    stream_volume=1.0,
                    stream_mute=False,
                )
            ],
            present_node_names={"mic"},
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
        )

        def fail_observe(_desired):
            raise AssertionError("observe should not run for optimistic submix writes")

        executor.observe = fail_observe

        result = executor.execute(
            [Action("set_submix_state", {
                "node_id": "55",
                "mix_name": "Monitor",
                "volume": 0.33,
                "mute": True,
            })],
            desired_state=desired,
            observed_state=observed,
        )

        self.assertIs(result, observed)
        self.assertEqual(result.mic_inputs[0].monitor_volume, 0.33)
        self.assertTrue(result.mic_inputs[0].monitor_mute)

    def test_observe_uses_fx_source_for_meter_when_effects_are_active(self):
        executor = RuntimeExecutor(FakeAdapter(FakeEngine()), DummyDiagnostics())
        desired = self._desired()

        observed = executor.observe(desired)

        self.assertEqual(len(observed.mic_inputs), 1)
        self.assertEqual(observed.mic_inputs[0].meter_source, "wavelinux.fx.mic.source")

    def test_check_invariants_reports_missing_submix_route(self):
        desired = DesiredState(selected_mic="mic")
        observed = ObservedState(
            present_node_names={"mic"},
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="Mic",
                    media_class="Audio/Source",
                    label="Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            submix_owner_by_channel={"mic": {"Monitor": None, "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": False, "Stream": True}},
        )

        health = RuntimeExecutor._check_invariants(desired, observed)

        self.assertEqual(health.get("mic"), "submix_monitor_missing")

    def test_check_invariants_reports_fx_source_absent_from_graph(self):
        desired = DesiredState(
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=1),
                )
            }
        )
        observed = ObservedState(
            present_node_names={"mic"},
            source_names={"mic"},
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
        )

        health = RuntimeExecutor._check_invariants(desired, observed)

        self.assertEqual(health.get("mic"), "fx_source_not_present")

    def test_check_invariants_reports_duplicate_fx_sources(self):
        desired = DesiredState(
            channels={
                "mic_a": ChannelSpec(node_name="mic_a", fx=FxSpec(effects=["rnnoise"])),
                "mic_b": ChannelSpec(node_name="mic_b", fx=FxSpec(effects=["rnnoise"])),
            }
        )
        observed = ObservedState(
            present_node_names={"mic_a", "mic_b"},
            source_names={"mic_a", "mic_b", "wavelinux.fx.shared.source"},
            fx_sources_by_channel={
                "mic_a": "wavelinux.fx.shared.source",
                "mic_b": "wavelinux.fx.shared.source",
            },
            fx_effects_by_channel={"mic_a": ["rnnoise"], "mic_b": ["rnnoise"]},
            submix_owner_by_channel={
                "mic_a": {"Monitor": "11", "Stream": "12"},
                "mic_b": {"Monitor": "13", "Stream": "14"},
            },
            submix_live_by_channel={
                "mic_a": {"Monitor": True, "Stream": True},
                "mic_b": {"Monitor": True, "Stream": True},
            },
        )

        health = RuntimeExecutor._check_invariants(desired, observed)

        self.assertEqual(health.get("mic_a"), "duplicate_fx_source")
        self.assertEqual(health.get("mic_b"), "duplicate_fx_source")

    def test_check_invariants_reports_default_source_mismatch_for_fx_mic(self):
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=3),
                )
            },
        )
        observed = ObservedState(
            default_source="mic",
            present_node_names={"mic"},
            source_names={"mic", "wavelinux.fx.mic.source"},
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="Mic",
                    media_class="Audio/Source",
                    label="Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
        )

        health = RuntimeExecutor._check_invariants(desired, observed)

        self.assertEqual(health.get("mic"), "default_source_mismatch")

    def test_build_app_views_reuses_parsed_volume_when_present(self):
        class AppEngine:
            def __init__(self):
                self.volume_calls = []

            def get_sink_input_volume(self, sink_input_index):
                self.volume_calls.append(sink_input_index)
                return 0.25

        engine = AppEngine()

        views = RuntimeExecutor._build_app_views(engine, [
            {
                "app_name": "Browser",
                "index": "42",
                "sink": "alsa_output.pci-1",
                "volume": 0.73,
            }
        ])

        self.assertEqual(len(views), 1)
        self.assertEqual(views[0].current_volume, 0.73)
        self.assertEqual(engine.volume_calls, [])

    def test_build_app_views_falls_back_to_engine_volume_probe_when_missing(self):
        class AppEngine:
            def __init__(self):
                self.volume_calls = []

            def get_sink_input_volume(self, sink_input_index):
                self.volume_calls.append(sink_input_index)
                return 0.61

        engine = AppEngine()

        views = RuntimeExecutor._build_app_views(engine, [
            {
                "app_name": "Browser",
                "index": "42",
                "sink": "alsa_output.pci-1",
            }
        ])

        self.assertEqual(len(views), 1)
        self.assertEqual(views[0].current_volume, 0.61)
        self.assertEqual(engine.volume_calls, ["42"])


if __name__ == "__main__":
    unittest.main()
