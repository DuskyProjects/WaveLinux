import unittest

from engine.fx_graph import fx_result, teardown_fx_plumbing, unload_submix_replacements


class _FakeEngine:
    def __init__(self):
        self.calls = []

    def _run(self, cmd, timeout=2):
        self.calls.append(("run", list(cmd), timeout))
        return "ok"

    def stop_rnnoise(self, key):
        self.calls.append(("stop_rnnoise", key))


class FxGraphModuleTests(unittest.TestCase):
    def test_fx_result_preserves_status_fields(self):
        result = fx_result(
            True,
            active_source="output.wavelinux.fx.mic.source",
            kept_source="output.wavelinux.fx.mic.source",
            rolled_back=False,
            failure_stage=None,
            message="FX chain active",
        )

        self.assertEqual(
            result,
            {
                "success": True,
                "active_source": "output.wavelinux.fx.mic.source",
                "kept_source": "output.wavelinux.fx.mic.source",
                "rolled_back": False,
                "failure_stage": None,
                "message": "FX chain active",
            },
        )

    def test_teardown_fx_plumbing_unloads_loopbacks_and_stops_processes(self):
        engine = _FakeEngine()

        teardown_fx_plumbing(engine, {"loopbacks": ["101", "102"], "procs": ["chain_a"]})

        self.assertEqual(
            engine.calls,
            [
                ("run", ["pactl", "unload-module", "101"], 2),
                ("run", ["pactl", "unload-module", "102"], 2),
                ("stop_rnnoise", "chain_a"),
            ],
        )

    def test_unload_submix_replacements_skips_missing_module_ids(self):
        engine = _FakeEngine()

        unload_submix_replacements(
            engine,
            {
                "55->Monitor": {"module_id": "201"},
                "55->Stream": {"module_id": None},
            },
        )

        self.assertEqual(
            engine.calls,
            [("run", ["pactl", "unload-module", "201"], 2)],
        )


if __name__ == "__main__":
    unittest.main()
