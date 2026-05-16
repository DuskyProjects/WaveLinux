import unittest

from tools.stress import fx_path_diagnostic


class FxPathDiagnosticTests(unittest.TestCase):
    def test_identify_loopbacks_finds_upstream_and_downstream_by_arguments(self):
        runtime = {
            "capture_target": "output.synthetic.source",
            "active_chain_sink": "wavelinux.fx.synthetic.input",
            "active_chain_source": "wavelinux.fx.synthetic.source",
            "proxy_sink_name": "wavelinux.fx.synthetic.proxy",
            "loopbacks": ["101", "102"],
        }
        modules_text = (
            "Module #101\n"
            "\tName: module-loopback\n"
            "\tArgument: source=wavelinux.fx.synthetic.source sink=wavelinux.fx.synthetic.proxy channels=1\n"
            "Module #102\n"
            "\tName: module-loopback\n"
            "\tArgument: source=output.synthetic.source sink=wavelinux.fx.synthetic.input channels=1\n"
        )

        result = fx_path_diagnostic.identify_loopbacks(runtime, modules_text)

        self.assertEqual(result["downstream"], "101")
        self.assertEqual(result["upstream"], "102")
        self.assertIn("source=output.synthetic.source", result["sections"]["102"])

    def test_section_for_owner_extracts_id_and_node_name(self):
        text = (
            "Source Output #42\n"
            "\tOwner Module: 102\n"
            "\tProperties:\n"
            "\t\tnode.name = \"input.loopback-test\"\n"
        )

        section = fx_path_diagnostic.section_for_owner(text, "Source Output #", "102")

        self.assertEqual(section["id"], "42")
        self.assertEqual(section["node_name"], "input.loopback-test")

    def test_parse_sequence_and_delays(self):
        self.assertEqual(fx_path_diagnostic.parse_sequence("a,b,a"), ("a", "b", "a"))
        self.assertEqual(fx_path_diagnostic.parse_delays("0,1.5,8"), (0.0, 1.5, 8.0))

    def test_classify_fx_path_requires_signal_at_each_fx_point(self):
        live_capture = {"bytes": 1024, "peak": 64, "rms": 3.5}
        silent_capture = {"bytes": 1024, "peak": 0, "rms": 0.0}

        result = fx_path_diagnostic.classify_fx_path({
            "raw_source": live_capture,
            "upstream_source_output_port": live_capture,
            "upstream_sink_input_port": live_capture,
            "chain_sink_monitor": silent_capture,
            "chain_source": live_capture,
            "proxy_source": live_capture,
        })

        self.assertTrue(result["raw_live"])
        self.assertTrue(result["fx_output_live"])
        self.assertFalse(result["chain_sink_monitor_live"])
        self.assertFalse(result["all_fx_points_live"])


if __name__ == "__main__":
    unittest.main()
