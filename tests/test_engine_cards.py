import subprocess
import unittest
from types import SimpleNamespace
from unittest import mock

from engine.cards import (
    list_cards,
    lock_bluetooth_to_a2dp,
    set_card_profile,
    unlock_bluetooth_autoswitch,
)


class EngineCardsTests(unittest.TestCase):
    def test_list_cards_parses_profiles_and_active_profile(self):
        cards_text = "\n".join(
            [
                "Card #42",
                "\tName: bluez_card.AA_BB_CC_DD_EE_FF",
                "\tProperties:",
                "\t\tdevice.description = \"Headset\"",
                "\tProfiles:",
                "\t\ta2dp-sink: High Fidelity Playback (sinks: 1, sources: 0, priority: 40, available: yes)",
                "\t\theadset-head-unit: Headset Head Unit (HSP/HFP) (sinks: 1, sources: 1, priority: 30, available: no)",
                "\tActive Profile: a2dp-sink",
            ]
        )
        engine = SimpleNamespace(_run=lambda cmd, timeout=2: cards_text)

        cards = list_cards(engine)

        self.assertEqual(
            cards,
            [
                {
                    "name": "bluez_card.AA_BB_CC_DD_EE_FF",
                    "description": "Headset",
                    "active_profile": "a2dp-sink",
                    "profiles": [
                        {
                            "name": "a2dp-sink",
                            "description": "High Fidelity Playback",
                            "available": True,
                        },
                        {
                            "name": "headset-head-unit",
                            "description": "Headset Head Unit (HSP/HFP)",
                            "available": False,
                        },
                    ],
                }
            ],
        )

    def test_set_card_profile_reports_run_success(self):
        calls = []
        engine = SimpleNamespace(_run=lambda cmd, timeout=2: calls.append(list(cmd)) or "ok")

        ok = set_card_profile(engine, "alsa_card.pci-1", "pro-audio")

        self.assertTrue(ok)
        self.assertEqual(
            calls,
            [["pactl", "set-card-profile", "alsa_card.pci-1", "pro-audio"]],
        )

    @mock.patch("engine.cards.subprocess.run")
    def test_lock_bluetooth_to_a2dp_sets_override_on_success(self, run_mock):
        run_mock.return_value = SimpleNamespace(returncode=0, stderr="")
        engine = SimpleNamespace(_bt_autoswitch_overridden=False)

        ok = lock_bluetooth_to_a2dp(engine)

        self.assertTrue(ok)
        self.assertTrue(engine._bt_autoswitch_overridden)
        run_mock.assert_called_once_with(
            ["wpctl", "settings", "bluetooth.autoswitch-to-headset-profile", "false"],
            capture_output=True,
            text=True,
            timeout=3,
        )

    @mock.patch("engine.cards.subprocess.run")
    def test_unlock_bluetooth_autoswitch_clears_override_on_success(self, run_mock):
        run_mock.return_value = SimpleNamespace(returncode=0)
        engine = SimpleNamespace(_bt_autoswitch_overridden=True)

        ok = unlock_bluetooth_autoswitch(engine)

        self.assertTrue(ok)
        self.assertFalse(engine._bt_autoswitch_overridden)
        run_mock.assert_called_once_with(
            ["wpctl", "settings", "bluetooth.autoswitch-to-headset-profile", "true"],
            capture_output=True,
            text=True,
            timeout=3,
        )

    @mock.patch("engine.cards.subprocess.run", side_effect=subprocess.TimeoutExpired(cmd="wpctl", timeout=3))
    def test_lock_bluetooth_to_a2dp_handles_timeout(self, _run_mock):
        engine = SimpleNamespace(_bt_autoswitch_overridden=False)

        ok = lock_bluetooth_to_a2dp(engine)

        self.assertFalse(ok)
        self.assertFalse(engine._bt_autoswitch_overridden)
