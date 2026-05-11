"""Serialized runtime controller for WaveLinux audio graph mutations."""

from __future__ import annotations

from collections import OrderedDict
import queue
import threading
import time

from PyQt6.QtCore import QObject, QThread, pyqtSignal

from .diagnostics import RuntimeDiagnostics
from .executor import RuntimeExecutor
from .models import (
    AppRouteSpec,
    ChannelSpec,
    ClearChannelFx,
    DesiredState,
    EnsureSubmixRoute,
    FxSpec,
    MixSpec,
    OperationStatus,
    RemoveNodeRouting,
    RecoverChannel,
    RefreshNow,
    RuntimeViewState,
    SetAppRoute,
    SetAppVolume,
    SetCardProfile,
    SetChannelFx,
    SetMixHardwareRoute,
    SetMixVolume,
    SetSelectedMic,
    SetSubmixState,
)
from .planner import RuntimePlanner


class AudioRuntimeWorker(QObject):
    """Long-lived serialized worker that owns desired-state mutation."""

    view_state_ready = pyqtSignal(object)
    fx_status_ready = pyqtSignal(object)

    def __init__(self, adapter, diagnostics=None):
        super().__init__()
        self.adapter = adapter
        self.diagnostics = diagnostics or RuntimeDiagnostics()
        self.planner = RuntimePlanner()
        self.executor = RuntimeExecutor(adapter, self.diagnostics)
        self.desired_state = DesiredState()
        self.state_lock = threading.RLock()
        self._enqueue_lock = threading.Lock()
        self._queue = queue.Queue()
        self._stop = threading.Event()
        self._fx_statuses = {}
        self._pending_operations = {}
        self._last_observed_state = None
        self._last_observed_channel_ids = {}
        self._refresh_pending = False

    def enqueue(self, intent):
        if isinstance(intent, RefreshNow):
            with self._enqueue_lock:
                if self._refresh_pending:
                    return False
                self._refresh_pending = True
        self._queue.put(intent)
        return True

    def stop(self):
        self._stop.set()
        self._queue.put(None)

    def run(self):
        with self.state_lock:
            observed = self.executor.observe(self.desired_state)
            observed.stale_channel_ids = {}
            self._last_observed_state = observed
            self._last_observed_channel_ids = dict(observed.channel_ids_by_name)
            self._publish_view_state(observed)
        while not self._stop.is_set():
            intent = self._queue.get()
            if intent is None:
                continue
            batch = [intent]
            while True:
                try:
                    batch.append(self._queue.get_nowait())
                except queue.Empty:
                    break
            for item in self._coalesce(batch):
                if item is None:
                    continue
                self._process_intent(item)

    def _coalesce(self, intents):
        collapsed = []
        latest_by_key = OrderedDict()
        for intent in intents:
            key = self._collapse_key(intent)
            if key is None:
                collapsed.append(intent)
                continue
            latest_by_key[key] = intent
        collapsed.extend(latest_by_key.values())
        return collapsed

    @staticmethod
    def _collapse_key(intent):
        if isinstance(intent, (SetChannelFx, ClearChannelFx, RecoverChannel)):
            return ("fx", intent.node_name)
        if isinstance(intent, SetSubmixState):
            return ("submix", str(intent.node_id), intent.mix_name)
        if isinstance(intent, EnsureSubmixRoute):
            return ("ensure_submix", str(intent.node_id), intent.mix_name)
        if isinstance(intent, RemoveNodeRouting):
            return ("remove_node", str(intent.node_id))
        if isinstance(intent, SetMixHardwareRoute):
            return ("mix", intent.mix_name)
        if isinstance(intent, SetMixVolume):
            return ("mix_volume", intent.mix_name)
        if isinstance(intent, SetAppRoute):
            return ("app_route", intent.app_id)
        if isinstance(intent, SetAppVolume):
            return ("app_volume", str(intent.sink_input_index))
        if isinstance(intent, SetCardProfile):
            return ("card_profile", intent.card_name)
        if isinstance(intent, SetSelectedMic):
            return ("selected_mic",)
        if isinstance(intent, RefreshNow):
            return ("refresh",)
        return None

    def _process_intent(self, intent):
        try:
            with self.state_lock:
                self.planner.apply_intent(self.desired_state, intent)
                if self._requires_fresh_observe(intent) or self._last_observed_state is None:
                    observed = self.executor.observe(self.desired_state)
                else:
                    observed = self._last_observed_state
                observed.stale_channel_ids = {
                    name: old_id
                    for name, old_id in self._last_observed_channel_ids.items()
                    if observed.channel_ids_by_name.get(name) != old_id
                }
                self._last_observed_channel_ids = dict(observed.channel_ids_by_name)
                actions = self.planner.reconcile(self.desired_state, observed, intent)
                self._mark_pending(intent, active=True)
                try:
                    observed = self.executor.execute(
                        actions,
                        desired_state=self.desired_state,
                        observed_state=observed,
                        status_callback=self._handle_status,
                    )
                finally:
                    self._mark_pending(intent, active=False)
                self._last_observed_state = observed
                self._publish_view_state(observed)
        finally:
            if isinstance(intent, RefreshNow):
                self._clear_refresh_pending()

    def _mark_pending(self, intent, *, active):
        key = self._collapse_key(intent)
        if key is None:
            return
        pending_key = ":".join(str(part) for part in key)
        if active:
            self._pending_operations[pending_key] = type(intent).__name__
        else:
            self._pending_operations.pop(pending_key, None)

    def _handle_status(self, status):
        if not isinstance(status, OperationStatus):
            return
        status.updated_at = time.time()
        if status.node_name:
            self._fx_statuses[status.node_name] = status
        self.fx_status_ready.emit(status)

    def _publish_view_state(self, observed):
        view = self.executor.build_view_state(
            self.desired_state,
            observed,
            self._fx_statuses,
            self._pending_operations,
        )
        self.diagnostics.snapshot("view-state", view)
        self.view_state_ready.emit(view)

    def _clear_refresh_pending(self):
        with self._enqueue_lock:
            self._refresh_pending = False

    @staticmethod
    def _requires_fresh_observe(intent):
        return isinstance(intent, (
            ClearChannelFx,
            RecoverChannel,
            RefreshNow,
            SetAppRoute,
            SetChannelFx,
            SetMixHardwareRoute,
        ))


class AudioRuntimeController(QObject):
    """Main-thread facade for the serialized runtime worker."""

    view_state_changed = pyqtSignal(object)
    fx_status_changed = pyqtSignal(object)

    def __init__(self, adapter, parent=None):
        super().__init__(parent)
        self.adapter = adapter
        existing = getattr(getattr(adapter, "_engine", None), "_runtime_diagnostics", None)
        self.diagnostics = existing or RuntimeDiagnostics()
        raw_engine = getattr(adapter, "_engine", None)
        if raw_engine is not None:
            raw_engine._runtime_diagnostics = self.diagnostics
        self._latest_view_state = RuntimeViewState()
        self._latest_fx_status = {}
        self._last_requested_fx = {}
        self._fx_generations = {}
        self._desired_selected_mic = None
        self._thread = QThread(self)
        self._worker = AudioRuntimeWorker(adapter, diagnostics=self.diagnostics)
        self._worker.moveToThread(self._thread)
        self._thread.started.connect(self._worker.run)
        self._worker.view_state_ready.connect(self._on_view_state_ready)
        self._worker.fx_status_ready.connect(self._on_fx_status_ready)
        self._thread.start()

    @property
    def latest_view_state(self):
        return self._latest_view_state

    def fx_status_for(self, node_name):
        return self._latest_fx_status.get(node_name, OperationStatus(node_name=node_name))

    def enqueue_intent(self, intent):
        self._worker.enqueue(intent)

    def set_channel_fx(self, node_name, capture_target, effects, params_map):
        wanted = {
            "capture_target": capture_target,
            "effects": list(effects or []),
            "params_map": {
                key: dict(vals) for key, vals in (params_map or {}).items()
            },
        }
        if self._last_requested_fx.get(node_name) == wanted:
            return self._fx_generations.get(node_name, 0)
        generation = self._fx_generations.get(node_name, 0) + 1
        self._fx_generations[node_name] = generation
        self._last_requested_fx[node_name] = wanted
        self.enqueue_intent(SetChannelFx(
            node_name=node_name,
            capture_target=capture_target,
            fx_spec=FxSpec(
                effects=wanted["effects"],
                params_map=wanted["params_map"],
                generation=generation,
            ),
        ))
        return generation

    def clear_channel_fx(self, node_name):
        if self._last_requested_fx.get(node_name) == {
                "capture_target": "",
                "effects": [],
                "params_map": {}}:
            return self._fx_generations.get(node_name, 0)
        generation = self._fx_generations.get(node_name, 0) + 1
        self._fx_generations[node_name] = generation
        self._last_requested_fx[node_name] = {
            "capture_target": "",
            "effects": [],
            "params_map": {},
        }
        self.enqueue_intent(ClearChannelFx(node_name=node_name, generation=generation))
        return generation

    def ensure_channel_fx(self, node_name, capture_target, effects, params_map):
        desired = {
            "capture_target": capture_target,
            "effects": list(effects or []),
            "params_map": {
                key: dict(vals) for key, vals in (params_map or {}).items()
            },
        }
        if self._last_requested_fx.get(node_name) == desired:
            return self._fx_generations.get(node_name, 0)
        return self.set_channel_fx(node_name, capture_target, effects, params_map)

    def recover_channel(self, node_name):
        self.enqueue_intent(RecoverChannel(node_name=node_name))

    def recover_channels(self, node_names):
        for node_name in node_names or ():
            if not node_name:
                continue
            self.enqueue_intent(RecoverChannel(node_name=str(node_name)))

    def set_submix_state(self, node_id, mix_name, volume, mute, node_name=""):
        self.enqueue_intent(SetSubmixState(
            node_id=str(node_id),
            mix_name=mix_name,
            volume=float(volume),
            mute=bool(mute),
            node_name=str(node_name or ""),
        ))

    def ensure_submix_route(self, node_id, node_name, media_class, mix_name, initial_state):
        self.enqueue_intent(EnsureSubmixRoute(
            node_id=str(node_id),
            node_name=node_name,
            media_class=media_class,
            mix_name=mix_name,
            initial_state=dict(initial_state or {}),
        ))

    def remove_node_routing(self, node_id):
        self.enqueue_intent(RemoveNodeRouting(node_id=str(node_id)))

    def set_mix_hardware_route(self, mix_name, sink_name):
        self.enqueue_intent(SetMixHardwareRoute(mix_name=mix_name, sink_name=sink_name))

    def set_mix_volume(self, mix_name, volume):
        self.enqueue_intent(SetMixVolume(mix_name=mix_name, volume=float(volume)))

    def ensure_output_mix_sync(self, mix_name):
        with self._worker.state_lock:
            self._worker.desired_state.mixes.setdefault(mix_name, MixSpec(name=mix_name))
            with self.adapter.session() as engine:
                out = engine.create_output_mix(mix_name)
        self.refresh_now(f"ensure-output-mix:{mix_name}")
        return out

    def ensure_virtual_channel_sync(self, display_name):
        with self._worker.state_lock:
            with self.adapter.session() as engine:
                out = engine.create_virtual_sink(display_name)
            if out:
                self._worker.desired_state.virtual_channels[out] = display_name
        self.refresh_now(f"ensure-virtual-channel:{display_name}")
        return out

    def remove_virtual_channel_sync(self, sink_name):
        with self._worker.state_lock:
            self._drop_channel_state(sink_name)
            self._worker.desired_state.virtual_channels.pop(sink_name, None)
            with self.adapter.session() as engine:
                out = engine.remove_virtual_sink(sink_name)
        self.refresh_now(f"remove-virtual-channel:{sink_name}")
        return out

    def rename_virtual_channel_sync(self, old_sink_name, new_display_name):
        with self._worker.state_lock:
            with self.adapter.session() as engine:
                out = engine.rename_virtual_sink(old_sink_name, new_display_name)
            if out:
                self._worker.desired_state.virtual_channels.pop(old_sink_name, None)
                self._worker.desired_state.virtual_channels[out] = new_display_name
                self._rename_channel_state(old_sink_name, out)
        self.refresh_now(f"rename-virtual-channel:{old_sink_name}")
        return out

    def full_audio_reset_sync(self):
        with self._worker.state_lock:
            with self.adapter.session() as engine:
                engine.full_audio_reset()
            self._reset_runtime_state()
        self.refresh_now("full-audio-reset")

    def cleanup_sync(self):
        with self._worker.state_lock:
            with self.adapter.session() as engine:
                engine.cleanup()

    def set_app_route(self, app_id, sink_name):
        self.enqueue_intent(SetAppRoute(app_id=app_id, sink_name=sink_name))

    def set_app_volume(self, sink_input_index, volume):
        self.enqueue_intent(SetAppVolume(
            sink_input_index=str(sink_input_index),
            volume=float(volume),
        ))

    def set_card_profile(self, card_name, profile_name):
        if not card_name or not profile_name:
            return
        self.enqueue_intent(SetCardProfile(
            card_name=str(card_name),
            profile_name=str(profile_name),
        ))

    def set_selected_mic(self, node_name):
        if node_name == self._desired_selected_mic:
            return
        self._desired_selected_mic = node_name
        self.enqueue_intent(SetSelectedMic(node_name=node_name))

    def sync_persistent_state(self, *, selected_mic, submix_state, active_effects,
                              effect_params, app_routing, virtual_channels,
                              monitor_hw, stream_hw):
        with self._worker.state_lock:
            desired = self._worker.desired_state
            desired.selected_mic = selected_mic
            desired.app_routes = {
                str(app_id): AppRouteSpec(app_id=str(app_id), sink_name=sink_name)
                for app_id, sink_name in (app_routing or {}).items()
            }
            desired.mixes.setdefault("Monitor", MixSpec(name="Monitor"))
            desired.mixes.setdefault("Stream", MixSpec(name="Stream"))
            desired.mixes["Monitor"].hardware_sink = monitor_hw
            desired.mixes["Stream"].hardware_sink = stream_hw
            desired.virtual_channels = dict(virtual_channels or {})
            channel_names = set(desired.channels.keys())
            channel_names.update(active_effects.keys())
            channel_names.update(effect_params.keys())
            if selected_mic:
                channel_names.add(selected_mic)
            for key in (submix_state or {}).keys():
                if not isinstance(key, str):
                    continue
                if key.endswith("_Monitor") or key.endswith("_Stream"):
                    channel_names.add(key.rsplit("_", 1)[0])
            for sink_name in desired.virtual_channels.keys():
                channel_names.add(sink_name)
            new_channels = {}
            for node_name in channel_names:
                previous = desired.channels.get(node_name)
                fx = previous.fx if previous is not None else FxSpec()
                wanted_effects = list((active_effects or {}).get(node_name, []))
                wanted_params = {
                    key: dict(vals)
                    for key, vals in ((effect_params or {}).get(node_name, {}) or {}).items()
                }
                if previous is not None and list(previous.fx.effects) == wanted_effects:
                    generation = previous.fx.generation
                else:
                    generation = getattr(fx, "generation", 0)
                capture_target = previous.capture_target if previous is not None else ""
                chan = desired.channels.get(node_name)
                if chan is None:
                    chan = ChannelSpec(node_name=node_name)
                chan.capture_target = capture_target
                chan.fx = FxSpec(
                    effects=wanted_effects,
                    params_map=wanted_params,
                    generation=generation,
                )
                chan.submix_state = {}
                for mix_name in ("Monitor", "Stream"):
                    raw = ((submix_state or {}).get(f"{node_name}_{mix_name}") or {})
                    if raw:
                        chan.submix_state[mix_name] = {
                            "vol": float(raw.get("vol", 1.0)),
                            "mute": bool(raw.get("mute", False)),
                        }
                new_channels[node_name] = chan
            desired.channels = new_channels
            self._desired_selected_mic = selected_mic

    def refresh_now(self, reason=""):
        self.enqueue_intent(RefreshNow(reason=reason))

    def export_diagnostics(self, reason="manual-export"):
        with self._worker.state_lock:
            desired = self._worker.desired_state
        return self.diagnostics.export_failure(
            reason,
            desired=desired,
            observed=self._latest_view_state,
            health=getattr(self._latest_view_state, "health", {}),
            status=self._latest_fx_status,
        )

    def shutdown(self):
        self._worker.stop()
        self._thread.quit()
        self._thread.wait(3000)

    def _on_view_state_ready(self, view_state):
        self._latest_view_state = view_state
        self.view_state_changed.emit(view_state)

    def _on_fx_status_ready(self, status):
        if status.node_name:
            self._latest_fx_status[status.node_name] = status
        self.fx_status_changed.emit(status)

    def _drop_channel_state(self, node_name):
        self._worker.desired_state.channels.pop(node_name, None)
        if self._worker.desired_state.selected_mic == node_name:
            self._worker.desired_state.selected_mic = None
            self._desired_selected_mic = None
        self._last_requested_fx.pop(node_name, None)
        self._fx_generations.pop(node_name, None)
        self._latest_fx_status.pop(node_name, None)
        self._worker._fx_statuses.pop(node_name, None)
        self._worker._pending_operations.pop(f"fx:{node_name}", None)
        self._worker._last_observed_channel_ids.pop(node_name, None)

    def _rename_channel_state(self, old_node_name, new_node_name):
        chan = self._worker.desired_state.channels.pop(old_node_name, None)
        if chan is not None:
            chan.node_name = new_node_name
            self._worker.desired_state.channels[new_node_name] = chan
        if self._worker.desired_state.selected_mic == old_node_name:
            self._worker.desired_state.selected_mic = new_node_name
        if self._desired_selected_mic == old_node_name:
            self._desired_selected_mic = new_node_name
        wanted = self._last_requested_fx.pop(old_node_name, None)
        if wanted is not None:
            self._last_requested_fx[new_node_name] = wanted
        generation = self._fx_generations.pop(old_node_name, None)
        if generation is not None:
            self._fx_generations[new_node_name] = generation
        status = self._latest_fx_status.pop(old_node_name, None)
        if status is not None:
            status.node_name = new_node_name
            self._latest_fx_status[new_node_name] = status
        worker_status = self._worker._fx_statuses.pop(old_node_name, None)
        if worker_status is not None:
            worker_status.node_name = new_node_name
            self._worker._fx_statuses[new_node_name] = worker_status
        pending = self._worker._pending_operations.pop(f"fx:{old_node_name}", None)
        if pending is not None:
            self._worker._pending_operations[f"fx:{new_node_name}"] = pending

    def _reset_runtime_state(self):
        schema_version = getattr(self._worker.desired_state, "schema_version", 1)
        self._worker.desired_state = DesiredState(schema_version=schema_version)
        self._worker._fx_statuses.clear()
        self._worker._pending_operations.clear()
        self._worker._last_observed_state = None
        self._worker._last_observed_channel_ids.clear()
        self._worker._refresh_pending = False
        self._latest_view_state = RuntimeViewState()
        self._latest_fx_status.clear()
        self._last_requested_fx.clear()
        self._fx_generations.clear()
        self._desired_selected_mic = None
