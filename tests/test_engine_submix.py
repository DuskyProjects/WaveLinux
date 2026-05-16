import unittest
from types import SimpleNamespace

from engine.models import OutputMix
from engine.submix import (
    build_loopback_index,
    create_submix_replacement,
    remove_node_routing,
    wait_load_loopback,
)


class EngineSubmixTests(unittest.TestCase):
    def test_build_loopback_index_extracts_source_sink_pairs(self):
        modules_text = "\n".join(
            [
                "Module #101",
                "Name: module-loopback",
                "Argument: source=alsa_input.real_mic sink=wavelinux_mix_monitor latency_msec=20",
                "Module #102",
                "Name: module-loopback",
                "Argument: source=wavelinux_music.monitor sink=wavelinux_mix_stream latency_msec=20",
            ]
        )

        index = build_loopback_index(modules_text)

        self.assertEqual(
            index,
            {
                ("alsa_input.real_mic", "wavelinux_mix_monitor"): "101",
                ("wavelinux_music.monitor", "wavelinux_mix_stream"): "102",
            },
        )

    def test_create_submix_replacement_preserves_cached_state(self):
        invalidations = []
        apply_calls = []
        engine = SimpleNamespace(
            output_mixes={"Stream": OutputMix("Stream", sink_name="wavelinux_mix_stream")},
            submix_loopbacks={"55->Stream": "401"},
            submix_state_cache={},
            _pending_submix_state_reapply=set(),
            _find_loopback_for=lambda source_name, sink_name: "401",
            _load_loopback_module=lambda source_name, sink_name: self.fail("should reuse loopback"),
            invalidate_snapshot=lambda: invalidations.append("invalidate"),
            _module_is_alive=lambda module_id: True,
            _clamp=lambda value: max(0.0, min(float(value), 1.0)),
            _apply_loopback_state=lambda module_id, state: apply_calls.append((module_id, dict(state))) or False,
        )

        module_id = create_submix_replacement(
            engine,
            "wavelinux.fx.mic.source",
            "Stream",
            initial_state={"vol": 0.33, "mute": True},
        )

        self.assertEqual(module_id, "401")
        self.assertEqual(
            engine.submix_state_cache["55->Stream"],
            {"vol": 0.33, "mute": True},
        )
        self.assertEqual(apply_calls, [("401", {"vol": 0.33, "mute": True})])
        self.assertIn("55->Stream", engine._pending_submix_state_reapply)
        self.assertEqual(invalidations, [])

    def test_remove_node_routing_clears_tracking_and_unloads_modules(self):
        calls = []
        engine = SimpleNamespace(
            submix_loopbacks={"55->Monitor": "201", "66->Stream": "202"},
            submix_sources={"55->Monitor": "mic", "66->Stream": "music"},
            submix_state_cache={"55->Monitor": {"vol": 0.4}},
            _pending_submix_state_reapply={"55->Monitor", "66->Stream"},
            _run=lambda cmd, timeout=2: calls.append(list(cmd)) or "",
        )

        remove_node_routing(engine, "55")

        self.assertEqual(engine.submix_loopbacks, {"66->Stream": "202"})
        self.assertEqual(engine.submix_sources, {"66->Stream": "music"})
        self.assertEqual(engine.submix_state_cache, {})
        self.assertEqual(engine._pending_submix_state_reapply, {"66->Stream"})
        self.assertEqual(calls, [["pactl", "unload-module", "201"]])

    def test_wait_load_loopback_normalizes_new_sink_input(self):
        calls = []
        sink_inputs = iter([None, "88"])
        engine = SimpleNamespace(
            _run=lambda cmd, timeout=2: calls.append(list(cmd)) or "301",
            _sink_input_for_module=lambda module_id: next(sink_inputs, "88"),
        )

        module_id = wait_load_loopback(
            engine,
            "mic",
            "wavelinux.fx.chain.input",
            attempts=1,
            delay=0,
            channels=1,
            channel_map="mono",
        )

        self.assertEqual(module_id, "301")
        self.assertEqual(
            calls,
            [
                [
                    "pactl",
                    "load-module",
                    "module-loopback",
                    "source=mic",
                    "sink=wavelinux.fx.chain.input",
                    "latency_msec=20",
                    "adjust_time=0",
                    "channels=1",
                    "channel_map=mono",
                ],
                ["pactl", "set-sink-input-volume", "88", "100%"],
                ["pactl", "set-sink-input-mute", "88", "0"],
            ],
        )
