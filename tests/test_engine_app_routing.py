import unittest
from types import SimpleNamespace

from engine.app_routing import get_sink_inputs, parse_pactl_si_map
from engine.models import AudioNode, EngineSnapshot


class EngineAppRoutingTests(unittest.TestCase):
    def test_parse_pactl_si_map_tracks_references_and_volume(self):
        text = (
            "Sink Input #117034\n"
            "\tSink: 89634\n"
            "\tVolume: front-left: 36044 / 55% / -13.00 dB, front-right: 36044 / 55% / -13.00 dB\n"
            "\tProperties:\n"
            "\t\tnode.id = \"234\"\n"
            "\t\tobject.serial = \"117034\"\n"
            "\t\tapplication.process.id = \"203984\"\n"
            "\t\tapplication.process.binary = \"spotify\"\n"
        )

        by_node_id, by_index = parse_pactl_si_map(text)

        self.assertEqual(by_node_id["234"]["_index"], "117034")
        self.assertEqual(by_node_id["117034"]["_sink_id"], "89634")
        self.assertEqual(by_index["117034"]["volume"], 0.55)
        self.assertEqual(by_index["117034"]["pid"], "203984")
        self.assertEqual(by_index["117034"]["binary"], "spotify")

    def test_get_sink_inputs_merges_pw_dump_and_pactl_properties(self):
        processed = []

        def fake_process(current, entries, sink_id_to_name):
            processed.append((dict(current), dict(sink_id_to_name)))
            entries.append(
                {
                    "index": current.get("index"),
                    "sink": current.get("sink"),
                    "app_name": current.get("application.name"),
                    "binary": current.get("application.process.binary"),
                }
            )

        node = AudioNode(
            234,
            "audio-src",
            "Audio Src",
            "Stream/Output/Audio",
            app_name="Spotify",
            props={
                "application.name": "Spotify",
                "object.serial": "117034",
            },
        )
        engine = SimpleNamespace(
            get_all_sinks=lambda snap=None: [{"index": "89634", "name": "wavelinux_mix_monitor"}],
            _run=lambda cmd, timeout=2: "",
            _parse_pactl_si_map=parse_pactl_si_map,
            get_app_streams=lambda snap=None: [node],
            _process_sink_input=fake_process,
        )
        snap = EngineSnapshot(
            sink_inputs_text=(
                "Sink Input #117034\n"
                "\tSink: 89634\n"
                "\tProperties:\n"
                "\t\tobject.serial = \"117034\"\n"
                "\t\tapplication.process.binary = \"spotify\"\n"
            ),
            sinks=[{"index": "89634", "name": "wavelinux_mix_monitor"}],
            nodes=[node],
        )

        entries = get_sink_inputs(engine, snap=snap)

        self.assertEqual(
            entries,
            [
                {
                    "index": "117034",
                    "sink": "wavelinux_mix_monitor",
                    "app_name": "Spotify",
                    "binary": "spotify",
                }
            ],
        )
        self.assertEqual(processed[0][0]["node.id"], "234")
        self.assertEqual(processed[0][1], {"89634": "wavelinux_mix_monitor"})

    def test_get_sink_inputs_keeps_pactl_only_entries(self):
        processed = []

        def fake_process(current, entries, sink_id_to_name):
            processed.append((dict(current), dict(sink_id_to_name)))
            entries.append(
                {
                    "index": current.get("index"),
                    "sink": current.get("sink"),
                    "pid": current.get("pid"),
                }
            )

        engine = SimpleNamespace(
            get_all_sinks=lambda snap=None: [{"index": "7", "name": "fallback"}],
            _run=lambda cmd, timeout=2: "",
            _parse_pactl_si_map=parse_pactl_si_map,
            get_app_streams=lambda snap=None: [],
            _process_sink_input=fake_process,
        )
        snap = EngineSnapshot(
            sink_inputs_text=(
                "Sink Input #42\n"
                "\tSink: 7\n"
                "\tProperties:\n"
                "\t\tapplication.process.id = \"999\"\n"
            ),
            sinks=[{"index": "7", "name": "fallback"}],
        )

        entries = get_sink_inputs(engine, snap=snap)

        self.assertEqual(entries, [{"index": "42", "sink": "fallback", "pid": "999"}])
        self.assertEqual(processed[0][0]["sink_id"], "7")
