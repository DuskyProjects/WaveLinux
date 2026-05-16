"""Update-check, install, rollback, and launcher actions."""

from __future__ import annotations

import os
import sys
import time

from PyQt6.QtCore import QTimer, QUrl
from PyQt6.QtGui import QDesktopServices
from PyQt6.QtWidgets import QMessageBox


class UpdatesActionsMixin:
    def check_for_updates(self):
        self.window._check_update_btn.setEnabled(False)
        self.window._update_status_lbl.setText("Checking for updates…")
        self.window._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

        prev = getattr(self.window, "_updater", None)
        if prev is not None:
            prev.cancel()

        self.window._updater = self._update_checker_cls()
        self.window._updater.check()
        timer = getattr(self.window, "_update_poll_timer", None)
        if timer is None:
            timer = QTimer(self.window)
            timer.setInterval(200)
            timer.timeout.connect(self.poll_updater)
            self.window._update_poll_timer = timer
        else:
            timer.stop()
        timer.start()

    def poll_updater(self):
        updater = getattr(self.window, "_updater", None)
        if updater is None:
            return
        while True:
            item = updater.poll()
            if item is None:
                break
            kind = item[0]
            if kind == "result":
                self.window._update_poll_timer.stop()
                self.handle_update_result(item[1])
            elif kind == "error":
                self.window._update_poll_timer.stop()
                self.handle_update_error(item[1])

    def handle_update_result(self, release_info):
        self.window._check_update_btn.setEnabled(True)
        if not isinstance(release_info, self._verified_release_info_cls):
            raise TypeError("Expected VerifiedReleaseInfo from update checker")
        latest_tag = str(release_info.version or "").strip()
        self.window._pending_verified_release = release_info
        self.window._pending_update_url = release_info.release_url or self._release_page_url()
        self.window._pending_update_asset_url = release_info.asset_url or ""
        self.window._pending_update_asset_name = release_info.asset_name or ""
        self.window._last_update_check_at = time.time()
        self.window._last_update_issue = None
        current = self._parse_version(self._window_app_version())
        latest = self._parse_version(latest_tag)
        mode, _description, guidance = self.runtime_mode_detail()
        if latest > current:
            self.window._pending_update_tag = latest_tag
            if self.window._pending_update_asset_url and mode.allows_self_update:
                self.window._last_update_attempt_result = f"Verified update available: v{latest_tag}"
                self.window._update_status_lbl.setText(
                    f"Verified update available: v{latest_tag}  (current: v{self._window_app_version()})"
                )
                self.window._update_status_lbl.setStyleSheet(
                    "color: #00d4aa; font-size: 12px; font-weight: bold;"
                )
                self.window._show_notification(
                    "WaveLinux Update Available",
                    f"Version {latest_tag} is available. Open Settings -> Updates to install it.",
                )
            elif self.window._pending_update_asset_url:
                self.window._last_update_attempt_result = (
                    f"Verified release v{latest_tag} is available; update this install through your package manager."
                )
                self.window._update_status_lbl.setText(
                    f"Verified release v{latest_tag} is available. {guidance}"
                )
                self.window._update_status_lbl.setStyleSheet("color: #d28b26; font-size: 12px;")
            else:
                self.window._last_update_attempt_result = (
                    f"Verified release v{latest_tag} has no eligible AppImage asset."
                )
                self.window._update_status_lbl.setText(
                    f"Version {latest_tag} is available, but the signed manifest exposes no eligible AppImage asset."
                )
                self.window._update_status_lbl.setStyleSheet("color: #d28b26; font-size: 12px;")
        else:
            self.window._last_update_attempt_result = (
                f"Already on the latest verified release: v{self._window_app_version()}"
            )
            self.window._update_status_lbl.setText(
                f"You're up to date on the latest verified release! (v{self._window_app_version()})"
            )
            self.window._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
            self.window._pending_update_tag = None
            self.window._pending_update_asset_url = ""
            self.window._pending_update_asset_name = ""
        self.refresh_update_tab()
        self.window._refresh_system_tab()

    def handle_update_error(self, payload):
        self.window._check_update_btn.setEnabled(True)
        payload = dict(payload or {})
        self.window._last_update_issue = payload
        self.window._pending_update_url = str(payload.get("release_url") or self._release_page_url())
        self.window._last_update_attempt_result = (
            f"Update check failed: {payload.get('message') or 'unknown error'}"
        )
        message = str(payload.get("message") or "unknown error")
        code = str(payload.get("code") or "")
        style = (
            "color: #d28b26; font-size: 12px;"
            if code in {"update.manifest_missing", "update.asset_missing"}
            else "color: #e05050; font-size: 12px;"
        )
        self.window._update_status_lbl.setText(f"Update check failed: {message}")
        self.window._update_status_lbl.setStyleSheet(style)
        self.refresh_update_tab()
        self.window._refresh_system_tab()

    def open_release_page(self):
        url = getattr(self.window, "_pending_update_url", None) or self._release_page_url()
        QDesktopServices.openUrl(QUrl(url))

    def download_and_install_update(self):
        mode, _description, guidance = self.runtime_mode_detail()
        if not mode.allows_self_update:
            QMessageBox.information(
                self.window.settings_dialog,
                "Package-managed install",
                "WaveLinux detected a package-managed install for this runtime.\n\n"
                f"{guidance}",
            )
            return
        progress = self.window.__dict__.get("_update_progress")
        if progress is not None:
            progress.setVisible(True)
            progress.setRange(0, 0)
            progress.setFormat("Checking latest release…")
        self.window._download_update_btn.setEnabled(False)
        self.window._check_update_btn.setEnabled(False)
        self.window._update_status_lbl.setText("Checking latest verified AppImage release…")
        self.window._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

        prev = self.window.__dict__.get("_update_installer")
        if prev is not None:
            prev.cancel()

        self.window._update_installer = self._appimage_update_installer_cls()
        self.window._update_installer.install(release_info=None)

        timer = self.window.__dict__.get("_update_install_poll_timer")
        if timer is None:
            timer = QTimer(self.window)
            timer.setInterval(200)
            timer.timeout.connect(self.poll_update_installer)
            self.window._update_install_poll_timer = timer
        else:
            timer.stop()
        timer.start()

    def poll_update_installer(self):
        installer = getattr(self.window, "_update_installer", None)
        if installer is None:
            return
        while True:
            item = installer.poll()
            if item is None:
                break
            kind = item[0]
            if kind == "progress":
                self.handle_update_install_progress(item[1], item[2], item[3])
            elif kind == "status":
                self.window._update_status_lbl.setText(str(item[1]))
                self.window._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
            elif kind == "installed":
                self.window._update_install_poll_timer.stop()
                self.handle_update_install_success(item[1], item[2])
            elif kind == "error":
                self.window._update_install_poll_timer.stop()
                self.handle_update_install_error(item[1])

    def handle_update_install_progress(self, asset_name, downloaded, total):
        progress = getattr(self.window, "_update_progress", None)
        if progress is not None:
            progress.setVisible(True)
            if total > 0:
                percent = max(0, min(int((downloaded / total) * 100), 100))
                progress.setRange(0, 100)
                progress.setValue(percent)
                progress.setFormat(f"{percent}%")
            else:
                progress.setRange(0, 0)
                progress.setFormat("Downloading…")
        if total > 0:
            percent = max(0, min(int((downloaded / total) * 100), 100))
            self.window._update_status_lbl.setText(
                f"Downloading {asset_name or 'AppImage'}… {percent}%"
            )
        else:
            self.window._update_status_lbl.setText(f"Downloading {asset_name or 'AppImage'}…")
        self.window._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

    def handle_update_install_success(self, result, release_info):
        self.window._download_update_btn.setEnabled(True)
        self.window._check_update_btn.setEnabled(True)
        rollback_btn = getattr(self.window, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(True)
        progress = getattr(self.window, "_update_progress", None)
        if progress is not None:
            progress.setVisible(False)
            progress.setRange(0, 100)
            progress.setValue(0)
        if not isinstance(release_info, self._verified_release_info_cls):
            raise TypeError("Expected VerifiedReleaseInfo from installer")
        tag = str(release_info.version or "").strip()
        self.window._pending_verified_release = release_info
        self.window._pending_update_tag = tag or None
        self.window._pending_update_url = release_info.release_url or self._release_page_url()
        self.window._pending_update_asset_url = release_info.asset_url or ""
        self.window._pending_update_asset_name = release_info.asset_name or ""
        self.window._last_update_check_at = time.time()
        self.window._last_update_issue = None
        version_text = f"v{tag}" if tag else "the latest version"
        self.window._last_update_attempt_result = (
            f"Installed verified {version_text} to {result.appimage_path}"
            + (f" with backup at {result.backup_path}" if result.backup_path else "")
        )
        self.window._update_status_lbl.setText(
            f"Installed verified {version_text} to {result.appimage_path}. Restart WaveLinux to run it."
        )
        self.window._update_status_lbl.setStyleSheet(
            "color: #00d4aa; font-size: 12px; font-weight: bold;"
        )
        self.refresh_update_tab()
        self.window._refresh_system_tab()

        install_message = (
            "WaveLinux downloaded, verified, and installed the latest AppImage.\n\n"
            f"Installed AppImage: {result.appimage_path}\n"
            + (f"Previous AppImage backup: {result.backup_path}\n" if result.backup_path else "")
            + f"Launcher: {result.wrapper_path}\n\n"
            "Restart into the updated AppImage now?"
        )

        yn = QMessageBox.question(
            self.window.settings_dialog,
            "Update installed",
            install_message,
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn == QMessageBox.StandardButton.Yes:
            self.restart_with_command([result.appimage_path])

    def handle_update_install_error(self, payload):
        self.window._download_update_btn.setEnabled(True)
        self.window._check_update_btn.setEnabled(True)
        rollback_btn = getattr(self.window, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(True)
        progress = getattr(self.window, "_update_progress", None)
        if progress is not None:
            progress.setVisible(False)
            progress.setRange(0, 100)
            progress.setValue(0)
        payload = dict(payload or {})
        self.window._last_update_issue = payload
        self.window._pending_update_url = str(payload.get("release_url") or self._release_page_url())
        self.window._last_update_attempt_result = (
            f"Update install failed: {payload.get('message') or 'unknown error'}"
        )
        code = str(payload.get("code") or "")
        self.window._update_status_lbl.setText(
            f"Update install failed: {payload.get('message') or 'unknown error'}"
        )
        self.window._update_status_lbl.setStyleSheet(
            "color: #d28b26; font-size: 12px;"
            if code in {"update.manifest_missing", "update.asset_missing"}
            else "color: #e05050; font-size: 12px;"
        )
        self.refresh_update_tab()
        self.window._refresh_system_tab()

    def restore_previous_appimage(self):
        mode, _description, guidance = self.runtime_mode_detail()
        if mode.kind == "package":
            QMessageBox.information(
                self.window.settings_dialog,
                "Package-managed install",
                "WaveLinux detected a package-managed install for this runtime.\n\n"
                f"{guidance}",
            )
            return
        backup_path = self._installed_appimage_backup_path()
        if not os.path.exists(backup_path):
            QMessageBox.information(
                self.window.settings_dialog,
                "No backup AppImage",
                f"No previous AppImage backup exists at:\n{backup_path}",
            )
            return

        progress = getattr(self.window, "_update_progress", None)
        if progress is not None:
            progress.setVisible(True)
            progress.setRange(0, 0)
            progress.setFormat("Preparing rollback…")
        self.window._download_update_btn.setEnabled(False)
        self.window._check_update_btn.setEnabled(False)
        rollback_btn = getattr(self.window, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(False)
        self.window._update_status_lbl.setText("Restoring previous AppImage backup…")
        self.window._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

        try:
            result = self._restore_previous_install()
        except self._update_error_cls as exc:
            self.handle_update_restore_error(exc.as_payload())
            return
        except Exception as exc:
            self.handle_update_restore_error(
                self._update_error_cls("update.rollback_failed", str(exc)).as_payload()
            )
            return

        self.handle_update_restore_success(result)

    def handle_update_restore_success(self, result):
        self.window._download_update_btn.setEnabled(True)
        self.window._check_update_btn.setEnabled(True)
        rollback_btn = getattr(self.window, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(True)
        progress = getattr(self.window, "_update_progress", None)
        if progress is not None:
            progress.setVisible(False)
            progress.setRange(0, 100)
            progress.setValue(0)
        if not isinstance(result, self._update_rollback_result_cls):
            raise TypeError("Expected UpdateRollbackResult from rollback")
        version_text = (
            f"v{result.restored_version}" if result.restored_version else "the previous version"
        )
        self.window._last_update_issue = None
        self.window._last_update_attempt_result = (
            f"Restored previous AppImage {version_text} from {result.backup_path}"
        )
        self.window._update_status_lbl.setText(
            f"Restored previous AppImage {version_text} to {result.appimage_path}. Restart WaveLinux to run it."
        )
        self.window._update_status_lbl.setStyleSheet(
            "color: #00d4aa; font-size: 12px; font-weight: bold;"
        )
        self.refresh_update_tab()
        self.window._refresh_system_tab()

        yn = QMessageBox.question(
            self.window.settings_dialog,
            "Previous AppImage restored",
            "WaveLinux restored the previous AppImage backup.\n\n"
            f"Backup: {result.backup_path}\n"
            f"Installed AppImage: {result.appimage_path}\n"
            f"Launcher: {result.wrapper_path}\n\n"
            "Restart into the restored AppImage now?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn == QMessageBox.StandardButton.Yes:
            self.restart_with_command([result.appimage_path])

    def handle_update_restore_error(self, payload):
        self.window._download_update_btn.setEnabled(True)
        self.window._check_update_btn.setEnabled(True)
        rollback_btn = getattr(self.window, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(True)
        progress = getattr(self.window, "_update_progress", None)
        if progress is not None:
            progress.setVisible(False)
            progress.setRange(0, 100)
            progress.setValue(0)
        payload = dict(payload or {})
        self.window._last_update_issue = payload
        self.window._pending_update_url = str(payload.get("release_url") or self._release_page_url())
        self.window._last_update_attempt_result = (
            f"Rollback failed: {payload.get('message') or 'unknown error'}"
        )
        self.window._update_status_lbl.setText(
            f"Rollback failed: {payload.get('message') or 'unknown error'}"
        )
        self.window._update_status_lbl.setStyleSheet("color: #e05050; font-size: 12px;")
        self.refresh_update_tab()
        self.window._refresh_system_tab()
        QMessageBox.warning(
            self.window.settings_dialog,
            "Rollback failed",
            str(payload.get("message") or "WaveLinux could not restore the previous AppImage."),
        )

    def install_current_runtime_launcher(self):
        mode = self.current_runtime_mode()
        try:
            if mode.kind == "appimage":
                result = self._install_current_appimage()
                title = "AppImage installed"
                message = (
                    "WaveLinux installed this AppImage for desktop use.\n\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}"
                )
            elif mode.kind == "bundle":
                result = self._install_current_bundle()
                title = "Local build launcher installed"
                message = (
                    "WaveLinux installed a launcher for this local bundled build.\n\n"
                    f"Binary: {result.bundle_path}\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}"
                )
            elif mode.kind == "source":
                result = self._install_current_source_checkout()
                title = "Source launcher installed"
                message = (
                    "WaveLinux installed a launcher for this source checkout.\n\n"
                    f"Source: {result.source_dir}\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}"
                )
            else:
                QMessageBox.information(
                    self.window.settings_dialog,
                    "Launcher install unavailable",
                    "This runtime mode does not support installing the current binary as a desktop launcher.",
                )
                return
        except Exception as exc:
            QMessageBox.warning(
                self.window.settings_dialog,
                "Launcher install failed",
                str(exc),
            )
            return
        self.refresh_update_tab()
        QMessageBox.information(self.window.settings_dialog, title, message)
        self.window._refresh_system_tab()

    def repair_installed_launchers(self):
        state = self._install_state_loader()
        mode = self.current_runtime_mode()
        try:
            if mode.kind == "source":
                result = self._repair_current_source_checkout_launchers()
                removed = len(result.removed_entries)
                msg = (
                    "WaveLinux rebuilt the source-checkout desktop launcher.\n\n"
                    f"Source: {result.source_dir}\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}\n"
                    f"Removed stale launchers: {removed}"
                )
            elif mode.kind == "bundle":
                result = self._repair_current_bundle_launchers()
                removed = len(result.removed_entries)
                msg = (
                    "WaveLinux rebuilt the local bundled-build desktop launcher.\n\n"
                    f"Binary: {result.bundle_path}\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}\n"
                    f"Removed stale launchers: {removed}"
                )
            elif state.wrapper_mode == "source":
                if state.wrapper_source_dir and os.path.isfile(
                    os.path.join(state.wrapper_source_dir, "main.py")
                ):
                    result = self._repair_current_source_checkout_launchers()
                    removed = len(result.removed_entries)
                    msg = (
                        "WaveLinux rebuilt the source-checkout desktop launcher.\n\n"
                        f"Source: {result.source_dir}\n"
                        f"Launcher: {result.wrapper_path}\n"
                        f"Desktop file: {result.desktop_path}\n"
                        f"Removed stale launchers: {removed}"
                    )
                else:
                    raise RuntimeError(
                        "The installed source launcher points at a missing checkout. Run WaveLinux from the desired checkout and use Reinstall This Source Checkout, or install a verified AppImage."
                    )
            elif state.wrapper_mode == "bundle":
                bundle_exec = getattr(state, "wrapper_bundle_exec", None)
                if bundle_exec and os.path.isfile(bundle_exec) and os.access(bundle_exec, os.X_OK):
                    result = self._repair_bundle_launchers(bundle_exec)
                    removed = len(result.removed_entries)
                    msg = (
                        "WaveLinux rebuilt the local bundled-build desktop launcher.\n\n"
                        f"Binary: {result.bundle_path}\n"
                        f"Launcher: {result.wrapper_path}\n"
                        f"Desktop file: {result.desktop_path}\n"
                        f"Removed stale launchers: {removed}"
                    )
                else:
                    raise RuntimeError(
                        "The installed bundled-build launcher points at a missing binary. Run WaveLinux from the desired local build and use Install This Local Build, or install a verified AppImage."
                    )
            elif state.installed_appimage_exists:
                result = self._repair_installed_appimage_launchers()
                removed = len(result.removed_entries)
                msg = (
                    "WaveLinux repaired the canonical desktop launcher files.\n\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}\n"
                    f"Removed stale launchers: {removed}"
                )
            elif self._is_running_in_appimage():
                result = self._install_current_appimage()
                msg = (
                    "WaveLinux installed this AppImage and rebuilt its desktop launcher.\n\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}"
                )
            else:
                raise RuntimeError(
                    "No installed AppImage was found to repair. Run WaveLinux from an AppImage and use Install This AppImage first."
                )
        except Exception as exc:
            QMessageBox.warning(
                self.window.settings_dialog,
                "Launcher repair failed",
                str(exc),
            )
            return
        self.window._refresh_system_tab()
        self.refresh_update_tab()
        QMessageBox.information(
            self.window.settings_dialog,
            "Desktop launchers repaired",
            msg,
        )

    def restart_app(self):
        self.restart_with_command(self._launch_command())

    def restart_with_command(self, command):
        self.window.save_config()
        self.window._shutting_down = True
        self.window._clear_runtime_pid()
        self.window.runtime.cleanup_sync()
        self.window.runtime.shutdown()
        os.execv(command[0], command + sys.argv[1:])

    def check_for_updates_bg(self):
        prev = getattr(self.window, "_bg_updater", None)
        if prev is not None:
            prev.cancel()

        self.window._bg_updater = self._update_checker_cls()
        self.window._bg_updater.check()
        timer = getattr(self.window, "_bg_poll_timer", None)
        if timer is None:
            timer = QTimer(self.window)
            timer.setInterval(500)
            timer.timeout.connect(self.poll_bg_updater)
            self.window._bg_poll_timer = timer
        else:
            timer.stop()
        timer.start()

    def poll_bg_updater(self):
        updater = getattr(self.window, "_bg_updater", None)
        if updater is None:
            return
        item = updater.poll()
        if item is None:
            return
        self.window._bg_poll_timer.stop()
        if item[0] == "result":
            info = item[1]
            if not isinstance(info, self._verified_release_info_cls):
                return
            tag = str(info.version or "").strip()
            mode, _description, guidance = self.runtime_mode_detail()
            self.window._pending_verified_release = info
            self.window._pending_update_url = info.release_url or self._release_page_url()
            self.window._pending_update_asset_url = info.asset_url or ""
            self.window._pending_update_asset_name = info.asset_name or ""
            self.window._last_update_check_at = time.time()
            self.window._last_update_issue = None
            if self._parse_version(tag) > self._parse_version(self._window_app_version()):
                self.window._pending_update_tag = tag
                self.window._show_notification(
                    "WaveLinux Update Available",
                    (
                        f"Version {tag} is available. Open Settings -> Updates to install it."
                        if self.window._pending_update_asset_url and mode.allows_self_update
                        else (
                            f"Version {tag} is available. {guidance}"
                            if self.window._pending_update_asset_url
                            else f"Version {tag} is available. Open Settings -> Updates for details."
                        )
                    ),
                )
        elif item[0] == "error":
            self.window._last_update_issue = dict(item[1] or {})
            self.window._pending_update_url = str(
                self.window._last_update_issue.get("release_url") or self._release_page_url()
            )
            self.window._last_update_attempt_result = (
                "Background update check failed: "
                f"{self.window._last_update_issue.get('message') or 'unknown error'}"
            )
