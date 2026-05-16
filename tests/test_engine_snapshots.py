import json
import unittest
from types import SimpleNamespace

from engine.models import EngineSnapshot
from engine.snapshots import (
    create_snapshot,
    invalidate_snapshot,
    is_internal_node_name,
    parse_nodes,
    parse_sink_descriptions,
    parse_sinks_state,
    parse_sources_state,
)


class EngineSnapshotHelpersTests(unittest.TestCase):
    def test_parse_sink_descriptions_and_state(self):
        sinks_text = """
Sink #1
    Name: bluez_output.AA_BB_CC_DD_EE_FF.1
    Description: Sony WH-1000XM4
    Mute: yes
    Volume: front-left: 65536 / 100% / 0.00 dB, front-right: 65536 / 100% / 0.00 dB
"""

        self.assertEqual(
            parse_sink_descriptions(sinks_text),
            {"bluez_output.AA_BB_CC_DD_EE_FF.1": "Sony WH-1000XM4"},
        )
        self.assertEqual(
            parse_sinks_state(sinks_text),
            {"bluez_output.AA_BB_CC_DD_EE_FF.1": (1.0, True)},
        )

    def test_parse_sources_state_extracts_volume_and_mute(self):
        sources_text = """
Source #5
    Name: alsa_input.usb_mic
    Mute: no
    Volume: front-left: 40632 / 62% / -12.00 dB, front-right: 40632 / 62% / -12.00 dB
"""

        self.assertEqual(
            parse_sources_state(sources_text),
            {"alsa_input.usb_mic": (0.62, False)},
        )

    def test_parse_nodes_merges_client_props_and_filters_non_audio_nodes(self):
        pw_dump = json.dumps(
            [
                {
                    "id": 11,
                    "type": "PipeWire:Interface:Client",
                    "info": {
                        "props": {
                            "application.name": "Recorder",
                            "application.process.binary": "obs",
                        }
                    },
                },
                {
                    "id": 55,
                    "type": "PipeWire:Interface:Node",
                    "info": {
                        "props": {
                            "client.id": "11",
                            "node.name": "alsa_input.usb_mic",
                            "node.description": "USB Mic",
                            "media.class": "Audio/Source",
                        }
                    },
                },
                {
                    "id": 56,
                    "type": "PipeWire:Interface:Node",
                    "info": {
                        "props": {
                            "node.name": "browser_stream",
                            "node.description": "Browser",
                            "media.class": "Stream/Output/Audio",
                        }
                    },
                },
                {
                    "id": 57,
                    "type": "PipeWire:Interface:Node",
                    "info": {
                        "props": {
                            "node.name": "camera0",
                            "media.class": "Video/Source",
                        }
                    },
                },
            ]
        )
        engine = SimpleNamespace(_run=lambda cmd, timeout=4: pw_dump)

        nodes = parse_nodes(engine)

        self.assertEqual([node.name for node in nodes], ["alsa_input.usb_mic", "browser_stream"])
        self.assertEqual(nodes[0].app_name, "Recorder")
        self.assertEqual(nodes[0].props["application.process.binary"], "obs")

    def test_create_snapshot_caches_until_forced_refresh(self):
        run_calls = []
        gc_calls = []
        reapply_calls = []

        def fake_run(cmd, timeout=2):
            run_calls.append(list(cmd))
            return "payload"

        engine = SimpleNamespace(
            _SNAPSHOT_TTL=60.0,
            _run=fake_run,
            _parse_nodes=lambda: ["node"],
            _parse_short_sinks=lambda: [{"index": "1", "name": "sink"}],
            reap_dead_processes=lambda: gc_calls.append("gc"),
            _reapply_submix_state_cache=lambda: reapply_calls.append("reapply"),
            _pending_submix_state_reapply=set(),
        )

        first = create_snapshot(engine)
        second = create_snapshot(engine)
        engine._pending_submix_state_reapply = {"55->Stream"}
        third = create_snapshot(engine)

        self.assertIsInstance(first, EngineSnapshot)
        self.assertIs(first, second)
        self.assertIsNot(first, third)
        self.assertEqual(len(run_calls), 10)
        self.assertEqual(gc_calls, ["gc", "gc"])
        self.assertEqual(reapply_calls, ["reapply"])

    def test_invalidate_snapshot_clears_cached_state(self):
        engine = SimpleNamespace(
            _snapshot_cache=object(),
            _snapshot_cache_at=12.5,
        )

        invalidate_snapshot(engine)

        self.assertIsNone(engine._snapshot_cache)
        self.assertEqual(engine._snapshot_cache_at, 0.0)

    def test_is_internal_node_name_keeps_stress_fx_sources_selectable(self):
        self.assertFalse(is_internal_node_name("wavelinux_stress_fx_a.source"))
        self.assertFalse(is_internal_node_name("output.wavelinux_stress_fx_a.source"))
        self.assertFalse(is_internal_node_name("input.wavelinux_stress_fx_a.source"))
        self.assertTrue(is_internal_node_name("output.wavelinux.fx.real_mic.source"))
