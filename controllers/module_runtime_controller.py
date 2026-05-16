"""Feature-module lifecycle helpers for the main window."""

from __future__ import annotations

import logging

from app_core import AppContext, ModuleManager
from health import HealthIssue
from modules import (
    AppRoutingModule,
    DevicePolicyModule,
    EffectsModule,
    HealthModule,
    MeteringModule,
    MixerUiModule,
    RuntimeModule,
    ScenesModule,
    SettingsUiModule,
    StressControlModule,
    UpdatesModule,
)


class ModuleRuntimeController:
    def __init__(self, window):
        self.window = window

    def setup_feature_modules(self):
        ctx = AppContext(
            runtime=self.window.runtime,
            engine=self.window.engine,
            config_store=self.window,
            event_bus=self.window._event_bus,
            module_manager=None,
            diagnostics=getattr(self.window.runtime, "diagnostics", None),
            main_window=self.window,
        )
        manager = ModuleManager(
            ctx,
            feature_flags=self.window._feature_flags,
            health_bus=self.window._module_health_bus,
        )
        ctx.module_manager = manager
        self.window.module_manager = manager
        for module in (
            RuntimeModule(self.window),
            MixerUiModule(self.window),
            MeteringModule(self.window),
            AppRoutingModule(self.window),
            EffectsModule(self.window),
            DevicePolicyModule(self.window),
            SettingsUiModule(self.window),
            ScenesModule(self.window),
            UpdatesModule(self.window),
            HealthModule(self.window),
            StressControlModule(self.window),
        ):
            manager.register(module)

    def start_feature_modules(self):
        manager = getattr(self.window, "module_manager", None)
        if manager is None:
            return
        manager.start_all()

    def module_enabled(self, module_id):
        key = str(module_id or "").strip().lower()
        return (self.window.__dict__.get("_module_feature_enabled") or {}).get(key, True)

    def set_feature_module_enabled(self, module_id, enabled, *, reason=""):
        key = str(module_id or "").strip().lower()
        previous = self.window._module_feature_enabled.get(key, True)
        self.window._module_feature_enabled[key] = bool(enabled)
        if previous == bool(enabled) and reason != "restore":
            return
        if key == "metering":
            if not enabled:
                self.window._stop_all_meters()
            else:
                self.restart_metering_module()
        elif key == "effects":
            if not enabled:
                self.disable_effects_module_runtime(reason=reason)
        elif key == "settings_ui":
            if not enabled:
                self.window._stress_close_settings()
        elif key == "mixer_ui":
            inputs_scroll = getattr(self.window, "inputs_scroll", None)
            if inputs_scroll is not None:
                inputs_scroll.setVisible(bool(enabled))
        elif key == "app_routing":
            self.set_app_routing_controls_enabled(bool(enabled))
        elif key == "updates":
            self.set_update_controls_enabled(bool(enabled))
        elif key == "health":
            system_tab = getattr(self.window, "_system_tab_widget", None)
            if system_tab is not None:
                system_tab.setEnabled(bool(enabled))
        elif key == "scenes":
            scenes_tab = getattr(self.window, "_scenes_tab_widget", None)
            if scenes_tab is not None:
                scenes_tab.setEnabled(bool(enabled))

    def restart_metering_module(self):
        if not self.module_enabled("metering"):
            return
        view = getattr(self.window, "_runtime_view_state", None)
        if view is None:
            return
        self.window._refresh_runtime_view()

    def disable_effects_module_runtime(self, *, reason=""):
        active_channels = set(getattr(self.window, "active_effects", {}).keys()) | set(
            getattr(self.window, "effect_params", {}).keys()
        )
        runtime = getattr(self.window, "runtime", None)
        if runtime is not None:
            for node_name in sorted(active_channels):
                runtime.clear_channel_fx(node_name)
        self.window.active_effects = {}
        if reason:
            logging.info("Effects module disabled: %s", reason)
        self.window.schedule_save()

    def enable_effects_module_runtime(self):
        self.set_feature_module_enabled("effects", True, reason="restore")
        self.window._sync_runtime_persistent_state(immediate=True)
        self.window._request_runtime_refresh("effects-module-restore")

    def set_app_routing_controls_enabled(self, enabled):
        for row in list(getattr(self.window, "app_widgets", {}).values()):
            try:
                row.combo.setEnabled(bool(enabled))
                row.manage_btn.setEnabled(bool(enabled))
                row.forget_btn.setEnabled(bool(enabled))
            except Exception:
                continue

    def set_update_controls_enabled(self, enabled):
        for attr in (
            "_check_update_btn",
            "_download_update_btn",
            "_install_runtime_btn",
            "_rollback_update_btn",
            "_download_install_btn",
            "_install_current_btn",
            "_restore_backup_btn",
        ):
            widget = self.window.__dict__.get(attr)
            if widget is not None:
                widget.setEnabled(bool(enabled))

    def module_health_issues(self):
        manager = self.window.__dict__.get("module_manager")
        if manager is None:
            return []
        issues = []
        for health in manager.list_modules():
            if health.state == "running":
                continue
            severity = "warning" if health.state in {"degraded", "failed"} else "info"
            title = f"Module {health.module_id} is {health.state}"
            detail = health.summary or f"The {health.module_id} module is currently {health.state}."
            primary_action = ""
            if health.state == "disabled":
                primary_action = "Enable module"
            elif health.restartable and health.state in {"degraded", "failed", "stopped"}:
                primary_action = "Restart module"
            issues.append(
                HealthIssue(
                    code=f"module.{health.module_id}.{health.state}",
                    severity=severity,
                    title=title,
                    detail=detail,
                    primary_action=primary_action,
                    context={"module_id": health.module_id},
                )
            )
        return issues
