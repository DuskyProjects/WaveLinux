"""Tray, notifications, autostart, and shutdown helpers."""

from __future__ import annotations

import logging
import os

from PyQt6.QtCore import QProcess, QTimer
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import QApplication, QDialog, QMenu, QSystemTrayIcon


class LifecycleController:
    def __init__(self, window, *, desktop_filename, desktop_exec_command_fn, friendly_name_fn):
        self.window = window
        self.desktop_filename = desktop_filename
        self.desktop_exec_command_fn = desktop_exec_command_fn
        self.friendly_name_fn = friendly_name_fn

    def _attrs(self):
        return self.window.__dict__

    @property
    def autostart_path(self):
        return os.path.expanduser(f"~/.config/autostart/{self.desktop_filename}")

    def is_autostart_enabled(self):
        return os.path.exists(self.autostart_path)

    def set_autostart(self, enable):
        path = self.autostart_path
        if not enable:
            try:
                os.remove(path)
            except FileNotFoundError:
                pass
            return
        os.makedirs(os.path.dirname(path), exist_ok=True)
        exec_cmd = self.desktop_exec_command_fn()
        contents = (
            "[Desktop Entry]\n"
            "Type=Application\n"
            "Name=WaveLinux\n"
            f"Exec={exec_cmd}\n"
            "Icon=wavelinux\n"
            "X-GNOME-Autostart-enabled=true\n"
            "NoDisplay=false\n"
        )
        with open(path, "w") as fh:
            fh.write(contents)

    def show_notification(self, title, body):
        tray = self._attrs().get("tray")
        if tray is not None and tray.isVisible():
            try:
                tray.showMessage(title, body, self._attrs().get("tray_icon_obj"), 3000)
            except Exception:
                pass
        logging.info("%s: %s", title, body)

    def notify_hotplug(self, node_names, *, added):
        view = self._attrs().get("_runtime_view_state")
        pretty_names = []
        for node_name in list(node_names)[:3]:
            source_label = self.window._display_name_for_source_name(node_name, view=view)
            if source_label and source_label != node_name:
                pretty_names.append(source_label)
                continue
            sink_label = self.window._display_name_for_sink_name(node_name, view=view)
            if sink_label and sink_label != node_name:
                pretty_names.append(sink_label)
                continue
            pretty_names.append(
                self.friendly_name_fn(
                    str(node_name or "").replace("wavelinux_", "").replace("_", " ")
                )
            )
        pretty = ", ".join(pretty_names)
        suffix = "" if len(node_names) <= 3 else f" (+{len(node_names) - 3} more)"
        if added:
            title, body = "Device connected", f"{pretty}{suffix}"
        else:
            title, body = "Device disconnected", f"{pretty}{suffix}"
        self.window._show_notification(title, body)

    def setup_tray(self):
        self.window.tray = None
        if not QSystemTrayIcon.isSystemTrayAvailable():
            logging.info("No system tray available; closing the window will quit.")
            return

        tray = QSystemTrayIcon(self.window)
        tray.setIcon(self._attrs().get("tray_icon_obj"))

        menu = QMenu()
        show_act = QAction("Show WaveLinux", self.window)
        show_act.triggered.connect(self.window.showNormal)
        menu.addAction(show_act)

        profiles_act = QAction("Sound Card Profiles…", self.window)
        profiles_act.triggered.connect(self.window._open_card_profiles)
        menu.addAction(profiles_act)

        autostart_act = QAction("Start at login", self.window)
        autostart_act.setCheckable(True)
        autostart_act.setChecked(self.window.is_autostart_enabled())
        autostart_act.toggled.connect(self.window.set_autostart)
        menu.addAction(autostart_act)

        menu.addSeparator()
        quit_act = QAction("Quit WaveLinux", self.window)
        quit_act.triggered.connect(self.window._request_quit_app)
        menu.addAction(quit_act)

        tray.setContextMenu(menu)
        tray.activated.connect(self.window._on_tray_activated)
        tray.show()

        self.window.tray = tray
        self.window.autostart_act = autostart_act

    def on_tray_activated(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            if self.window.isVisible():
                self.window.hide()
            else:
                self.window.showNormal()

    def on_hide_event(self):
        tray = self._attrs().get("tray")
        if tray is not None and not self.window.isVisible():
            self.window._stop_all_meters()

    def on_show_event(self):
        QTimer.singleShot(0, self.window._refresh)

    def request_quit_app(self):
        if self._attrs().get("_quit_in_progress", False):
            return
        self.window._quit_in_progress = True
        self.window._shutting_down = True
        self.window._suppress_pactl_events_for(3.0)
        status_lbl = self._attrs().get("status_lbl")
        if status_lbl is not None and hasattr(status_lbl, "setText"):
            status_lbl.setText("Shutting down WaveLinux...")
        self.window._close_open_dialogs_for_quit()
        tray = self._attrs().get("tray")
        if tray is not None:
            tray.hide()
        self.window.setEnabled(False)
        QTimer.singleShot(0, self.window._quit_app)

    def close_event(self, event):
        if self._attrs().get("_quit_in_progress", False):
            event.accept()
            return
        tray = self._attrs().get("tray")
        if tray is not None and tray.isVisible():
            event.ignore()
            self.window.hide()
            return
        self.window._request_quit_app()
        event.accept()

    def close_open_dialogs_for_quit(self):
        app = QApplication.instance()
        if app is None:
            return
        for widget in list(app.topLevelWidgets()):
            if widget is None or widget is self.window:
                continue
            if isinstance(widget, QDialog):
                try:
                    widget.hide()
                    widget.done(QDialog.DialogCode.Rejected)
                    widget.close()
                except Exception:
                    pass

    def _stop_timer(self, name):
        timer = self._attrs().get(name)
        if timer is not None and hasattr(timer, "stop"):
            timer.stop()

    def quit_app(self):
        app = QApplication.instance()
        if self._attrs().get("_runtime_stopped", False):
            if app is not None:
                app.quit()
            return
        logging.info("Shutting down WaveLinux...")
        self.window._shutting_down = True
        manager = self._attrs().get("module_manager")
        if manager is not None:
            manager.stop_all("app-quit")
        for timer_name in (
            "refresh_timer",
            "_save_timer",
            "_event_refresh_timer",
            "_event_proc_restart_timer",
            "_device_settle_refresh_timer",
            "_bluetooth_refresh_timer",
            "_runtime_view_refresh_timer",
            "_mic_cutover_refresh_timer",
        ):
            self._stop_timer(timer_name)
        self.window._stop_all_meters()
        self.window.save_config()
        proc = self._attrs().get("_event_proc")
        if proc is not None and proc.state() != QProcess.ProcessState.NotRunning:
            proc.terminate()
            if not proc.waitForFinished(300):
                proc.kill()
                proc.waitForFinished(500)
        self.window._clear_runtime_pid()
        runtime = self._attrs().get("runtime")
        if runtime is not None:
            runtime.full_audio_reset_sync(refresh=False)
            runtime.shutdown()
        self.window._runtime_stopped = True
        logging.info("Audio reset complete. Exiting.")
        if app is not None:
            app.quit()

    def cleanup_before_exit(self):
        self.window._stop_stress_control()
        self.window._clear_runtime_pid()
        if not self._attrs().get("_runtime_stopped", False):
            runtime = self._attrs().get("runtime")
            if runtime is not None:
                runtime.cleanup_sync()
