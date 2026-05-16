import unittest
from types import SimpleNamespace

from engine.levels import (
    clamp,
    get_sink_input_volume,
    get_source_volume_by_name,
    get_volume,
    set_sink_input_volume,
    snapshot_sink_inputs_by_owner,
)


class EngineLevelsTests(unittest.TestCase):
    def test_get_volume_parses_wpctl_output_and_muted_flag(self):
        engine = SimpleNamespace(_run=lambda cmd: "Volume: 0.75 [MUTED]")

        self.assertEqual(get_volume(engine, 42), (0.75, True))

    def test_get_source_volume_by_name_uses_snapshot_cache(self):
        snap = SimpleNamespace(
            sources_text="ignored",
            _source_state_by_name=None,
        )
        engine = SimpleNamespace(
            MAX_VOLUME=1.0,
            _source_name_aliases=lambda source_name: [source_name],
            _parse_sources_state=lambda text: {"alsa_input.mic": (1.2, True)},
        )

        volume, muted = get_source_volume_by_name(engine, "alsa_input.mic", snap=snap)

        self.assertEqual(volume, 1.0)
        self.assertTrue(muted)
        self.assertEqual(snap._source_state_by_name, {"alsa_input.mic": (1.2, True)})

    def test_set_sink_input_volume_clamps_to_max(self):
        calls = []
        engine = SimpleNamespace(MAX_VOLUME=1.0, _run=lambda cmd: calls.append(cmd))

        set_sink_input_volume(engine, 55, 1.8)

        self.assertEqual(
            calls,
            [["pactl", "set-sink-input-volume", "55", "100%"]],
        )

    def test_get_sink_input_volume_reads_target_stream(self):
        text = """
Sink Input #11
    Volume: front-left: 32768 / 50% / -18.06 dB
Sink Input #22
    Volume: front-left: 49152 / 75% / -6.02 dB
"""
        engine = SimpleNamespace(_run=lambda cmd: text)

        self.assertEqual(get_sink_input_volume(engine, 22), 0.75)

    def test_snapshot_sink_inputs_by_owner_maps_volume_and_mute(self):
        snap = SimpleNamespace(
            sink_inputs_text="""
Sink Input #7
    Owner Module: 601
    Mute: yes
    Volume: front-left: 65536 / 100% / 0.00 dB
Sink Input #8
    Owner Module: 777
    Mute: no
    Volume: front-left: 32768 / 50% / -18.06 dB
""",
        )

        self.assertEqual(
            snapshot_sink_inputs_by_owner(SimpleNamespace(), snap=snap),
            {"601": (1.0, True), "777": (0.5, False)},
        )

    def test_clamp_handles_bad_values(self):
        engine = SimpleNamespace(MAX_VOLUME=1.0)

        self.assertEqual(clamp(engine, "bad"), 1.0)


if __name__ == "__main__":
    unittest.main()
