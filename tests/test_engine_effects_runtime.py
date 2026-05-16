import unittest

import engine.effects_runtime as effects_runtime


class _EffectsEngine:
    _EFFECT_PARAMS = effects_runtime.EFFECT_PARAMS

    def __init__(self):
        self.rnnoise_calls = []
        self.channel_fx = {}
        self.submix_sources = {}
        self.submix_loopbacks = {}
        self.teardown_calls = []
        self.run_calls = []
        self.invalidated = False

    def _render_control_block(self, params):
        return effects_runtime.render_control_block(params)

    def ladspa_plugin_path(self, plugin):
        return f"/plugins/{plugin}.so"

    def start_rnnoise(self, channel_key, params=None):
        self.rnnoise_calls.append((channel_key, params))
        return "rnnoise-started"

    def _teardown_fx_plumbing(self, info):
        self.teardown_calls.append(info)

    def invalidate_snapshot(self):
        self.invalidated = True

    def _run(self, cmd, timeout=2):
        self.run_calls.append((list(cmd), timeout))
        return "ok"


class _EffectsClass:
    _CHAIN_ORDER = effects_runtime.CHAIN_ORDER


class EffectsRuntimeModuleTests(unittest.TestCase):
    def test_resolved_params_clamps_and_ignores_invalid_values(self):
        engine = _EffectsEngine()

        resolved = effects_runtime.resolved_params(
            engine,
            "rnnoise",
            {"VAD Threshold (%)": 500.0, "VAD Grace Period (ms)": "oops"},
        )

        self.assertEqual(resolved["VAD Threshold (%)"], 100.0)
        self.assertEqual(resolved["VAD Grace Period (ms)"], 200.0)

    def test_ordered_chain_uses_canonical_signal_flow(self):
        ordered = effects_runtime.ordered_chain(
            _EffectsClass,
            ["limiter", "gate", "rnnoise"],
        )

        self.assertEqual(ordered, ["rnnoise", "gate", "limiter"])

    def test_apply_effect_routes_rnnoise_to_start_rnnoise(self):
        engine = _EffectsEngine()

        result = effects_runtime.apply_effect(
            engine,
            "mic",
            "rnnoise",
            params={"VAD Threshold (%)": 75.0},
        )

        self.assertEqual(result, "rnnoise-started")
        self.assertEqual(engine.rnnoise_calls, [("mic", {"VAD Threshold (%)": 75.0})])

    def test_clear_channel_fx_info_unloads_matching_submixes(self):
        engine = _EffectsEngine()
        info = {"source": "wavelinux.fx.mic.source", "procs": ["proc-a"], "loopbacks": []}
        engine.submix_sources = {
            "55->Monitor": "wavelinux.fx.mic.source",
            "55->Stream": "other.source",
        }
        engine.submix_loopbacks = {
            "55->Monitor": "301",
            "55->Stream": "302",
        }

        cleared = effects_runtime.clear_channel_fx_info(engine, info)

        self.assertTrue(cleared)
        self.assertEqual(engine.run_calls, [(["pactl", "unload-module", "301"], 2)])
        self.assertNotIn("55->Monitor", engine.submix_sources)
        self.assertNotIn("55->Monitor", engine.submix_loopbacks)
        self.assertEqual(engine.teardown_calls, [info])
        self.assertTrue(engine.invalidated)


if __name__ == "__main__":
    unittest.main()
