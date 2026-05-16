"""Updates tab control, install-state rendering, and launcher actions."""

from __future__ import annotations

from distribution import (
    install_current_bundle,
    install_current_appimage,
    install_current_source_checkout,
    install_state,
    installed_appimage_backup_path,
    is_running_in_appimage,
    launch_command,
    repair_bundle_launchers,
    repair_current_bundle_launchers,
    repair_current_source_checkout_launchers,
    repair_installed_appimage_launchers,
    resource_path,
    runtime_mode,
)
from updates import (
    AppImageUpdateInstaller,
    UpdateChecker,
    UpdateError,
    UpdateRollbackResult,
    VerifiedReleaseInfo,
    release_page_url,
    restore_previous_install,
)
from .updates_actions import UpdatesActionsMixin
from .updates_rendering import UpdatesRenderingMixin
from .updates_runtime import UpdatesRuntimeMixin


class UpdatesTabController(
    UpdatesActionsMixin,
    UpdatesRenderingMixin,
    UpdatesRuntimeMixin,
):
    def __init__(
        self,
        window,
        *,
        parse_version,
        update_checker_cls=UpdateChecker,
        appimage_update_installer_cls=AppImageUpdateInstaller,
        verified_release_info_cls=VerifiedReleaseInfo,
        update_error_cls=UpdateError,
        update_rollback_result_cls=UpdateRollbackResult,
        release_page_url_fn=release_page_url,
        restore_previous_install_fn=restore_previous_install,
        install_current_appimage_fn=install_current_appimage,
        install_current_bundle_fn=install_current_bundle,
        install_current_source_checkout_fn=install_current_source_checkout,
        repair_bundle_launchers_fn=repair_bundle_launchers,
        repair_current_bundle_launchers_fn=repair_current_bundle_launchers,
        repair_current_source_checkout_launchers_fn=repair_current_source_checkout_launchers,
        repair_installed_appimage_launchers_fn=repair_installed_appimage_launchers,
        launch_command_fn=launch_command,
        runtime_mode_fn=runtime_mode,
        is_running_in_appimage_fn=is_running_in_appimage,
        installed_appimage_backup_path_fn=installed_appimage_backup_path,
        install_state_loader=install_state,
        resource_path_fn=resource_path,
    ):
        self.window = window
        self._parse_version = parse_version
        self._update_checker_cls = update_checker_cls
        self._appimage_update_installer_cls = appimage_update_installer_cls
        self._verified_release_info_cls = verified_release_info_cls
        self._update_error_cls = update_error_cls
        self._update_rollback_result_cls = update_rollback_result_cls
        self._release_page_url = release_page_url_fn
        self._restore_previous_install = restore_previous_install_fn
        self._install_current_appimage = install_current_appimage_fn
        self._install_current_bundle = install_current_bundle_fn
        self._install_current_source_checkout = install_current_source_checkout_fn
        self._repair_bundle_launchers = repair_bundle_launchers_fn
        self._repair_current_bundle_launchers = repair_current_bundle_launchers_fn
        self._repair_current_source_checkout_launchers = repair_current_source_checkout_launchers_fn
        self._repair_installed_appimage_launchers = repair_installed_appimage_launchers_fn
        self._launch_command = launch_command_fn
        self._runtime_mode = runtime_mode_fn
        self._is_running_in_appimage = is_running_in_appimage_fn
        self._installed_appimage_backup_path = installed_appimage_backup_path_fn
        self._install_state_loader = install_state_loader
        self._resource_path = resource_path_fn

    def _window_app_version(self):
        return str(self.window.__dict__.get("_app_version", "0") or "0")
