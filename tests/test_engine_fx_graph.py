import unittest
from unittest import mock

from engine.fx_graph import (
    apply_channel_fx_transaction,
    fx_result,
    reprime_channel_fx_capture,
    teardown_fx_plumbing,
    unload_submix_replacements,
)


class _FakeEngine:
    def __init__(self):
        self.calls = []

    def _run(self, cmd, timeout=2):
        self.calls.append(("run", list(cmd), timeout))
        return "ok"

    def stop_rnnoise(self, key):
        self.calls.append(("stop_rnnoise", key))


class _ApplyEngine:
    def __init__(self):
        self.channel_fx = {}
        self.loopback_calls = []
        self.default_source = "alsa_input.test"

    def _ordered_chain(self, effects):
        return list(effects or [])

    def effect_available(self, effect_id):
        return True

    def _is_inline_fx_info(self, info):
        return False

    def _safe_channel_key(self, node_name):
        return "mic"

    def _build_unified_chain_config(self, safe_key, ordered, params_map, stamp):
        return ("config.conf", "chain.input", "chain.source", list(ordered))

    def _fx_log_path(self, safe_key, effect_id):
        return "fx.log"

    def _spawn_fx(self, config_path, log_path, proc_key):
        return True

    def get_default_source(self):
        return self.default_source

    def _snapshot_submix_bindings(self, effective_source):
        return {}

    def snapshot_external_source_outputs(self, effective_source, exclude_modules=None):
        return []

    def _wait_source_visible(self, source_name, attempts=20, delay=0.05):
        return source_name

    def _sink_visible(self, sink_name):
        return True

    def _ensure_fx_proxy(self, safe_key):
        return {
            "sink_name": "proxy.sink",
            "sink_module_id": "901",
            "source_name": "proxy.source",
            "source_request_name": "proxy.source",
            "source_module_id": "902",
        }

    def _wait_load_loopback(self, source, sink, latency_msec=20, attempts=20, delay=0.1,
                            channels=None, channel_map=None, source_dont_move=False,
                            sink_dont_move=False):
        self.loopback_calls.append(
            (source, sink, channels, channel_map, source_dont_move, sink_dont_move)
        )
        return str(700 + len(self.loopback_calls))

    def _module_is_alive(self, module_id):
        return True

    def _create_submix_replacement(self, source_name, mix_name, initial_state=None):
        return None

    def _source_names_match(self, left, right):
        return str(left or "").strip() == str(right or "").strip()

    def set_default_source(self, source_name):
        self.default_source = source_name
        return True

    def _move_known_source_outputs(self, source_output_ids, from_source, to_source):
        return True

    def _commit_submix_replacements(self, replacements, *, new_source):
        return None

    def _teardown_fx_plumbing(self, info):
        return None

    def invalidate_snapshot(self):
        return None

    def stop_rnnoise(self, proc_key):
        return None

    def _destroy_fx_proxy(self, info):
        return None

    def _run(self, cmd, timeout=2):
        return "ok"


class _ReprimeEngine:
    def __init__(self):
        self.channel_fx = {
            "mic": {
                "mode": "proxy",
                "capture_target": "output.synthetic.source",
                "active_chain_sink": "wavelinux.fx.synthetic.input",
                "loopbacks": ["downstream-2", "upstream-1"],
            }
        }
        self.commands = []

    def _run(self, cmd, timeout=2):
        self.commands.append(list(cmd))
        if cmd[:3] == ["pactl", "list", "modules"]:
            return (
                "Module #downstream-2\n"
                "    Name: module-loopback\n"
                "    Argument: source=wavelinux.fx.synthetic.source sink=wavelinux.fx.proxy.sink channels=1 channel_map=mono\n"
                "Module #upstream-1\n"
                "    Name: module-loopback\n"
                "    Argument: source=output.synthetic.source sink=wavelinux.fx.synthetic.input channels=1 channel_map=mono\n"
            )
        return "ok"

    def _load_loopback_module(self, source, sink, latency_msec=20, channels=None,
                              channel_map=None, source_dont_move=False,
                              sink_dont_move=False):
        self.commands.append(
            [
                "reload",
                source,
                sink,
                str(channels),
                str(channel_map),
                str(source_dont_move),
                str(sink_dont_move),
            ]
        )
        return "upstream-9"

    def invalidate_snapshot(self):
        self.commands.append(["invalidate"])


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

    def test_apply_channel_fx_transaction_primes_proxy_feed_before_upstream_loopback(self):
        engine = _ApplyEngine()

        with mock.patch("engine.fx_graph.time.sleep", return_value=None), mock.patch(
            "engine.fx_graph.effects_pipeline.verify_channel_fx_runtime",
            return_value=mock.Mock(ready=True, reasons=[]),
        ):
            result = apply_channel_fx_transaction(
                engine,
                "output.test.source",
                "output.test.source",
                ["rnnoise"],
                params_map={},
            )

        self.assertTrue(result["success"])
        self.assertEqual(
            engine.loopback_calls[:2],
            [
                ("chain.source", "proxy.sink", 1, "mono", True, True),
                ("output.test.source", "chain.input", 1, "mono", True, True),
            ],
        )

    def test_reprime_channel_fx_capture_reloads_upstream_loopback_by_arguments(self):
        engine = _ReprimeEngine()

        with mock.patch(
            "engine.fx_graph.time.sleep",
            side_effect=lambda seconds: engine.commands.append(["sleep", str(seconds)]),
        ):
            result = reprime_channel_fx_capture(engine, "mic", settle_s=1.0)

        self.assertTrue(result)
        reload_index = engine.commands.index(
            [
                "reload",
                "output.synthetic.source",
                "wavelinux.fx.synthetic.input",
                "1",
                "mono",
                "True",
                "True",
            ]
        )
        sleep_index = engine.commands.index(["sleep", "1.0"])
        unload_index = engine.commands.index(["pactl", "unload-module", "upstream-1"])
        self.assertLess(reload_index, sleep_index)
        self.assertLess(sleep_index, unload_index)
        self.assertEqual(
            engine.channel_fx["mic"]["loopbacks"],
            ["downstream-2", "upstream-9"],
        )


if __name__ == "__main__":
    unittest.main()
