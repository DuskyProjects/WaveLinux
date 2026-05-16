"""Runtime-view refresh and startup-audio readiness helpers."""

from __future__ import annotations

import logging
import time

from PyQt6.QtWidgets import QApplication

from app_core import RuntimeViewUpdated


class RuntimeViewController:
    DEFAULT_STARTUP_READY_SETTLE_S = 0.5

    def __init__(self, window):
        self.window = window

    def _attrs(self):
        return self.window.__dict__

    def _status_label(self):
        return self._attrs().get("status_lbl")

    def _set_status_text(self, text):
        label = self._status_label()
        if label is not None and hasattr(label, "setText"):
            label.setText(str(text))

    def _status_text(self):
        label = self._status_label()
        if label is not None and hasattr(label, "text"):
            return str(label.text() or "")
        return ""

    def _sync_window_state(self):
        sync = getattr(type(self.window), "_sync_window_state", None)
        if sync is not None:
            sync(self.window)

    def on_runtime_view_state(self, view_state):
        started = time.monotonic()
        self.window._runtime_view_state = view_state
        checkpoint = time.monotonic()
        event_bus = self._attrs().get("_event_bus")
        if event_bus is not None:
            event_bus.publish(RuntimeViewUpdated(view_state=view_state))
        event_bus_elapsed_ms = (time.monotonic() - checkpoint) * 1000.0
        checkpoint = time.monotonic()
        manager = self._attrs().get("module_manager")
        if manager is not None:
            manager.on_runtime_view(view_state)
        module_elapsed_ms = (time.monotonic() - checkpoint) * 1000.0
        checkpoint = time.monotonic()
        health = getattr(view_state, "health", {}) or {}
        pending_ops = getattr(view_state, "pending_operations", {}) or {}
        if not self.window._selected_mic_transition_in_progress(health, pending_ops):
            self.window._reconcile_device_policy(view_state)
        reconcile_elapsed_ms = (time.monotonic() - checkpoint) * 1000.0
        checkpoint = time.monotonic()
        degraded = [name for name, state in health.items() if state]
        if degraded:
            self._set_status_text(f"Runtime degraded: {', '.join(degraded[:2])}")
        elif pending_ops:
            self._set_status_text(f"Applying audio changes ({len(pending_ops)})...")
        elif self._status_text().startswith("Runtime degraded:"):
            self._set_status_text("PipeWire connected")
        elif self._status_text().startswith("Applying audio changes"):
            self._set_status_text("PipeWire connected")
        if self.window._selected_mic_needs_followup_refresh(health, pending_ops):
            timer = self._attrs().get("_mic_cutover_refresh_timer")
            if timer is not None and not timer.isActive():
                timer.start()
        refresh_timer = self._attrs().get("_runtime_view_refresh_timer")
        if refresh_timer is not None:
            refresh_timer.setInterval(
                self.window._runtime_view_refresh_delay_ms(
                    health=health,
                    pending_ops=pending_ops,
                )
            )
        self.window._schedule_runtime_view_refresh()
        refresh_schedule_elapsed_ms = (time.monotonic() - checkpoint) * 1000.0
        checkpoint = time.monotonic()
        self._sync_window_state()
        sync_elapsed_ms = (time.monotonic() - checkpoint) * 1000.0
        total_elapsed_ms = (time.monotonic() - started) * 1000.0
        if total_elapsed_ms >= 80.0:
            logging.info(
                "Slow runtime view update: %.1f ms total (event_bus=%.1f, modules=%.1f, "
                "reconcile=%.1f, schedule=%.1f, sync=%.1f, pending_ops=%d, health=%d)",
                total_elapsed_ms,
                event_bus_elapsed_ms,
                module_elapsed_ms,
                reconcile_elapsed_ms,
                refresh_schedule_elapsed_ms,
                sync_elapsed_ms,
                len(pending_ops),
                len(health),
            )

    def request_runtime_refresh(self, reason=""):
        if bool(self._attrs().get("_shutting_down", False)):
            return
        runtime = self._attrs().get("runtime")
        if runtime is not None and hasattr(runtime, "refresh_now"):
            runtime.refresh_now(reason or "runtime-refresh")

    def schedule_runtime_view_refresh(self):
        timer = self._attrs().get("_runtime_view_refresh_timer")
        if timer is not None:
            timer.start()

    def runtime_view_refresh_delay_ms(self, *, health=None, pending_ops=None):
        delay_ms = 40
        if pending_ops:
            delay_ms = 180
        elif health:
            delay_ms = 90
        if self.window._settings_dialog_visible():
            delay_ms = max(delay_ms, 80)
        return delay_ms

    def runtime_view_has_pending_ops(self, view=None):
        view = view or self._attrs().get("_runtime_view_state")
        if view is None:
            return False
        return bool(getattr(view, "pending_operations", {}) or {})

    def selected_mic_change_settling(self, *, window_s=3.0):
        changed_at = float(self._attrs().get("_last_selected_mic_change_at", 0.0) or 0.0)
        if changed_at <= 0.0:
            return False
        return (time.monotonic() - changed_at) < max(0.0, float(window_s or 0.0))

    def selected_mic_needs_followup_refresh(self, health, pending_ops):
        if pending_ops or bool(self._attrs().get("_shutting_down", False)):
            return False
        selected_mic = str(self._attrs().get("selected_mic", "") or "").strip()
        if not selected_mic:
            return False
        health_code = str((health or {}).get(selected_mic) or "").strip()
        return health_code in {
            "default_source_mismatch",
            "default_source_expected_fx_missing",
            "desired_fx_missing",
            "fx_source_not_present",
        }

    def selected_mic_transition_in_progress(self, health, pending_ops):
        if bool(self._attrs().get("_shutting_down", False)):
            return False
        selected_mic = str(self._attrs().get("selected_mic", "") or "").strip()
        if not selected_mic or not self.window._selected_mic_change_settling():
            return False
        if pending_ops:
            return True
        health_code = str((health or {}).get(selected_mic) or "").strip()
        return health_code in {
            "default_source_mismatch",
            "default_source_expected_fx_missing",
            "desired_fx_missing",
            "fx_source_not_present",
        }

    def startup_graph_health_blockers(self, view=None):
        view = view or self._attrs().get("_runtime_view_state")
        if view is None:
            return {}
        health = getattr(view, "health", {}) or {}
        managed_names = set(self.window._virtual_channel_specs().keys())
        selected_mic = str(self._attrs().get("selected_mic", "") or "").strip()
        if selected_mic:
            managed_names.add(selected_mic)
        blockers = {}
        for node_name in managed_names:
            code = str(health.get(node_name) or "").strip()
            if code in {
                "submix_monitor_missing",
                "submix_stream_missing",
                "submix_monitor_dead",
                "submix_stream_dead",
                "default_source_mismatch",
                "default_source_expected_fx_missing",
                "desired_fx_missing",
                "fx_source_not_present",
            }:
                blockers[node_name] = code
        return blockers

    def startup_audio_ready(self, view=None):
        selected_mic = str(self._attrs().get("selected_mic", "") or "").strip()
        view = view or self._attrs().get("_runtime_view_state")
        runtime = self._attrs().get("runtime")
        if view is None and runtime is not None:
            view = getattr(runtime, "latest_view_state", None)
        if view is None:
            return False
        if self.window._startup_graph_health_blockers(view=view):
            return False
        if not selected_mic:
            return True
        mic_names = {
            str(getattr(mic_view, "name", "") or "").strip()
            for mic_view in (getattr(view, "mic_inputs", []) or [])
        }
        if mic_names and selected_mic not in mic_names:
            return False
        health = getattr(view, "health", {}) or {}
        health_code = str(health.get(selected_mic) or "").strip()
        if health_code in {
            "default_source_mismatch",
            "default_source_expected_fx_missing",
            "desired_fx_missing",
            "fx_source_not_present",
        }:
            return False
        default_source = str(getattr(view, "default_source", "") or "").strip()
        if not default_source:
            return False
        active_fx = list((self._attrs().get("active_effects", {}) or {}).get(selected_mic, []) or [])
        if not active_fx:
            return default_source == selected_mic
        engine = self._attrs().get("engine")
        expected_source = ""
        if engine is not None and hasattr(engine, "get_channel_fx_source"):
            expected_source = str(engine.get_channel_fx_source(selected_mic) or "").strip()
        if not expected_source:
            return False
        return default_source == expected_source

    def startup_audio_ready_settled(self, view=None, *, settle_s=None):
        if not self.startup_audio_ready(view=view):
            self._attrs().pop("_startup_audio_ready_since", None)
            return False
        settle_s = self.DEFAULT_STARTUP_READY_SETTLE_S if settle_s is None else float(settle_s or 0.0)
        settle_s = max(0.0, settle_s)
        if settle_s <= 0.0:
            return True
        now = time.monotonic()
        ready_since = float(self._attrs().get("_startup_audio_ready_since", 0.0) or 0.0)
        if ready_since <= 0.0:
            self.window._startup_audio_ready_since = now
            return False
        return (now - ready_since) >= settle_s

    def wait_for_startup_audio_ready(self, timeout_s=8.0):
        if bool(self._attrs().get("_shutting_down", False)):
            return True
        deadline = time.monotonic() + max(0.0, float(timeout_s or 0.0))
        next_refresh_at = 0.0
        while time.monotonic() < deadline:
            view = self._attrs().get("_runtime_view_state")
            runtime = self._attrs().get("runtime")
            if view is None and runtime is not None:
                view = getattr(runtime, "latest_view_state", None)
            if self.window._startup_audio_ready_settled(view=view):
                return True
            now = time.monotonic()
            if now >= next_refresh_at:
                self.window._request_runtime_refresh("startup-audio-ready")
                next_refresh_at = now + 0.2
            QApplication.processEvents()
            time.sleep(0.05)
        return self.window._startup_audio_ready_settled()

    def apply_scheduled_runtime_view_refresh(self):
        tray = self._attrs().get("tray")
        hidden_to_tray = tray is not None and not self.window.isVisible()
        settings_open = self.window._settings_dialog_visible()
        if settings_open:
            self.window._schedule_active_settings_tab_refresh()
        if hidden_to_tray and not settings_open:
            return
        if self.window._any_slider_dragging():
            return
        if self.window._runtime_view_has_pending_ops():
            timer = self._attrs().get("_runtime_view_refresh_timer")
            view = self._attrs().get("_runtime_view_state")
            if timer is not None and view is not None:
                timer.setInterval(
                    self.window._runtime_view_refresh_delay_ms(
                        health=getattr(view, "health", {}) or {},
                        pending_ops=getattr(view, "pending_operations", {}) or {},
                    )
                )
                timer.start()
            return
        if settings_open and self.window._selected_mic_change_settling():
            self.window._apply_lightweight_runtime_view_refresh()
            timer = self._attrs().get("_runtime_view_refresh_timer")
            if timer is not None:
                timer.setInterval(120)
                timer.start()
            return
        self.window._refresh_runtime_view()

    def apply_lightweight_runtime_view_refresh(self):
        view = self._attrs().get("_runtime_view_state")
        if view is None:
            self._set_status_text("PipeWire syncing...")
            return
        mics = list(getattr(view, "mic_inputs", []) or [])
        self.window._mixer_panel_controller().sync_mic_picker(
            mics,
            default_src=getattr(view, "default_source", None),
        )
        if not getattr(view, "health", {}):
            self._set_status_text(
                f"PipeWire connected · {getattr(view, 'node_count', 0)} nodes · "
                f"{getattr(view, 'app_count', 0)} apps"
            )
