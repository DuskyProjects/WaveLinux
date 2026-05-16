import unittest

from engine.models import AudioNode, EngineSnapshot, OutputMix


class EngineModelsTests(unittest.TestCase):
    def test_audio_node_initializes_runtime_defaults(self):
        node = AudioNode(55, "mic", "Mic", "Audio/Source", app_name="Recorder")

        self.assertEqual(node.pw_id, 55)
        self.assertEqual(node.name, "mic")
        self.assertEqual(node.app_name, "Recorder")
        self.assertEqual(node.props, {})
        self.assertEqual(node.volume, 1.0)
        self.assertFalse(node.muted)

    def test_output_mix_initializes_expected_fields(self):
        mix = OutputMix("Monitor", sink_module_id=42, sink_name="wavelinux_mix_monitor")

        self.assertEqual(mix.name, "Monitor")
        self.assertEqual(mix.sink_module_id, 42)
        self.assertEqual(mix.sink_name, "wavelinux_mix_monitor")
        self.assertEqual(mix.channel_volumes, {})
        self.assertEqual(mix.channel_mutes, {})
        self.assertEqual(mix.master_volume, 1.0)
        self.assertFalse(mix.master_muted)

    def test_engine_snapshot_defaults_to_empty_cached_views(self):
        snap = EngineSnapshot()

        self.assertEqual(snap.modules_text, "")
        self.assertEqual(snap.short_modules_text, "")
        self.assertEqual(snap.sink_inputs_text, "")
        self.assertEqual(snap.sinks_text, "")
        self.assertEqual(snap.sources_text, "")
        self.assertEqual(snap.nodes, [])
        self.assertEqual(snap.sinks, [])
        self.assertIsNone(snap._loopback_index)


if __name__ == "__main__":
    unittest.main()
