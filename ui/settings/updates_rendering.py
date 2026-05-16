"""Install-state rendering for the updates settings tab."""

from __future__ import annotations

import os
import sys


class UpdatesRenderingMixin:
    def _update_install_button(self, btn, *, state_ready, state, mode):
        if mode.kind == "appimage":
            btn.setVisible(True)
            btn.setText(
                "Reinstall This AppImage"
                if state_ready and state.installed_appimage_exists
                else "Install This AppImage"
            )
            btn.setToolTip(
                "Install the currently running AppImage into ~/.local/bin and refresh its desktop launcher."
            )
        elif mode.kind == "bundle":
            btn.setVisible(True)
            btn.setText(
                "Reinstall This Local Build"
                if state_ready
                and state.wrapper_mode == "bundle"
                and getattr(state, "wrapper_bundle_exec", None) == mode.running_path
                else "Install This Local Build"
            )
            btn.setToolTip(
                "Install or refresh the desktop launcher for the current local bundled WaveLinux binary."
            )
        elif mode.kind == "source":
            btn.setVisible(True)
            btn.setText("Reinstall This Source Checkout")
            btn.setToolTip("Install or refresh the desktop launcher for the current source checkout.")
        else:
            btn.setVisible(False)

    def _update_rollback_button(self, btn, *, mode, backup_path, backup_exists):
        btn.setVisible(mode.kind in {"appimage", "source", "bundle"})
        btn.setEnabled(mode.kind in {"appimage", "source", "bundle"} and backup_exists)
        btn.setToolTip(
            f"Restore the previous installed AppImage backup from {backup_path}."
            if backup_exists else
            "No previous AppImage backup is available to restore."
        )

    def _update_download_button(self, btn, *, mode, guidance):
        pending_tag = getattr(self.window, "_pending_update_tag", None)
        pending_asset_url = getattr(self.window, "_pending_update_asset_url", "") or ""
        if not mode.allows_self_update:
            btn.setText("Use Package Manager to Update")
            btn.setEnabled(False)
            btn.setToolTip(guidance)
        elif pending_tag and self._parse_version(pending_tag) > self._parse_version(self._window_app_version()):
            btn.setText(f"Download && Install v{pending_tag}")
            btn.setEnabled(bool(pending_asset_url))
            if not pending_asset_url:
                btn.setToolTip(
                    "The latest signed release manifest does not expose an eligible x86_64 AppImage asset."
                )
            else:
                btn.setToolTip(
                    "Download the latest verified AppImage from GitHub and replace the installed desktop build."
                )
        else:
            btn.setText("Download && Install Latest AppImage")
            btn.setEnabled(True)
            btn.setToolTip(
                "Fetch the latest verified GitHub AppImage and install it to ~/.local/bin/WaveLinux.AppImage."
            )

    def _install_state_lines(self, *, state_ready, state, mode, backup_path, backup_exists, launcher_targets_active):
        lines = [f"Current running version: v{self._window_app_version()}", f"Runtime mode: {mode.kind}"]
        if not state_ready:
            lines.append("Install state: refreshing in background…")
            return lines
        if state.wrapper_mode == "source":
            lines.append(
                "Installed launcher mode: source checkout"
                + (f" ({state.wrapper_source_dir})" if state.wrapper_source_dir else "")
            )
        elif state.wrapper_mode == "bundle":
            lines.append(
                "Installed launcher mode: local bundle"
                + (
                    f" ({state.wrapper_bundle_exec})"
                    if getattr(state, "wrapper_bundle_exec", None) else ""
                )
            )
        elif state.wrapper_mode == "appimage":
            lines.append("Installed launcher mode: AppImage")
        if state.running_appimage_path:
            lines.append(f"Running AppImage: {state.running_appimage_path}")
        elif getattr(sys, "frozen", False):
            lines.append(f"Running binary: {os.path.abspath(sys.executable)}")
        else:
            lines.append(f"Running from source: {self._resource_path('main.py')}")
        lines.append(
            "Installed AppImage: "
            + (
                state.installed_appimage_path
                if state.installed_appimage_exists else "not installed"
            )
        )
        lines.append("Backup AppImage: " + (backup_path if backup_exists else "not available"))
        lines.append(
            "Desktop launcher: "
            + (
                (state.desktop_exec_target or state.desktop_path)
                if state.desktop_exists else "not installed"
            )
        )
        if launcher_targets_active is None:
            lines.append("Launcher targets active runtime: n/a")
        else:
            lines.append(
                "Launcher targets active runtime: "
                + ("yes" if launcher_targets_active else "no")
            )
        if state.stale_launcher_entries:
            stale_names = ", ".join(
                os.path.basename(entry.path)
                for entry in state.stale_launcher_entries[:3]
            )
            lines.append(f"Extra launchers: {stale_names}")
        return lines

    def _update_note_label(self, note_lbl, *, mode):
        if mode.allows_self_update:
            note_lbl.setText(
                "WaveLinux verifies a signed GitHub release manifest, downloads the matching "
                "AppImage, validates its checksum, runs smoke checks, and only then replaces "
                "~/.local/bin/WaveLinux.AppImage for you."
            )
        else:
            note_lbl.setText(
                "WaveLinux can still check verified GitHub releases, but this runtime should be "
                "updated through your package manager instead of replacing it with an AppImage."
            )

    def _update_repair_button(self, repair_btn, *, state_ready, state):
        needs_repair = bool(
            state_ready
            and (
                state.warnings
                or state.stale_launcher_entries
                or (
                    state.wrapper_mode == "appimage"
                    and state.installed_appimage_exists
                    and (
                        not state.wrapper_exists
                        or not state.desktop_exists
                        or state.wrapper_mismatch
                        or state.desktop_exec_target not in {
                            os.path.abspath(state.wrapper_path),
                            state.wrapper_path,
                        }
                    )
                )
            )
        )
        repair_btn.setEnabled(needs_repair or self._is_running_in_appimage())

    def refresh_update_tab(self, *, state=None, allow_async=True):
        if not self.window._module_enabled("updates"):
            return
        btn = getattr(self.window, "_install_runtime_btn", None)
        state = state or self.window._cached_install_state(
            target_tabs=("Updates",),
            max_age_s=5.0,
            allow_async=allow_async,
        )
        state_ready = state is not None
        mode, description, guidance = self.runtime_mode_detail()
        backup_path = getattr(
            state,
            "installed_appimage_backup_path",
            self._installed_appimage_backup_path(),
        )
        backup_exists = bool(
            getattr(state, "installed_appimage_backup_exists", os.path.exists(backup_path))
        )
        launcher_targets_active = (
            self.launcher_targets_active_runtime(state=state, mode=mode)
            if state_ready else None
        )
        if btn is not None:
            self._update_install_button(btn, state_ready=state_ready, state=state, mode=mode)
        rollback_btn = getattr(self.window, "_rollback_update_btn", None)
        if rollback_btn is not None:
            self._update_rollback_button(
                rollback_btn,
                mode=mode,
                backup_path=backup_path,
                backup_exists=backup_exists,
            )
        download_btn = getattr(self.window, "_download_update_btn", None)
        if download_btn is not None:
            self._update_download_button(download_btn, mode=mode, guidance=guidance)
        policy_lbl = getattr(self.window, "_update_policy_lbl", None)
        if policy_lbl is not None:
            policy_lbl.setText(description)
        info_lbl = getattr(self.window, "_install_state_lbl", None)
        if info_lbl is not None:
            info_lbl.setText(
                "\n".join(
                    self._install_state_lines(
                        state_ready=state_ready,
                        state=state,
                        mode=mode,
                        backup_path=backup_path,
                        backup_exists=backup_exists,
                        launcher_targets_active=launcher_targets_active,
                    )
                )
            )
        warning_lbl = getattr(self.window, "_install_warning_lbl", None)
        if warning_lbl is not None:
            warning_lbl.setVisible(bool(state_ready and state.warnings))
            warning_lbl.setText("\n".join(getattr(state, "warnings", ()) or ()))
        note_lbl = getattr(self.window, "_update_note_lbl", None)
        if note_lbl is not None:
            self._update_note_label(note_lbl, mode=mode)
        repair_btn = getattr(self.window, "_repair_launcher_btn", None)
        if repair_btn is not None:
            self._update_repair_button(repair_btn, state_ready=state_ready, state=state)
        self.window._mark_settings_tab_refreshed("Updates")
