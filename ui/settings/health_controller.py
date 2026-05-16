"""Health tab aggregation and recovery UI helpers."""

from __future__ import annotations

import os
import time

from PyQt6.QtCore import QUrl
from PyQt6.QtGui import QDesktopServices
from PyQt6.QtWidgets import QMessageBox

from distribution import install_state, installed_appimage_backup_path, is_running_in_appimage
from health import HealthIssue
from ui.health.health_card import HealthCard


class HealthTabController:
    def __init__(
        self,
        window,
        *,
        startup_preflight_reporter,
        install_state_loader=install_state,
        installed_appimage_backup_path_fn=installed_appimage_backup_path,
        is_running_in_appimage_fn=is_running_in_appimage,
    ):
        self.window = window
        self._startup_preflight_reporter = startup_preflight_reporter
        self._install_state_loader = install_state_loader
        self._installed_appimage_backup_path = installed_appimage_backup_path_fn
        self._is_running_in_appimage = is_running_in_appimage_fn

    @staticmethod
    def format_timestamp(stamp):
        if not stamp:
            return "never"
        return time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(stamp))

    def diagnostics_root_path(self):
        runtime = getattr(self.window, "runtime", None)
        diagnostics = getattr(runtime, "diagnostics", None) if runtime is not None else None
        root_dir = getattr(diagnostics, "root_dir", None)
        return str(root_dir) if root_dir else os.path.expanduser("~/.config/wavelinux/diagnostics")

    def health_issue_for_runtime_detail(self, detail):
        code = str(detail.get("code") or "").strip()
        message = str(detail.get("detail") or "").strip()
        context = dict(detail.get("context") or {})
        if code == "runtime.missing_tool":
            return HealthIssue(
                code=code,
                severity="error",
                title="Missing host audio tools",
                detail=message,
                primary_action="Re-run check",
                secondary_action="Open Releases Page",
                context=context,
            )
        if code == "runtime.pipewire_unreachable":
            return HealthIssue(
                code=code,
                severity="error",
                title="PipeWire compatibility query failed",
                detail=message,
                primary_action="Re-run check",
                secondary_action="Open Releases Page",
                context=context,
            )
        if code == "runtime.wireplumber_unreachable":
            return HealthIssue(
                code=code,
                severity="error",
                title="WirePlumber query failed",
                detail=message,
                primary_action="Re-run check",
                secondary_action="Open Releases Page",
                context=context,
            )
        if code == "runtime.config_unwritable":
            return HealthIssue(
                code=code,
                severity="error",
                title="WaveLinux config directory is not writable",
                detail=message,
                primary_action="Re-run check",
                secondary_action="Open Releases Page",
                context=context,
            )
        return HealthIssue(
            code=code or "runtime.issue",
            severity="warning",
            title="Runtime issue",
            detail=message or "WaveLinux detected a runtime issue.",
            primary_action="Re-run check",
            secondary_action="Open Releases Page",
            context=context,
        )

    def health_issue_for_channel(self, node_name, *, recovered=False):
        label = self.window._channel_label(node_name)
        recovery = self.window.recovery_status_for_channel(node_name)
        issue = self.window.channel_runtime_issue(node_name)
        if recovered:
            detail = f"Automatic recovery completed after {max(1, recovery.retry_count)} attempt(s)."
            if recovery.diagnostics_path:
                detail += f"\nDiagnostics: {recovery.diagnostics_path}"
            return HealthIssue(
                code="fx.channel_recovered",
                severity="info",
                title=f"{label} recovered",
                detail=detail,
                primary_action="Open diagnostics" if recovery.diagnostics_path else "",
                context={"node_name": node_name, "diagnostics_path": recovery.diagnostics_path},
            )
        detail_lines = []
        if issue.get("summary"):
            detail_lines.append(str(issue["summary"]))
        tooltip = str(issue.get("tooltip") or "").strip()
        if tooltip:
            detail_lines.extend(line.strip() for line in tooltip.splitlines() if line.strip())
        if recovery.state == "scheduled" and recovery.next_retry_at:
            detail_lines.append(
                f"Next retry around {self.format_timestamp(recovery.next_retry_at)}."
            )
        elif recovery.state == "retrying":
            detail_lines.append(f"Recovery attempt count: {recovery.retry_count}.")
        elif recovery.state == "exhausted":
            detail_lines.append("Automatic recovery is exhausted; manual recovery is required.")
        code = (
            "fx.channel_recovery_exhausted"
            if recovery.state == "exhausted"
            else "fx.channel_degraded"
        )
        return HealthIssue(
            code=code,
            severity="error" if recovery.state == "exhausted" else "warning",
            title=f"{label} degraded",
            detail="\n".join(dict.fromkeys(line for line in detail_lines if line)),
            primary_action="Recover channel",
            secondary_action="Open diagnostics" if recovery.diagnostics_path else "",
            context={
                "node_name": node_name,
                "diagnostics_path": recovery.diagnostics_path,
                "recovery_state": recovery.state,
                "retry_count": recovery.retry_count,
            },
        )

    def collect_health_issues(self, *, preflight=None, state=None):
        preflight = preflight or self._startup_preflight_reporter()
        state = state or self._install_state_loader()
        mode = self.window._current_runtime_mode()
        wrapper_mode = getattr(state, "wrapper_mode", "unknown")
        wrapper_source_dir = getattr(state, "wrapper_source_dir", None)
        wrapper_bundle_exec = getattr(state, "wrapper_bundle_exec", None)
        warnings = tuple(getattr(state, "warnings", ()) or ())
        issues = [
            self.health_issue_for_runtime_detail(detail)
            for detail in preflight.get("issue_details", [])
        ]

        expects_appimage_install = (
            mode.kind == "appimage"
            or wrapper_mode == "appimage"
            or (
                getattr(state, "desktop_exists", False)
                and wrapper_mode not in {"source", "bundle"}
            )
            or bool(getattr(state, "stale_launcher_entries", ()))
        )
        if expects_appimage_install and getattr(state, "appimage_missing", False):
            detail = (
                f"No installed AppImage was found at {state.installed_appimage_path}. "
                "WaveLinux can still run from source or a package manager, but one-click "
                "AppImage restart/update flows install into that path."
            )
            issues.append(
                HealthIssue(
                    code="install.appimage_missing",
                    severity="warning",
                    title="Installed AppImage missing",
                    detail=detail,
                    primary_action="Open Releases Page",
                    secondary_action="Re-run check",
                    context={"path": state.installed_appimage_path},
                )
            )
        if getattr(state, "wrapper_mismatch", False):
            issues.append(
                HealthIssue(
                    code="install.wrapper_mismatch",
                    severity="warning",
                    title="Desktop wrapper points at the wrong AppImage",
                    detail=(
                        f"The launcher wrapper at {state.wrapper_path} points at "
                        f"{state.wrapper_target or 'an unexpected target'} instead of "
                        f"{state.installed_appimage_path}."
                    ),
                    primary_action="Repair launchers",
                    secondary_action="Re-run check",
                    context={"path": state.wrapper_path, "target": state.wrapper_target or ""},
                )
            )
        if (
            wrapper_mode == "source"
            and any("source wrapper points at a missing WaveLinux checkout" in warning for warning in warnings)
        ):
            issues.append(
                HealthIssue(
                    code="install.wrapper_mismatch",
                    severity="warning",
                    title="Installed source launcher points at a missing checkout",
                    detail=(
                        f"The source launcher wrapper at {state.wrapper_path} points at "
                        f"{wrapper_source_dir or 'a missing checkout'}.\n"
                        "Re-run install.sh from the current checkout, or install a verified AppImage."
                    ),
                    primary_action="Repair launchers",
                    secondary_action="Re-run check",
                    context={"path": state.wrapper_path, "source_dir": wrapper_source_dir or ""},
                )
            )
        if (
            wrapper_mode == "bundle"
            and any("bundle launcher points at a missing WaveLinux binary" in warning for warning in warnings)
        ):
            issues.append(
                HealthIssue(
                    code="install.wrapper_mismatch",
                    severity="warning",
                    title="Installed local build launcher points at a missing binary",
                    detail=(
                        f"The bundled-build launcher wrapper at {state.wrapper_path} points at "
                        f"{wrapper_bundle_exec or 'a missing binary'}.\n"
                        "Run WaveLinux from the desired local build and reinstall its launcher, or install a verified AppImage."
                    ),
                    primary_action="Repair launchers",
                    secondary_action="Re-run check",
                    context={"path": state.wrapper_path, "bundle_exec": wrapper_bundle_exec or ""},
                )
            )
        if (
            mode.kind == "source"
            and wrapper_mode == "source"
            and wrapper_source_dir
            and os.path.abspath(wrapper_source_dir) != os.path.abspath(os.path.dirname(mode.running_path))
        ):
            issues.append(
                HealthIssue(
                    code="install.runtime_target_mismatch",
                    severity="info",
                    title="Installed source launcher targets a different checkout",
                    detail=(
                        f"The installed source launcher points at {wrapper_source_dir}, but the current "
                        f"WaveLinux session is running from {os.path.dirname(mode.running_path)}.\n"
                        "Repair launchers if you want the desktop/menu entry to launch this checkout instead."
                    ),
                    primary_action="Repair launchers",
                    secondary_action="Re-run check",
                    context={
                        "path": state.wrapper_path,
                        "source_dir": wrapper_source_dir,
                        "running_source_dir": os.path.dirname(mode.running_path),
                    },
                )
            )
        if (
            mode.kind == "bundle"
            and wrapper_mode == "bundle"
            and wrapper_bundle_exec
            and os.path.abspath(wrapper_bundle_exec) != os.path.abspath(mode.running_path)
        ):
            issues.append(
                HealthIssue(
                    code="install.runtime_target_mismatch",
                    severity="info",
                    title="Installed local build launcher targets a different binary",
                    detail=(
                        f"The installed bundled-build launcher points at {wrapper_bundle_exec}, but the "
                        f"current WaveLinux session is running from {mode.running_path}.\n"
                        "Repair launchers if you want the desktop/menu entry to launch this build instead."
                    ),
                    primary_action="Repair launchers",
                    secondary_action="Re-run check",
                    context={
                        "path": state.wrapper_path,
                        "bundle_exec": wrapper_bundle_exec,
                        "running_bundle": mode.running_path,
                    },
                )
            )
        if (
            mode.kind == "appimage"
            and wrapper_mode == "appimage"
            and getattr(state, "running_appimage_path", None)
            and getattr(state, "installed_appimage_exists", False)
            and os.path.abspath(state.running_appimage_path) != os.path.abspath(state.installed_appimage_path)
        ):
            issues.append(
                HealthIssue(
                    code="install.runtime_target_mismatch",
                    severity="info",
                    title="Installed AppImage launcher targets a different file",
                    detail=(
                        f"The installed AppImage launcher points at {state.installed_appimage_path}, but the "
                        f"current WaveLinux session is running from {state.running_appimage_path}.\n"
                        "Repair launchers if you want the desktop/menu entry to launch this AppImage instead."
                    ),
                    primary_action="Repair launchers",
                    secondary_action="Re-run check",
                    context={
                        "path": state.wrapper_path,
                        "installed_appimage_path": state.installed_appimage_path,
                        "running_appimage_path": state.running_appimage_path,
                    },
                )
            )
        backup_path = getattr(state, "installed_appimage_backup_path", self._installed_appimage_backup_path())
        backup_exists = bool(
            getattr(state, "installed_appimage_backup_exists", os.path.exists(backup_path))
        )
        if backup_exists:
            issues.append(
                HealthIssue(
                    code="update.backup_available",
                    severity="info",
                    title="Previous AppImage backup is available",
                    detail=(
                        f"WaveLinux has a restorable backup AppImage at {backup_path}."
                        + (
                            "\nThis runtime can restore that backup directly from Settings -> Updates."
                            if mode.kind in {"appimage", "source", "bundle"}
                            else "\nThis runtime is package-managed, so rollback stays informational only."
                        )
                    ),
                    primary_action="Restore Previous AppImage" if mode.kind in {"appimage", "source", "bundle"} else "",
                    secondary_action="Re-run check" if mode.kind in {"appimage", "source", "bundle"} else "",
                    context={"backup_path": backup_path},
                )
            )
        if getattr(state, "desktop_mismatch", False) or getattr(state, "stale_launcher_entries", ()):
            stale_paths = "\n".join(
                entry.path for entry in getattr(state, "stale_launcher_entries", ())
            )
            detail = (
                f"The canonical desktop entry at {state.desktop_path} does not match the "
                "current install state."
            )
            if stale_paths:
                detail += f"\nStale launchers:\n{stale_paths}"
            issues.append(
                HealthIssue(
                    code="install.desktop_stale",
                    severity="warning",
                    title="Desktop launchers need repair",
                    detail=detail,
                    primary_action="Repair launchers",
                    secondary_action="Re-run check",
                    context={"desktop_path": state.desktop_path},
                )
            )

        update_issue = dict(getattr(self.window, "_last_update_issue", {}) or {})
        if update_issue:
            code = str(update_issue.get("code") or "update.asset_missing")
            severity = (
                "warning"
                if code in {"update.manifest_missing", "update.asset_missing"}
                else "error"
            )
            issues.append(
                HealthIssue(
                    code=code,
                    severity=severity,
                    title=self.window._update_issue_title(code),
                    detail=str(
                        update_issue.get("message")
                        or "WaveLinux detected an update verification issue."
                    ),
                    primary_action="Retry update check",
                    secondary_action="Open Releases Page",
                    context=update_issue,
                )
            )

        issues.extend(self.window._device_health_issues(self.window.__dict__.get("_runtime_view_state")))
        issues.extend(self.window._module_health_issues())

        degraded = set(self.window._runtime_degraded_channels())
        for node_name in sorted(degraded):
            issues.append(self.health_issue_for_channel(node_name))

        for node_name, payload in sorted((self.window._recent_recovery_status or {}).items()):
            if node_name in degraded:
                continue
            if (time.time() - float(payload.get("at", 0) or 0)) >= 90:
                continue
            issues.append(self.health_issue_for_channel(node_name, recovered=True))

        return issues

    def run_health_issue_action(self, issue, action):
        action = str(action or "").strip()
        if not action:
            return
        if action == "Re-run check":
            self.rerun_system_check()
            return
        if action == "Repair launchers":
            self.window._repair_installed_launchers()
            return
        if action == "Open Releases Page":
            self.window._open_release_page()
            return
        if action == "Retry update check":
            self.window._check_for_updates()
            return
        if action == "Re-run device reconcile":
            self.window._reconcile_device_policy()
            self.window._request_runtime_refresh("device-reconcile")
            return
        if action == "Restore Previous AppImage":
            self.window._restore_previous_appimage()
            return
        if action == "Restore monitor device":
            self.window._restore_preferred_monitor()
            return
        if action == "Restore microphone device":
            self.window._restore_preferred_mic()
            return
        if action == "Recover channel":
            self.window.recover_channel(str(issue.context.get("node_name") or ""))
            return
        if action == "Open diagnostics":
            self.window.open_channel_diagnostics(str(issue.context.get("node_name") or ""))
            return
        if action == "Restart module":
            module_id = str(issue.context.get("module_id") or "").strip()
            if module_id and getattr(self.window, "module_manager", None) is not None:
                self.window.module_manager.restart_module(module_id, "health-action")
                self.window._request_runtime_refresh(f"module-restart:{module_id}")
                self.window._schedule_active_settings_tab_refresh(force=True)
            return
        if action == "Enable module":
            module_id = str(issue.context.get("module_id") or "").strip()
            if module_id and getattr(self.window, "module_manager", None) is not None:
                self.window.module_manager.enable_module(module_id)
                self.window._request_runtime_refresh(f"module-enable:{module_id}")
                self.window._schedule_active_settings_tab_refresh(force=True)

    def render_health_cards(self, issues):
        layout = getattr(self.window, "_health_cards_layout", None)
        if layout is None:
            return
        self.window._clear_layout(layout)
        if not issues:
            issues = [
                HealthIssue(
                    code="health.ok",
                    severity="ok",
                    title="No active issues detected",
                    detail="Host runtime checks, install state, runtime recovery, and updater status all look healthy.",
                )
            ]
        for issue in issues:
            card = HealthCard(self.window._health_cards_container)
            card.configure(
                issue,
                primary_handler=(
                    lambda checked=False, issue=issue: self.run_health_issue_action(issue, issue.primary_action)
                ) if issue.primary_action else None,
                secondary_handler=(
                    lambda checked=False, issue=issue: self.run_health_issue_action(issue, issue.secondary_action)
                ) if issue.secondary_action else None,
            )
            layout.addWidget(card)
        layout.addStretch(1)

    def open_diagnostics_folder(self):
        path = self.diagnostics_root_path()
        if os.path.isdir(path) and QDesktopServices.openUrl(QUrl.fromLocalFile(path)):
            self.window.status_lbl.setText("Opened diagnostics folder")
            return
        QMessageBox.information(
            self.window,
            "Diagnostics Folder",
            f"WaveLinux stores runtime diagnostics here:\n{path}",
        )

    def refresh_system_tab(self, *, preflight=None, state=None, allow_async=True):
        if not self.window._module_enabled("health"):
            return
        preflight = preflight or self._startup_preflight_reporter()
        self.window._startup_preflight = preflight
        state = state or self.window._cached_install_state(
            target_tabs=("Health",),
            max_age_s=5.0,
            allow_async=allow_async,
        )
        state_ready = state is not None
        backup_path = getattr(
            state,
            "installed_appimage_backup_path",
            self._installed_appimage_backup_path(),
        )
        backup_exists = bool(
            getattr(state, "installed_appimage_backup_exists", os.path.exists(backup_path))
        )
        launcher_targets_active = (
            self.window._launcher_targets_active_runtime(state=state)
            if state_ready else None
        )

        summary_lbl = getattr(self.window, "_system_summary_lbl", None)
        runtime_lbl = getattr(self.window, "_system_runtime_lbl", None)
        if not all((summary_lbl, runtime_lbl)):
            return
        if state_ready:
            issues = self.collect_health_issues(preflight=preflight, state=state)
        else:
            issues = self.window._device_health_issues(self.window.__dict__.get("_runtime_view_state"))
        active_issues = [
            issue for issue in issues
            if issue.severity in {"warning", "error"}
            and not issue.code.startswith("fx.channel_recovered")
        ]
        if active_issues:
            summary_lbl.setText(
                f"Health check found {len(active_issues)} active issue(s) that can affect routing, recovery, or updates."
            )
            summary_lbl.setStyleSheet("color: #d28b26; font-size: 13px; font-weight: bold;")
        else:
            summary_lbl.setText(
                "WaveLinux health looks good. Host runtime, install state, recovery, and updater status are healthy."
            )
            summary_lbl.setStyleSheet("color: #00d4aa; font-size: 13px; font-weight: bold;")

        runtime_lines = [
            f"Current running version: v{self.window._app_version}",
            f"Running binary: {self.window._running_binary_path(state) if state_ready else self.window._current_runtime_mode().running_path}",
            "Installed AppImage: "
            + (
                state.installed_appimage_path
                if state_ready and state.installed_appimage_exists
                else ("not installed" if state_ready else "refreshing…")
            ),
            "Backup AppImage: " + (backup_path if backup_exists else "not available"),
            "Desktop launcher target: "
            + ((state.desktop_exec_target or "not installed") if state_ready else "refreshing…"),
            "Wrapper target: "
            + ((state.wrapper_target or "not installed") if state_ready else "refreshing…"),
            "Launcher targets active runtime: "
            + ("n/a" if launcher_targets_active is None else ("yes" if launcher_targets_active else "no")),
            "Current system default sink: "
            + (
                getattr(self.window._runtime_view_state, "default_sink", None)
                or self.window.engine.get_default_sink()
                or "unknown"
            ),
            "Current system default source: "
            + (
                getattr(self.window._runtime_view_state, "default_source", None)
                or self.window.engine.get_default_source()
                or "unknown"
            ),
            "Host tools present: " + ", ".join(sorted(cmd for cmd, present in preflight["deps"].items() if present)),
            f"LADSPA plugins detected: {len(getattr(self.window.engine, 'ladspa_plugins', set()) or set())}",
            f"Last successful update check: {self.format_timestamp(self.window._last_update_check_at)}",
            f"Last update attempt result: {self.window._last_update_attempt_result}",
            f"Diagnostics directory: {self.diagnostics_root_path()}",
        ]
        runtime_lbl.setText("\n".join(runtime_lines))
        self.render_health_cards(issues)

        repair_btn = getattr(self.window, "_repair_launcher_btn", None)
        if repair_btn is not None:
            repair_btn.setEnabled(
                bool(
                    state_ready
                    and (
                        state.stale_launcher_entries
                        or state.wrapper_mismatch
                        or state.desktop_mismatch
                    )
                )
                or self._is_running_in_appimage()
            )
        recover_btn = getattr(self.window, "_health_recover_btn", None)
        if recover_btn is not None:
            degraded = len(self.window._runtime_degraded_channels())
            recover_btn.setEnabled(degraded > 0)
            recover_btn.setText(
                f"Recover degraded channels ({degraded})" if degraded else "Recover degraded channels"
            )
        self.window._mark_settings_tab_refreshed("Health")

    def rerun_system_check(self):
        self.refresh_system_tab()
        self.window._refresh_update_tab()
        QMessageBox.information(
            self.window.settings_dialog,
            "Health check complete",
            "WaveLinux refreshed its host-runtime, install-state, recovery, and updater checks.",
        )
