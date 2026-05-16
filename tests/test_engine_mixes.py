import unittest
from types import SimpleNamespace

from engine.mixes import (
    create_output_mix,
    move_app_streams_off_managed_sinks,
    remove_output_mix,
)
from engine.models import OutputMix


class EngineMixesTests(unittest.TestCase):
    def test_create_output_mix_tracks_sink_and_resolved_source(self):
        created = []
        run_calls = []
        engine = SimpleNamespace(
            output_mixes={},
            virtual_sink_modules={"wavelinux_mix_stream": "501"},
            _sanitize_channel_name=lambda name: ("Stream", "stream"),
            _branding_label=lambda name: "WaveLinux-Stream",
            create_virtual_sink=lambda name, custom_name=None: created.append((name, custom_name)) or custom_name,
            _find_module_by_arg=lambda pattern: None,
            _run=lambda cmd, timeout=2: run_calls.append(list(cmd)) or "601",
            _wait_source_visible=lambda source_name, attempts=20, delay=0.05: "output.wavelinux_src_stream",
            resolve_source_name=lambda source_name: source_name,
        )

        mix = create_output_mix(engine, "Stream")

        self.assertEqual(created, [("Stream", "wavelinux_mix_stream")])
        self.assertEqual(mix.sink_name, "wavelinux_mix_stream")
        self.assertEqual(mix.sink_module_id, "501")
        self.assertEqual(mix.source_module_id, "601")
        self.assertEqual(mix.source_name, "output.wavelinux_src_stream")
        self.assertIs(engine.output_mixes["Stream"], mix)
        self.assertEqual(run_calls[0][:4], ["pactl", "load-module", "module-virtual-source", "source_name=wavelinux_src_stream"])

    def test_remove_output_mix_unloads_mix_and_submix_modules(self):
        calls = []
        mix = OutputMix("Monitor", sink_module_id="501", sink_name="wavelinux_mix_monitor")
        mix.source_module_id = "601"
        engine = SimpleNamespace(
            output_mixes={"Monitor": mix},
            loopback_modules={"Monitor->bluez_output.headset": "701"},
            submix_loopbacks={"55->Monitor": "801", "66->Stream": "802"},
            submix_sources={"55->Monitor": "mic", "66->Stream": "music"},
            _run=lambda cmd, timeout=2: calls.append(list(cmd)) or "",
        )

        removed = remove_output_mix(engine, "Monitor")

        self.assertTrue(removed)
        self.assertEqual(engine.output_mixes, {})
        self.assertEqual(engine.loopback_modules, {})
        self.assertEqual(engine.submix_loopbacks, {"66->Stream": "802"})
        self.assertEqual(engine.submix_sources, {"66->Stream": "music"})
        self.assertEqual(
            calls,
            [
                ["pactl", "unload-module", "601"],
                ["pactl", "unload-module", "501"],
                ["pactl", "unload-module", "701"],
                ["pactl", "unload-module", "801"],
            ],
        )

    def test_move_app_streams_off_managed_sinks_only_moves_wavelinux_routes(self):
        moves = []
        engine = SimpleNamespace(
            resolve_hardware_sink_name=lambda sink_name, snap=None: sink_name,
            _is_internal_node_name=lambda sink_name: False,
            get_sink_inputs=lambda snap=None: [
                {"index": "91", "sink": "wavelinux_music"},
                {"index": "92", "sink": "alsa_output.speakers"},
                {"index": "", "sink": "wavelinux_stream"},
            ],
            move_app_to_sink=lambda sink_input_index, sink_name: moves.append((sink_input_index, sink_name)),
        )

        moved = move_app_streams_off_managed_sinks(engine, "bluez_output.headset")

        self.assertEqual(moves, [("91", "bluez_output.headset")])
        self.assertEqual(moved, [("91", "wavelinux_music", "bluez_output.headset")])
