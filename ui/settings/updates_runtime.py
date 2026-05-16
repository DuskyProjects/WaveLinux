"""Runtime-mode helpers for the updates settings controller."""

from __future__ import annotations

import os


class UpdatesRuntimeMixin:
    def current_runtime_mode(self):
        override = self.window.__dict__.get("_current_runtime_mode")
        if callable(override):
            return override()
        return self._runtime_mode()

    def launcher_targets_active_runtime(self, *, state=None, mode=None):
        override = self.window.__dict__.get("_launcher_targets_active_runtime")
        if callable(override):
            return override(state=state, mode=mode)
        state = state or self._install_state_loader()
        mode = mode or self.current_runtime_mode()
        wrapper_path = getattr(state, "wrapper_path", "")
        desktop_target = getattr(state, "desktop_exec_target", None)
        if not getattr(state, "desktop_exists", False) or not getattr(state, "wrapper_exists", False):
            return False if mode.kind in {"appimage", "source", "bundle"} else None
        if desktop_target not in {
            os.path.abspath(wrapper_path),
            wrapper_path,
            os.path.basename(wrapper_path),
        }:
            return False
        if mode.kind == "appimage":
            running_appimage = getattr(state, "running_appimage_path", None)
            return bool(
                getattr(state, "wrapper_mode", "") == "appimage"
                and running_appimage
                and getattr(state, "installed_appimage_exists", False)
                and os.path.abspath(running_appimage) == os.path.abspath(state.installed_appimage_path)
            )
        if mode.kind == "source":
            source_dir = getattr(state, "wrapper_source_dir", None)
            return bool(
                getattr(state, "wrapper_mode", "") == "source"
                and source_dir
                and os.path.abspath(source_dir) == os.path.abspath(os.path.dirname(mode.running_path))
            )
        if mode.kind == "bundle":
            bundle_exec = getattr(state, "wrapper_bundle_exec", None)
            return bool(
                getattr(state, "wrapper_mode", "") == "bundle"
                and bundle_exec
                and os.path.abspath(bundle_exec) == os.path.abspath(mode.running_path)
            )
        return None

    def runtime_mode_detail(self):
        override = self.window.__dict__.get("_runtime_mode_detail")
        if callable(override):
            return override()
        mode = self.current_runtime_mode()
        details = {
            "appimage": (
                "Running from an AppImage. Verified in-app AppImage install/update is enabled.",
                "Download and install verified AppImages from GitHub releases.",
            ),
            "source": (
                "Running from a source checkout. Verified AppImage install is available if you want a desktop-managed build.",
                "Keep using source, or install a verified AppImage into ~/.local/bin.",
            ),
            "bundle": (
                "Running from a local bundled binary. Verified AppImage install is available if you want the standard desktop-managed build.",
                "Install a verified AppImage into ~/.local/bin, or replace this local bundle manually.",
            ),
            "package": (
                "Package-managed install detected. One-click AppImage replacement is disabled for this runtime.",
                "Update WaveLinux through your distro or package manager.",
            ),
        }
        description, guidance = details.get(
            mode.kind,
            ("Unknown runtime mode.", "Update WaveLinux using the mechanism that installed this build."),
        )
        return mode, description, guidance

    def running_binary_path(self, state):
        override = self.window.__dict__.get("_running_binary_path")
        if callable(override):
            return override(state)
        if state.running_appimage_path:
            return state.running_appimage_path
        return self.current_runtime_mode().running_path

    def update_issue_title(self, code):
        override = self.window.__dict__.get("_update_issue_title")
        if callable(override):
            return override(code)
        titles = {
            "update.manifest_missing": "Signed release manifest unavailable",
            "update.signature_invalid": "Release signature verification failed",
            "update.asset_missing": "Verified AppImage asset unavailable",
            "update.checksum_mismatch": "Downloaded AppImage checksum mismatch",
            "update.smoke_test_failed": "Downloaded AppImage failed validation",
            "update.rollback_failed": "Previous AppImage restore failed",
        }
        return titles.get(code, "Update verification issue")
