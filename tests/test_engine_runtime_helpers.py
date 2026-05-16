import unittest
from types import SimpleNamespace

from engine.runtime_helpers import (
    find_module_by_arg,
    module_is_alive,
    preferred_hardware_source_fallback,
    rename_virtual_sink,
    sink_visible,
)


class EngineRuntimeHelpersTests(unittest.TestCase):
    def test_sink_visible_matches_short_sink_listing(self):
        engine = SimpleNamespace(
            _run=lambda cmd: "12\talsa_output.speakers\tPipeWire\trunning\n"
        )

        self.assertTrue(sink_visible(engine, "alsa_output.speakers"))
        self.assertFalse(sink_visible(engine, "missing_sink"))

    def test_preferred_hardware_source_fallback_prefers_visible_default(self):
        engine = SimpleNamespace(
            get_default_source=lambda: "alsa_input.usb_mic",
            resolve_hardware_source_name=lambda candidate, snap=None: None,
            resolve_source_name=lambda candidate, snap=None: candidate,
            _is_internal_node_name=lambda name: False,
            channel_fx={},
            get_hardware_inputs=lambda snap=None: [],
        )

        self.assertEqual(
            preferred_hardware_source_fallback(engine),
            "alsa_input.usb_mic",
        )

    def test_preferred_hardware_source_fallback_uses_fx_then_hardware_inputs(self):
        engine = SimpleNamespace(
            get_default_source=lambda: "wavelinux.fx.mic",
            resolve_hardware_source_name=lambda candidate, snap=None: None,
            resolve_source_name=lambda candidate, snap=None: candidate,
            _is_internal_node_name=lambda name: name.startswith("wavelinux."),
            channel_fx={"mic": {"prev_default": "", "capture_target": "alsa_input.dji"}},
            get_hardware_inputs=lambda snap=None: [SimpleNamespace(name="alsa_input.fallback")],
        )

        self.assertEqual(
            preferred_hardware_source_fallback(engine),
            "alsa_input.dji",
        )

    def test_rename_virtual_sink_returns_new_name_for_managed_sink(self):
        removed = []
        created = []
        engine = SimpleNamespace(
            _sanitize_channel_name=lambda name: ("Voice Chat", "voice_chat"),
            remove_virtual_sink=lambda sink_name: removed.append(sink_name),
            create_virtual_sink=lambda display_name: created.append(display_name) or "wavelinux_voice_chat",
        )

        renamed = rename_virtual_sink(engine, "wavelinux_old", "Voice Chat")

        self.assertEqual(renamed, "wavelinux_voice_chat")
        self.assertEqual(removed, ["wavelinux_old"])
        self.assertEqual(created, ["Voice Chat"])

    def test_find_module_by_arg_matches_whole_token(self):
        modules_text = """
Module #11
    Argument: source=12 sink=alsa_output
Module #22
    Argument: source=1 sink=alsa_output
"""
        engine = SimpleNamespace(_run=lambda cmd: modules_text)

        self.assertEqual(find_module_by_arg(engine, "source=1"), "22")

    def test_module_is_alive_checks_short_module_listing(self):
        short_text = "11\tmodule-null-sink\targs\n22\tmodule-loopback\targs\n"
        engine = SimpleNamespace(_run=lambda cmd: short_text)

        self.assertTrue(module_is_alive(engine, "22"))
        self.assertFalse(module_is_alive(engine, "33"))


if __name__ == "__main__":
    unittest.main()
