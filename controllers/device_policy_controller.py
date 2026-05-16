"""Monitor/microphone preference, fallback, and restore helpers."""

from __future__ import annotations

import time

from PyQt6.QtWidgets import QMessageBox

from health import HealthIssue


class DevicePolicyController:
    def __init__(self, window):
        self.window = window

    def _attrs(self):
        return self.window.__dict__

    def _sync_window_state(self):
        sync = getattr(type(self.window), "_sync_window_state", None)
        if sync is not None:
            sync(self.window)

    def _set_status_text(self, text):
        label = self._attrs().get("status_lbl")
        if label is not None and hasattr(label, "setText"):
            label.setText(str(text))

    def resolve_startup_monitor_target(self, view=None):
        default_sink = self.window._visible_default_sink(view=view)
        if default_sink:
            return default_sink
        rows = self.window._hardware_sink_rows(view=view)
        if rows:
            return str(getattr(rows[0], "name", "") or "") or None
        engine = self._attrs().get("engine")
        if engine is not None and hasattr(engine, "stable_sink_inventory"):
            inventory = list(engine.stable_sink_inventory() or [])
            if inventory:
                return str(inventory[0].get("name") or "") or None
        return None

    def resolve_startup_mic_target(self, view=None):
        default_source = self.window._visible_default_source(view=view)
        if default_source:
            return default_source
        rows = self.window._hardware_source_rows(view=view)
        if rows:
            return str(getattr(rows[0], "name", "") or "") or None
        engine = self._attrs().get("engine")
        if engine is not None and hasattr(engine, "stable_source_inventory"):
            inventory = list(engine.stable_source_inventory() or [])
            if inventory:
                return str(inventory[0].get("name") or "") or None
        return None

    def record_preferred_monitor(self, sink_name, *, view=None):
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            self.window._preferred_monitor_hw_id = ""
            self.window._preferred_monitor_hw_name = ""
            self.window._active_monitor_fallback = False
            self.window._restorable_monitor_hw_id = ""
            self.window._restorable_monitor_hw_name = ""
            self._sync_window_state()
            return
        resolved = self.window._resolve_hardware_sink_name(sink_name) or sink_name
        row = self.window._sink_row_for_name(resolved, view=view)
        stable_id = self.window._sink_stable_id_from_row(row)
        if not stable_id:
            stable_id = self.window._stable_sink_id_for_name(resolved)
        self.window._preferred_monitor_hw_name = resolved
        self.window._preferred_monitor_hw_id = stable_id
        self.window._last_good_monitor_hw_name = resolved
        self.window._last_good_monitor_hw_id = stable_id
        self.window._active_monitor_fallback = False
        self.window._restorable_monitor_hw_id = ""
        self.window._restorable_monitor_hw_name = ""
        self._sync_window_state()

    def record_preferred_mic(self, source_name, *, view=None):
        source_name = str(source_name or "").strip()
        if not source_name:
            self.window._preferred_selected_mic_id = ""
            self.window._preferred_selected_mic_name = ""
            self.window._active_mic_fallback = False
            self.window._restorable_selected_mic_id = ""
            self.window._restorable_selected_mic_name = ""
            self._sync_window_state()
            return
        resolved = self.window._resolve_hardware_source_name(source_name) or source_name
        row = self.window._source_row_for_name(resolved, view=view)
        stable_id = self.window._source_stable_id_from_row(row)
        if not stable_id:
            stable_id = self.window._stable_source_id_for_name(resolved)
        self.window._preferred_selected_mic_name = resolved
        self.window._preferred_selected_mic_id = stable_id
        self.window._last_good_selected_mic_name = resolved
        self.window._last_good_selected_mic_id = stable_id
        self.window._active_mic_fallback = False
        self.window._restorable_selected_mic_id = ""
        self.window._restorable_selected_mic_name = ""
        self._sync_window_state()

    def set_selected_mic_target(
        self,
        mic_name,
        *,
        record_preference=False,
        persist=True,
        request_refresh=True,
        view=None,
    ):
        mic_name = str(mic_name or "").strip() or None
        if mic_name != self._attrs().get("selected_mic"):
            self.window._last_selected_mic_change_at = time.monotonic()
        self.window.selected_mic = mic_name
        self.window._mic_selection_initialized = bool(mic_name)
        runtime = self._attrs().get("runtime")
        if runtime is not None:
            self.window._suppress_pactl_events_for(1.5)
            runtime.set_selected_mic(mic_name)
        if record_preference and mic_name:
            self.window._record_preferred_mic(mic_name, view=view)
        if persist:
            self.window.schedule_save()
        if request_refresh:
            self.window._request_runtime_refresh("selected-mic-change")
        self._sync_window_state()

    def restore_preferred_monitor(self):
        target = str(
            self._attrs().get("_preferred_monitor_hw_name", "")
            or self._attrs().get("_restorable_monitor_hw_name", "")
            or ""
        ).strip()
        if not target:
            return
        resolved = self.window._resolve_hardware_sink_name(
            self._attrs().get("_preferred_monitor_hw_id", "") or target
        ) or self.window._resolve_hardware_sink_name(target)
        if not resolved:
            QMessageBox.information(
                self.window.settings_dialog,
                "Monitor device unavailable",
                "WaveLinux could not find the preferred monitor device right now.",
            )
            return
        self.window._set_mix_output_target(
            "Monitor",
            resolved,
            persist=True,
            update_combo=True,
            sync_runtime=True,
        )
        self.window._record_preferred_monitor(
            resolved,
            view=self._attrs().get("_runtime_view_state"),
        )
        self._set_status_text(
            f"Restored monitor device: {self.window._display_name_for_sink_name(resolved)}"
        )
        self.window._refresh_system_tab(preflight=self._attrs().get("_startup_preflight"))
        self._sync_window_state()

    def restore_preferred_mic(self):
        target = str(
            self._attrs().get("_preferred_selected_mic_name", "")
            or self._attrs().get("_restorable_selected_mic_name", "")
            or ""
        ).strip()
        if not target:
            return
        resolved = self.window._resolve_hardware_source_name(
            self._attrs().get("_preferred_selected_mic_id", "") or target
        ) or self.window._resolve_hardware_source_name(target)
        if not resolved:
            QMessageBox.information(
                self.window.settings_dialog,
                "Microphone unavailable",
                "WaveLinux could not find the preferred microphone right now.",
            )
            return
        self.window._set_selected_mic_target(
            resolved,
            record_preference=True,
            persist=True,
            request_refresh=True,
            view=self._attrs().get("_runtime_view_state"),
        )
        self._set_status_text(
            f"Restored microphone device: {self.window._display_name_for_source_name(resolved)}"
        )
        self.window._refresh_system_tab(preflight=self._attrs().get("_startup_preflight"))
        self._sync_window_state()

    def reconcile_device_policy(self, view=None):
        if not self.window._module_enabled("device_policy"):
            return False
        view = view or self._attrs().get("_runtime_view_state")
        if view is None:
            return False
        changed = False
        preferred_monitor_hw_name = self._attrs().get("_preferred_monitor_hw_name", "")
        preferred_monitor_hw_id = self._attrs().get("_preferred_monitor_hw_id", "")
        active_monitor_fallback = bool(self._attrs().get("_active_monitor_fallback", False))
        preferred_selected_mic_name = self._attrs().get("_preferred_selected_mic_name", "")
        preferred_selected_mic_id = self._attrs().get("_preferred_selected_mic_id", "")
        active_mic_fallback = bool(self._attrs().get("_active_mic_fallback", False))

        desired_mix_hw = self._attrs().get("_desired_mix_hw", {}) or {}
        active_monitor = desired_mix_hw.get("Monitor") or getattr(view.mixes.get("Monitor"), "hardware_sink", None)
        resolved_monitor = self.window._resolve_hardware_sink_name(active_monitor) if active_monitor else None
        active_monitor_row = self.window._sink_row_for_name(resolved_monitor or active_monitor, view=view)
        if active_monitor_row is not None:
            active_monitor_name = str(getattr(active_monitor_row, "name", "") or "")
            active_monitor_id = self.window._sink_stable_id_from_row(active_monitor_row)
            self.window._last_good_monitor_hw_name = active_monitor_name
            self.window._last_good_monitor_hw_id = active_monitor_id
            if not preferred_monitor_hw_name:
                self.window._preferred_monitor_hw_name = active_monitor_name
                self.window._preferred_monitor_hw_id = active_monitor_id
                preferred_monitor_hw_name = active_monitor_name
                preferred_monitor_hw_id = active_monitor_id
            preferred_monitor_row = self.window._sink_row_for_stable_id(preferred_monitor_hw_id, view=view)
            if preferred_monitor_row is not None and active_monitor_name == getattr(preferred_monitor_row, "name", None):
                self.window._active_monitor_fallback = False
                self.window._restorable_monitor_hw_id = ""
                self.window._restorable_monitor_hw_name = ""
            elif active_monitor_fallback and preferred_monitor_row is not None:
                self.window._restorable_monitor_hw_id = preferred_monitor_hw_id
                self.window._restorable_monitor_hw_name = (
                    getattr(preferred_monitor_row, "name", "") or preferred_monitor_hw_name
                )
        elif active_monitor:
            fallback_monitor = self.window._resolve_monitor_fallback(view=view)
            if fallback_monitor and fallback_monitor != active_monitor:
                if not active_monitor_fallback:
                    if not preferred_monitor_hw_name:
                        self.window._preferred_monitor_hw_name = str(active_monitor)
                        preferred_monitor_hw_name = str(active_monitor)
                    if not preferred_monitor_hw_id:
                        self.window._preferred_monitor_hw_id = self.window._stable_sink_id_for_name(
                            self.window._preferred_monitor_hw_name
                        )
                        preferred_monitor_hw_id = self.window._preferred_monitor_hw_id
                    self.window._restorable_monitor_hw_name = preferred_monitor_hw_name
                    self.window._restorable_monitor_hw_id = preferred_monitor_hw_id
                self.window._active_monitor_fallback = True
                self.window._set_mix_output_target(
                    "Monitor",
                    fallback_monitor,
                    persist=True,
                    update_combo=True,
                    sync_runtime=True,
                )
                changed = True

        active_mic = str(self._attrs().get("selected_mic") or "").strip()
        active_mic_row = self.window._source_row_for_name(active_mic, view=view)
        if active_mic_row is not None:
            active_mic_name = str(getattr(active_mic_row, "name", "") or "")
            active_mic_id = self.window._source_stable_id_from_row(active_mic_row)
            self.window._last_good_selected_mic_name = active_mic_name
            self.window._last_good_selected_mic_id = active_mic_id
            if not preferred_selected_mic_name:
                self.window._preferred_selected_mic_name = active_mic_name
                self.window._preferred_selected_mic_id = active_mic_id
                preferred_selected_mic_name = active_mic_name
                preferred_selected_mic_id = active_mic_id
            preferred_mic_row = self.window._source_row_for_stable_id(preferred_selected_mic_id, view=view)
            if preferred_mic_row is not None and active_mic_name == getattr(preferred_mic_row, "name", None):
                self.window._active_mic_fallback = False
                self.window._restorable_selected_mic_id = ""
                self.window._restorable_selected_mic_name = ""
            elif active_mic_fallback and preferred_mic_row is not None:
                self.window._restorable_selected_mic_id = preferred_selected_mic_id
                self.window._restorable_selected_mic_name = (
                    getattr(preferred_mic_row, "name", "") or preferred_selected_mic_name
                )
        else:
            fallback_mic = self.window._resolve_mic_fallback(view=view)
            if fallback_mic and fallback_mic != active_mic:
                if not active_mic_fallback:
                    if not preferred_selected_mic_name:
                        self.window._preferred_selected_mic_name = active_mic
                        preferred_selected_mic_name = active_mic
                    if not preferred_selected_mic_id:
                        self.window._preferred_selected_mic_id = self.window._stable_source_id_for_name(
                            self.window._preferred_selected_mic_name
                        )
                        preferred_selected_mic_id = self.window._preferred_selected_mic_id
                    self.window._restorable_selected_mic_name = preferred_selected_mic_name
                    self.window._restorable_selected_mic_id = preferred_selected_mic_id
                self.window._active_mic_fallback = True
                self.window._set_selected_mic_target(
                    fallback_mic,
                    record_preference=False,
                    persist=True,
                    request_refresh=True,
                    view=view,
                )
                changed = True

        self._sync_window_state()
        return changed

    def device_health_issues(self, view=None):
        view = view or self._attrs().get("_runtime_view_state")
        issues = []
        desired_mix_hw = self._attrs().get("_desired_mix_hw", {}) or {}
        active_monitor = str(desired_mix_hw.get("Monitor") or "").strip()
        active_mic = str(self._attrs().get("selected_mic") or "").strip()
        if self._attrs().get("_active_monitor_fallback", False):
            issues.append(
                HealthIssue(
                    code="device.monitor_fallback_active",
                    severity="warning",
                    title="Monitor output is running on a fallback device",
                    detail=(
                        f"WaveLinux is routing Monitor to {self.window._display_name_for_sink_name(active_monitor, view=view) or active_monitor} "
                        f"because the preferred device {self.window._display_name_for_sink_name(self._attrs().get('_preferred_monitor_hw_name', ''), view=view) or self._attrs().get('_preferred_monitor_hw_name', '') or 'is unavailable'}."
                    ),
                    primary_action=(
                        "Restore monitor device"
                        if self._attrs().get("_restorable_monitor_hw_name", "")
                        else "Re-run device reconcile"
                    ),
                    secondary_action="Re-run device reconcile"
                    if self._attrs().get("_restorable_monitor_hw_name", "")
                    else "",
                    context={
                        "active_sink": active_monitor,
                        "preferred_sink": self._attrs().get("_preferred_monitor_hw_name", ""),
                        "restorable_sink": self._attrs().get("_restorable_monitor_hw_name", ""),
                    },
                )
            )
        if self._attrs().get("_active_monitor_fallback", False) and self._attrs().get("_restorable_monitor_hw_name", ""):
            issues.append(
                HealthIssue(
                    code="device.monitor_preferred_restorable",
                    severity="info",
                    title="Preferred monitor device is available again",
                    detail=(
                        f"{self.window._display_name_for_sink_name(self._attrs().get('_restorable_monitor_hw_name', ''), view=view) or self._attrs().get('_restorable_monitor_hw_name', '')} "
                        "has returned. WaveLinux will not switch automatically."
                    ),
                    primary_action="Restore monitor device",
                    secondary_action="Re-run device reconcile",
                    context={"sink_name": self._attrs().get("_restorable_monitor_hw_name", "")},
                )
            )
        if self._attrs().get("_active_mic_fallback", False):
            issues.append(
                HealthIssue(
                    code="device.mic_fallback_active",
                    severity="warning",
                    title="Microphone is running on a fallback device",
                    detail=(
                        f"WaveLinux is using {self.window._display_name_for_source_name(active_mic, view=view) or active_mic} "
                        f"because the preferred microphone {self.window._display_name_for_source_name(self._attrs().get('_preferred_selected_mic_name', ''), view=view) or self._attrs().get('_preferred_selected_mic_name', '') or 'is unavailable'}."
                    ),
                    primary_action=(
                        "Restore microphone device"
                        if self._attrs().get("_restorable_selected_mic_name", "")
                        else "Re-run device reconcile"
                    ),
                    secondary_action="Re-run device reconcile"
                    if self._attrs().get("_restorable_selected_mic_name", "")
                    else "",
                    context={
                        "active_source": active_mic,
                        "preferred_source": self._attrs().get("_preferred_selected_mic_name", ""),
                        "restorable_source": self._attrs().get("_restorable_selected_mic_name", ""),
                    },
                )
            )
        if self._attrs().get("_active_mic_fallback", False) and self._attrs().get("_restorable_selected_mic_name", ""):
            issues.append(
                HealthIssue(
                    code="device.mic_preferred_restorable",
                    severity="info",
                    title="Preferred microphone is available again",
                    detail=(
                        f"{self.window._display_name_for_source_name(self._attrs().get('_restorable_selected_mic_name', ''), view=view) or self._attrs().get('_restorable_selected_mic_name', '')} "
                        "has returned. WaveLinux will not switch automatically."
                    ),
                    primary_action="Restore microphone device",
                    secondary_action="Re-run device reconcile",
                    context={"source_name": self._attrs().get("_restorable_selected_mic_name", "")},
                )
            )
        stream_target = str(desired_mix_hw.get("Stream") or "").strip()
        if stream_target and not self.window._resolve_hardware_sink_name(stream_target):
            issues.append(
                HealthIssue(
                    code="device.stream_target_missing",
                    severity="warning",
                    title="Stream output target is unavailable",
                    detail=(
                        f"WaveLinux cannot currently resolve the explicit Stream target "
                        f"{self.window._display_name_for_sink_name(stream_target, view=view) or stream_target}. "
                        "Stream does not auto-follow Monitor."
                    ),
                    primary_action="Re-run device reconcile",
                    secondary_action="Open diagnostics",
                    context={"sink_name": stream_target},
                )
            )
        return issues

    def schedule_monitor_route_followups(self, hw_sink_name):
        """Schedule settle/reassert passes after a manual Monitor output switch."""
        settle_timer = (
            self._attrs().get("_device_settle_refresh_timer")
            or self._attrs().get("_hotplug_refresh_timer")
        )
        if settle_timer is not None and hasattr(settle_timer, "start"):
            settle_timer.start()
        reassert_timer = self._attrs().get("_monitor_route_reassert_timer")
        if reassert_timer is not None and hasattr(reassert_timer, "start"):
            reassert_timer.start()
        target = str(hw_sink_name or "").strip().lower()
        stable_id = ""
        if target and not target.startswith("bt:"):
            try:
                stable_id = str(self.window._stable_sink_id_for_name(hw_sink_name) or "").strip().lower()
            except Exception:
                stable_id = ""
        if "bluez_output." not in target and not target.startswith("bt:") and not stable_id.startswith("bt:"):
            return
        bluetooth_timer = self._attrs().get("_bluetooth_refresh_timer")
        if bluetooth_timer is not None and hasattr(bluetooth_timer, "start"):
            bluetooth_timer.start()
        bluetooth_reassert_timer = self._attrs().get("_monitor_route_bluetooth_reassert_timer")
        if bluetooth_reassert_timer is not None and hasattr(bluetooth_reassert_timer, "start"):
            bluetooth_reassert_timer.start()

    def reassert_persistent_state_after_monitor_switch(self, reason):
        if bool(self._attrs().get("_shutting_down", False)):
            return
        self.window._request_runtime_refresh(reason)
