import unittest

from engine.cleanup import full_audio_reset
from engine.defaults import set_default_source, source_name_aliases
from engine.devices import branding_label, friendly_name, pretty_bt, sanitize_channel_name


class _FakeEngine:
    def __init__(self):
        self.calls = []
        self.loopback_modules = {"Monitor->sink": "11"}
        self.submix_loopbacks = {"a": "22"}
        self.submix_sources = {"a": "wavelinux_music.monitor"}
        self.virtual_sink_modules = {"wavelinux_music": "33"}
        self.output_mixes = {"Monitor": object()}
        self.channel_fx = {"mic": {"effects": ["rnnoise"]}}
        self.submix_state_cache = {"a": {"vol": 1.0}}
        self._pending_submix_state_reapply = {"a"}
        self.rnnoise_processes = {"mic": object()}

    def _run(self, cmd, timeout=2):
        self.calls.append((tuple(cmd), timeout))
        if cmd[:3] == ["pactl", "list", "short"]:
            return "\n".join(
                [
                    "10\tmodule-loopback\twavelinux",
                    "11\tmodule-virtual-source\twavelinux",
                    "12\tmodule-null-sink\twavelinux",
                ]
            )
        if cmd[:3] == ["pactl", "list", "modules"]:
            return "\n".join(
                [
                    "Module #10",
                    "Argument: sink_name=wavelinux_music",
                    "Module #11",
                    "Argument: source_name=wavelinux_src_stream",
                ]
            )
        return ""

    def unlock_bluetooth_autoswitch(self):
        self.calls.append((("unlock_bluetooth_autoswitch",), None))

    def create_snapshot(self, force=False):
        self.calls.append((("create_snapshot", force), None))
        return object()

    def _restore_physical_defaults_before_reset(self, snap=None):
        self.calls.append((("_restore_physical_defaults_before_reset", snap is not None), None))

    def clear_channel_fx(self, node_name):
        self.calls.append((("clear_channel_fx", node_name), None))

    def stop_rnnoise(self, key):
        self.calls.append((("stop_rnnoise", key), None))

    def _reap_orphan_fx_processes(self):
        self.calls.append((("_reap_orphan_fx_processes",), None))

    def invalidate_snapshot(self):
        self.calls.append((("invalidate_snapshot",), None))

    def _wait_source_visible(self, source_name, attempts=20, delay=0.05):
        self.calls.append((("_wait_source_visible", source_name, attempts, delay), None))
        return source_name

    def resolve_source_name(self, source_name):
        self.calls.append((("resolve_source_name", source_name), None))
        return source_name

    def get_default_source(self):
        self.calls.append((("get_default_source",), None))
        return "output.wavelinux.fx.mic.source"

    def _source_names_match(self, left, right):
        return left == f"output.{right}" or left == right


class EngineModuleTests(unittest.TestCase):
    def test_pretty_bt_formats_mac(self):
        self.assertEqual(
            pretty_bt("bluez_output.AC_80_0A_72_BD_10.1"),
            "Bluetooth AC:80:0A:72:BD:10",
        )

    def test_friendly_name_preserves_existing_behavior(self):
        self.assertEqual(
            friendly_name("alsa_input.usb-DJI_Technology_Co.__Ltd._Wireless_Mic_Rx_XSP12345678B-01.iec958-stereo"),
            "01 Iec958 Stereo",
        )
        self.assertEqual(friendly_name("Bd 10 1"), "Bd 10 1")

    def test_channel_name_helpers_normalize_labels(self):
        self.assertEqual(sanitize_channel_name("  Voice Chat  "), ("Voice Chat", "voice_chat"))
        self.assertEqual(branding_label("Voice Chat"), "WaveLinux-Voice-Chat")

    def test_source_name_aliases_include_output_alias(self):
        self.assertEqual(
            source_name_aliases("wavelinux.fx.mic.source"),
            ["wavelinux.fx.mic.source", "output.wavelinux.fx.mic.source"],
        )
        self.assertEqual(
            source_name_aliases("output.wavelinux.fx.mic.source"),
            ["output.wavelinux.fx.mic.source", "wavelinux.fx.mic.source"],
        )

    def test_set_default_source_uses_alias_aware_retry_logic(self):
        engine = _FakeEngine()
        ok = set_default_source(engine, "wavelinux.fx.mic.source", attempts=1, delay=0.0)
        self.assertTrue(ok)
        self.assertIn(
            (("pactl", "set-default-source", "wavelinux.fx.mic.source"), 2),
            engine.calls,
        )

    def test_full_audio_reset_clears_runtime_state(self):
        engine = _FakeEngine()
        full_audio_reset(engine)

        self.assertEqual(engine.loopback_modules, {})
        self.assertEqual(engine.submix_loopbacks, {})
        self.assertEqual(engine.submix_sources, {})
        self.assertEqual(engine.virtual_sink_modules, {})
        self.assertEqual(engine.output_mixes, {})
        self.assertEqual(engine.channel_fx, {})
        self.assertEqual(engine.submix_state_cache, {})
        self.assertEqual(engine._pending_submix_state_reapply, set())
        self.assertIn((("invalidate_snapshot",), None), engine.calls)


if __name__ == "__main__":
    unittest.main()
