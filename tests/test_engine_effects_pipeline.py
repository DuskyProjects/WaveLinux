import unittest

from engine.effects_pipeline import list_channel_fx_artifacts
from engine.fx_graph import apply_channel_fx_transaction


class _FakeProc:
    def __init__(self, alive=True):
        self._alive = bool(alive)

    def poll(self):
        return None if self._alive else 1


class _PipelineEngine:
    def __init__(self):
        self.channel_fx = {}
        self.rnnoise_processes = {}
        self.submix_sources = {}
        self.submix_loopbacks = {}
        self.output_mixes = {}
        self._run_calls = []
        self.stopped = []
        self.destroyed = []
        self.default_source = "mic"

    def _is_inline_fx_info(self, info):
        return False

    def _clear_inline_channel_fx(self, node_name, info):
        return None

    def _ordered_chain(self, effects):
        return list(effects)

    def effect_available(self, effect_id):
        return True

    def _safe_channel_key(self, node_name):
        return "mic"

    def _build_unified_chain_config(self, safe_key, ordered, params_map, stamp):
        return (
            "/tmp/wavelinux-fx.conf",
            "wavelinux.fx.chain.input",
            "wavelinux.fx.chain.source",
            list(ordered),
        )

    def _fx_log_path(self, safe_key, suffix):
        return "/tmp/wavelinux-fx.log"

    def _spawn_fx(self, config_path, log_path, proc_key):
        self.rnnoise_processes[proc_key] = _FakeProc(True)
        return True

    def _wait_source_visible(self, source_name, attempts=20, delay=0.05):
        return str(source_name or "").strip()

    def _wait_load_loopback(self, source_name, sink_name, **kwargs):
        return "300" if source_name == "mic" else "301"

    def _ensure_fx_proxy(self, safe_key):
        return {
            "sink_name": "wavelinux.fx.mic.sink",
            "sink_module_id": "501",
            "source_name": "wavelinux.fx.mic.source",
            "source_module_id": "502",
        }

    def _module_is_alive(self, module_id, short_text=None):
        return True

    def _snapshot_submix_bindings(self, source_name):
        return {}

    def snapshot_external_source_outputs(self, source_name, exclude_modules=None):
        return []

    def get_default_source(self):
        return self.default_source

    def set_default_source(self, source_name):
        self.default_source = source_name
        return True

    def _move_known_source_outputs(self, source_output_ids, from_source, to_source, attempts=20, delay=0.05):
        return True

    def resolve_source_name(self, source_name, snap=None):
        source_name = str(source_name or "").strip()
        if source_name == "wavelinux.fx.mic.source":
            return source_name
        return ""

    def _source_names_match(self, left, right):
        return str(left or "").strip() == str(right or "").strip()

    def stop_rnnoise(self, key="default"):
        self.stopped.append(key)
        self.rnnoise_processes.pop(key, None)
        return True

    def _destroy_fx_proxy(self, info):
        self.destroyed.append(dict(info or {}))

    def _run(self, cmd, *args, **kwargs):
        self._run_calls.append(list(cmd))
        return ""

    def invalidate_snapshot(self):
        return None

    def _commit_submix_replacements(self, replacements, new_source):
        return None

    def _unload_submix_replacements(self, replacements):
        return None

    def _teardown_fx_plumbing(self, info):
        return None


class EffectsPipelineTests(unittest.TestCase):
    def test_list_channel_fx_artifacts_reports_proxy_runtime_handles(self):
        engine = _PipelineEngine()
        engine.channel_fx["mic"] = {
            "mode": "proxy",
            "effects": ["rnnoise"],
            "params": {},
            "procs": ["proc-a"],
            "loopbacks": ["301", "302"],
            "source": "wavelinux.fx.mic.source",
            "active_chain_source": "wavelinux.rnnoise.mic.source",
            "active_chain_sink": "wavelinux.rnnoise.mic.capture",
            "capture_target": "mic",
            "proxy_sink_name": "wavelinux.fx.mic.sink",
            "proxy_sink_module_id": "501",
            "proxy_source_name": "wavelinux.fx.mic.source",
            "proxy_source_module_id": "502",
        }
        engine.rnnoise_processes["proc-a"] = _FakeProc(True)

        artifacts = list_channel_fx_artifacts(
            engine,
            "mic",
            fx_status={"state": "active"},
        )

        self.assertEqual(artifacts["node_name"], "mic")
        self.assertEqual(artifacts["proxy_sink_module_id"], "501")
        self.assertEqual(artifacts["loopbacks"], ["301", "302"])
        self.assertEqual(artifacts["processes"], ["proc-a"])

    def test_apply_channel_fx_transaction_rolls_back_when_post_build_verification_fails(self):
        engine = _PipelineEngine()

        result = apply_channel_fx_transaction(engine, "mic", "mic", ["rnnoise"], {})

        self.assertFalse(result["success"])
        self.assertTrue(result["rolled_back"])
        self.assertEqual(result["failure_stage"], "verification")
        self.assertEqual(engine.channel_fx, {})
        self.assertTrue(any(cmd[:2] == ["pactl", "unload-module"] for cmd in engine._run_calls))
        self.assertTrue(engine.stopped)
        self.assertTrue(engine.destroyed)


if __name__ == "__main__":
    unittest.main()
