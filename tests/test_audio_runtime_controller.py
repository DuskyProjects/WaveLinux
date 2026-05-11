import unittest
from types import SimpleNamespace

from audio_runtime.controller import AudioRuntimeController
from audio_runtime.models import (
    ChannelSpec,
    DesiredState,
    OperationStatus,
    RecoverChannel,
    RuntimeViewState,
    SetCardProfile,
)


class RuntimeControllerStateTests(unittest.TestCase):
    def _controller(self):
        controller = AudioRuntimeController.__new__(AudioRuntimeController)
        controller._worker = SimpleNamespace(
            desired_state=DesiredState(),
            _fx_statuses={},
            _pending_operations={},
            _last_observed_channel_ids={},
        )
        controller._latest_view_state = RuntimeViewState()
        controller._latest_fx_status = {}
        controller._last_requested_fx = {}
        controller._fx_generations = {}
        controller._desired_selected_mic = None
        return controller

    def test_drop_channel_state_clears_desired_and_fx_caches(self):
        controller = self._controller()
        controller._worker.desired_state.channels["mic"] = ChannelSpec(node_name="mic")
        controller._worker.desired_state.selected_mic = "mic"
        controller._worker._fx_statuses["mic"] = OperationStatus(node_name="mic")
        controller._worker._pending_operations["fx:mic"] = "SetChannelFx"
        controller._latest_fx_status["mic"] = OperationStatus(node_name="mic")
        controller._last_requested_fx["mic"] = {"effects": ["rnnoise"]}
        controller._fx_generations["mic"] = 4
        controller._desired_selected_mic = "mic"

        controller._drop_channel_state("mic")

        self.assertNotIn("mic", controller._worker.desired_state.channels)
        self.assertIsNone(controller._worker.desired_state.selected_mic)
        self.assertIsNone(controller._desired_selected_mic)
        self.assertEqual(controller._worker._fx_statuses, {})
        self.assertEqual(controller._worker._pending_operations, {})
        self.assertEqual(controller._latest_fx_status, {})
        self.assertEqual(controller._last_requested_fx, {})
        self.assertEqual(controller._fx_generations, {})

    def test_rename_channel_state_moves_desired_and_status_entries(self):
        controller = self._controller()
        controller._worker.desired_state.channels["old"] = ChannelSpec(node_name="old")
        controller._worker.desired_state.selected_mic = "old"
        controller._worker._fx_statuses["old"] = OperationStatus(node_name="old")
        controller._worker._pending_operations["fx:old"] = "SetChannelFx"
        controller._latest_fx_status["old"] = OperationStatus(node_name="old")
        controller._last_requested_fx["old"] = {"effects": ["rnnoise"]}
        controller._fx_generations["old"] = 7
        controller._desired_selected_mic = "old"

        controller._rename_channel_state("old", "new")

        self.assertNotIn("old", controller._worker.desired_state.channels)
        self.assertIn("new", controller._worker.desired_state.channels)
        self.assertEqual(controller._worker.desired_state.channels["new"].node_name, "new")
        self.assertEqual(controller._worker.desired_state.selected_mic, "new")
        self.assertEqual(controller._desired_selected_mic, "new")
        self.assertIn("new", controller._worker._fx_statuses)
        self.assertEqual(controller._worker._fx_statuses["new"].node_name, "new")
        self.assertIn("new", controller._latest_fx_status)
        self.assertEqual(controller._latest_fx_status["new"].node_name, "new")
        self.assertEqual(controller._last_requested_fx["new"], {"effects": ["rnnoise"]})
        self.assertEqual(controller._fx_generations["new"], 7)
        self.assertEqual(controller._worker._pending_operations["fx:new"], "SetChannelFx")

    def test_reset_runtime_state_clears_desired_and_pending_state(self):
        controller = self._controller()
        controller._worker.desired_state.channels["mic"] = ChannelSpec(node_name="mic")
        controller._worker.desired_state.selected_mic = "mic"
        controller._worker._fx_statuses["mic"] = OperationStatus(node_name="mic")
        controller._worker._pending_operations["fx:mic"] = "SetChannelFx"
        controller._latest_view_state = RuntimeViewState(
            channels={"mic": ChannelSpec(node_name="mic")},
        )
        controller._latest_fx_status["mic"] = OperationStatus(node_name="mic")
        controller._last_requested_fx["mic"] = {"effects": ["rnnoise"]}
        controller._fx_generations["mic"] = 2
        controller._desired_selected_mic = "mic"

        controller._reset_runtime_state()

        self.assertEqual(controller._worker.desired_state.channels, {})
        self.assertIsNone(controller._worker.desired_state.selected_mic)
        self.assertEqual(controller._worker._fx_statuses, {})
        self.assertEqual(controller._worker._pending_operations, {})
        self.assertEqual(controller._latest_view_state.channels, {})
        self.assertEqual(controller._latest_fx_status, {})
        self.assertEqual(controller._last_requested_fx, {})
        self.assertEqual(controller._fx_generations, {})
        self.assertIsNone(controller._desired_selected_mic)

    def test_recover_channel_enqueues_recover_intent(self):
        controller = self._controller()
        intents = []
        controller.enqueue_intent = intents.append

        controller.recover_channel("mic")

        self.assertEqual(len(intents), 1)
        self.assertIsInstance(intents[0], RecoverChannel)
        self.assertEqual(intents[0].node_name, "mic")

    def test_set_card_profile_enqueues_intent(self):
        controller = self._controller()
        intents = []
        controller.enqueue_intent = intents.append

        controller.set_card_profile("alsa_card.pci-1", "pro-audio")

        self.assertEqual(len(intents), 1)
        self.assertIsInstance(intents[0], SetCardProfile)
        self.assertEqual(intents[0].card_name, "alsa_card.pci-1")
        self.assertEqual(intents[0].profile_name, "pro-audio")

    def test_recover_channels_enqueues_one_intent_per_valid_name(self):
        controller = self._controller()
        intents = []
        controller.enqueue_intent = intents.append

        controller.recover_channels(["mic", "", None, "voice"])

        self.assertEqual(len(intents), 2)
        self.assertIsInstance(intents[0], RecoverChannel)
        self.assertIsInstance(intents[1], RecoverChannel)
        self.assertEqual(intents[0].node_name, "mic")
        self.assertEqual(intents[1].node_name, "voice")


if __name__ == "__main__":
    unittest.main()
