import unittest

from audio_runtime.controller import AudioRuntimeWorker
from audio_runtime.models import (
    ClearChannelFx,
    DesiredState,
    EnsureSubmixRoute,
    FxSpec,
    ObservedState,
    RefreshNow,
    SetCardProfile,
    SetChannelFx,
    SetSubmixState,
)


class DummyAdapter:
    pass


class RuntimeWorkerCoalesceTests(unittest.TestCase):
    def test_latest_fx_intent_wins_per_channel(self):
        worker = AudioRuntimeWorker(DummyAdapter())
        intents = [
            SetChannelFx("mic", "mic", FxSpec(effects=["rnnoise"], generation=1)),
            SetChannelFx("mic", "mic", FxSpec(effects=["rnnoise", "eq"], generation=2)),
            ClearChannelFx("other", generation=3),
        ]

        collapsed = worker._coalesce(intents)

        self.assertEqual(len(collapsed), 2)
        self.assertIsInstance(collapsed[0], SetChannelFx)
        self.assertEqual(collapsed[0].fx_spec.generation, 2)
        self.assertIsInstance(collapsed[1], ClearChannelFx)

    def test_latest_submix_write_wins_per_channel_and_mix(self):
        worker = AudioRuntimeWorker(DummyAdapter())
        intents = [
            SetSubmixState("55", "Monitor", 0.5, False),
            SetSubmixState("55", "Monitor", 0.8, True),
            SetSubmixState("55", "Stream", 0.3, False),
        ]

        collapsed = worker._coalesce(intents)

        self.assertEqual(len(collapsed), 2)
        self.assertEqual(collapsed[0].volume, 0.8)
        self.assertTrue(collapsed[0].mute)
        self.assertEqual(collapsed[1].mix_name, "Stream")

    def test_latest_route_ensure_wins_per_channel_and_mix(self):
        worker = AudioRuntimeWorker(DummyAdapter())
        intents = [
            EnsureSubmixRoute("55", "mic", "Audio/Source", "Monitor", {"vol": 0.5, "mute": False}),
            EnsureSubmixRoute("55", "mic", "Audio/Source", "Monitor", {"vol": 1.0, "mute": True}),
        ]

        collapsed = worker._coalesce(intents)

        self.assertEqual(len(collapsed), 1)
        self.assertEqual(collapsed[0].initial_state["vol"], 1.0)
        self.assertTrue(collapsed[0].initial_state["mute"])

    def test_latest_card_profile_intent_wins_per_card(self):
        worker = AudioRuntimeWorker(DummyAdapter())
        intents = [
            SetCardProfile("alsa_card.pci-1", "analog-stereo"),
            SetCardProfile("alsa_card.pci-1", "pro-audio"),
            SetCardProfile("alsa_card.usb-2", "analog-stereo"),
        ]

        collapsed = worker._coalesce(intents)

        self.assertEqual(len(collapsed), 2)
        self.assertEqual(collapsed[0].profile_name, "pro-audio")
        self.assertEqual(collapsed[1].card_name, "alsa_card.usb-2")

    def test_only_one_refresh_can_be_enqueued_at_a_time(self):
        worker = AudioRuntimeWorker(DummyAdapter())

        first = worker.enqueue(RefreshNow("tick-1"))
        second = worker.enqueue(RefreshNow("tick-2"))

        self.assertTrue(first)
        self.assertFalse(second)
        queued = worker._queue.get_nowait()
        self.assertEqual(queued.reason, "tick-1")

        worker._clear_refresh_pending()
        third = worker.enqueue(RefreshNow("tick-3"))

        self.assertTrue(third)
        queued = worker._queue.get_nowait()
        self.assertEqual(queued.reason, "tick-3")

    def test_submix_state_reuses_cached_observation(self):
        worker = AudioRuntimeWorker(DummyAdapter())
        worker.desired_state = DesiredState()
        cached = ObservedState(channel_ids_by_name={})
        worker._last_observed_state = cached
        worker._last_observed_channel_ids = {}
        worker._publish_view_state = lambda observed: None

        class FakePlanner:
            def __init__(self):
                self.seen_observed = None

            def apply_intent(self, desired_state, intent):
                return None

            def reconcile(self, desired_state, observed_state, intent):
                self.seen_observed = observed_state
                return []

        class FakeExecutor:
            def observe(self, desired_state):
                raise AssertionError("observe should not be called for cached submix writes")

            def execute(self, actions, *, desired_state, observed_state, status_callback=None):
                return observed_state

        planner = FakePlanner()
        worker.planner = planner
        worker.executor = FakeExecutor()

        worker._process_intent(SetSubmixState("55", "Monitor", 0.5, False))

        self.assertIs(planner.seen_observed, cached)


if __name__ == "__main__":
    unittest.main()
