"""Startup preflight and stale-runtime recovery helpers."""

from __future__ import annotations

import logging
import os

from PyQt6.QtWidgets import QMessageBox


class StartupController:
    def __init__(self, window, *, startup_preflight_reporter):
        self.window = window
        self.startup_preflight_reporter = startup_preflight_reporter

    def _attrs(self):
        return self.window.__dict__

    def run_startup_preflight(self):
        """Check for required runtime binaries and surface a clear warning."""
        report = self.startup_preflight_reporter()
        self.window._startup_preflight = report
        issues = list(report.get("issues", ()))
        if issues:
            msg = (
                "WaveLinux detected one or more runtime issues:\n"
                + "\n".join(f"  - {issue}" for issue in issues)
                + "\n\nWaveLinux can still start, but routing, meters, or updates may fail.\n"
                "Install PipeWire + WirePlumber + PulseAudio compatibility tools on the host OS.\n"
                "If you're using the AppImage, these tools still need to exist outside the AppImage."
            )
            logging.warning(msg.replace("\n", " "))
            QMessageBox.warning(self.window, "WaveLinux dependency check", msg)
        self.window._recover_unclean_runtime_state()
        self.window._write_runtime_pid()

    @staticmethod
    def pid_is_alive(pid):
        try:
            os.kill(int(pid), 0)
        except (OSError, ValueError, TypeError):
            return False
        return True

    def recover_unclean_runtime_state(self):
        pid_path = str(self._attrs().get("_runtime_pid_path", "") or "").strip()
        if not pid_path or not os.path.exists(pid_path):
            return
        try:
            with open(pid_path, "r") as fh:
                previous_pid = fh.read().strip()
        except OSError:
            previous_pid = ""
        if not previous_pid or previous_pid == str(os.getpid()):
            return
        if self.window._pid_is_alive(previous_pid):
            return
        logging.warning(
            "Detected stale WaveLinux runtime from pid %s; resetting audio graph",
            previous_pid,
        )
        try:
            runtime = self._attrs().get("runtime")
            if runtime is not None:
                runtime.full_audio_reset_sync()
        except Exception as exc:
            logging.error("Startup stale-state reset failed: %s", exc)

    def write_runtime_pid(self):
        pid_path = str(self._attrs().get("_runtime_pid_path", "") or "").strip()
        if not pid_path:
            return
        try:
            with open(pid_path, "w") as fh:
                fh.write(str(os.getpid()))
        except OSError as exc:
            logging.warning("Could not write runtime pid file %s: %s", pid_path, exc)

    def clear_runtime_pid(self):
        pid_path = str(self._attrs().get("_runtime_pid_path", "") or "").strip()
        if not pid_path:
            return
        try:
            if os.path.exists(pid_path):
                os.remove(pid_path)
        except OSError as exc:
            logging.warning("Could not clear runtime pid file %s: %s", pid_path, exc)
