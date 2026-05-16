"""Advanced settings actions and refresh helpers."""

from __future__ import annotations

from PyQt6.QtWidgets import QMessageBox


class AdvancedTabController:
    def __init__(self, window):
        self.window = window

    def refresh_advanced_tab(self):
        if hasattr(self.window, "prune_spin"):
            self.window.prune_spin.blockSignals(True)
            self.window.prune_spin.setValue(self.window.app_prune_days)
            self.window.prune_spin.blockSignals(False)
        if hasattr(self.window, "autostart_check"):
            self.window.autostart_check.blockSignals(True)
            self.window.autostart_check.setChecked(self.window.is_autostart_enabled())
            self.window.autostart_check.blockSignals(False)
        if hasattr(self.window, "restore_forgotten_btn"):
            count = len(self.window.forgotten_apps)
            self.window.restore_forgotten_btn.setEnabled(count > 0)
            self.window.restore_forgotten_btn.setText(
                f"Restore forgotten apps ({count})" if count else "Restore forgotten apps"
            )
        if hasattr(self.window, "recover_degraded_btn"):
            count = len(self.window._runtime_degraded_channels())
            self.window.recover_degraded_btn.setEnabled(count > 0)
            self.window.recover_degraded_btn.setText(
                f"Recover degraded channels ({count})" if count else "Recover degraded channels"
            )
        self.window._mark_settings_tab_refreshed("Advanced")

    def restore_forgotten_apps(self):
        if not self.window.forgotten_apps:
            return
        count = len(self.window.forgotten_apps)
        yn = QMessageBox.question(
            self.window.settings_dialog,
            "Restore forgotten apps",
            f"Clear the blocklist of {count} forgotten app(s)? They will reappear in the routing tab the next time they make sound.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        self.window.forgotten_apps.clear()
        self.window.save_config()
        self.refresh_advanced_tab()
        self.window._refresh()

    def on_prune_days_change(self, value):
        self.window.app_prune_days = int(value)
        self.window.schedule_save()

    def forget_all_offline(self):
        active_ids = {
            app_id for app_id in self.window.app_widgets
            if self.window.app_widgets[app_id]._active_indices
        }
        remembered_ids = (
            set(getattr(self.window, "app_routing", {}).keys())
            | set(getattr(self.window, "app_volumes", {}).keys())
            | set(getattr(self.window, "app_last_seen", {}).keys())
        )
        to_forget = [app_id for app_id in remembered_ids if app_id not in active_ids]
        if not to_forget:
            QMessageBox.information(
                self.window.settings_dialog,
                "Forget offline apps",
                "No offline apps to forget.",
            )
            return
        yn = QMessageBox.question(
            self.window.settings_dialog,
            "Forget offline apps",
            f"Drop saved routing and volume settings for {len(to_forget)} offline app(s)?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        for app_id in to_forget:
            self.window.app_routing.pop(app_id, None)
            self.window.app_volumes.pop(app_id, None)
            self.window.app_last_seen.pop(app_id, None)
            self.window.forgotten_apps.add(app_id)
            widget = self.window.app_widgets.pop(app_id, None)
            if widget is not None:
                widget.setParent(None)
                widget.deleteLater()
        self.window.save_config()
        self.window._refresh()

    def on_emergency_reset(self):
        yn = QMessageBox.warning(
            self.window.settings_dialog,
            "Emergency Reset",
            "Unload ALL WaveLinux audio modules and rebuild from config? Use this if your audio has wedged.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn == QMessageBox.StandardButton.Yes:
            self.window.runtime.full_audio_reset_sync()
            self.window.load_config()
            self.window._refresh()

    def export_runtime_diagnostics(self):
        path = self.window.runtime.export_diagnostics()
        QMessageBox.information(
            self.window,
            "Diagnostics Exported",
            f"Saved runtime diagnostics to:\n{path}",
        )
