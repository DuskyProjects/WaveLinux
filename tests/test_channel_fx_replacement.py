import unittest

from pipewire_engine import PipeWireEngine


class ChannelFxReplacementTests(unittest.TestCase):
    def _build_engine(self):
        engine = PipeWireEngine.__new__(PipeWireEngine)
        engine.channel_fx = {}
        engine._safe_channel_key = lambda node: "safe_key"
        engine._ordered_chain = lambda effects: effects
        engine.effect_available = lambda fid: False
        return engine

    def test_replacement_detaches_state_before_teardown_and_failure(self):
        engine = self._build_engine()
        node = "mic0"
        engine.channel_fx[node] = {
            "safe_key": "safe_key",
            "effects": ["rnnoise"],
            "procs": ["chain_safe_key"],
            "loopbacks": [123],
            "params": {"rnnoise": {"strength": 0.9}},
        }

        teardown_calls = []

        def fake_teardown(node_name, info, replacement_safe_key=None):
            teardown_calls.append((node_name, info, replacement_safe_key))
            self.assertNotIn(node_name, engine.channel_fx)

        engine._teardown_channel_fx_info = fake_teardown

        result = engine._set_channel_fx_inner(node, capture_target="source", effects=["rnnoise"], params_map={})

        self.assertIsNone(result)
        self.assertNotIn(node, engine.channel_fx)
        self.assertEqual(len(teardown_calls), 1)
        self.assertEqual(teardown_calls[0][0], node)
        self.assertEqual(teardown_calls[0][2], "safe_key")
        self.assertEqual(teardown_calls[0][1]["procs"], ["chain_safe_key"])


if __name__ == "__main__":
    unittest.main()
