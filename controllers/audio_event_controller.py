"""pactl event subscription and audio-server recovery helpers."""

from __future__ import annotations

import logging

from PyQt6.QtCore import QProcess


class AudioEventController:
    def __init__(self, window):
        self.window = window

    def _attrs(self):
        return self.window.__dict__

    def start_event_subscriber(self):
        """Run `pactl subscribe` under a QProcess for low-latency refreshes."""
        event_proc = QProcess(self.window)
        event_proc.setProcessChannelMode(QProcess.ProcessChannelMode.MergedChannels)
        event_proc.readyReadStandardOutput.connect(self.window._on_pactl_event)
        event_proc.errorOccurred.connect(self.window._on_event_proc_error)
        event_proc.finished.connect(self.window._on_event_proc_finished)
        self.window._event_proc = event_proc
        try:
            event_proc.start("pactl", ["subscribe"])
        except Exception as exc:
            logging.warning("pactl subscribe unavailable: %s — falling back to poll", exc)

    def on_pactl_event(self):
        """Debounce relevant external graph changes and ignore noisy churn."""
        event_proc = self._attrs().get("_event_proc")
        try:
            payload = bytes(event_proc.readAllStandardOutput()).decode("utf-8", "replace")
        except Exception:
            payload = ""
        if self.window._pactl_events_suppressed():
            return
        if payload and not self.window._should_refresh_for_pactl_event(payload):
            return
        event_timer = self._attrs().get("_event_refresh_timer")
        if event_timer is not None:
            event_timer.start()
        if payload and self.window._should_schedule_settle_refresh_for_pactl_event(payload):
            settle_timer = (
                self._attrs().get("_device_settle_refresh_timer")
                or self._attrs().get("_hotplug_refresh_timer")
            )
            if settle_timer is not None:
                settle_timer.start()
        if payload and self.window._should_schedule_bluetooth_settle_refresh_for_pactl_event(payload):
            bluetooth_timer = self._attrs().get("_bluetooth_refresh_timer")
            if bluetooth_timer is not None:
                bluetooth_timer.start()

    def on_event_proc_error(self, err):
        if self._attrs().get("_shutting_down", False):
            return
        logging.warning("pactl subscribe error: %s", err)
        self.window._schedule_audio_server_recovery()

    def on_event_proc_finished(self, exit_code, exit_status):
        if self._attrs().get("_shutting_down", False):
            return
        logging.warning(
            "pactl subscribe exited (code=%s, status=%s)",
            exit_code,
            exit_status,
        )
        self.window._schedule_audio_server_recovery()

    def schedule_audio_server_recovery(self):
        if self._attrs().get("_shutting_down", False):
            return
        self.window._bluetooth_profile_reassert_retries = max(
            int(self._attrs().get("_bluetooth_profile_reassert_retries", 0) or 0),
            6,
        )
        reconnect_scheduled = False
        if not self.window._selected_mic_uses_bluetooth_input():
            reconnect_scheduled = self.window._schedule_known_bluetooth_monitor_reconnect(
                disconnect_first=False,
                settle_delay_ms=250,
            )
        restart_timer = self._attrs().get("_event_proc_restart_timer")
        if restart_timer is not None:
            restart_timer.start()
        event_timer = self._attrs().get("_event_refresh_timer")
        if event_timer is not None:
            event_timer.start()
        bluetooth_timer = self._attrs().get("_bluetooth_refresh_timer")
        if bluetooth_timer is not None:
            if reconnect_scheduled:
                bluetooth_timer.start(600)
            else:
                bluetooth_timer.start()

    def restart_event_subscriber_if_needed(self):
        if self._attrs().get("_shutting_down", False):
            return
        proc = self._attrs().get("_event_proc")
        if proc is not None and proc.state() != QProcess.ProcessState.NotRunning:
            return
        if proc is not None:
            try:
                proc.deleteLater()
            except Exception:
                pass
            self.window._event_proc = None
        self.window._start_event_subscriber()
