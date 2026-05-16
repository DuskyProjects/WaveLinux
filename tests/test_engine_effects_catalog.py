import unittest

import engine.effects_catalog as effects_catalog


class _CatalogEngine:
    _EFFECT_PARAMS = effects_catalog.EFFECT_PARAMS
    _CHAIN_ORDER = effects_catalog.CHAIN_ORDER

    def _render_control_block(self, params):
        return effects_catalog.render_control_block(params)

    def ladspa_plugin_path(self, plugin):
        return f"/plugins/{plugin}.so"

    def _ladspa_node(self, name, plugin, label, values):
        return effects_catalog.ladspa_node(self, name, plugin, label, values)

    def _resolved_params(self, effect_id, overrides):
        return effects_catalog.resolved_params(self, effect_id, overrides)

    def _effect_stage_blocks(self, effect_id, values, stage_idx):
        return effects_catalog.effect_stage_blocks(self, effect_id, values, stage_idx)


class EffectsCatalogModuleTests(unittest.TestCase):
    def test_build_filter_graph_renders_highpass_builtin(self):
        engine = _CatalogEngine()

        graph = effects_catalog.build_filter_graph(engine, "highpass", {"Freq": 80.0})

        self.assertIn("bq_highpass", graph)
        self.assertIn('"Freq" = 80.000', graph)

    def test_build_unified_filter_graph_skips_unknown_effects(self):
        engine = _CatalogEngine()

        graph, used = effects_catalog.build_unified_filter_graph(
            engine,
            ["unknown", "rnnoise", "limiter"],
            {"rnnoise": {}, "limiter": {}},
        )

        self.assertEqual(used, ["rnnoise", "limiter"])
        self.assertIn('inputs = [ "s1_rnnoise:Input" ]', graph)
        self.assertIn('outputs = [ "s2_lim_out:Out" ]', graph)

    def test_ordered_chain_uses_canonical_signal_flow(self):
        ordered = effects_catalog.ordered_chain(
            _CatalogEngine,
            ["limiter", "gate", "rnnoise"],
        )

        self.assertEqual(ordered, ["rnnoise", "gate", "limiter"])


if __name__ == "__main__":
    unittest.main()
