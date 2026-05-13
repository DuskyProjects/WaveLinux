"""Pure reconcile logic for runtime intents."""

from __future__ import annotations

import re

from .models import (
    Action,
    AppRouteSpec,
    ChannelSpec,
    ClearChannelFx,
    EnsureSubmixRoute,
    FxSpec,
    RemoveNodeRouting,
    RecoverChannel,
    RefreshNow,
    SetAppRoute,
    SetAppVolume,
    SetCardProfile,
    SetChannelFx,
    SetMixHardwareRoute,
    SetMixVolume,
    SetSelectedMic,
    SetSubmixState,
)


class RuntimePlanner:
    """Translate desired-state changes into side-effect actions."""

    _BT_SINK_FAMILY_RE = re.compile(
        r'^(bluez_output\.[0-9A-Fa-f]{2}(?:[_:-][0-9A-Fa-f]{2}){5})(?:\..+)?$',
        re.IGNORECASE,
    )
    _APP_VOLUME_EPSILON = 0.01

    @classmethod
    def _canonical_sink_name(cls, sink_name):
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            return ""
        match = cls._BT_SINK_FAMILY_RE.match(sink_name)
        if match:
            return match.group(1).lower().replace('-', '_').replace(':', '_')
        return sink_name

    @classmethod
    def _sink_names_match(cls, left, right):
        return cls._canonical_sink_name(left) == cls._canonical_sink_name(right)

    @staticmethod
    def _fx_params_match(current_params, wanted_params, effect_ids):
        current_params = current_params or {}
        wanted_params = wanted_params or {}
        for effect_id in effect_ids:
            if (current_params.get(effect_id) or {}) != (wanted_params.get(effect_id) or {}):
                return False
        return True

    def apply_intent(self, desired_state, intent):
        if isinstance(intent, SetChannelFx):
            chan = desired_state.channels.get(intent.node_name)
            if chan is None:
                chan = ChannelSpec(
                    node_name=intent.node_name,
                    capture_target=intent.capture_target,
                )
                desired_state.channels[intent.node_name] = chan
            chan.capture_target = intent.capture_target
            chan.fx = FxSpec(
                effects=list(intent.fx_spec.effects),
                params_map={k: dict(v) for k, v in intent.fx_spec.params_map.items()},
                generation=int(intent.fx_spec.generation),
            )
            return
        if isinstance(intent, ClearChannelFx):
            chan = desired_state.channels.get(intent.node_name)
            if chan is None:
                chan = ChannelSpec(node_name=intent.node_name)
                desired_state.channels[intent.node_name] = chan
            chan.fx = FxSpec(generation=int(intent.generation))
            return
        if isinstance(intent, SetSubmixState):
            if intent.node_name:
                chan = desired_state.channels.get(intent.node_name)
                if chan is None:
                    chan = ChannelSpec(node_name=intent.node_name)
                    desired_state.channels[intent.node_name] = chan
                chan.submix_state[intent.mix_name] = {
                    "vol": float(intent.volume),
                    "mute": bool(intent.mute),
                }
            return
        if isinstance(intent, EnsureSubmixRoute):
            chan = desired_state.channels.get(intent.node_name)
            if chan is None:
                chan = ChannelSpec(node_name=intent.node_name)
                desired_state.channels[intent.node_name] = chan
            chan.submix_state[intent.mix_name] = dict(intent.initial_state or {})
            return
        if isinstance(intent, SetMixHardwareRoute):
            mix = desired_state.mixes.get(intent.mix_name)
            if mix is None:
                from .models import MixSpec

                mix = MixSpec(name=intent.mix_name)
                desired_state.mixes[intent.mix_name] = mix
            mix.hardware_sink = intent.sink_name
            return
        if isinstance(intent, SetMixVolume):
            mix = desired_state.mixes.get(intent.mix_name)
            if mix is None:
                from .models import MixSpec

                mix = MixSpec(name=intent.mix_name)
                desired_state.mixes[intent.mix_name] = mix
            mix.master_volume = float(intent.volume)
            return
        if isinstance(intent, SetAppRoute):
            existing = desired_state.app_routes.get(intent.app_id)
            desired_state.app_routes[intent.app_id] = AppRouteSpec(
                app_id=intent.app_id,
                sink_name=intent.sink_name,
                volume=existing.volume if existing is not None else None,
            )
            return
        if isinstance(intent, SetCardProfile):
            return
        if isinstance(intent, SetSelectedMic):
            desired_state.selected_mic = intent.node_name
            return

    def reconcile(self, desired_state, observed_state, intent):
        if isinstance(intent, SetChannelFx):
            current_effects = observed_state.fx_effects_by_channel.get(intent.node_name, [])
            current_source = observed_state.fx_sources_by_channel.get(intent.node_name)
            wanted_effects = list(intent.fx_spec.effects)
            wanted_params = {
                key: dict(vals) for key, vals in intent.fx_spec.params_map.items()
            }
            current_params = observed_state.fx_params_by_channel.get(intent.node_name, {})
            if (current_effects != wanted_effects
                    or not self._fx_params_match(current_params, wanted_params, wanted_effects)
                    or not current_source):
                return [Action("apply_channel_fx", {
                    "node_name": intent.node_name,
                    "capture_target": intent.capture_target,
                    "fx_spec": FxSpec(
                        effects=wanted_effects,
                        params_map=wanted_params,
                        generation=int(intent.fx_spec.generation),
                    ),
                    "previous_effects": current_effects,
                })]
            return []
        if isinstance(intent, ClearChannelFx):
            if observed_state.fx_sources_by_channel.get(intent.node_name):
                return [Action("clear_channel_fx", {
                    "node_name": intent.node_name,
                    "generation": int(intent.generation),
                })]
            return []
        if isinstance(intent, SetSubmixState):
            return [Action("set_submix_state", {
                "node_id": str(intent.node_id),
                "mix_name": intent.mix_name,
                "volume": float(intent.volume),
                "mute": bool(intent.mute),
            })]
        if isinstance(intent, EnsureSubmixRoute):
            return [Action("ensure_submix_route", {
                "node_id": str(intent.node_id),
                "node_name": intent.node_name,
                "media_class": intent.media_class,
                "mix_name": intent.mix_name,
                "initial_state": dict(intent.initial_state or {}),
            })]
        if isinstance(intent, RemoveNodeRouting):
            return [Action("remove_node_routing", {
                "node_id": str(intent.node_id),
            })]
        if isinstance(intent, SetMixHardwareRoute):
            current = observed_state.mix_hardware_routes.get(intent.mix_name)
            if not self._sink_names_match(current, intent.sink_name):
                return [Action("set_mix_hardware_route", {
                    "mix_name": intent.mix_name,
                    "sink_name": intent.sink_name,
                })]
            return []
        if isinstance(intent, SetMixVolume):
            return [Action("set_mix_volume", {
                "mix_name": intent.mix_name,
                "volume": float(intent.volume),
            })]
        if isinstance(intent, SetAppRoute):
            if observed_state.app_routes.get(intent.app_id) != intent.sink_name:
                return [Action("set_app_route", {
                    "app_id": intent.app_id,
                    "sink_name": intent.sink_name,
                })]
            return []
        if isinstance(intent, SetAppVolume):
            return [Action("set_app_volume", {
                "sink_input_index": str(intent.sink_input_index),
                "volume": float(intent.volume),
            })]
        if isinstance(intent, SetCardProfile):
            return [Action("set_card_profile", {
                "card_name": intent.card_name,
                "profile_name": intent.profile_name,
            })]
        if isinstance(intent, RecoverChannel):
            chan = desired_state.channels.get(intent.node_name)
            if chan and chan.fx.effects:
                return [Action("apply_channel_fx", {
                    "node_name": chan.node_name,
                    "capture_target": chan.capture_target,
                    "fx_spec": chan.fx,
                    "previous_effects": observed_state.fx_effects_by_channel.get(chan.node_name, []),
                })]
            return [Action("clear_channel_fx", {
                "node_name": intent.node_name,
                "generation": 0,
            })]
        if isinstance(intent, RefreshNow):
            return self._reconcile_refresh(desired_state, observed_state)
        if isinstance(intent, SetSelectedMic):
            return []
        return []

    def _reconcile_refresh(self, desired_state, observed_state):
        actions = []
        cleared_fx = set()
        observed_mix_names = set(observed_state.mix_hardware_routes.keys())
        for mix_name, mix in desired_state.mixes.items():
            if mix_name not in observed_mix_names:
                actions.append(Action("ensure_output_mix", {"mix_name": mix_name}))
            current_hw = observed_state.mix_hardware_routes.get(mix_name)
            if not self._sink_names_match(current_hw, mix.hardware_sink):
                actions.append(Action("set_mix_hardware_route", {
                    "mix_name": mix_name,
                    "sink_name": mix.hardware_sink,
                }))

        present_virtual_names = {view.name for view in observed_state.virtual_channels}
        for sink_name, display_name in desired_state.virtual_channels.items():
            if sink_name not in present_virtual_names:
                actions.append(Action("ensure_virtual_channel", {
                    "sink_name": sink_name,
                    "display_name": display_name,
                }))

        managed_channels = []
        selected_mic = desired_state.selected_mic
        if selected_mic:
            managed_channels.extend(
                view for view in observed_state.mic_inputs if view.name == selected_mic
            )
        managed_channels.extend(observed_state.virtual_channels)
        present_managed = {view.name for view in managed_channels}

        for channel in managed_channels:
            chan_spec = desired_state.channels.get(channel.name)
            monitor_state = self._desired_submix_state(
                chan_spec, "Monitor", is_mic=channel.is_mic
            )
            stream_state = self._desired_submix_state(
                chan_spec, "Stream", is_mic=channel.is_mic
            )
            route_owners = observed_state.submix_owner_by_channel.get(channel.name, {})
            route_live = observed_state.submix_live_by_channel.get(channel.name, {})
            route_sources = observed_state.submix_source_by_channel.get(channel.name, {})
            fx_source = observed_state.fx_sources_by_channel.get(channel.name)
            if fx_source:
                expected_source = str(fx_source)
            else:
                expected_source = str(
                    (chan_spec.capture_target if chan_spec and chan_spec.capture_target else "")
                    or channel.capture_target
                )
            for mix_name, initial_state in (
                ("Monitor", monitor_state),
                ("Stream", stream_state),
            ):
                owner = route_owners.get(mix_name)
                live = bool(route_live.get(mix_name, False))
                current_source = route_sources.get(mix_name)
                if owner and live and current_source == expected_source:
                    continue
                actions.append(Action("ensure_submix_route", {
                    "node_id": channel.node_id,
                    "node_name": channel.name,
                    "media_class": channel.media_class,
                    "mix_name": mix_name,
                    "initial_state": initial_state,
                }))
            if chan_spec and chan_spec.fx.effects:
                current_effects = observed_state.fx_effects_by_channel.get(channel.name, [])
                current_params = observed_state.fx_params_by_channel.get(channel.name, {})
                current_source = observed_state.fx_sources_by_channel.get(channel.name)
                wanted_params = {
                    key: dict(vals)
                    for key, vals in chan_spec.fx.params_map.items()
                }
                if (current_effects != list(chan_spec.fx.effects)
                        or not self._fx_params_match(
                            current_params,
                            wanted_params,
                            list(chan_spec.fx.effects),
                        )
                        or not current_source):
                    actions.append(Action("apply_channel_fx", {
                        "node_name": channel.name,
                        "capture_target": chan_spec.capture_target or channel.capture_target,
                        "fx_spec": chan_spec.fx,
                        "previous_effects": current_effects,
                    }))
            elif observed_state.fx_sources_by_channel.get(channel.name):
                generation = int(chan_spec.fx.generation) if chan_spec else 0
                actions.append(Action("clear_channel_fx", {
                    "node_name": channel.name,
                    "generation": generation,
                }))
                cleared_fx.add(channel.name)

        if selected_mic and selected_mic in present_managed:
            selected_spec = desired_state.channels.get(selected_mic)
            expected_default = selected_mic
            if selected_spec and selected_spec.fx.effects:
                expected_default = observed_state.fx_sources_by_channel.get(selected_mic)
            elif selected_spec and selected_spec.capture_target:
                expected_default = selected_spec.capture_target
            if expected_default and observed_state.default_source != expected_default:
                actions.append(Action("set_default_source", {
                    "source_name": expected_default,
                }))

        for node_name, old_node_id in observed_state.stale_channel_ids.items():
            actions.append(Action("remove_node_routing", {
                "node_id": str(old_node_id),
            }))
            if observed_state.fx_sources_by_channel.get(node_name):
                actions.append(Action("clear_channel_fx", {
                    "node_name": node_name,
                    "generation": 0,
                }))
                cleared_fx.add(node_name)

        for node_name, chan_spec in desired_state.channels.items():
            if node_name in present_managed or node_name in cleared_fx:
                continue
            if chan_spec.fx.effects and observed_state.fx_sources_by_channel.get(node_name):
                actions.append(Action("clear_channel_fx", {
                    "node_name": node_name,
                    "generation": int(chan_spec.fx.generation),
                }))

        active_app_ids = {view.app_id for view in observed_state.app_views}
        for app_id, route_spec in desired_state.app_routes.items():
            if app_id not in active_app_ids:
                continue
            if observed_state.app_routes.get(app_id) != route_spec.sink_name:
                actions.append(Action("set_app_route", {
                    "app_id": app_id,
                    "sink_name": route_spec.sink_name,
                }))
            if route_spec.volume is None:
                continue
            app_view = next(
                (view for view in observed_state.app_views if view.app_id == app_id),
                None,
            )
            if app_view is None:
                continue
            live_volume = app_view.current_volume
            if (
                live_volume is not None
                and abs(float(live_volume) - float(route_spec.volume)) <= self._APP_VOLUME_EPSILON
            ):
                continue
            for sink_input_index in app_view.active_indices:
                actions.append(Action("set_app_volume", {
                    "sink_input_index": str(sink_input_index),
                    "volume": float(route_spec.volume),
                }))
        return actions

    @staticmethod
    def _desired_submix_state(chan_spec, mix_name, *, is_mic):
        state = {}
        if chan_spec is not None:
            state.update(chan_spec.submix_state.get(mix_name, {}))
        if "vol" not in state:
            state["vol"] = 1.0
        if "mute" not in state:
            state["mute"] = bool(is_mic and mix_name == "Monitor")
        return state
