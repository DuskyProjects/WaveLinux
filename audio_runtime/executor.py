"""Serial action executor for runtime operations."""

from __future__ import annotations

from .models import (
    MixSpec,
    ObservedState,
    OperationStatus,
    RuntimeAppView,
    RuntimeChannelView,
    RuntimeSinkView,
    RuntimeViewState,
)


class RuntimeExecutor:
    """Applies planned actions and verifies the resulting graph state."""

    _OPTIMISTIC_ACTIONS = {
        "set_app_route",
        "set_app_volume",
        "set_mix_volume",
        "set_source_volume",
        "set_submix_state",
    }

    def __init__(self, adapter, diagnostics):
        self.adapter = adapter
        self.diagnostics = diagnostics
        self._last_invariant_signature = ()

    def observe(self, desired_state):
        with self.adapter.session() as engine:
            snap = engine.create_snapshot()
            mics = engine.get_hardware_inputs(snap=snap)
            virtual_nodes = engine.get_virtual_sinks(snap=snap)
            all_sinks = engine.get_all_sinks(snap=snap)
            apps = engine.get_sink_inputs(snap=snap)
            live_by_owner = engine.snapshot_sink_inputs_by_owner(snap=snap)
            tracked_names = (
                set(desired_state.channels.keys())
                | {node.name for node in mics}
                | {node.name for node in virtual_nodes}
            )
            observed = ObservedState(
                default_source=engine.get_default_source(),
                snapshot=snap,
            )
            observed.source_names = {
                node.name for node in getattr(snap, "nodes", [])
                if getattr(node, "media_class", "").startswith("Audio/Source")
            }
            for node_name in tracked_names:
                observed.fx_sources_by_channel[node_name] = engine.get_channel_fx_source(
                    node_name,
                    snap=snap,
                )
                observed.fx_effects_by_channel[node_name] = engine.get_channel_effects(node_name)
                info = engine.channel_fx.get(node_name) or {}
                observed.fx_params_by_channel[node_name] = {
                    effect_id: dict(values)
                    for effect_id, values in (info.get("params", {}) or {}).items()
                }
            for mix_name, mix in engine.output_mixes.items():
                observed.mix_hardware_routes[mix_name] = engine.get_live_mix_hardware_route(
                    mix_name,
                    snap=snap,
                )
                master_volume, _ = engine.get_sink_volume_by_name(mix.sink_name, snap=snap)
                observed.mix_master_volumes[mix_name] = master_volume
            observed.mic_inputs = [
                self._build_channel_view(
                    engine,
                    node,
                    is_mic=True,
                    live_by_owner=live_by_owner,
                    desired_state=desired_state,
                    observed=observed,
                )
                for node in mics
            ]
            observed.virtual_channels = [
                self._build_channel_view(
                    engine,
                    node,
                    is_mic=False,
                    live_by_owner=live_by_owner,
                    desired_state=desired_state,
                    observed=observed,
                )
                for node in virtual_nodes
            ]
            observed.present_node_names = {
                view.name for view in (observed.mic_inputs + observed.virtual_channels)
            }
            observed.channel_ids_by_name = {
                view.name: view.node_id
                for view in (observed.mic_inputs + observed.virtual_channels)
            }
            observed.sinks = [
                RuntimeSinkView(
                    name=sink["name"],
                    display_name=self._display_name_for_sink(engine, sink["name"], snap=snap),
                    is_internal=(
                        sink["name"].startswith("wavelinux_mix_")
                        or sink["name"].startswith("wavelinux_src_")
                        or sink["name"].endswith(".monitor")
                    ),
                )
                for sink in all_sinks
            ]
            observed.app_views = self._build_app_views(engine, apps)
            for app_view in observed.app_views:
                observed.app_routes[app_view.app_id] = app_view.current_sink
            return observed

    def execute(self, actions, *, desired_state, observed_state,
                status_callback=None):
        if not actions:
            return self._finalize_observed(
                desired_state,
                observed_state,
                actions=actions,
            )
        for action in actions:
            kind = action.kind
            payload = dict(action.payload)
            if kind == "apply_channel_fx":
                self._apply_channel_fx(payload, desired_state, status_callback)
            elif kind == "clear_channel_fx":
                self._clear_channel_fx(payload, desired_state, status_callback)
            elif kind == "set_submix_state":
                self._set_submix_state(payload)
            elif kind == "ensure_submix_route":
                self._ensure_submix_route(payload)
            elif kind == "remove_node_routing":
                self._remove_node_routing(payload)
            elif kind == "set_mix_hardware_route":
                self._set_mix_hardware_route(payload)
            elif kind == "set_mix_volume":
                self._set_mix_volume(payload)
            elif kind == "set_source_volume":
                self._set_source_volume(payload)
            elif kind == "set_default_source":
                self._set_default_source(payload)
            elif kind == "set_app_route":
                self._set_app_route(payload)
            elif kind == "set_app_volume":
                self._set_app_volume(payload)
            elif kind == "set_card_profile":
                self._set_card_profile(payload)
            elif kind == "ensure_output_mix":
                self._ensure_output_mix(payload)
            elif kind == "ensure_virtual_channel":
                self._ensure_virtual_channel(payload)
        if self._can_finalize_optimistically(actions):
            self._apply_optimistic_updates(observed_state, actions)
            return self._finalize_observed(
                desired_state,
                observed_state,
                actions=actions,
            )
        refreshed = self.observe(desired_state)
        return self._finalize_observed(
            desired_state,
            refreshed,
            actions=actions,
        )

    def _finalize_observed(self, desired_state, observed_state, *, actions):
        observed_state.health = self._check_invariants(desired_state, observed_state)
        signature = tuple(sorted(observed_state.health.items()))
        if observed_state.health and signature != self._last_invariant_signature:
            self.diagnostics.export_failure(
                "runtime_invariant_failed",
                desired=desired_state,
                observed=observed_state,
                actions=actions,
                health=observed_state.health,
                status={"intent": "execute"},
            )
        self._last_invariant_signature = signature
        self.diagnostics.snapshot("post-execute", {
            "desired": desired_state,
            "observed": observed_state,
            "actions": actions,
            "health": observed_state.health,
        })
        return observed_state

    @classmethod
    def _can_finalize_optimistically(cls, actions):
        return all(action.kind in cls._OPTIMISTIC_ACTIONS for action in actions)

    @staticmethod
    def _apply_optimistic_updates(observed_state, actions):
        for action in actions:
            payload = action.payload
            if action.kind == "set_submix_state":
                RuntimeExecutor._apply_optimistic_submix_state(observed_state, payload)
            elif action.kind == "set_mix_volume":
                observed_state.mix_master_volumes[payload["mix_name"]] = float(payload["volume"])
            elif action.kind == "set_source_volume":
                RuntimeExecutor._apply_optimistic_source_volume(observed_state, payload)
            elif action.kind == "set_app_volume":
                RuntimeExecutor._apply_optimistic_app_volume(observed_state, payload)
            elif action.kind == "set_app_route":
                RuntimeExecutor._apply_optimistic_app_route(observed_state, payload)

    @staticmethod
    def _apply_optimistic_submix_state(observed_state, payload):
        node_id = str(payload["node_id"])
        mix_name = payload["mix_name"]
        volume = float(payload["volume"])
        mute = bool(payload["mute"])
        for node in list(observed_state.mic_inputs) + list(observed_state.virtual_channels):
            if str(node.node_id) != node_id:
                continue
            if mix_name == "Monitor":
                node.monitor_volume = volume
                node.monitor_mute = mute
            elif mix_name == "Stream":
                node.stream_volume = volume
                node.stream_mute = mute
            break

    @staticmethod
    def _apply_optimistic_source_volume(observed_state, payload):
        node_name = str(payload["node_name"])
        volume = float(payload["volume"])
        for node in observed_state.mic_inputs:
            if node.name != node_name:
                continue
            node.source_volume = volume
            break

    @staticmethod
    def _apply_optimistic_app_volume(observed_state, payload):
        sink_input_index = str(payload["sink_input_index"])
        volume = float(payload["volume"])
        for app_view in observed_state.app_views:
            if sink_input_index not in app_view.active_indices:
                continue
            app_view.current_volume = volume
            break

    @staticmethod
    def _apply_optimistic_app_route(observed_state, payload):
        app_id = payload["app_id"]
        sink_name = payload["sink_name"]
        observed_state.app_routes[app_id] = sink_name
        for app_view in observed_state.app_views:
            if app_view.app_id != app_id:
                continue
            app_view.current_sink = sink_name
            break

    def build_view_state(self, desired_state, observed_state, fx_statuses,
                         pending_operations):
        mixes = {}
        for mix_name in sorted(
                set(desired_state.mixes.keys()) | set(observed_state.mix_hardware_routes.keys())):
            desired_mix = desired_state.mixes.get(mix_name, MixSpec(name=mix_name))
            mixes[mix_name] = MixSpec(
                name=mix_name,
                hardware_sink=observed_state.mix_hardware_routes.get(
                    mix_name, desired_mix.hardware_sink
                ),
                sink_name=desired_mix.sink_name,
                source_name=desired_mix.source_name,
                master_volume=observed_state.mix_master_volumes.get(
                    mix_name, desired_mix.master_volume
                ),
            )
        return RuntimeViewState(
            channels={k: v for k, v in desired_state.channels.items()},
            mixes=mixes,
            apps=dict(observed_state.app_routes),
            mic_inputs=list(observed_state.mic_inputs),
            virtual_channels=list(observed_state.virtual_channels),
            sinks=list(observed_state.sinks),
            app_views=list(observed_state.app_views),
            present_node_names=set(observed_state.present_node_names),
            default_source=observed_state.default_source,
            fx_status_by_channel={k: v for k, v in fx_statuses.items()},
            health=dict(observed_state.health),
            pending_operations=dict(pending_operations),
            node_count=len(observed_state.mic_inputs) + len(observed_state.virtual_channels),
            app_count=len(observed_state.app_views),
            observed_at=observed_state.timestamp,
        )

    def _apply_channel_fx(self, payload, desired_state, status_callback):
        node_name = payload["node_name"]
        capture_target = payload["capture_target"]
        fx_spec = payload["fx_spec"]
        self._emit_status(
            status_callback,
            OperationStatus(
                node_name=node_name,
                state="building",
                generation=fx_spec.generation,
                message="Building replacement FX chain",
            ),
        )
        with self.adapter.session() as engine:
            result = engine.apply_channel_fx_transaction(
                node_name,
                capture_target,
                list(fx_spec.effects),
                params_map=fx_spec.params_map,
            )
        verified = bool(result.get("success"))
        if verified:
            self._emit_status(
                status_callback,
                OperationStatus(
                    node_name=node_name,
                    state="active" if fx_spec.effects else "idle",
                    generation=fx_spec.generation,
                    message="FX chain active" if fx_spec.effects else "FX chain cleared",
                ),
            )
            return
        bundle = self.diagnostics.export_failure(
            "fx_apply_verification_failed",
            desired=desired_state,
            observed=self.observe(desired_state),
            actions=[payload],
            health={"channel": node_name},
            status={"result": dict(result or {})},
        )
        failure_stage = result.get("failure_stage") or "unknown"
        message = (result.get("message") or "").strip()
        self._emit_status(
            status_callback,
            OperationStatus(
                node_name=node_name,
                state="degraded",
                generation=fx_spec.generation,
                message=(
                    f"FX rebuild failed at {failure_stage}; diagnostics: {bundle}"
                    + (f" ({message})" if message else "")
                ),
                error=message,
                diagnostics_path=bundle,
            ),
        )

    def _clear_channel_fx(self, payload, desired_state, status_callback):
        node_name = payload["node_name"]
        generation = int(payload.get("generation", 0))
        self._emit_status(
            status_callback,
            OperationStatus(
                node_name=node_name,
                state="clearing",
                generation=generation,
                message="Clearing FX chain",
            ),
        )
        with self.adapter.session() as engine:
            result = engine.clear_channel_fx_transaction(node_name)
        if not result.get("success"):
            bundle = self.diagnostics.export_failure(
                "fx_clear_verification_failed",
                desired=desired_state,
                observed=self.observe(desired_state),
                actions=[payload],
                health={"channel": node_name},
                status={"result": dict(result or {})},
            )
            failure_stage = result.get("failure_stage") or "unknown"
            message = (result.get("message") or "").strip()
            self._emit_status(
                status_callback,
                OperationStatus(
                    node_name=node_name,
                    state="degraded",
                    generation=generation,
                    message=(
                        f"FX clear failed at {failure_stage}; diagnostics: {bundle}"
                        + (f" ({message})" if message else "")
                    ),
                    error=message,
                    diagnostics_path=bundle,
                ),
            )
            return
        self._emit_status(
            status_callback,
            OperationStatus(
                node_name=node_name,
                state="idle",
                generation=generation,
                message="FX chain cleared",
            ),
        )

    def _set_submix_state(self, payload):
        with self.adapter.session() as engine:
            engine.set_submix_volume(
                payload["node_id"], payload["mix_name"], payload["volume"]
            )
            engine.set_submix_mute(
                payload["node_id"], payload["mix_name"], payload["mute"]
            )

    def _ensure_submix_route(self, payload):
        with self.adapter.session() as engine:
            engine.route_input_to_submix(
                payload["node_id"],
                payload["node_name"],
                payload["media_class"],
                payload["mix_name"],
                initial_state=payload.get("initial_state") or {},
            )
            initial = payload.get("initial_state") or {}
            if initial:
                engine.set_submix_volume(
                    payload["node_id"],
                    payload["mix_name"],
                    initial.get("vol", 1.0),
                )
                engine.set_submix_mute(
                    payload["node_id"],
                    payload["mix_name"],
                    bool(initial.get("mute", False)),
                )

    def _remove_node_routing(self, payload):
        with self.adapter.session() as engine:
            engine.remove_node_routing(payload["node_id"])

    def _set_mix_hardware_route(self, payload):
        with self.adapter.session() as engine:
            if payload["sink_name"]:
                engine.route_mix_to_hardware(payload["mix_name"], payload["sink_name"])
            else:
                engine.unroute_mix_from_hardware(payload["mix_name"])

    def _set_mix_volume(self, payload):
        with self.adapter.session() as engine:
            mix = engine.output_mixes.get(payload["mix_name"])
            if mix:
                engine.set_sink_volume_by_name(mix.sink_name, payload["volume"])

    def _set_source_volume(self, payload):
        node_name = payload.get("node_name")
        if not node_name:
            return
        with self.adapter.session() as engine:
            engine.set_source_volume_by_name(node_name, payload["volume"])

    def _set_default_source(self, payload):
        source_name = payload.get("source_name")
        if not source_name:
            return
        with self.adapter.session() as engine:
            engine.set_default_source(source_name)

    def _set_app_route(self, payload):
        with self.adapter.session() as engine:
            snap = engine.create_snapshot(force=True)
            for app in engine.get_sink_inputs(snap=snap):
                app_id = app.get("app_id") or app.get("app_name") or app.get("binary") or "Unknown App"
                if app_id != payload["app_id"]:
                    continue
                idx = app.get("index")
                if idx is not None and payload["sink_name"]:
                    engine.move_app_to_sink(idx, payload["sink_name"])

    def _set_app_volume(self, payload):
        with self.adapter.session() as engine:
            engine.set_sink_input_volume(payload["sink_input_index"], payload["volume"])

    def _set_card_profile(self, payload):
        with self.adapter.session() as engine:
            engine.set_card_profile(payload["card_name"], payload["profile_name"])

    def _ensure_output_mix(self, payload):
        with self.adapter.session() as engine:
            engine.create_output_mix(payload["mix_name"])

    def _ensure_virtual_channel(self, payload):
        with self.adapter.session() as engine:
            engine.create_virtual_sink(payload["display_name"])

    @staticmethod
    def _emit_status(callback, status):
        if callback is not None:
            callback(status)

    @staticmethod
    def _check_invariants(desired_state, observed_state):
        health = {}
        managed_channels = {
            view.name for view in (
                list(observed_state.mic_inputs) + list(observed_state.virtual_channels)
            )
            if (not view.is_mic) or (view.name == desired_state.selected_mic)
        }
        for node_name in managed_channels:
            route_owners = observed_state.submix_owner_by_channel.get(node_name, {})
            route_live = observed_state.submix_live_by_channel.get(node_name, {})
            for mix_name in ("Monitor", "Stream"):
                owner = route_owners.get(mix_name)
                if not owner:
                    health.setdefault(node_name, f"submix_{mix_name.lower()}_missing")
                    break
                if not route_live.get(mix_name, False):
                    health.setdefault(node_name, f"submix_{mix_name.lower()}_dead")
                    break
        for node_name, chan in desired_state.channels.items():
            if node_name not in observed_state.present_node_names:
                continue
            if not chan.fx.effects:
                continue
            fx_source = observed_state.fx_sources_by_channel.get(node_name)
            fx_effects = observed_state.fx_effects_by_channel.get(node_name) or []
            desired_effects = list(chan.fx.effects)
            desired_params = {
                key: dict(values)
                for key, values in (chan.fx.params_map or {}).items()
            }
            observed_params = observed_state.fx_params_by_channel.get(node_name) or {}
            if not fx_source:
                health.setdefault(node_name, "desired_fx_missing")
                continue
            if not fx_effects:
                health.setdefault(node_name, "fx_effects_not_visible")
            elif fx_effects != desired_effects:
                health.setdefault(node_name, "fx_effects_mismatch")
            elif any(
                (desired_params.get(effect_id) or {}) != (observed_params.get(effect_id) or {})
                for effect_id in desired_effects
            ):
                health.setdefault(node_name, "fx_params_mismatch")
            if fx_source not in observed_state.source_names:
                health.setdefault(node_name, "fx_source_not_present")

        source_owners = {}
        for node_name, fx_source in observed_state.fx_sources_by_channel.items():
            if not fx_source:
                continue
            source_owners.setdefault(fx_source, []).append(node_name)
        for owners in source_owners.values():
            if len(owners) < 2:
                continue
            for node_name in owners:
                health.setdefault(node_name, "duplicate_fx_source")

        selected_mic = desired_state.selected_mic
        if selected_mic and selected_mic in observed_state.present_node_names:
            chan = desired_state.channels.get(selected_mic)
            if chan and chan.fx.effects:
                expected = observed_state.fx_sources_by_channel.get(selected_mic)
                if not expected:
                    health.setdefault(selected_mic, "default_source_expected_fx_missing")
                elif observed_state.default_source != expected:
                    health.setdefault(selected_mic, "default_source_mismatch")
        return health

    @staticmethod
    def _desired_submix_state(desired_state, node_name, mix_name, *, is_mic):
        chan = desired_state.channels.get(node_name)
        state = {}
        if chan is not None:
            state.update(chan.submix_state.get(mix_name, {}))
        if "vol" not in state:
            state["vol"] = 1.0
        if "mute" not in state:
            state["mute"] = bool(is_mic and mix_name == "Monitor")
        return state

    def _build_channel_view(self, engine, node, *, is_mic, live_by_owner,
                            desired_state, observed):
        node_id = str(node.pw_id)
        node_name = node.name
        mon_default = self._desired_submix_state(
            desired_state, node_name, "Monitor", is_mic=is_mic
        )
        str_default = self._desired_submix_state(
            desired_state, node_name, "Stream", is_mic=is_mic
        )
        owner_mon = engine.submix_loopbacks.get(f"{node_id}->Monitor")
        owner_str = engine.submix_loopbacks.get(f"{node_id}->Stream")
        source_mon = engine.submix_sources.get(f"{node_id}->Monitor")
        source_str = engine.submix_sources.get(f"{node_id}->Stream")
        owner_mon_key = str(owner_mon) if owner_mon is not None else None
        owner_str_key = str(owner_str) if owner_str is not None else None
        mon_live = live_by_owner.get(owner_mon_key) if owner_mon_key is not None else None
        str_live = live_by_owner.get(owner_str_key) if owner_str_key is not None else None
        observed.submix_owner_by_channel.setdefault(node_name, {})
        observed.submix_owner_by_channel[node_name]["Monitor"] = owner_mon_key
        observed.submix_owner_by_channel[node_name]["Stream"] = owner_str_key
        observed.submix_live_by_channel.setdefault(node_name, {})
        observed.submix_live_by_channel[node_name]["Monitor"] = mon_live is not None
        observed.submix_live_by_channel[node_name]["Stream"] = str_live is not None
        observed.submix_source_by_channel.setdefault(node_name, {})
        observed.submix_source_by_channel[node_name]["Monitor"] = (
            str(source_mon) if source_mon is not None else None
        )
        observed.submix_source_by_channel[node_name]["Stream"] = (
            str(source_str) if source_str is not None else None
        )
        mon_volume, mon_mute = mon_live or (
            float(mon_default.get("vol", 1.0)),
            bool(mon_default.get("mute", False)),
        )
        str_volume, str_mute = str_live or (
            float(str_default.get("vol", 1.0)),
            bool(str_default.get("mute", False)),
        )
        fx_source = observed.fx_sources_by_channel.get(node_name)
        if is_mic:
            label = engine.friendly_name(node.description)
            channel_type = "Microphone"
            icon = "🎤"
            capture_target = node_name
            meter_source = fx_source or node_name
        else:
            label = node_name.replace("wavelinux_", "").replace("_", " ").title()
            channel_type = "Virtual"
            icon = "🎵"
            capture_target = f"{node_name}.monitor"
            meter_source = fx_source or capture_target
        return RuntimeChannelView(
            node_id=node_id,
            name=node_name,
            description=node.description,
            media_class=node.media_class,
            label=label,
            channel_type=channel_type,
            icon=icon,
            is_mic=is_mic,
            capture_target=capture_target,
            meter_source=meter_source,
            source_volume=float(getattr(node, "volume", 1.0)),
            source_mute=bool(getattr(node, "muted", False)),
            monitor_volume=float(mon_volume),
            monitor_mute=bool(mon_mute),
            stream_volume=float(str_volume),
            stream_mute=bool(str_mute),
            fx_effects=list(observed.fx_effects_by_channel.get(node_name, [])),
            fx_running=bool(observed.fx_sources_by_channel.get(node_name)),
        )

    @staticmethod
    def _display_name_for_sink(engine, sink_name, *, snap):
        if sink_name.startswith("wavelinux_mix_") or sink_name.startswith("wavelinux_src_"):
            return sink_name
        if sink_name.startswith("wavelinux_"):
            pretty = sink_name.replace("wavelinux_", "").replace("_", " ").title()
            return pretty
        return engine.display_name_for_sink(sink_name, snap=snap)

    @staticmethod
    def _build_app_views(engine, apps):
        grouped = {}
        for app in apps:
            app_id = app.get("app_id") or app.get("app_name") or app.get("binary") or "Unknown App"
            app_name = app.get("app_name") or app.get("binary") or app_id
            icon_candidates = list(app.get("app_icon_candidates") or [])
            resolved_app_id = app.get("resolved_app_id") or app_id
            resolved_app_name = app.get("resolved_app_name") or app_name
            identity_source = app.get("app_identity_source") or ""
            override_applied = bool(app.get("app_identity_override_applied"))
            view = grouped.get(app_id)
            if view is None:
                view = RuntimeAppView(
                    app_id=app_id,
                    app_name=app_name,
                    icon_candidates=icon_candidates,
                    resolved_app_id=resolved_app_id,
                    resolved_app_name=resolved_app_name,
                    identity_source=identity_source,
                    override_applied=override_applied,
                )
                grouped[app_id] = view
            elif app_name and (view.app_name == view.app_id or len(app_name) > len(view.app_name)):
                view.app_name = app_name
            for candidate in icon_candidates:
                if candidate and candidate not in view.icon_candidates:
                    view.icon_candidates.append(candidate)
            if (
                resolved_app_name
                and (
                    not view.resolved_app_name
                    or view.resolved_app_name == view.resolved_app_id
                    or len(resolved_app_name) > len(view.resolved_app_name)
                )
            ):
                view.resolved_app_name = resolved_app_name
            if not view.identity_source and identity_source:
                view.identity_source = identity_source
            view.override_applied = bool(view.override_applied or override_applied)
            if not view.resolved_app_id:
                view.resolved_app_id = resolved_app_id
            elif resolved_app_id and view.resolved_app_id != resolved_app_id:
                # Multiple resolved identities were merged into one canonical
                # target, so reset/pin must treat the source as ambiguous.
                view.resolved_app_id = ""
            idx = app.get("index")
            if idx is not None:
                view.active_indices.append(str(idx))
            if view.current_sink is None:
                view.current_sink = app.get("sink")
            app_volume = app.get("volume")
            if app_volume is not None:
                try:
                    view.current_volume = float(app_volume)
                except (TypeError, ValueError):
                    pass
        for view in grouped.values():
            if view.active_indices and view.current_volume is None:
                view.current_volume = engine.get_sink_input_volume(view.active_indices[0])
        return sorted(grouped.values(), key=lambda item: item.app_name.lower())
