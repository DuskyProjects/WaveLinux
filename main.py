#!/usr/bin/env python3
"""WaveLinux — Native PipeWire Audio Mixer for KDE/Linux"""

import json
import logging
import shutil
import subprocess
import sys
import os
import re
import time
import threading
import queue

from PyQt6.QtWidgets import QApplication, QInputDialog, QMainWindow, QMessageBox
from PyQt6.QtCore import QTimer, QLockFile
from PyQt6.QtGui import QDesktopServices, QFont, QIcon

from app_core import (
    AppContext,
    ConfigChanged,
    EventBus,
    HealthBus,
    ModuleManager,
    load_feature_flags,
)
from audio_runtime import AudioRuntimeAdapter, AudioRuntimeController
from controllers import (
    AudioEventController,
    AppIdentityController,
    BluetoothController,
    ChannelController,
    ConfigController,
    DevicePolicyController,
    LifecycleController,
    ModuleRuntimeController,
    RecoveryController,
    RuntimeViewController,
    StartupController,
    StressControlController,
)
from distribution import (
    APP_DESKTOP_ID,
    DESKTOP_FILENAME,
    current_runtime_path,
    desktop_exec_command,
    installed_appimage_backup_path,
    install_current_bundle,
    install_current_appimage,
    install_current_source_checkout,
    install_state,
    is_running_in_appimage,
    launch_command,
    repair_bundle_launchers,
    repair_current_bundle_launchers,
    repair_current_source_checkout_launchers,
    repair_installed_appimage_launchers,
    resource_path,
    runtime_mode,
)
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
from pipewire_engine import PipeWireEngine
from state import WaveLinuxWindowState
from ui.dialogs.fx_dialog import FXSelectionDialog
from ui.mixer import ChannelStrip, MeterWorker, MixerStripMetrics
from ui.routing import AppRoutingRow
from ui.settings import (
    AdvancedTabController,
    DialogController,
    HealthTabController,
    ScenesTabController,
    UpdatesTabController,
)
from ui.window_device_mixin import WindowDeviceMixin
from ui.window_shell_mixin import WindowShellMixin
from updates import (
    AppImageUpdateInstaller,
    UpdateError,
    UpdateChecker,
    UpdateRollbackResult,
    VerifiedReleaseInfo,
    release_page_url,
    restore_previous_install,
)
from wavelinux_theme import STYLESHEET

APP_VERSION = "3.1"
_RUNTIME_DEPS = ["pactl", "pw-dump", "wpctl", "parec", "pipewire", "pw-cli"]
_RUNTIME_HEALTH_MESSAGES = {
    "submix_monitor_missing": "Monitor route is missing.",
    "submix_monitor_dead": "Monitor route exists but is not live.",
    "submix_stream_missing": "Stream route is missing.",
    "submix_stream_dead": "Stream route exists but is not live.",
    "desired_fx_missing": "The requested FX source is missing.",
    "fx_effects_not_visible": "The FX source exists, but its effects are not visible yet.",
    "fx_effects_mismatch": "The active FX chain does not match the requested effects.",
    "fx_params_mismatch": "The active FX parameters do not match the saved settings.",
    "fx_source_not_present": "The FX source is not present in PipeWire.",
    "duplicate_fx_source": "Multiple channels are sharing the same FX source.",
    "default_source_expected_fx_missing": "The selected mic's FX source is missing.",
    "default_source_mismatch": "The selected mic is not the current default source.",
    "fx_status_not_active": "The requested FX chain is not active yet.",
    "fx_proxy_source_missing": "The stable FX source is not visible in PipeWire.",
    "fx_proxy_sink_dead": "The stable FX sink module is missing or dead.",
    "fx_proxy_source_dead": "The stable FX source module is missing or dead.",
    "fx_process_dead": "The FX process is missing or no longer running.",
    "fx_loopback_dead": "One or more FX loopbacks are missing or dead.",
    "fx_active_chain_source_missing": "The live FX chain source is missing.",
    "fx_passthrough_feed_dead": "The FX proxy passthrough feed is missing or dead.",
}


def stress_control_enabled():
    return str(os.environ.get("WAVELINUX_STRESS_CONTROL", "")).strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }

def _parse_version(v):
    """Return a comparable tuple from a semver string like '1.2.3' or 'v1.2.3'."""
    v = v.lstrip('v').strip()
    try:
        return tuple(int(x) for x in v.split('.'))
    except ValueError:
        return (0,)


def _probe_command(cmd, *, timeout=3, runner=subprocess.run):
    try:
        result = runner(
            list(cmd),
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except FileNotFoundError:
        return {"ok": False, "error": "missing"}
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "timeout"}
    except Exception as exc:
        return {"ok": False, "error": str(exc)}
    return {
        "ok": result.returncode == 0,
        "returncode": int(result.returncode),
        "stderr": (result.stderr or "").strip()[:200],
    }


def startup_preflight_report(*, which=shutil.which, runner=subprocess.run,
                             config_dir=None):
    config_root = os.path.expanduser("~/.config/wavelinux") if config_dir is None else os.path.abspath(config_dir)
    deps = {cmd: bool(which(cmd)) for cmd in _RUNTIME_DEPS}
    missing = [cmd for cmd, present in deps.items() if not present]
    checks = {}
    issue_details = []
    if deps.get("pactl"):
        checks["pactl_info"] = _probe_command(["pactl", "info"], runner=runner)
    if deps.get("wpctl"):
        checks["wpctl_status"] = _probe_command(["wpctl", "status"], runner=runner)

    config_ok = True
    config_error = ""
    try:
        os.makedirs(config_root, exist_ok=True)
        probe_path = os.path.join(config_root, ".wavelinux-write-check")
        with open(probe_path, "w", encoding="utf-8") as handle:
            handle.write("ok")
        os.remove(probe_path)
    except OSError as exc:
        config_ok = False
        config_error = str(exc)

    issues = []
    if missing:
        detail = "Missing required audio/runtime tools: " + ", ".join(missing)
        issues.append(detail)
        issue_details.append({
            "code": "runtime.missing_tool",
            "detail": detail,
            "context": {"tools": list(missing)},
        })
    pactl_check = checks.get("pactl_info")
    if pactl_check and not pactl_check.get("ok"):
        detail = "Could not query the PulseAudio compatibility server via `pactl info`."
        issues.append(detail)
        issue_details.append({
            "code": "runtime.pipewire_unreachable",
            "detail": detail,
            "context": {"check": dict(pactl_check)},
        })
    wpctl_check = checks.get("wpctl_status")
    if wpctl_check and not wpctl_check.get("ok"):
        detail = "Could not query WirePlumber via `wpctl status`."
        issues.append(detail)
        issue_details.append({
            "code": "runtime.wireplumber_unreachable",
            "detail": detail,
            "context": {"check": dict(wpctl_check)},
        })
    if not config_ok:
        detail = f"Could not prepare config directory {config_root}: {config_error}"
        issues.append(detail)
        issue_details.append({
            "code": "runtime.config_unwritable",
            "detail": detail,
            "context": {"config_dir": config_root, "error": config_error},
        })

    return {
        "deps": deps,
        "missing": missing,
        "checks": checks,
        "config_dir": config_root,
        "config_ok": config_ok,
        "config_error": config_error,
        "issues": issues,
        "issue_details": issue_details,
    }


def build_self_test_report():
    install = install_state()
    mode = runtime_mode()
    preflight = startup_preflight_report()
    resources = {
        "icon.png": os.path.exists(resource_path("icon.png")),
        "tray_icon.png": os.path.exists(resource_path("tray_icon.png")),
    }
    running_binary = (
        install.running_appimage_path
        or current_runtime_path()
    )
    ok = all(resources.values()) and bool(running_binary)
    return {
        "ok": ok,
        "version": APP_VERSION,
        "frozen": bool(getattr(sys, "frozen", False)),
        "runtime_mode": mode.kind,
        "running_binary": running_binary,
        "running_in_appimage": bool(install.running_appimage_path),
        "resources": resources,
        "install_state": {
            "installed_appimage_exists": install.installed_appimage_exists,
            "desktop_exists": install.desktop_exists,
            "wrapper_exists": install.wrapper_exists,
            "warnings": list(install.warnings),
        },
        "preflight": {
            "missing": list(preflight["missing"]),
            "issues": list(preflight["issues"]),
        },
    }


def _handle_cli_args(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    if "--version" in args or "-V" in args:
        print(APP_VERSION)
        return 0
    if "--self-test" in args:
        report = build_self_test_report()
        print(json.dumps(report, indent=2, sort_keys=True))
        return 0 if report.get("ok") else 1
    return None



# ── Main Window ────────────────────────────────────────────────────
class WaveLinuxWindow(WindowDeviceMixin, WindowShellMixin, QMainWindow):
    _AUTO_RECOVERY_DELAYS_MS = (1500, 5000)

    def __init__(self):
        super().__init__()
        self.setWindowTitle("WaveLinux")
        self._app_version = APP_VERSION
        
        self.resize(1200, 720)
        
        # Set app icon and tray icon.
        app_icon_path = resource_path("icon.png")
        tray_icon_path = resource_path("tray_icon.png")

        if os.path.exists(app_icon_path):
            app_icon = QIcon(app_icon_path)
            self.setWindowIcon(app_icon)
            QApplication.instance().setWindowIcon(app_icon)
        else:
            app_icon = QIcon.fromTheme("audio-card")

        if os.path.exists(tray_icon_path):
            self.tray_icon_obj = QIcon(tray_icon_path)
        else:
            self.tray_icon_obj = app_icon
        
        self.engine = AudioRuntimeAdapter()
        self.runtime = AudioRuntimeController(self.engine, self)
        self.runtime.view_state_changed.connect(self._on_runtime_view_state)
        self.runtime.fx_status_changed.connect(self._on_runtime_fx_status)
        self._runtime_view_state = None
        self._shutting_down = False
        self._event_bus = EventBus()
        self._module_health_bus = HealthBus()
        self._feature_flags = load_feature_flags()
        self.module_manager = None
        self._module_feature_enabled = {}
        self.state = WaveLinuxWindowState()
        # ── State ──
        self.channel_widgets = {}   # node_id -> ChannelStrip
        self.app_widgets = {}       # app_id -> AppRoutingRow
        self.submix_state = {}      # "node_id_MixName" -> {'vol': 1.0, 'mute': False}
        self.app_routing = {}       # app_id -> sink_name (persistent)
        self.app_volumes = {}       # app_id -> normalized volume (persistent)
        self.app_last_seen = {}     # app_id -> epoch seconds (for stale prune)
        self.app_display_names = {} # app_id -> last known display label
        self.app_identity_overrides = {}  # source app_id -> canonical target app_id
        self.app_label_overrides = {}     # canonical app_id -> user-facing label
        self.app_prune_days = 14    # forget routing entries not seen in this many days
        # ✕'d apps. Consulted in `_refresh` BEFORE the row is built so
        # the ✕ button sticks across re-syntheses.
        self.forgotten_apps = set()  # {app_id}
        self.virtual_channels = []   # list of display names
        self.scenes = {}             # scene name -> saved snapshot
        self._onboarding_completed = True
        self._selected_setup_template = ""
        self._show_first_run_setup = False
        # Single-mic mode: one mic strip at a time, picked from the master
        # combo. None → resolved to `pactl get-default-source` on first refresh.
        self.selected_mic = None
        self._mic_selection_initialized = False
        # State below is keyed by PipeWire node.name (stable across PW
        # restarts). pw_id is only used at the engine boundary.
        self.hidden_nodes = set()
        self.show_hidden = False
        self.effect_params = {}        # node.name -> {effect_id: {key: val}}
        self.active_effects = {}       # node.name -> [effect_id, ...]
        self.channel_order = []        # [node.name, ...] — persistent UI order
        self.meters = {}               # pw_id -> MeterWorker
        self._known_node_names = set() # for hot-plug detection
        self._visible_strip_ids = ()
        self._app_widget_order = ()
        self._monitor_sink_fp = ()
        self._desired_mix_hw = {"Monitor": None, "Stream": None}
        self._desired_mix_volumes = {"Monitor": 1.0, "Stream": 1.0}
        self._stress_control_server = None
        self._preferred_monitor_hw_id = ""
        self._preferred_monitor_hw_name = ""
        self._restorable_monitor_hw_id = ""
        self._restorable_monitor_hw_name = ""
        self._active_monitor_fallback = False
        self._last_good_monitor_hw_id = ""
        self._last_good_monitor_hw_name = ""
        self._preferred_selected_mic_id = ""
        self._preferred_selected_mic_name = ""
        self._restorable_selected_mic_id = ""
        self._restorable_selected_mic_name = ""
        self._active_mic_fallback = False
        self._last_good_selected_mic_id = ""
        self._last_good_selected_mic_name = ""
        self._auto_recovery_state = {}
        self.config_path = os.path.expanduser("~/.config/wavelinux/config.json")
        os.makedirs(os.path.dirname(self.config_path), exist_ok=True)
        self._runtime_pid_path = os.path.expanduser("~/.config/wavelinux/runtime.pid")
        self._set_engine_identity_overrides()
        
        # Backstop poll. `pactl subscribe` drives most refreshes; this
        # only fires when an event was missed.
        self.refresh_timer = QTimer(self)
        self.refresh_timer.timeout.connect(
            lambda: self._request_runtime_refresh("periodic-refresh")
        )

        # Coalesce rapid save requests (slider drags).
        self._save_timer = QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.timeout.connect(self.save_config)

        # Coalesce rapid refresh requests — pactl-subscribe storms can
        # fire 5+ events per operation.
        self._event_refresh_timer = QTimer(self)
        self._event_refresh_timer.setSingleShot(True)
        self._event_refresh_timer.setInterval(150)
        self._event_refresh_timer.timeout.connect(
            lambda: self._request_runtime_refresh("pactl-event")
        )
        self._event_proc_restart_timer = QTimer(self)
        self._event_proc_restart_timer.setSingleShot(True)
        self._event_proc_restart_timer.setInterval(1000)
        self._event_proc_restart_timer.timeout.connect(
            self._restart_event_subscriber_if_needed
        )
        self._bluetooth_profile_reassert_retries = 0
        self._pending_bluetooth_reconnect_macs = set()
        self._device_settle_refresh_timer = QTimer(self)
        self._device_settle_refresh_timer.setSingleShot(True)
        self._device_settle_refresh_timer.setInterval(700)
        self._device_settle_refresh_timer.timeout.connect(
            lambda: self._request_runtime_refresh("device-settle")
        )
        self._hotplug_refresh_timer = self._device_settle_refresh_timer
        self._bluetooth_refresh_timer = QTimer(self)
        self._bluetooth_refresh_timer.setSingleShot(True)
        self._bluetooth_refresh_timer.setInterval(1800)
        self._bluetooth_refresh_timer.timeout.connect(
            self._handle_bluetooth_settle_refresh
        )
        self._monitor_route_reassert_timer = QTimer(self)
        self._monitor_route_reassert_timer.setSingleShot(True)
        self._monitor_route_reassert_timer.setInterval(350)
        self._monitor_route_reassert_timer.timeout.connect(
            lambda: self._reassert_persistent_state_after_monitor_switch("monitor-route-reassert")
        )
        self._monitor_route_bluetooth_reassert_timer = QTimer(self)
        self._monitor_route_bluetooth_reassert_timer.setSingleShot(True)
        self._monitor_route_bluetooth_reassert_timer.setInterval(1800)
        self._monitor_route_bluetooth_reassert_timer.timeout.connect(
            lambda: self._reassert_persistent_state_after_monitor_switch("monitor-route-reassert-bluetooth")
        )
        self._runtime_view_refresh_timer = QTimer(self)
        self._runtime_view_refresh_timer.setSingleShot(True)
        self._runtime_view_refresh_timer.setInterval(40)
        self._runtime_view_refresh_timer.timeout.connect(
            self._apply_scheduled_runtime_view_refresh
        )
        self._settings_tab_refresh_timer = QTimer(self)
        self._settings_tab_refresh_timer.setSingleShot(True)
        self._settings_tab_refresh_timer.setInterval(60)
        self._settings_tab_refresh_timer.timeout.connect(
            self._apply_scheduled_settings_tab_refresh
        )
        self._settings_tab_last_refresh_at = {}
        self._pending_settings_tab_refresh = ""
        self._last_selected_mic_change_at = 0.0
        self._install_state_cache = None
        self._install_state_cache_at = 0.0
        self._install_state_refresh_inflight = False
        self._install_state_refresh_tabs = set()
        self._install_state_refresh_queue = queue.SimpleQueue()
        self._install_state_refresh_poll_timer = QTimer(self)
        self._install_state_refresh_poll_timer.setInterval(40)
        self._install_state_refresh_poll_timer.timeout.connect(
            self._poll_install_state_refresh
        )
        self._mic_cutover_refresh_timer = QTimer(self)
        self._mic_cutover_refresh_timer.setSingleShot(True)
        self._mic_cutover_refresh_timer.setInterval(250)
        self._mic_cutover_refresh_timer.timeout.connect(
            lambda: self._request_runtime_refresh("mic-cutover-followup")
        )
        self._pactl_event_suppressed_until = 0.0
        self._quit_in_progress = False
        self._runtime_stopped = False
        self._scenes_controller = ScenesTabController(self)

        self._setup_ui()
        self._setup_feature_modules()
        self._run_startup_preflight()
        self.load_config()
        self._start_feature_modules()
        self._prime_bluetooth_playback_profile()
        self._wait_for_startup_audio_ready()
        self._refresh()
        QTimer.singleShot(300, lambda: self._request_runtime_refresh("startup-followup-1"))
        QTimer.singleShot(1200, lambda: self._request_runtime_refresh("startup-followup-2"))
        if self._show_first_run_setup:
            QTimer.singleShot(400, self._open_quick_start_setup)
        # 5s backstop interval — subscribe-driven refreshes carry the
        # real-time signal; this just catches missed events.
        self.refresh_timer.start(5000)
        self._start_event_subscriber()
        self._pending_update_tag = None
        self._pending_verified_release = None
        self._pending_update_url = release_page_url()
        self._pending_update_asset_url = ""
        self._pending_update_asset_name = ""
        self._last_update_check_at = None
        self._last_update_issue = None
        self._last_update_attempt_result = "No update activity yet."
        self._recent_recovery_status = {}
        self._sync_window_state()
        # Silent update check 30 s after startup so it never blocks startup.
        QTimer.singleShot(30_000, self._check_for_updates_bg)
        # Prime install-state cache off the UI path so heavy settings tabs can
        # paint immediately from cached data later.
        QTimer.singleShot(
            1500,
            lambda: self._schedule_install_state_refresh(
                target_tabs=("Health", "Updates"),
                force=True,
            ),
        )

    def _sync_window_state(self):
        attrs = self.__dict__
        state = attrs.get("state")
        if state is None:
            state = WaveLinuxWindowState()
            attrs["state"] = state

        state.content.virtual_channels = attrs.get("virtual_channels", [])
        state.content.hidden_nodes = attrs.get("hidden_nodes", set())
        state.content.scenes = attrs.get("scenes", {})
        state.content.channel_order = attrs.get("channel_order", [])
        state.content.submix_state = attrs.get("submix_state", {})
        state.content.active_effects = attrs.get("active_effects", {})
        state.content.effect_params = attrs.get("effect_params", {})

        state.device_policy.desired_mix_hw = attrs.get("_desired_mix_hw", {})
        state.device_policy.desired_mix_volumes = attrs.get("_desired_mix_volumes", {})
        state.device_policy.preferred_monitor_hw_id = attrs.get("_preferred_monitor_hw_id", "")
        state.device_policy.preferred_monitor_hw_name = attrs.get("_preferred_monitor_hw_name", "")
        state.device_policy.restorable_monitor_hw_id = attrs.get("_restorable_monitor_hw_id", "")
        state.device_policy.restorable_monitor_hw_name = attrs.get("_restorable_monitor_hw_name", "")
        state.device_policy.last_good_monitor_hw_id = attrs.get("_last_good_monitor_hw_id", "")
        state.device_policy.last_good_monitor_hw_name = attrs.get("_last_good_monitor_hw_name", "")
        state.device_policy.preferred_selected_mic_id = attrs.get("_preferred_selected_mic_id", "")
        state.device_policy.preferred_selected_mic_name = attrs.get("_preferred_selected_mic_name", "")
        state.device_policy.restorable_selected_mic_id = attrs.get("_restorable_selected_mic_id", "")
        state.device_policy.restorable_selected_mic_name = attrs.get("_restorable_selected_mic_name", "")
        state.device_policy.last_good_selected_mic_id = attrs.get("_last_good_selected_mic_id", "")
        state.device_policy.last_good_selected_mic_name = attrs.get("_last_good_selected_mic_name", "")
        state.device_policy.selected_mic = attrs.get("selected_mic")
        state.device_policy.mic_selection_initialized = bool(
            attrs.get("_mic_selection_initialized", False)
        )
        state.device_policy.active_monitor_fallback = bool(
            attrs.get("_active_monitor_fallback", False)
        )
        state.device_policy.active_mic_fallback = bool(attrs.get("_active_mic_fallback", False))

        state.recovery.auto_recovery_state = attrs.get("_auto_recovery_state", {})
        state.recovery.recent_recovery_status = attrs.get("_recent_recovery_status", {})
        state.recovery.runtime_stopped = bool(attrs.get("_runtime_stopped", False))
        state.recovery.last_selected_mic_change_at = float(
            attrs.get("_last_selected_mic_change_at", 0.0) or 0.0
        )
        state.recovery.pactl_event_suppressed_until = float(
            attrs.get("_pactl_event_suppressed_until", 0.0) or 0.0
        )

        state.updates.pending_update_tag = attrs.get("_pending_update_tag")
        state.updates.pending_verified_release = attrs.get("_pending_verified_release")
        state.updates.pending_update_url = attrs.get("_pending_update_url", "")
        state.updates.pending_update_asset_url = attrs.get("_pending_update_asset_url", "")
        state.updates.pending_update_asset_name = attrs.get("_pending_update_asset_name", "")
        state.updates.last_update_check_at = attrs.get("_last_update_check_at")
        state.updates.last_update_error = attrs.get("_last_update_issue")
        state.updates.last_update_attempt_result = attrs.get(
            "_last_update_attempt_result",
            "No update activity yet.",
        )
        state.updates.install_state_cache = attrs.get("_install_state_cache")
        state.updates.install_state_cache_at = float(attrs.get("_install_state_cache_at", 0.0) or 0.0)
        state.updates.install_state_refresh_inflight = bool(
            attrs.get("_install_state_refresh_inflight", False)
        )
        state.updates.install_state_refresh_tabs = attrs.get("_install_state_refresh_tabs", set())

        state.app_identity.app_routing = attrs.get("app_routing", {})
        state.app_identity.app_volumes = attrs.get("app_volumes", {})
        state.app_identity.app_last_seen = attrs.get("app_last_seen", {})
        state.app_identity.app_display_names = attrs.get("app_display_names", {})
        state.app_identity.app_identity_overrides = attrs.get("app_identity_overrides", {})
        state.app_identity.app_label_overrides = attrs.get("app_label_overrides", {})
        state.app_identity.forgotten_apps = attrs.get("forgotten_apps", set())
        state.app_identity.prune_days = int(attrs.get("app_prune_days", 14) or 14)

        state.lifecycle.quit_in_progress = bool(attrs.get("_quit_in_progress", False))
        state.lifecycle.shutting_down = bool(attrs.get("_shutting_down", False))
        state.lifecycle.runtime_pid_path = attrs.get("_runtime_pid_path", "")
        state.lifecycle.onboarding_completed = bool(attrs.get("_onboarding_completed", True))
        state.lifecycle.selected_setup_template = attrs.get("_selected_setup_template", "")
        state.lifecycle.show_first_run_setup = bool(attrs.get("_show_first_run_setup", False))
        state.lifecycle.pending_bluetooth_reconnect_macs = attrs.get(
            "_pending_bluetooth_reconnect_macs",
            set(),
        )
        state.lifecycle.bluetooth_profile_reassert_retries = int(
            attrs.get("_bluetooth_profile_reassert_retries", 0) or 0
        )
        return state

    def _on_runtime_view_state(self, view_state):
        self._runtime_view_controller().on_runtime_view_state(view_state)

    def _request_runtime_refresh(self, reason=""):
        self._runtime_view_controller().request_runtime_refresh(reason)

    def _on_runtime_fx_status(self, status):
        self._recovery_controller().on_runtime_fx_status(status)

    def _settings_dialog_visible(self):
        dialog = self.__dict__.get("settings_dialog")
        return bool(dialog is not None and dialog.isVisible())

    def _suppress_pactl_events_for(self, duration_s):
        duration_s = max(0.0, float(duration_s or 0.0))
        if duration_s <= 0.0:
            return
        self._pactl_event_suppressed_until = max(
            float(self.__dict__.get("_pactl_event_suppressed_until", 0.0) or 0.0),
            time.monotonic() + duration_s,
        )

    def _pactl_events_suppressed(self):
        return time.monotonic() < float(
            self.__dict__.get("_pactl_event_suppressed_until", 0.0) or 0.0
        )

    def _schedule_runtime_view_refresh(self):
        self._runtime_view_controller().schedule_runtime_view_refresh()

    def _runtime_view_refresh_delay_ms(self, *, health=None, pending_ops=None):
        return self._runtime_view_controller().runtime_view_refresh_delay_ms(
            health=health,
            pending_ops=pending_ops,
        )

    def _runtime_view_has_pending_ops(self, view=None):
        return self._runtime_view_controller().runtime_view_has_pending_ops(view=view)

    def _selected_mic_change_settling(self, *, window_s=3.0):
        return self._runtime_view_controller().selected_mic_change_settling(window_s=window_s)

    def _selected_mic_needs_followup_refresh(self, health, pending_ops):
        return self._runtime_view_controller().selected_mic_needs_followup_refresh(
            health,
            pending_ops,
        )

    def _selected_mic_transition_in_progress(self, health, pending_ops):
        return self._runtime_view_controller().selected_mic_transition_in_progress(
            health,
            pending_ops,
        )

    def _startup_graph_health_blockers(self, view=None):
        return self._runtime_view_controller().startup_graph_health_blockers(view=view)

    def _startup_audio_ready(self, view=None):
        return self._runtime_view_controller().startup_audio_ready(view=view)

    def _startup_audio_ready_settled(self, view=None, *, settle_s=None):
        return self._runtime_view_controller().startup_audio_ready_settled(
            view=view,
            settle_s=settle_s,
        )

    def _wait_for_startup_audio_ready(self, timeout_s=8.0):
        return self._runtime_view_controller().wait_for_startup_audio_ready(timeout_s=timeout_s)

    def _apply_scheduled_runtime_view_refresh(self):
        self._runtime_view_controller().apply_scheduled_runtime_view_refresh()

    def _apply_lightweight_runtime_view_refresh(self):
        self._runtime_view_controller().apply_lightweight_runtime_view_refresh()

    def _make_auto_recovery_timer(self):
        timer = QTimer(self)
        timer.setSingleShot(True)
        return timer

    def _cancel_auto_recovery_timer(self, node_name):
        self._recovery_controller().cancel_auto_recovery_timer(node_name)

    def _clear_auto_recovery_state(self, node_name):
        self._recovery_controller().clear_auto_recovery_state(node_name)

    def _ensure_auto_recovery_entry(self, node_name, generation):
        return self._recovery_controller().ensure_auto_recovery_entry(node_name, generation)

    def _channel_label(self, node_name):
        view = self._runtime_view_state
        if view is not None:
            for channel in list(getattr(view, "mic_inputs", []) or []) + list(getattr(view, "virtual_channels", []) or []):
                if getattr(channel, "name", "") == node_name:
                    return getattr(channel, "label", "") or node_name
        return PipeWireEngine.friendly_name(node_name) or node_name

    @staticmethod
    def _format_runtime_health_code(code):
        code = (code or "").strip()
        if not code:
            return ""
        if code in _RUNTIME_HEALTH_MESSAGES:
            return _RUNTIME_HEALTH_MESSAGES[code]
        return code.replace("_", " ").strip().capitalize() + "."

    @staticmethod
    def _fx_status_diagnostics_path(status):
        path = (getattr(status, "diagnostics_path", "") or "").strip()
        if path:
            return path
        message = (getattr(status, "message", "") or "").strip()
        match = re.search(r"diagnostics:\s*(\S+)", message)
        return match.group(1) if match else ""

    def format_fx_status_message(self, status):
        message = (getattr(status, "message", "") or "").strip()
        if not message:
            return ""
        path = self._fx_status_diagnostics_path(status)
        if path:
            message = message.replace(f"diagnostics: {path}", "diagnostics saved")
        return re.sub(r"\s+", " ", message).strip()

    def fx_recovery_status_message(self, node_name):
        return self._recovery_controller().fx_recovery_status_message(node_name)

    def recovery_status_for_channel(self, node_name):
        return self._recovery_controller().recovery_status_for_channel(node_name)

    def channel_runtime_issue(self, node_name):
        return self._recovery_controller().channel_runtime_issue(node_name)

    def _find_channel_strip(self, node_name):
        for strip in getattr(self, "channel_widgets", {}).values():
            if getattr(strip, "node_name", "") == node_name:
                return strip
        return None

    def _refresh_channel_runtime_status(self, node_name):
        strip = self._find_channel_strip(node_name)
        if strip is None:
            return
        issue = self.channel_runtime_issue(node_name)
        strip.set_runtime_issue(issue["degraded"], issue["tooltip"])
        strip.setToolTip(issue["tooltip"] if issue["degraded"] else "")

    def _schedule_auto_recovery(self, status):
        self._recovery_controller().schedule_auto_recovery(status)

    def _run_auto_recovery(self, node_name, generation):
        self._recovery_controller().run_auto_recovery(node_name, generation)

    def _run_startup_preflight(self):
        self._startup_controller().run_startup_preflight()

    @staticmethod
    def _pid_is_alive(pid):
        return StartupController.pid_is_alive(pid)

    def _recover_unclean_runtime_state(self):
        self._startup_controller().recover_unclean_runtime_state()

    def _write_runtime_pid(self):
        self._startup_controller().write_runtime_pid()

    def _clear_runtime_pid(self):
        self._startup_controller().clear_runtime_pid()

    def _setup_stress_control(self):
        self._stress_control_controller().setup_stress_control()

    def _setup_feature_modules(self):
        self._module_runtime_controller().setup_feature_modules()

    def _start_feature_modules(self):
        self._module_runtime_controller().start_feature_modules()

    def _stop_stress_control(self):
        self._stress_control_controller().stop_stress_control()

    def _module_enabled(self, module_id):
        return self._module_runtime_controller().module_enabled(module_id)

    def _set_feature_module_enabled(self, module_id, enabled, *, reason=""):
        self._module_runtime_controller().set_feature_module_enabled(
            module_id,
            enabled,
            reason=reason,
        )

    def _restart_metering_module(self):
        self._module_runtime_controller().restart_metering_module()

    def _disable_effects_module_runtime(self, *, reason=""):
        self._module_runtime_controller().disable_effects_module_runtime(reason=reason)

    def _enable_effects_module_runtime(self):
        self._module_runtime_controller().enable_effects_module_runtime()

    def _set_app_routing_controls_enabled(self, enabled):
        self._module_runtime_controller().set_app_routing_controls_enabled(enabled)

    def _set_update_controls_enabled(self, enabled):
        self._module_runtime_controller().set_update_controls_enabled(enabled)

    def _module_health_issues(self):
        return self._module_runtime_controller().module_health_issues()

    def _stress_runtime_summary(self):
        return self._stress_control_controller().stress_runtime_summary()

    def _stress_health_summary(self):
        return self._stress_control_controller().stress_health_summary()

    def _stress_list_modules(self):
        return self._stress_control_controller().stress_list_modules()

    def _stress_get_module_health(self, module_id):
        return self._stress_control_controller().stress_get_module_health(module_id)

    def _stress_disable_module(self, module_id, *, reason="stress-disable"):
        return self._stress_control_controller().stress_disable_module(
            module_id,
            reason=reason,
        )

    def _stress_enable_module(self, module_id):
        return self._stress_control_controller().stress_enable_module(module_id)

    def _stress_restart_module(self, module_id, *, reason="stress-restart"):
        return self._stress_control_controller().stress_restart_module(
            module_id,
            reason=reason,
        )

    def _stress_list_known_sinks(self):
        return self._stress_control_controller().stress_list_known_sinks()

    def _stress_list_known_sources(self):
        return self._stress_control_controller().stress_list_known_sources()

    def _stress_set_monitor_output(self, sink_name, *, persist=True, include_summary=False):
        return self._stress_control_controller().stress_set_monitor_output(
            sink_name,
            persist=persist,
            include_summary=include_summary,
        )

    def _stress_set_stream_output(self, sink_name, *, persist=True, include_summary=False):
        return self._stress_control_controller().stress_set_stream_output(
            sink_name,
            persist=persist,
            include_summary=include_summary,
        )

    def _stress_set_selected_mic(self, mic_name, *, persist=True, include_summary=False):
        return self._stress_control_controller().stress_set_selected_mic(
            mic_name,
            persist=persist,
            include_summary=include_summary,
        )

    def _stress_set_channel_fx(self, node_name, effects=None, params_map=None, *, persist=True, include_summary=False):
        return self._stress_control_controller().stress_set_channel_fx(
            node_name,
            effects=effects,
            params_map=params_map,
            persist=persist,
            include_summary=include_summary,
        )

    def _stress_open_settings_tab(self, tab_name):
        return self._stress_control_controller().stress_open_settings_tab(tab_name)

    def _stress_close_settings(self):
        return self._stress_control_controller().stress_close_settings()

    def _open_settings(self):
        self._dialog_controller().open_settings()

    def _active_settings_tab_name(self):
        return self._dialog_controller().active_settings_tab_name()

    def _refresh_settings_tab_by_name(self, tab_name):
        self._dialog_controller().refresh_settings_tab_by_name(tab_name)

    def _install_state_cache_is_stale(self, *, max_age_s=5.0):
        return self._dialog_controller().install_state_cache_is_stale(max_age_s=max_age_s)

    def _invalidate_install_state_cache(self):
        self._dialog_controller().invalidate_install_state_cache()

    def _cached_install_state(self, *, target_tabs=(), max_age_s=5.0, allow_async=True):
        return self._dialog_controller().cached_install_state(
            target_tabs=target_tabs,
            max_age_s=max_age_s,
            allow_async=allow_async,
        )

    def _schedule_install_state_refresh(self, *, target_tabs=(), force=False):
        self._dialog_controller().schedule_install_state_refresh(
            target_tabs=target_tabs,
            force=force,
        )

    def _load_install_state_refresh_worker(self):
        self._dialog_controller().load_install_state_refresh_worker()

    def _poll_install_state_refresh(self):
        self._dialog_controller().poll_install_state_refresh()

    def _apply_install_state_refresh(self):
        self._dialog_controller().apply_install_state_refresh()

    def _settings_tab_stale_seconds(self, tab_name):
        tab_name = str(tab_name or "").strip()
        return {
            "Hidden": 0.5,
            "Scenes": 0.5,
            "Health": 1.0,
            "Advanced": 1.0,
            "Updates": 5.0,
        }.get(tab_name, 1.0)

    def _settings_tab_refresh_is_stale(self, tab_name, *, force=False):
        if force:
            return True
        tab_name = str(tab_name or "").strip()
        if not tab_name:
            return False
        last_refresh = float((self.__dict__.get("_settings_tab_last_refresh_at", {}) or {}).get(tab_name, 0.0) or 0.0)
        if last_refresh <= 0.0:
            return True
        return (time.monotonic() - last_refresh) >= self._settings_tab_stale_seconds(tab_name)

    def _mark_settings_tab_refreshed(self, tab_name):
        self._dialog_controller().mark_settings_tab_refreshed(tab_name)

    def _mark_settings_tab_stale(self, tab_name):
        self._dialog_controller().mark_settings_tab_stale(tab_name)

    def _refresh_active_settings_tab(self, *, force=False):
        self._dialog_controller().refresh_active_settings_tab(force=force)

    def _schedule_active_settings_tab_refresh(self, *, force=False):
        self._dialog_controller().schedule_active_settings_tab_refresh(force=force)

    def _apply_scheduled_settings_tab_refresh(self):
        self._dialog_controller().apply_scheduled_settings_tab_refresh()

    def _on_settings_tab_changed(self, index):
        self._dialog_controller().on_settings_tab_changed(index)

    def _dialog_controller(self):
        return DialogController(self, install_state_loader=install_state)

    def _advanced_tab_controller(self):
        return AdvancedTabController(self)

    def _updates_tab_controller(self):
        return UpdatesTabController(
            self,
            parse_version=_parse_version,
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
        )

    def _health_tab_controller(self):
        return HealthTabController(
            self,
            startup_preflight_reporter=startup_preflight_report,
            install_state_loader=install_state,
            installed_appimage_backup_path_fn=installed_appimage_backup_path,
            is_running_in_appimage_fn=is_running_in_appimage,
        )

    def _recovery_controller(self):
        controller = self.__dict__.get("_recovery_controller_obj")
        if controller is None:
            controller = RecoveryController(self)
            self._recovery_controller_obj = controller
        return controller

    def _runtime_view_controller(self):
        controller = self.__dict__.get("_runtime_view_controller_obj")
        if controller is None:
            controller = RuntimeViewController(self)
            self._runtime_view_controller_obj = controller
        return controller

    def _audio_event_controller(self):
        controller = self.__dict__.get("_audio_event_controller_obj")
        if controller is None:
            controller = AudioEventController(self)
            self._audio_event_controller_obj = controller
        return controller

    def _bluetooth_controller(self):
        controller = self.__dict__.get("_bluetooth_controller_obj")
        if controller is None:
            controller = BluetoothController(self)
            self._bluetooth_controller_obj = controller
        return controller

    def _app_identity_controller(self):
        controller = self.__dict__.get("_app_identity_controller_obj")
        if controller is None:
            controller = AppIdentityController(self)
            self._app_identity_controller_obj = controller
        return controller

    def _config_controller(self):
        controller = self.__dict__.get("_config_controller_obj")
        if controller is None:
            controller = ConfigController(self)
            self._config_controller_obj = controller
        return controller

    def _channel_controller(self):
        controller = self.__dict__.get("_channel_controller_obj")
        if controller is None:
            controller = ChannelController(self)
            self._channel_controller_obj = controller
        return controller

    def _device_policy_controller(self):
        controller = self.__dict__.get("_device_policy_controller_obj")
        if controller is None:
            controller = DevicePolicyController(self)
            self._device_policy_controller_obj = controller
        return controller

    def _module_runtime_controller(self):
        return ModuleRuntimeController(self)

    def _lifecycle_controller(self):
        controller = self.__dict__.get("_lifecycle_controller_obj")
        if controller is None:
            controller = LifecycleController(
                self,
                desktop_filename=DESKTOP_FILENAME,
                desktop_exec_command_fn=desktop_exec_command,
                friendly_name_fn=PipeWireEngine.friendly_name,
            )
            self._lifecycle_controller_obj = controller
        return controller

    def _startup_controller(self):
        controller = self.__dict__.get("_startup_controller_obj")
        if controller is None:
            controller = StartupController(
                self,
                startup_preflight_reporter=startup_preflight_report,
            )
            self._startup_controller_obj = controller
        return controller

    def _stress_control_controller(self):
        controller = self.__dict__.get("_stress_control_controller_obj")
        if controller is None:
            controller = StressControlController(
                self,
                startup_preflight_reporter=startup_preflight_report,
                install_state_loader=install_state,
                enabled_fn=stress_control_enabled,
            )
            self._stress_control_controller_obj = controller
        return controller

    def _scenes_tab_controller(self):
        controller = self.__dict__.get("_scenes_controller")
        if controller is None:
            controller = ScenesTabController(self)
            self._scenes_controller = controller
        return controller

    @staticmethod
    def _dedupe_names(values):
        return ScenesTabController.dedupe_names(values)

    @staticmethod
    def _scene_owner_from_key(key):
        return ScenesTabController.scene_owner_from_key(key)

    @staticmethod
    def _normalize_mix_volume(value, default=1.0):
        return ScenesTabController.normalize_mix_volume(value, default)

    def _current_mix_master_volume(self, mix_name, default=1.0):
        return self._scenes_tab_controller().current_mix_master_volume(
            mix_name,
            default,
        )

    def _set_mix_master_volume(self, mix_name, volume, *, persist=True, update_slider=False):
        self._scenes_tab_controller().set_mix_master_volume(
            mix_name,
            volume,
            persist=persist,
            update_slider=update_slider,
        )

    def _capture_scene_snapshot(self):
        return self._scenes_tab_controller().capture_scene_snapshot()

    @classmethod
    def _normalize_app_volume_prefs(cls, raw):
        _ = cls
        return ScenesTabController.normalize_app_volume_prefs(raw)

    @classmethod
    def _normalize_scene_snapshot(cls, raw):
        _ = cls
        return ScenesTabController.normalize_scene_snapshot(raw)

    @classmethod
    def _normalize_scene_library(cls, raw):
        _ = cls
        return ScenesTabController.normalize_scene_library(raw)

    def _selected_scene_name(self):
        return self._scenes_tab_controller().selected_scene_name()

    def _scene_summary_text(self, snapshot):
        return self._scenes_tab_controller().scene_summary_text(snapshot)

    def _on_scene_selection_change(self, _index):
        self._scenes_tab_controller().on_scene_selection_change(_index)

    def _refresh_scenes_tab(self, selected_name=None):
        self._scenes_tab_controller().refresh_scenes_tab(selected_name)

    def _apply_scene_snapshot(self, snapshot, *, scene_name=""):
        return self._scenes_tab_controller().apply_scene_snapshot(
            snapshot,
            scene_name=scene_name,
        )

    def _save_current_scene_as(self):
        self._scenes_tab_controller().save_current_scene_as()

    def _overwrite_selected_scene(self):
        self._scenes_tab_controller().overwrite_selected_scene()

    def _apply_selected_scene(self):
        self._scenes_tab_controller().apply_selected_scene()

    def _rename_selected_scene(self):
        self._scenes_tab_controller().rename_selected_scene()

    def _delete_selected_scene(self):
        self._scenes_tab_controller().delete_selected_scene()

    def _check_for_updates(self):
        self._updates_tab_controller().check_for_updates()

    def _poll_updater(self):
        self._updates_tab_controller().poll_updater()

    def _handle_update_result(self, release_info):
        self._updates_tab_controller().handle_update_result(release_info)

    def _handle_update_error(self, payload):
        self._updates_tab_controller().handle_update_error(payload)

    def _open_release_page(self):
        self._updates_tab_controller().open_release_page()

    def _download_and_install_update(self):
        self._updates_tab_controller().download_and_install_update()

    def _poll_update_installer(self):
        self._updates_tab_controller().poll_update_installer()

    def _handle_update_install_progress(self, asset_name, downloaded, total):
        self._updates_tab_controller().handle_update_install_progress(
            asset_name,
            downloaded,
            total,
        )

    def _handle_update_install_success(self, result, release_info):
        self._updates_tab_controller().handle_update_install_success(result, release_info)

    def _handle_update_install_error(self, payload):
        self._updates_tab_controller().handle_update_install_error(payload)

    def _restore_previous_appimage(self):
        self._updates_tab_controller().restore_previous_appimage()

    def _handle_update_restore_success(self, result):
        self._updates_tab_controller().handle_update_restore_success(result)

    def _handle_update_restore_error(self, payload):
        self._updates_tab_controller().handle_update_restore_error(payload)

    def _install_current_runtime_launcher(self):
        self._updates_tab_controller().install_current_runtime_launcher()

    def _repair_installed_launchers(self):
        self._updates_tab_controller().repair_installed_launchers()

    def _restart_app(self):
        self._updates_tab_controller().restart_app()

    def _restart_with_command(self, command):
        self._updates_tab_controller().restart_with_command(command)

    def _check_for_updates_bg(self):
        self._updates_tab_controller().check_for_updates_bg()

    def _poll_bg_updater(self):
        self._updates_tab_controller().poll_bg_updater()

    def _refresh_update_tab(self, *, state=None, allow_async=True):
        self._updates_tab_controller().refresh_update_tab(
            state=state,
            allow_async=allow_async,
        )

    @staticmethod
    def _format_timestamp(stamp):
        if not stamp:
            return "never"
        return time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(stamp))

    def _diagnostics_root_path(self):
        runtime = getattr(self, "runtime", None)
        diagnostics = getattr(runtime, "diagnostics", None) if runtime is not None else None
        root_dir = getattr(diagnostics, "root_dir", None)
        return str(root_dir) if root_dir else os.path.expanduser("~/.config/wavelinux/diagnostics")

    def _current_runtime_mode(self):
        return self._updates_tab_controller().current_runtime_mode()

    def _launcher_targets_active_runtime(self, *, state=None, mode=None):
        return self._updates_tab_controller().launcher_targets_active_runtime(
            state=state,
            mode=mode,
        )

    def _runtime_mode_detail(self):
        return self._updates_tab_controller().runtime_mode_detail()

    def _running_binary_path(self, state):
        return self._updates_tab_controller().running_binary_path(state)

    def _update_issue_title(self, code):
        return self._updates_tab_controller().update_issue_title(code)

    def _health_issue_for_runtime_detail(self, detail):
        return self._health_tab_controller().health_issue_for_runtime_detail(detail)

    def _health_issue_for_channel(self, node_name, *, recovered=False):
        return self._health_tab_controller().health_issue_for_channel(
            node_name,
            recovered=recovered,
        )

    def _collect_health_issues(self, *, preflight=None, state=None):
        return self._health_tab_controller().collect_health_issues(
            preflight=preflight,
            state=state,
        )

    def _run_health_issue_action(self, issue, action):
        self._health_tab_controller().run_health_issue_action(issue, action)

    def _render_health_cards(self, issues):
        self._health_tab_controller().render_health_cards(issues)

    def _open_diagnostics_folder(self):
        self._health_tab_controller().open_diagnostics_folder()

    def _refresh_system_tab(self, *, preflight=None, state=None, allow_async=True):
        self._health_tab_controller().refresh_system_tab(
            preflight=preflight,
            state=state,
            allow_async=allow_async,
        )

    def _rerun_system_check(self):
        self._health_tab_controller().rerun_system_check()

    def _refresh_advanced_tab(self):
        self._advanced_tab_controller().refresh_advanced_tab()

    def _restore_forgotten_apps(self):
        self._advanced_tab_controller().restore_forgotten_apps()

    def _on_prune_days_change(self, value):
        self._advanced_tab_controller().on_prune_days_change(value)

    def _forget_all_offline(self):
        self._advanced_tab_controller().forget_all_offline()

    def _on_emergency_reset(self):
        self._advanced_tab_controller().on_emergency_reset()

    def _export_runtime_diagnostics(self):
        self._advanced_tab_controller().export_runtime_diagnostics()

    def open_channel_diagnostics(self, node_name):
        self._recovery_controller().open_channel_diagnostics(node_name)

    def _runtime_degraded_channels(self):
        view = self._runtime_view_state
        if view is None:
            return []
        health = getattr(view, "health", {}) or {}
        return sorted(name for name, state in health.items() if state)

    def _recover_all_degraded_channels(self):
        self._recovery_controller().recover_all_degraded_channels()

    def _refresh_hidden_list(self):
        self._channel_controller().refresh_hidden_list()

    def _unhide_from_settings(self, node_name):
        self._channel_controller().unhide_from_settings(node_name)

    def schedule_save(self):
        self._config_controller().schedule_save()

    def _flush_pending_ui_state(self):
        self._config_controller().flush_pending_ui_state()

    def _virtual_channel_specs(self):
        return self._config_controller().virtual_channel_specs()

    def _sync_runtime_persistent_state(self, *, immediate=False):
        self._config_controller().sync_runtime_persistent_state(immediate=immediate)

    @staticmethod
    def _normalize_app_identity_overrides(raw):
        return AppIdentityController.normalize_app_identity_overrides(raw)

    @staticmethod
    def _normalize_app_label_overrides(raw):
        return AppIdentityController.normalize_app_label_overrides(raw)

    def _set_engine_identity_overrides(self):
        self._app_identity_controller().set_engine_identity_overrides()

    def _identity_dialog_parent(self):
        return self._app_identity_controller().identity_dialog_parent()

    def _all_scene_app_ids(self):
        return self._app_identity_controller().all_scene_app_ids()

    def _known_persistent_app_ids(self):
        return self._app_identity_controller().known_persistent_app_ids()

    def _override_sources_for_target(self, target_app_id, *, exclude_source=None):
        return self._app_identity_controller().override_sources_for_target(
            target_app_id,
            exclude_source=exclude_source,
        )

    def _app_id_has_runtime_or_saved_references(self, app_id):
        return self._app_identity_controller().app_id_has_runtime_or_saved_references(app_id)

    def _cleanup_orphaned_custom_identity(self, app_id):
        self._app_identity_controller().cleanup_orphaned_custom_identity(app_id)

    def _allocate_custom_app_id(self, label, *, keep_existing=""):
        return self._app_identity_controller().allocate_custom_app_id(
            label,
            keep_existing=keep_existing,
        )

    def _migrate_scene_library_app_identity(self, source_app_id, target_app_id):
        return self._app_identity_controller().migrate_scene_library_app_identity(
            source_app_id,
            target_app_id,
        )

    def _migrate_app_identity_state(self, source_app_id, target_app_id):
        return self._app_identity_controller().migrate_app_identity_state(
            source_app_id,
            target_app_id,
        )

    def _display_name_for_app_id(self, app_id, fallback=None):
        return self._app_identity_controller().display_name_for_app_id(
            app_id,
            fallback=fallback,
        )

    def _app_identity_context(self, app_view_or_row):
        return self._app_identity_controller().app_identity_context(app_view_or_row)

    def _migrate_legacy_app_identity(self, app_id, display_name):
        return self._app_identity_controller().migrate_legacy_app_identity(
            app_id,
            display_name,
        )

    def _apply_app_identity_changes(self, status_message):
        self._app_identity_controller().apply_app_identity_changes(status_message)

    def _pin_app_identity(self, app_view_or_row):
        return self._app_identity_controller().pin_app_identity(app_view_or_row)

    def _merge_app_identity(self, app_view_or_row):
        return self._app_identity_controller().merge_app_identity(app_view_or_row)

    def _reset_app_identity_override(self, app_view_or_row):
        return self._app_identity_controller().reset_app_identity_override(app_view_or_row)

# ── Entry Point ────────────────────────────────────────────────────
def main(argv=None):
    cli_exit = _handle_cli_args(argv)
    if cli_exit is not None:
        return cli_exit
    app = QApplication(sys.argv)
    app.setApplicationName("WaveLinux")
    app.setDesktopFileName(APP_DESKTOP_ID)
    app.setStyleSheet(STYLESHEET)

    # Try to use a nice font
    font = QFont("Inter", 10)
    app.setFont(font)

    # Single instance lock
    lock_path = os.path.join(os.path.expanduser("~"), ".wavelinux.lock")
    lock_file = QLockFile(lock_path)
    if not lock_file.tryLock(100):
        print("WaveLinux is already running.")
        return 0

    window = WaveLinuxWindow()
    window.show()

    # Ensure cleanup on standard application quit
    app.aboutToQuit.connect(window._cleanup_before_exit)

    return app.exec()


if __name__ == '__main__':
    sys.exit(main())
