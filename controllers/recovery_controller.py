"""Runtime recovery, diagnostics, and channel-issue helpers."""

from __future__ import annotations

import os
import time

from PyQt6.QtCore import QUrl
from PyQt6.QtGui import QDesktopServices
from PyQt6.QtWidgets import QMessageBox

from app_core import FxStatusUpdated
from health import RecoveryStatus


class RecoveryController:
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

    def _sync_window_state(self):
        sync = getattr(type(self.window), "_sync_window_state", None)
        if sync is not None:
            sync(self.window)

    def on_runtime_fx_status(self, status):
        event_bus = self._attrs().get("_event_bus")
        if event_bus is not None:
            event_bus.publish(FxStatusUpdated(status=status))
        node_name = getattr(status, "node_name", "")
        state = getattr(status, "state", "")
        if state in {"building", "cutover_pending", "clearing"}:
            self.window._cancel_auto_recovery_timer(node_name)
        elif state in {"active", "idle"}:
            self.window._clear_auto_recovery_state(node_name)
            if node_name and not self._attrs().get("_shutting_down", False):
                if self.window._selected_mic_change_settling():
                    self.window._schedule_runtime_view_refresh()
                else:
                    self.window._request_runtime_refresh(f"fx-status:{state}:{node_name}")
        if state == "degraded":
            self._set_status_text(
                self.window.format_fx_status_message(status) or "FX runtime degraded"
            )
            self.window._schedule_auto_recovery(status)
        if self.window._settings_dialog_visible():
            self.window._mark_settings_tab_stale("Health")
            if self.window._active_settings_tab_name() == "Health":
                self.window._schedule_active_settings_tab_refresh(force=True)
        self.window._refresh_channel_runtime_status(node_name)

    def cancel_auto_recovery_timer(self, node_name):
        state = (self._attrs().get("_auto_recovery_state") or {}).get(node_name)
        if not state:
            return
        timer = state.get("timer")
        if timer is not None:
            try:
                timer.stop()
            except Exception:
                pass
        state["timer"] = None
        self._sync_window_state()

    def clear_auto_recovery_state(self, node_name):
        if not node_name:
            return
        attrs = self._attrs()
        attrs.setdefault("_recent_recovery_status", {})
        auto_recovery_state = attrs.setdefault("_auto_recovery_state", {})
        entry = auto_recovery_state.get(node_name)
        if entry and int(entry.get("attempts", 0)) > 0:
            runtime = attrs.get("runtime")
            status = runtime.fx_status_for(node_name) if runtime is not None else None
            attrs["_recent_recovery_status"][node_name] = {
                "at": time.time(),
                "status": RecoveryStatus(
                    node_name=node_name,
                    state="recovered",
                    retry_count=int(entry.get("attempts", 0)),
                    diagnostics_path=self.window._fx_status_diagnostics_path(status),
                ),
            }
        self.window._cancel_auto_recovery_timer(node_name)
        auto_recovery_state.pop(node_name, None)
        self.window._refresh_channel_runtime_status(node_name)
        self._sync_window_state()

    def ensure_auto_recovery_entry(self, node_name, generation):
        attrs = self._attrs()
        auto_recovery_state = attrs.setdefault("_auto_recovery_state", {})
        entry = auto_recovery_state.get(node_name)
        generation = int(generation or 0)
        attrs.setdefault("_recent_recovery_status", {})
        attrs["_recent_recovery_status"].pop(node_name, None)
        if entry is None or int(entry.get("generation", 0)) != generation:
            if entry is not None:
                self.window._cancel_auto_recovery_timer(node_name)
            entry = {
                "generation": generation,
                "attempts": 0,
                "timer": None,
                "last_delay_ms": 0,
                "exhausted": False,
            }
            auto_recovery_state[node_name] = entry
        self._sync_window_state()
        return entry

    def fx_recovery_status_message(self, node_name):
        state = (self._attrs().get("_auto_recovery_state") or {}).get(node_name) or {}
        timer = state.get("timer")
        attempts = int(state.get("attempts", 0))
        total = len(self.window._AUTO_RECOVERY_DELAYS_MS)
        if timer is not None and timer.isActive():
            delay_ms = int(state.get("last_delay_ms", 0))
            seconds = max(1, int(round(delay_ms / 1000.0)))
            return f"Automatic recovery scheduled in ~{seconds}s ({attempts}/{total})."
        if state.get("exhausted"):
            return "Automatic recovery attempts were exhausted. Use Recover channel to try again manually."
        if attempts:
            return f"Automatic recovery attempt {attempts}/{total} is in progress."
        return ""

    def recovery_status_for_channel(self, node_name):
        if not node_name:
            return RecoveryStatus(node_name="", state="idle", retry_count=0)
        attrs = self._attrs()
        runtime = attrs.get("runtime")
        fx_status = runtime.fx_status_for(node_name) if runtime is not None else None
        diagnostics_path = self.window._fx_status_diagnostics_path(fx_status)
        entry = (attrs.get("_auto_recovery_state") or {}).get(node_name) or {}
        timer = entry.get("timer")
        attempts = int(entry.get("attempts", 0))
        if entry.get("exhausted"):
            return RecoveryStatus(
                node_name=node_name,
                state="exhausted",
                retry_count=attempts,
                diagnostics_path=diagnostics_path,
            )
        if timer is not None and timer.isActive():
            remaining_ms = max(0, int(timer.remainingTime()))
            next_retry_at = time.time() + (remaining_ms / 1000.0)
            return RecoveryStatus(
                node_name=node_name,
                state="scheduled",
                retry_count=attempts,
                next_retry_at=next_retry_at,
                diagnostics_path=diagnostics_path,
            )
        if attempts:
            return RecoveryStatus(
                node_name=node_name,
                state="retrying",
                retry_count=attempts,
                diagnostics_path=diagnostics_path,
            )
        recent = (attrs.get("_recent_recovery_status") or {}).get(node_name) or {}
        recent_status = recent.get("status")
        recent_at = float(recent.get("at", 0) or 0)
        if recent_status is not None and (time.time() - recent_at) < 90:
            return recent_status
        if getattr(fx_status, "state", "") == "degraded":
            return RecoveryStatus(
                node_name=node_name,
                state="retrying",
                retry_count=0,
                diagnostics_path=diagnostics_path,
            )
        return RecoveryStatus(
            node_name=node_name,
            state="idle",
            retry_count=0,
            diagnostics_path=diagnostics_path,
        )

    def channel_runtime_issue(self, node_name):
        view = self._attrs().get("_runtime_view_state")
        health = getattr(view, "health", {}) if view is not None else {}
        health_code = ((health or {}).get(node_name) or "").strip()
        runtime = self._attrs().get("runtime")
        fx_status = runtime.fx_status_for(node_name) if runtime is not None else None
        fx_degraded = getattr(fx_status, "state", "") == "degraded"
        diagnostics_path = self.window._fx_status_diagnostics_path(fx_status)
        verification = None
        verification_reasons = []
        requested_effects = list((self._attrs().get("active_effects", {}) or {}).get(node_name, []) or [])
        settling = bool(self.window._selected_mic_change_settling())
        if requested_effects and not fx_degraded and not settling and getattr(fx_status, "state", "") not in {"building", "clearing"}:
            engine = self._attrs().get("engine")
            if engine is not None and hasattr(engine, "verify_channel_fx_runtime"):
                try:
                    verification = engine.verify_channel_fx_runtime(
                        node_name,
                        expected_default=(node_name == str(self._attrs().get("selected_mic") or "").strip()),
                        fx_status=fx_status,
                        requested_effects=requested_effects,
                    )
                except Exception:
                    verification = None
            if verification is not None and not verification.ready:
                verification_reasons = list(getattr(verification, "reasons", []) or [])
                if verification_reasons and not health_code:
                    health_code = str(getattr(verification_reasons[0], "code", "") or "").strip()
                fx_degraded = True
        lines = []
        summary = ""
        if health_code:
            summary = self.window._format_runtime_health_code(health_code).rstrip(".")
            lines.append(f"Health: {self.window._format_runtime_health_code(health_code)}")
        if fx_degraded:
            if not summary:
                summary = "FX runtime degraded"
            formatted = self.window.format_fx_status_message(fx_status)
            if formatted:
                lines.append(formatted)
        if verification_reasons:
            for reason in verification_reasons:
                detail = str(getattr(reason, "detail", "") or "").strip()
                if detail:
                    lines.append(detail)
        recovery_note = self.window.fx_recovery_status_message(node_name)
        if recovery_note:
            lines.append(recovery_note)
        if diagnostics_path:
            lines.append(f"Diagnostics: {diagnostics_path}")
        degraded = bool(health_code) or fx_degraded
        if degraded:
            lines.append("Right-click for Retry FX Now and Open Diagnostics.")
        return {
            "degraded": degraded,
            "health_code": health_code,
            "summary": summary or "Runtime issue detected",
            "tooltip": "\n".join(line for line in lines if line),
            "diagnostics_path": diagnostics_path,
            "status": fx_status,
        }

    def schedule_auto_recovery(self, status):
        node_name = getattr(status, "node_name", "")
        if not node_name:
            return
        entry = self.window._ensure_auto_recovery_entry(
            node_name,
            getattr(status, "generation", 0),
        )
        timer = entry.get("timer")
        if timer is not None and timer.isActive():
            return
        attempts = int(entry.get("attempts", 0))
        label = self.window._channel_label(node_name)
        if attempts >= len(self.window._AUTO_RECOVERY_DELAYS_MS):
            entry["exhausted"] = True
            self._set_status_text(
                f"{label} is still degraded. Automatic recovery attempts are exhausted."
            )
            self.window._refresh_channel_runtime_status(node_name)
            self._sync_window_state()
            return
        delay_ms = int(self.window._AUTO_RECOVERY_DELAYS_MS[attempts])
        timer = self.window._make_auto_recovery_timer()
        timer.timeout.connect(
            lambda node_name=node_name, generation=int(getattr(status, "generation", 0)): self.window._run_auto_recovery(node_name, generation)
        )
        entry["timer"] = timer
        entry["attempts"] = attempts + 1
        entry["last_delay_ms"] = delay_ms
        timer.start(delay_ms)
        self._set_status_text(
            f"{label} degraded; retrying automatically in ~{max(1, int(round(delay_ms / 1000.0)))}s "
            f"({entry['attempts']}/{len(self.window._AUTO_RECOVERY_DELAYS_MS)})"
        )
        self.window._refresh_channel_runtime_status(node_name)
        self._sync_window_state()

    def run_auto_recovery(self, node_name, generation):
        entry = (self._attrs().get("_auto_recovery_state") or {}).get(node_name)
        if not entry or int(entry.get("generation", 0)) != int(generation or 0):
            return
        entry["timer"] = None
        runtime = self._attrs().get("runtime")
        current = runtime.fx_status_for(node_name) if runtime is not None else None
        if getattr(current, "state", "") != "degraded":
            self._sync_window_state()
            return
        current_generation = int(getattr(current, "generation", 0) or 0)
        if current_generation and int(generation or 0) and current_generation != int(generation or 0):
            self._sync_window_state()
            return
        self._set_status_text(
            f"Attempting automatic recovery for {self.window._channel_label(node_name)}..."
        )
        self.window._refresh_channel_runtime_status(node_name)
        if runtime is not None:
            runtime.recover_channel(node_name)
        self._sync_window_state()

    def open_channel_diagnostics(self, node_name):
        if not node_name:
            return
        issue = self.window.channel_runtime_issue(node_name)
        path = str(issue.get("diagnostics_path") or "").strip()
        runtime = self._attrs().get("runtime")
        if not path and runtime is not None:
            path = runtime.export_diagnostics(f"channel-diagnostics:{node_name}")
        if not path:
            QMessageBox.information(
                self.window,
                "Diagnostics Unavailable",
                "WaveLinux could not locate or export diagnostics for that channel.",
            )
            return
        if os.path.exists(path) and QDesktopServices.openUrl(QUrl.fromLocalFile(path)):
            self._set_status_text(
                f"Opened diagnostics for {self.window._channel_label(node_name)}"
            )
            return
        QMessageBox.information(
            self.window,
            "Diagnostics Path",
            f"WaveLinux saved diagnostics for {self.window._channel_label(node_name)} here:\n{path}",
        )

    def recover_all_degraded_channels(self):
        degraded = self.window._runtime_degraded_channels()
        if not degraded:
            QMessageBox.information(self.window, "Recovery", "No degraded channels detected.")
            return
        yn = QMessageBox.question(
            self.window.settings_dialog,
            "Recover degraded channels",
            f"Attempt recovery for {len(degraded)} degraded channel(s)?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        for node_name in degraded:
            self.window._clear_auto_recovery_state(node_name)
        runtime = self._attrs().get("runtime")
        if runtime is not None:
            runtime.recover_channels(degraded)
        self._set_status_text(f"Recovering {len(degraded)} channel(s)...")
        self.window._refresh_advanced_tab()
        self._sync_window_state()

    def recover_channel(self, node_name):
        if not node_name:
            return
        self.window._clear_auto_recovery_state(node_name)
        runtime = self._attrs().get("runtime")
        if runtime is not None:
            runtime.recover_channel(node_name)
        self._sync_window_state()
