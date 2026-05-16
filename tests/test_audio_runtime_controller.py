import unittest
import threading
from types import SimpleNamespace
from unittest import mock

from audio_runtime.controller import AudioRuntimeController
from audio_runtime.models import (
    Action,
    ChannelSpec,
    DesiredState,
    OperationStatus,
    ReprimeChannelFx,
    RecoverChannel,
    RuntimeViewState,
    SetCardProfile,
    SetSelectedMic,
)


class _FakeTimerSignal:
    def __init__(self):
        self._callback = None

    def connect(self, callback):
        self._callback = callback


class _FakeTimer:
    def __init__(self, parent=None):
        self.parent = parent
        self.timeout = _FakeTimerSignal()
        self._active = False
        self.delay_ms = None
        self.deleted = False

    def setSingleShot(self, single_shot):
        return None

    def start(self, delay_ms):
        self._active = True
        self.delay_ms = int(delay_ms)

    def stop(self):
        self._active = False

    def deleteLater(self):
        self.deleted = True

    def fire(self):
        self._active = False
        callback = self.timeout._callback
        if callback is not None:
            callback()


class RuntimeControllerStateTests(unittest.TestCase):
    def _controller(self):
        controller = AudioRuntimeController.__new__(AudioRuntimeController)
        controller._worker = SimpleNamespace(
            desired_state=DesiredState(),
            _fx_statuses={},
            _pending_operations={},
            _last_observed_channel_ids={},
            state_lock=threading.RLock(),
        )
        controller._latest_view_state = RuntimeViewState()
        controller._latest_fx_status = {}
        controller._last_requested_fx = {}
        controller._fx_generations = {}
        controller._fx_reprime_timers = {}
        controller._desired_selected_mic = None
        controller.view_state_changed = SimpleNamespace(emit=lambda *args, **kwargs: None)
        controller.fx_status_changed = SimpleNamespace(emit=lambda *args, **kwargs: None)
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

    def test_fx_request_for_falls_back_to_desired_channel_fx_state(self):
        controller = self._controller()
        controller._worker.desired_state.channels["mic"] = ChannelSpec(
            node_name="mic",
            capture_target="alsa_input.synthetic",
            fx=SimpleNamespace(
                effects=["rnnoise"],
                params_map={"rnnoise": {"VAD Threshold (%)": 75.0}},
            ),
        )

        request = controller.fx_request_for("mic")

        self.assertEqual(request["capture_target"], "alsa_input.synthetic")
        self.assertEqual(request["effects"], ["rnnoise"])
        self.assertEqual(
            request["params_map"],
            {"rnnoise": {"VAD Threshold (%)": 75.0}},
        )

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

    def test_reprime_channel_fx_enqueues_reprime_intent(self):
        controller = self._controller()
        intents = []
        controller.enqueue_intent = intents.append

        controller.reprime_channel_fx("mic", generation=4, settle_s=1.25)

        self.assertEqual(len(intents), 1)
        self.assertIsInstance(intents[0], ReprimeChannelFx)
        self.assertEqual(intents[0].node_name, "mic")
        self.assertEqual(intents[0].generation, 4)
        self.assertEqual(intents[0].settle_s, 1.25)

    @mock.patch("audio_runtime.controller.QTimer", _FakeTimer)
    def test_active_fx_status_does_not_blind_reprime_live_chain(self):
        controller = self._controller()
        intents = []
        controller.enqueue_intent = intents.append
        controller._worker.desired_state.selected_mic = "mic"
        controller._worker.desired_state.channels["mic"] = ChannelSpec(
            node_name="mic",
            fx=SimpleNamespace(effects=["rnnoise"], generation=4),
        )

        controller._on_fx_status_ready(
            OperationStatus(
                node_name="mic",
                state="active",
                generation=4,
                message="FX chain active",
            )
        )

        self.assertNotIn("mic", controller._fx_reprime_timers)
        self.assertEqual(intents, [])

    @mock.patch("audio_runtime.controller.QTimer", _FakeTimer)
    def test_set_selected_mic_cancels_old_post_active_reprime(self):
        controller = self._controller()
        intents = []
        controller.enqueue_intent = intents.append
        controller._desired_selected_mic = "mic"
        controller._worker.desired_state.selected_mic = "mic"
        controller._worker.desired_state.channels["mic"] = ChannelSpec(
            node_name="mic",
            fx=SimpleNamespace(effects=["rnnoise"], generation=4),
        )
        controller._schedule_post_active_fx_reprime("mic", generation=4)

        controller.set_selected_mic("usb")

        self.assertNotIn("mic", controller._fx_reprime_timers)
        self.assertEqual(len(intents), 1)
        self.assertIsInstance(intents[0], SetSelectedMic)
        self.assertEqual(intents[0].node_name, "usb")

    def test_set_mix_hardware_route_sync_applies_route_immediately(self):
        calls = []

        class _Engine:
            def create_output_mix(self, mix_name):
                calls.append(("create_output_mix", mix_name))
                return True

            def route_mix_to_hardware(self, mix_name, sink_name):
                calls.append(("route_mix_to_hardware", mix_name, sink_name))
                return True

            def resolve_hardware_sink_name(self, sink_name):
                calls.append(("resolve_hardware_sink_name", sink_name))
                return sink_name

            def set_default_sink(self, sink_name):
                calls.append(("set_default_sink", sink_name))

        class _Session:
            def __enter__(self_inner):
                return _Engine()

            def __exit__(self_inner, exc_type, exc, tb):
                return False

        controller = self._controller()
        controller._worker.state_lock = _Session()
        controller._worker.desired_state = DesiredState()
        controller.adapter = SimpleNamespace(session=lambda: _Session())
        controller.refresh_now = lambda reason: calls.append(("refresh_now", reason))

        result = controller.set_mix_hardware_route_sync("Monitor", "bluez_output.headset")

        self.assertTrue(result)
        self.assertEqual(
            calls,
            [
                ("create_output_mix", "Monitor"),
                ("route_mix_to_hardware", "Monitor", "bluez_output.headset"),
                ("resolve_hardware_sink_name", None),
                ("resolve_hardware_sink_name", "bluez_output.headset"),
                ("set_default_sink", "bluez_output.headset"),
                ("refresh_now", "set-mix-hardware-route:Monitor"),
            ],
        )
        self.assertEqual(
            controller._worker.desired_state.mixes["Monitor"].hardware_sink,
            "bluez_output.headset",
        )

    def test_set_mix_hardware_route_sync_uses_fallback_route_for_default_sink(self):
        calls = []

        class _Engine:
            output_mixes = {
                "Monitor": SimpleNamespace(hardware_output="alsa_output.speakers")
            }

            def create_output_mix(self, mix_name):
                calls.append(("create_output_mix", mix_name))
                return True

            def route_mix_to_hardware(self, mix_name, sink_name):
                calls.append(("route_mix_to_hardware", mix_name, sink_name))
                self.output_mixes[mix_name].hardware_output = "alsa_output.speakers"
                return True

            def get_live_mix_hardware_route(self, mix_name):
                calls.append(("get_live_mix_hardware_route", mix_name))
                return "alsa_output.speakers"

            def resolve_hardware_sink_name(self, sink_name):
                calls.append(("resolve_hardware_sink_name", sink_name))
                return sink_name if sink_name == "alsa_output.speakers" else None

            def set_default_sink(self, sink_name):
                calls.append(("set_default_sink", sink_name))

        class _Session:
            def __enter__(self_inner):
                return _Engine()

            def __exit__(self_inner, exc_type, exc, tb):
                return False

        controller = self._controller()
        controller._worker.state_lock = _Session()
        controller._worker.desired_state = DesiredState()
        controller.adapter = SimpleNamespace(session=lambda: _Session())
        controller.refresh_now = lambda reason: calls.append(("refresh_now", reason))

        result = controller.set_mix_hardware_route_sync(
            "Monitor",
            "bluez_output.headset",
        )

        self.assertTrue(result)
        self.assertIn(("set_default_sink", "alsa_output.speakers"), calls)
        self.assertNotIn(("set_default_sink", "bluez_output.headset"), calls)

    def test_full_audio_reset_sync_can_skip_refresh(self):
        calls = []

        class _Engine:
            def full_audio_reset(self):
                calls.append(("full_audio_reset",))

        class _Lock:
            def __enter__(self_inner):
                return None

            def __exit__(self_inner, exc_type, exc, tb):
                return False

        class _Session:
            def __enter__(self_inner):
                return _Engine()

            def __exit__(self_inner, exc_type, exc, tb):
                return False

        controller = self._controller()
        controller._worker.state_lock = _Lock()
        controller.adapter = SimpleNamespace(session=lambda: _Session())
        controller._reset_runtime_state = lambda: calls.append(("reset_runtime_state",))
        controller.refresh_now = lambda reason: calls.append(("refresh_now", reason))

        controller.full_audio_reset_sync(refresh=False)

        self.assertEqual(
            calls,
            [
                ("full_audio_reset",),
                ("reset_runtime_state",),
            ],
        )

    def test_sync_persistent_state_persists_mix_master_volumes(self):
        controller = self._controller()
        controller._worker.state_lock = SimpleNamespace(
            __enter__=lambda self_inner: None,
            __exit__=lambda self_inner, exc_type, exc, tb: False,
        )

        class _Lock:
            def __enter__(self_inner):
                return None

            def __exit__(self_inner, exc_type, exc, tb):
                return False

        controller._worker.state_lock = _Lock()

        controller.sync_persistent_state(
            selected_mic="usb_mic",
            submix_state={},
            active_effects={},
            effect_params={},
            app_routing={},
            app_volumes={},
            virtual_channels={},
            monitor_hw="alsa_output.headphones",
            stream_hw="alsa_output.stream",
            monitor_mix_volume=0.63,
            stream_mix_volume=0.27,
        )

        self.assertEqual(
            controller._worker.desired_state.mixes["Monitor"].master_volume,
            0.63,
        )
        self.assertEqual(
            controller._worker.desired_state.mixes["Stream"].master_volume,
            0.27,
        )

    def test_sync_persistent_state_apply_now_reconciles_immediately(self):
        controller = self._controller()

        class _Lock:
            def __enter__(self_inner):
                return None

            def __exit__(self_inner, exc_type, exc, tb):
                return False

        calls = []
        controller._worker.state_lock = _Lock()
        controller._reconcile_refresh_sync = lambda reason: calls.append(reason)

        controller.sync_persistent_state(
            selected_mic="usb_mic",
            submix_state={},
            active_effects={},
            effect_params={},
            app_routing={},
            app_volumes={},
            virtual_channels={},
            monitor_hw="alsa_output.headphones",
            stream_hw="alsa_output.stream",
            apply_now=True,
        )

        self.assertEqual(calls, ["sync-persistent-state"])

    def test_reconcile_refresh_sync_runs_followup_passes_until_virtual_routes_exist(self):
        controller = self._controller()

        class _Lock:
            def __enter__(self_inner):
                return None

            def __exit__(self_inner, exc_type, exc, tb):
                return False

        observe_states = [
            SimpleNamespace(
                channel_ids_by_name={},
                virtual_channels=[],
                mic_inputs=[],
                mix_hardware_routes={"Monitor": "sink", "Stream": "sink"},
                health={},
            ),
            SimpleNamespace(
                channel_ids_by_name={"wavelinux_music": "42"},
                virtual_channels=[SimpleNamespace(name="wavelinux_music")],
                mic_inputs=[],
                mix_hardware_routes={"Monitor": "sink", "Stream": "sink"},
                health={"wavelinux_music": "submix_monitor_missing"},
            ),
            SimpleNamespace(
                channel_ids_by_name={"wavelinux_music": "42"},
                virtual_channels=[SimpleNamespace(name="wavelinux_music")],
                mic_inputs=[],
                mix_hardware_routes={"Monitor": "sink", "Stream": "sink"},
                health={},
            ),
        ]
        action_batches = [
            [Action("ensure_virtual_channel", {"sink_name": "wavelinux_music", "display_name": "Music"})],
            [
                Action("ensure_submix_route", {
                    "node_id": "42",
                    "node_name": "wavelinux_music",
                    "media_class": "Audio/Sink",
                    "mix_name": "Monitor",
                    "initial_state": {},
                }),
                Action("ensure_submix_route", {
                    "node_id": "42",
                    "node_name": "wavelinux_music",
                    "media_class": "Audio/Sink",
                    "mix_name": "Stream",
                    "initial_state": {},
                }),
            ],
        ]
        executed_batches = []
        controller._worker.state_lock = _Lock()
        controller._worker.desired_state.virtual_channels = {"wavelinux_music": "Music"}
        controller._worker.desired_state.mixes["Monitor"] = SimpleNamespace()
        controller._worker.desired_state.mixes["Stream"] = SimpleNamespace()
        controller._worker._handle_status = lambda status: None
        controller._worker._mark_pending = lambda intent, active: None
        controller._worker._clear_refresh_pending = lambda: None
        controller._worker.planner = SimpleNamespace(
            reconcile=lambda desired_state, observed_state, intent: (
                action_batches.pop(0) if action_batches else []
            )
        )
        controller._worker.executor = SimpleNamespace(
            observe=lambda desired_state: observe_states.pop(0),
            execute=lambda actions, **kwargs: executed_batches.append(list(actions)) or kwargs["observed_state"],
            build_view_state=lambda desired_state, observed_state, fx_statuses, pending: RuntimeViewState(),
        )

        controller._reconcile_refresh_sync("sync-persistent-state")

        self.assertEqual(len(executed_batches), 3)
        self.assertEqual([action.kind for action in executed_batches[0]], ["ensure_virtual_channel"])
        self.assertEqual(
            [action.kind for action in executed_batches[1]],
            ["ensure_submix_route", "ensure_submix_route"],
        )
        self.assertEqual(executed_batches[2], [])


if __name__ == "__main__":
    unittest.main()
