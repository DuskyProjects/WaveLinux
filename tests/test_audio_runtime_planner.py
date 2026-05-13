import unittest

from audio_runtime.models import (
    AppRouteSpec,
    ChannelSpec,
    ClearChannelFx,
    DesiredState,
    EnsureSubmixRoute,
    FxSpec,
    MixSpec,
    ObservedState,
    RecoverChannel,
    RefreshNow,
    RuntimeAppView,
    RuntimeChannelView,
    SetAppRoute,
    SetCardProfile,
    SetChannelFx,
    SetMixHardwareRoute,
    SetSourceVolume,
    SetSubmixState,
)
from audio_runtime.planner import RuntimePlanner


class RuntimePlannerTests(unittest.TestCase):
    def test_set_mix_hardware_route_treats_rotated_bluetooth_sink_names_as_same_device(self):
        planner = RuntimePlanner()
        desired = DesiredState(mixes={"Monitor": MixSpec(name="Monitor")})
        observed = ObservedState(
            mix_hardware_routes={"Monitor": "bluez_output.AA_BB_CC_DD_EE_FF.2"}
        )
        intent = SetMixHardwareRoute(
            mix_name="Monitor",
            sink_name="bluez_output.AA_BB_CC_DD_EE_FF.1",
        )

        planner.apply_intent(desired, intent)
        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(actions, [])

    def test_set_channel_fx_creates_apply_action_when_chain_missing(self):
        planner = RuntimePlanner()
        desired = DesiredState()
        observed = ObservedState(
            fx_sources_by_channel={"mic": None},
            fx_effects_by_channel={"mic": []},
        )
        intent = SetChannelFx(
            node_name="mic",
            capture_target="mic",
            fx_spec=FxSpec(
                effects=["rnnoise"],
                params_map={"rnnoise": {"VAD Threshold (%)": 50.0}},
                generation=3,
            ),
        )

        planner.apply_intent(desired, intent)
        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "apply_channel_fx")
        self.assertEqual(actions[0].payload["fx_spec"].generation, 3)

    def test_set_channel_fx_no_action_when_observed_matches_desired(self):
        planner = RuntimePlanner()
        desired = DesiredState()
        intent = SetChannelFx(
            node_name="mic",
            capture_target="mic",
            fx_spec=FxSpec(
                effects=["rnnoise"],
                params_map={"rnnoise": {"VAD Threshold (%)": 25.0}},
                generation=1,
            ),
        )
        planner.apply_intent(desired, intent)
        observed = ObservedState(
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            fx_params_by_channel={"mic": {"rnnoise": {"VAD Threshold (%)": 25.0}}},
        )

        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(actions, [])

    def test_set_channel_fx_rebuilds_when_only_params_change(self):
        planner = RuntimePlanner()
        desired = DesiredState()
        intent = SetChannelFx(
            node_name="mic",
            capture_target="mic",
            fx_spec=FxSpec(
                effects=["rnnoise"],
                params_map={"rnnoise": {"VAD Threshold (%)": 75.0}},
                generation=2,
            ),
        )
        planner.apply_intent(desired, intent)
        observed = ObservedState(
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            fx_params_by_channel={"mic": {"rnnoise": {"VAD Threshold (%)": 25.0}}},
        )

        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "apply_channel_fx")
        self.assertEqual(actions[0].payload["fx_spec"].generation, 2)

    def test_set_source_volume_returns_runtime_action(self):
        planner = RuntimePlanner()
        desired = DesiredState()
        observed = ObservedState()
        intent = SetSourceVolume("mic", 0.72)

        planner.apply_intent(desired, intent)
        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(actions[0].kind, "set_source_volume")
        self.assertEqual(actions[0].payload["node_name"], "mic")
        self.assertEqual(actions[0].payload["volume"], 0.72)
        self.assertEqual(desired.channels, {})

    def test_clear_channel_fx_only_runs_when_source_exists(self):
        planner = RuntimePlanner()
        desired = DesiredState()
        intent = ClearChannelFx(node_name="mic", generation=7)
        planner.apply_intent(desired, intent)

        empty = ObservedState(fx_sources_by_channel={"mic": None})
        live = ObservedState(fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"})

        self.assertEqual(planner.reconcile(desired, empty, intent), [])
        actions = planner.reconcile(desired, live, intent)
        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "clear_channel_fx")

    def test_ensure_submix_route_always_emits_route_action(self):
        planner = RuntimePlanner()
        desired = DesiredState()
        observed = ObservedState()
        intent = EnsureSubmixRoute(
            node_id="55",
            node_name="mic",
            media_class="Audio/Source",
            mix_name="Monitor",
            initial_state={"vol": 1.0, "mute": True},
        )

        planner.apply_intent(desired, intent)
        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "ensure_submix_route")
        self.assertEqual(desired.channels["mic"].submix_state["Monitor"]["mute"], True)

    def test_set_submix_state_updates_desired_channel_state_when_node_name_known(self):
        planner = RuntimePlanner()
        desired = DesiredState()
        observed = ObservedState()
        intent = SetSubmixState(
            node_id="55",
            mix_name="Monitor",
            volume=0.42,
            mute=True,
            node_name="mic",
        )

        planner.apply_intent(desired, intent)
        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(desired.channels["mic"].submix_state["Monitor"], {
            "vol": 0.42,
            "mute": True,
        })
        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "set_submix_state")

    def test_set_app_route_preserves_saved_volume_preference(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            app_routes={
                "app:io.ferdium.ferdium": AppRouteSpec(
                    app_id="app:io.ferdium.ferdium",
                    volume=0.42,
                )
            }
        )

        planner.apply_intent(
            desired,
            SetAppRoute(
                app_id="app:io.ferdium.ferdium",
                sink_name="wavelinux_voice_chat",
            ),
        )

        self.assertEqual(
            desired.app_routes["app:io.ferdium.ferdium"].sink_name,
            "wavelinux_voice_chat",
        )
        self.assertEqual(
            desired.app_routes["app:io.ferdium.ferdium"].volume,
            0.42,
        )

    def test_refresh_now_reconciles_selected_mic_fx_and_submixes(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=4),
                    submix_state={"Monitor": {"vol": 0.7, "mute": True}},
                )
            },
            mixes={"Monitor": MixSpec(name="Monitor")},
        )
        observed = ObservedState(
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            mix_hardware_routes={"Monitor": None},
            fx_sources_by_channel={"mic": None},
            fx_effects_by_channel={"mic": []},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        kinds = [action.kind for action in actions]
        self.assertEqual(kinds.count("ensure_submix_route"), 2)
        self.assertIn("apply_channel_fx", kinds)

    def test_refresh_now_rebuilds_when_only_params_change(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(
                        effects=["rnnoise"],
                        params_map={"rnnoise": {"VAD Threshold (%)": 75.0}},
                        generation=5,
                    ),
                )
            },
        )
        observed = ObservedState(
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            fx_params_by_channel={"mic": {"rnnoise": {"VAD Threshold (%)": 25.0}}},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        apply_actions = [action for action in actions if action.kind == "apply_channel_fx"]
        self.assertEqual(len(apply_actions), 1)
        self.assertEqual(apply_actions[0].payload["fx_spec"].generation, 5)

    def test_refresh_now_ignores_saved_params_for_disabled_effects(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(
                        effects=["compressor", "gate", "limiter"],
                        params_map={
                            "rnnoise": {"VAD Threshold (%)": 25.0},
                            "highpass": {"Freq": 80.0},
                            "eq": {"High Gain": 1.5},
                            "compressor": {"Threshold level (dB)": -16.02},
                            "gate": {"Threshold (dB)": -40.0},
                            "limiter": {"Limit (dB)": -0.5},
                        },
                        generation=6,
                    ),
                )
            },
        )
        observed = ObservedState(
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="wavelinux.fx.mic.source",
                )
            ],
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["compressor", "gate", "limiter"]},
            fx_params_by_channel={
                "mic": {
                    "compressor": {"Threshold level (dB)": -16.02},
                    "gate": {"Threshold (dB)": -40.0},
                    "limiter": {"Limit (dB)": -0.5},
                }
            },
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
            submix_source_by_channel={
                "mic": {
                    "Monitor": "wavelinux.fx.mic.source",
                    "Stream": "wavelinux.fx.mic.source",
                }
            },
            default_source="wavelinux.fx.mic.source",
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        self.assertNotIn("apply_channel_fx", [action.kind for action in actions])

    def test_refresh_now_recreates_missing_virtual_channel(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            mixes={"Monitor": MixSpec(name="Monitor")},
            virtual_channels={"wavelinux_game": "Game"},
        )
        observed = ObservedState(mix_hardware_routes={"Monitor": None})

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "ensure_virtual_channel")
        self.assertEqual(actions[0].payload["display_name"], "Game")

    def test_refresh_now_reapplies_saved_app_volume_to_returned_app(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            app_routes={
                "app:io.ferdium.ferdium": AppRouteSpec(
                    app_id="app:io.ferdium.ferdium",
                    volume=0.35,
                )
            }
        )
        observed = ObservedState(
            app_views=[
                RuntimeAppView(
                    app_id="app:io.ferdium.ferdium",
                    app_name="Ferdium",
                    active_indices=["71", "72"],
                    current_volume=1.0,
                )
            ],
            app_routes={"app:io.ferdium.ferdium": None},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        volume_actions = [action for action in actions if action.kind == "set_app_volume"]
        self.assertEqual(len(volume_actions), 2)
        self.assertEqual(volume_actions[0].payload["sink_input_index"], "71")
        self.assertEqual(volume_actions[1].payload["sink_input_index"], "72")
        for action in volume_actions:
            self.assertEqual(action.payload["volume"], 0.35)

    def test_refresh_now_removes_stale_channel_routing_and_fx(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=5),
                )
            }
        )
        observed = ObservedState(
            stale_channel_ids={"mic": "55"},
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        kinds = [action.kind for action in actions]
        self.assertIn("remove_node_routing", kinds)
        self.assertIn("clear_channel_fx", kinds)
        clear = next(action for action in actions if action.kind == "clear_channel_fx")
        self.assertEqual(clear.payload["generation"], 0)

    def test_refresh_now_clears_fx_for_channel_not_currently_managed(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic=None,
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=9),
                )
            },
        )
        observed = ObservedState(
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "clear_channel_fx")
        self.assertEqual(actions[0].payload["generation"], 9)

    def test_refresh_now_recreates_mixes_and_hardware_routes_after_restart(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            mixes={
                "Monitor": MixSpec(name="Monitor", hardware_sink="alsa_output.pci-1"),
                "Stream": MixSpec(name="Stream", hardware_sink="alsa_output.usb-2"),
            }
        )
        observed = ObservedState(
            mix_hardware_routes={},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("restart"))

        kinds = [action.kind for action in actions]
        self.assertEqual(kinds.count("ensure_output_mix"), 2)
        self.assertEqual(kinds.count("set_mix_hardware_route"), 2)

    def test_refresh_now_skips_submix_route_when_route_is_live_and_correct(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                )
            },
        )
        observed = ObservedState(
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
            submix_source_by_channel={"mic": {"Monitor": "mic", "Stream": "mic"}},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        ensure_actions = [action for action in actions if action.kind == "ensure_submix_route"]
        self.assertEqual(ensure_actions, [])

    def test_refresh_now_rebuilds_submix_route_when_source_is_stale(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=2),
                )
            },
        )
        observed = ObservedState(
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
            submix_source_by_channel={"mic": {"Monitor": "mic", "Stream": "mic"}},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        ensure_actions = [action for action in actions if action.kind == "ensure_submix_route"]
        self.assertEqual(len(ensure_actions), 2)

    def test_refresh_now_removes_non_selected_mic_routing_and_fx(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="usb_mic",
            channels={
                "built_in_mic": ChannelSpec(
                    node_name="built_in_mic",
                    capture_target="built_in_mic",
                    fx=FxSpec(effects=["rnnoise"], generation=4),
                ),
                "usb_mic": ChannelSpec(
                    node_name="usb_mic",
                    capture_target="usb_mic",
                ),
            },
        )
        observed = ObservedState(
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="built_in_mic",
                    description="Built-in Mic",
                    media_class="Audio/Source",
                    label="Built-in Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="built_in_mic",
                    meter_source="wavelinux.fx.built_in_mic.source",
                ),
                RuntimeChannelView(
                    node_id="77",
                    name="usb_mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="usb_mic",
                    meter_source="usb_mic",
                ),
            ],
            submix_owner_by_channel={
                "built_in_mic": {"Monitor": "11", "Stream": "12"},
            },
            fx_sources_by_channel={
                "built_in_mic": "wavelinux.fx.built_in_mic.source",
                "usb_mic": None,
            },
            fx_effects_by_channel={
                "built_in_mic": ["rnnoise"],
                "usb_mic": [],
            },
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        remove_actions = [
            action for action in actions
            if action.kind == "remove_node_routing"
        ]
        clear_actions = [
            action for action in actions
            if action.kind == "clear_channel_fx"
        ]
        self.assertEqual(len(remove_actions), 1)
        self.assertEqual(remove_actions[0].payload["node_id"], "55")
        self.assertEqual(len(clear_actions), 1)
        self.assertEqual(clear_actions[0].payload["node_name"], "built_in_mic")
        self.assertEqual(clear_actions[0].payload["generation"], 4)

    def test_refresh_now_clears_unwanted_fx_for_managed_channel(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="mic",
            channels={"mic": ChannelSpec(node_name="mic", capture_target="mic")},
        )
        observed = ObservedState(
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
            submix_source_by_channel={"mic": {"Monitor": "mic", "Stream": "mic"}},
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        clear_actions = [action for action in actions if action.kind == "clear_channel_fx"]
        self.assertEqual(len(clear_actions), 1)
        self.assertEqual(clear_actions[0].payload["node_name"], "mic")

    def test_refresh_now_sets_default_source_to_fx_source_when_mismatched(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="mic",
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=2),
                )
            },
        )
        observed = ObservedState(
            default_source="mic",
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
            submix_source_by_channel={"mic": {"Monitor": "wavelinux.fx.mic.source", "Stream": "wavelinux.fx.mic.source"}},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        default_actions = [a for a in actions if a.kind == "set_default_source"]
        self.assertEqual(len(default_actions), 1)
        self.assertEqual(default_actions[0].payload["source_name"], "wavelinux.fx.mic.source")

    def test_refresh_now_sets_default_source_to_selected_mic_when_mismatched(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            selected_mic="mic",
            channels={"mic": ChannelSpec(node_name="mic", capture_target="mic")},
        )
        observed = ObservedState(
            default_source="alsa_input.usb-other",
            mic_inputs=[
                RuntimeChannelView(
                    node_id="55",
                    name="mic",
                    description="USB Mic",
                    media_class="Audio/Source",
                    label="USB Mic",
                    channel_type="Microphone",
                    icon="mic",
                    is_mic=True,
                    capture_target="mic",
                    meter_source="mic",
                )
            ],
            submix_owner_by_channel={"mic": {"Monitor": "11", "Stream": "12"}},
            submix_live_by_channel={"mic": {"Monitor": True, "Stream": True}},
            submix_source_by_channel={"mic": {"Monitor": "mic", "Stream": "mic"}},
        )

        actions = planner.reconcile(desired, observed, RefreshNow("tick"))

        default_actions = [a for a in actions if a.kind == "set_default_source"]
        self.assertEqual(len(default_actions), 1)
        self.assertEqual(default_actions[0].payload["source_name"], "mic")

    def test_set_card_profile_emits_serialized_action(self):
        planner = RuntimePlanner()
        desired = DesiredState()
        observed = ObservedState()
        intent = SetCardProfile(card_name="alsa_card.pci-1", profile_name="pro-audio")

        planner.apply_intent(desired, intent)
        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "set_card_profile")
        self.assertEqual(actions[0].payload["card_name"], "alsa_card.pci-1")

    def test_recover_channel_reapplies_desired_fx_when_present(self):
        planner = RuntimePlanner()
        desired = DesiredState(
            channels={
                "mic": ChannelSpec(
                    node_name="mic",
                    capture_target="mic",
                    fx=FxSpec(effects=["rnnoise"], generation=4),
                )
            }
        )
        observed = ObservedState(
            fx_sources_by_channel={"mic": None},
            fx_effects_by_channel={"mic": []},
        )
        intent = RecoverChannel(node_name="mic")

        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "apply_channel_fx")
        self.assertEqual(actions[0].payload["node_name"], "mic")

    def test_recover_channel_clears_fx_when_no_desired_chain(self):
        planner = RuntimePlanner()
        desired = DesiredState(channels={"mic": ChannelSpec(node_name="mic")})
        observed = ObservedState(
            fx_sources_by_channel={"mic": "wavelinux.fx.mic.source"},
            fx_effects_by_channel={"mic": ["rnnoise"]},
        )
        intent = RecoverChannel(node_name="mic")

        actions = planner.reconcile(desired, observed, intent)

        self.assertEqual(len(actions), 1)
        self.assertEqual(actions[0].kind, "clear_channel_fx")
        self.assertEqual(actions[0].payload["node_name"], "mic")


if __name__ == "__main__":
    unittest.main()
