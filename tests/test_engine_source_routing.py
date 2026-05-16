import unittest

from engine.source_routing import (
    move_known_source_outputs,
    snapshot_external_source_outputs,
    snapshot_submix_bindings,
)
from pipewire_engine import PipeWireEngine


class EngineSourceRoutingTests(unittest.TestCase):
    def _engine(self):
        engine = PipeWireEngine.__new__(PipeWireEngine)
        engine.submix_sources = {}
        engine.submix_loopbacks = {}
        engine.submix_state_cache = {}
        return engine

    def test_snapshot_external_source_outputs_filters_excluded_modules(self):
        engine = self._engine()
        outputs = {
            ("pactl", "list", "short", "sources"): (
                "33\talsa_input.real_mic\tmodule-alsa-card.c\t...\n"
                "55\toutput.wavelinux.fx.mic.source\tmodule-null-sink.c\t...\n"
            ),
            ("pactl", "list", "short", "source-outputs"): (
                "91\t33\tprotocol-native.c\t...\n"
                "92\t44\tprotocol-native.c\t...\n"
                "93\t55\tprotocol-native.c\t...\n"
            ),
            ("pactl", "list", "source-outputs"): (
                "Source Output #91\n"
                "\tOwner Module: 201\n"
                "\tProperties:\n"
                "\t\ttarget.object = \"alsa_input.real_mic\"\n"
                "Source Output #92\n"
                "\tOwner Module: 202\n"
                "\tProperties:\n"
                "\t\ttarget.object = \"alsa_input.real_mic\"\n"
                "Source Output #93\n"
                "\tOwner Module: 203\n"
                "\tProperties:\n"
                "\t\ttarget.object = \"output.wavelinux.fx.mic.source\"\n"
            ),
        }
        engine._run = lambda cmd, timeout=2: outputs.get(tuple(cmd), "")
        engine.resolve_source_name = lambda source_name, snap=None: source_name

        result = snapshot_external_source_outputs(
            engine,
            "alsa_input.real_mic",
            exclude_modules=["201"],
        )

        self.assertEqual(result, ["92"])

    def test_move_known_source_outputs_waits_for_rebind(self):
        engine = self._engine()
        run_calls = []
        locations = iter(
            [
                {"901": "alsa_input.real_mic"},
                {"901": "wavelinux.fx.mic.source"},
            ]
        )

        engine._wait_source_visible = (
            lambda source_name, attempts=20, delay=0.05: "wavelinux.fx.mic.source"
        )
        engine._source_output_locations = lambda: next(
            locations,
            {"901": "wavelinux.fx.mic.source"},
        )
        engine._run = lambda cmd, timeout=2: run_calls.append(list(cmd)) or ""

        success = move_known_source_outputs(
            engine,
            ["901"],
            "alsa_input.real_mic",
            "wavelinux.fx.mic.source",
            attempts=2,
            delay=0,
        )

        self.assertTrue(success)
        move_calls = [
            cmd for cmd in run_calls
            if cmd[:2] == ["pactl", "move-source-output"]
        ]
        self.assertEqual(
            move_calls,
            [
                ["pactl", "move-source-output", "901", "wavelinux.fx.mic.source"],
                ["pactl", "move-source-output", "901", "output.wavelinux.fx.mic.source"],
            ],
        )

    def test_move_known_source_outputs_accepts_alias_match_for_rebound_source(self):
        engine = self._engine()
        run_calls = []

        engine._wait_source_visible = (
            lambda source_name, attempts=20, delay=0.05: "wavelinux.fx.mic.source"
        )
        engine._source_output_locations = lambda: {
            "901": "output.wavelinux.fx.mic.source",
        }
        engine._run = lambda cmd, timeout=2: run_calls.append(list(cmd)) or ""

        success = move_known_source_outputs(
            engine,
            ["901"],
            "alsa_input.real_mic",
            "wavelinux.fx.mic.source",
            attempts=1,
            delay=0,
        )

        self.assertTrue(success)
        move_calls = [
            cmd for cmd in run_calls
            if cmd[:2] == ["pactl", "move-source-output"]
        ]
        self.assertEqual(
            move_calls,
            [["pactl", "move-source-output", "901", "wavelinux.fx.mic.source"]],
        )

    def test_move_known_source_outputs_tries_bare_alias_after_output_alias_fails(self):
        engine = self._engine()
        run_calls = []
        moved = {"value": False}

        engine._wait_source_visible = (
            lambda source_name, attempts=20, delay=0.05: "output.wavelinux.fx.mic.source"
        )

        def fake_locations():
            if moved["value"]:
                return {"901": "output.wavelinux.fx.mic.source"}
            return {"901": "alsa_input.real_mic"}

        def fake_run(cmd, timeout=2):
            run_calls.append(list(cmd))
            if cmd[-1] == "wavelinux.fx.mic.source":
                moved["value"] = True
            return ""

        engine._source_output_locations = fake_locations
        engine._run = fake_run

        success = move_known_source_outputs(
            engine,
            ["901"],
            "alsa_input.real_mic",
            "output.wavelinux.fx.mic.source",
            attempts=1,
            delay=0,
        )

        self.assertTrue(success)
        move_calls = [
            cmd for cmd in run_calls
            if cmd[:2] == ["pactl", "move-source-output"]
        ]
        self.assertEqual(
            move_calls,
            [
                ["pactl", "move-source-output", "901", "output.wavelinux.fx.mic.source"],
                ["pactl", "move-source-output", "901", "wavelinux.fx.mic.source"],
            ],
        )

    def test_snapshot_submix_bindings_copies_cached_state(self):
        engine = self._engine()
        engine.submix_sources = {
            "55->Monitor": "mic",
            "55->Stream": "fx.source",
        }
        engine.submix_loopbacks = {
            "55->Monitor": "201",
            "55->Stream": "202",
        }
        engine.submix_state_cache = {
            "55->Monitor": {"vol": 0.4, "mute": True},
        }

        bindings = snapshot_submix_bindings(engine, "mic")
        bindings["55->Monitor"]["state"]["vol"] = 0.9

        self.assertEqual(
            bindings,
            {
                "55->Monitor": {
                    "mix_name": "Monitor",
                    "module_id": "201",
                    "state": {"vol": 0.9, "mute": True},
                }
            },
        )
        self.assertEqual(engine.submix_state_cache["55->Monitor"]["vol"], 0.4)
