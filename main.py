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

from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QLabel, QPushButton, QDialog,
    QDialogButtonBox, QComboBox, QMessageBox, QSystemTrayIcon,
    QMenu, QInputDialog, QProgressBar, QSizePolicy,
    QFileDialog,
)
from PyQt6.QtCore import Qt, QTimer, QLockFile, QProcess, QUrl
from PyQt6.QtGui import QFont, QIcon, QAction, QDesktopServices

from app_core import (
    AppContext,
    ConfigChanged,
    EventBus,
    FxStatusUpdated,
    HealthBus,
    ModuleManager,
    RuntimeViewUpdated,
    load_feature_flags,
)
from audio_runtime import AudioRuntimeAdapter, AudioRuntimeController
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
from health import HealthIssue, RecoveryStatus
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
from ui.dialogs.card_profile_dialog import CardProfileDialog
from ui.dialogs.fx_dialog import FXSelectionDialog
from ui.health.health_card import HealthCard
from ui.main_window import build_main_window
from ui.mixer import ChannelStrip, MeterWorker, MixerPanelController, MixerStripMetrics
from ui.routing import AppRoutingPanelController, AppRoutingRow
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

APP_VERSION = "3.0"
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
}
_QUICK_START_TEMPLATES = {
    "laptop_mic": {
        "title": "Laptop Mic",
        "description": "Lean setup for a built-in microphone with voice cleanup and a compact starter layout.",
        "channels": ["Music", "Browser", "Voice Chat"],
        "mic_effects": ["rnnoise", "limiter"],
    },
    "usb_interface": {
        "title": "USB Interface",
        "description": "Balanced setup for a USB mic or interface with a simple routing layout and light protection.",
        "channels": ["Music", "Game", "Voice Chat"],
        "mic_effects": ["limiter"],
    },
    "streaming_obs": {
        "title": "Streaming / OBS",
        "description": "Streaming-oriented layout with separate content channels and a fuller default voice chain.",
        "channels": ["Game", "Music", "Browser", "Voice Chat", "Alerts"],
        "mic_effects": ["rnnoise", "compressor", "limiter"],
    },
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
class WaveLinuxWindow(QMainWindow):
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

    def _on_runtime_view_state(self, view_state):
        self._runtime_view_state = view_state
        event_bus = self.__dict__.get("_event_bus")
        if event_bus is not None:
            event_bus.publish(RuntimeViewUpdated(view_state=view_state))
        manager = self.__dict__.get("module_manager")
        if manager is not None:
            manager.on_runtime_view(view_state)
        health = getattr(view_state, "health", {}) or {}
        pending_ops = getattr(view_state, "pending_operations", {}) or {}
        if not self._selected_mic_transition_in_progress(health, pending_ops):
            self._reconcile_device_policy(view_state)
        degraded = [name for name, state in health.items() if state]
        if degraded:
            self.status_lbl.setText(
                f"Runtime degraded: {', '.join(degraded[:2])}"
            )
        elif pending_ops:
            self.status_lbl.setText(
                f"Applying audio changes ({len(pending_ops)})..."
            )
        elif self.status_lbl.text().startswith("Runtime degraded:"):
            self.status_lbl.setText("PipeWire connected")
        elif self.status_lbl.text().startswith("Applying audio changes"):
            self.status_lbl.setText("PipeWire connected")
        if self._selected_mic_needs_followup_refresh(health, pending_ops):
            timer = self.__dict__.get("_mic_cutover_refresh_timer")
            if timer is not None and not timer.isActive():
                timer.start()
        refresh_timer = self.__dict__.get("_runtime_view_refresh_timer")
        if refresh_timer is not None:
            refresh_timer.setInterval(
                self._runtime_view_refresh_delay_ms(
                    health=health,
                    pending_ops=pending_ops,
                )
            )
        self._schedule_runtime_view_refresh()

    def _request_runtime_refresh(self, reason=""):
        if bool(self.__dict__.get("_shutting_down", False)):
            return
        runtime = getattr(self, "runtime", None)
        if runtime is not None and hasattr(runtime, "refresh_now"):
            runtime.refresh_now(reason or "runtime-refresh")

    def _on_runtime_fx_status(self, status):
        event_bus = self.__dict__.get("_event_bus")
        if event_bus is not None:
            event_bus.publish(FxStatusUpdated(status=status))
        node_name = getattr(status, "node_name", "")
        state = getattr(status, "state", "")
        if state in {"building", "cutover_pending", "clearing"}:
            self._cancel_auto_recovery_timer(node_name)
        elif state in {"active", "idle"}:
            self._clear_auto_recovery_state(node_name)
            if node_name and not getattr(self, "_shutting_down", False):
                if self._selected_mic_change_settling():
                    self._schedule_runtime_view_refresh()
                else:
                    self._request_runtime_refresh(f"fx-status:{state}:{node_name}")
        if state == "degraded":
            self.status_lbl.setText(
                self.format_fx_status_message(status) or "FX runtime degraded"
            )
            self._schedule_auto_recovery(status)
        if self._settings_dialog_visible():
            self._mark_settings_tab_stale("Health")
            if self._active_settings_tab_name() == "Health":
                self._schedule_active_settings_tab_refresh(force=True)
        self._refresh_channel_runtime_status(node_name)

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
        timer = self.__dict__.get("_runtime_view_refresh_timer")
        if timer is not None:
            timer.start()

    def _runtime_view_refresh_delay_ms(self, *, health=None, pending_ops=None):
        delay_ms = 40
        if pending_ops:
            delay_ms = 180
        elif health:
            delay_ms = 90
        if self._settings_dialog_visible():
            delay_ms = max(delay_ms, 80)
        return delay_ms

    def _runtime_view_has_pending_ops(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        if view is None:
            return False
        return bool(getattr(view, "pending_operations", {}) or {})

    def _selected_mic_change_settling(self, *, window_s=3.0):
        changed_at = float(self.__dict__.get("_last_selected_mic_change_at", 0.0) or 0.0)
        if changed_at <= 0.0:
            return False
        return (time.monotonic() - changed_at) < max(0.0, float(window_s or 0.0))

    def _selected_mic_needs_followup_refresh(self, health, pending_ops):
        if pending_ops or bool(self.__dict__.get("_shutting_down", False)):
            return False
        selected_mic = str(self.__dict__.get("selected_mic", "") or "").strip()
        if not selected_mic:
            return False
        health_code = str((health or {}).get(selected_mic) or "").strip()
        return health_code in {
            "default_source_mismatch",
            "default_source_expected_fx_missing",
            "desired_fx_missing",
            "fx_source_not_present",
        }

    def _selected_mic_transition_in_progress(self, health, pending_ops):
        if bool(self.__dict__.get("_shutting_down", False)):
            return False
        selected_mic = str(self.__dict__.get("selected_mic", "") or "").strip()
        if not selected_mic or not self._selected_mic_change_settling():
            return False
        if pending_ops:
            return True
        health_code = str((health or {}).get(selected_mic) or "").strip()
        return health_code in {
            "default_source_mismatch",
            "default_source_expected_fx_missing",
            "desired_fx_missing",
            "fx_source_not_present",
        }

    def _startup_graph_health_blockers(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        if view is None:
            return {}
        health = getattr(view, "health", {}) or {}
        managed_names = set(self._virtual_channel_specs().keys())
        selected_mic = str(self.__dict__.get("selected_mic", "") or "").strip()
        if selected_mic:
            managed_names.add(selected_mic)
        blockers = {}
        for node_name in managed_names:
            code = str(health.get(node_name) or "").strip()
            if code in {
                "submix_monitor_missing",
                "submix_stream_missing",
                "submix_monitor_dead",
                "submix_stream_dead",
                "default_source_mismatch",
                "default_source_expected_fx_missing",
                "desired_fx_missing",
                "fx_source_not_present",
            }:
                blockers[node_name] = code
        return blockers

    def _startup_audio_ready(self, view=None):
        selected_mic = str(self.__dict__.get("selected_mic", "") or "").strip()
        view = view or self.__dict__.get("_runtime_view_state")
        runtime = self.__dict__.get("runtime")
        if view is None and runtime is not None:
            view = getattr(runtime, "latest_view_state", None)
        if view is None:
            return False
        if self._startup_graph_health_blockers(view=view):
            return False
        if not selected_mic:
            return True
        mic_names = {
            str(getattr(mic_view, "name", "") or "").strip()
            for mic_view in (getattr(view, "mic_inputs", []) or [])
        }
        if mic_names and selected_mic not in mic_names:
            return False
        health = getattr(view, "health", {}) or {}
        health_code = str(health.get(selected_mic) or "").strip()
        if health_code in {
            "default_source_mismatch",
            "default_source_expected_fx_missing",
            "desired_fx_missing",
            "fx_source_not_present",
        }:
            return False
        default_source = str(getattr(view, "default_source", "") or "").strip()
        if not default_source:
            return False
        active_fx = list((self.__dict__.get("active_effects", {}) or {}).get(selected_mic, []) or [])
        if not active_fx:
            return default_source == selected_mic
        engine = self.__dict__.get("engine")
        expected_source = ""
        if engine is not None and hasattr(engine, "get_channel_fx_source"):
            expected_source = str(engine.get_channel_fx_source(selected_mic) or "").strip()
        if not expected_source:
            return False
        return default_source == expected_source

    def _wait_for_startup_audio_ready(self, timeout_s=8.0):
        if bool(self.__dict__.get("_shutting_down", False)):
            return True
        deadline = time.monotonic() + max(0.0, float(timeout_s or 0.0))
        next_refresh_at = 0.0
        while time.monotonic() < deadline:
            view = self.__dict__.get("_runtime_view_state")
            runtime = self.__dict__.get("runtime")
            if view is None and runtime is not None:
                view = getattr(runtime, "latest_view_state", None)
            if self._startup_audio_ready(view=view):
                return True
            now = time.monotonic()
            if now >= next_refresh_at:
                self._request_runtime_refresh("startup-audio-ready")
                next_refresh_at = now + 0.2
            QApplication.processEvents()
            time.sleep(0.05)
        return self._startup_audio_ready()

    def _apply_scheduled_runtime_view_refresh(self):
        hidden_to_tray = self.tray is not None and not self.isVisible()
        settings_open = self._settings_dialog_visible()
        active_settings_tab = self._active_settings_tab_name() if settings_open else ""
        if settings_open:
            self._schedule_active_settings_tab_refresh()
        if hidden_to_tray and not settings_open:
            return
        if self._any_slider_dragging():
            return
        if self._runtime_view_has_pending_ops():
            timer = self.__dict__.get("_runtime_view_refresh_timer")
            view = self.__dict__.get("_runtime_view_state")
            if timer is not None and view is not None:
                timer.setInterval(
                    self._runtime_view_refresh_delay_ms(
                        health=getattr(view, "health", {}) or {},
                        pending_ops=getattr(view, "pending_operations", {}) or {},
                    )
                )
                timer.start()
            return
        if settings_open and self._selected_mic_change_settling():
            self._apply_lightweight_runtime_view_refresh()
            timer = self.__dict__.get("_runtime_view_refresh_timer")
            if timer is not None:
                timer.setInterval(120)
                timer.start()
            return
        self._refresh_runtime_view()

    def _apply_lightweight_runtime_view_refresh(self):
        view = self.__dict__.get("_runtime_view_state")
        if view is None:
            self.status_lbl.setText("PipeWire syncing...")
            return
        mics = list(getattr(view, "mic_inputs", []) or [])
        self._mixer_panel_controller().sync_mic_picker(
            mics,
            default_src=getattr(view, "default_source", None),
        )
        if not getattr(view, "health", {}):
            self.status_lbl.setText(
                f"PipeWire connected · {getattr(view, 'node_count', 0)} nodes · "
                f"{getattr(view, 'app_count', 0)} apps"
            )

    def _make_auto_recovery_timer(self):
        timer = QTimer(self)
        timer.setSingleShot(True)
        return timer

    def _cancel_auto_recovery_timer(self, node_name):
        state = self._auto_recovery_state.get(node_name)
        if not state:
            return
        timer = state.get("timer")
        if timer is not None:
            try:
                timer.stop()
            except Exception:
                pass
        state["timer"] = None

    def _clear_auto_recovery_state(self, node_name):
        if not node_name:
            return
        if not hasattr(self, "_recent_recovery_status"):
            self._recent_recovery_status = {}
        entry = self._auto_recovery_state.get(node_name)
        if entry and int(entry.get("attempts", 0)) > 0:
            status = getattr(self, "runtime", None).fx_status_for(node_name) if getattr(self, "runtime", None) is not None else None
            self._recent_recovery_status[node_name] = {
                "at": time.time(),
                "status": RecoveryStatus(
                    node_name=node_name,
                    state="recovered",
                    retry_count=int(entry.get("attempts", 0)),
                    diagnostics_path=self._fx_status_diagnostics_path(status),
                ),
            }
        self._cancel_auto_recovery_timer(node_name)
        self._auto_recovery_state.pop(node_name, None)
        self._refresh_channel_runtime_status(node_name)

    def _ensure_auto_recovery_entry(self, node_name, generation):
        entry = self._auto_recovery_state.get(node_name)
        generation = int(generation or 0)
        if not hasattr(self, "_recent_recovery_status"):
            self._recent_recovery_status = {}
        self._recent_recovery_status.pop(node_name, None)
        if entry is None or int(entry.get("generation", 0)) != generation:
            if entry is not None:
                self._cancel_auto_recovery_timer(node_name)
            entry = {
                "generation": generation,
                "attempts": 0,
                "timer": None,
                "last_delay_ms": 0,
                "exhausted": False,
            }
            self._auto_recovery_state[node_name] = entry
        return entry

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
        state = self._auto_recovery_state.get(node_name) or {}
        timer = state.get("timer")
        attempts = int(state.get("attempts", 0))
        total = len(self._AUTO_RECOVERY_DELAYS_MS)
        if timer is not None and timer.isActive():
            delay_ms = int(state.get("last_delay_ms", 0))
            seconds = max(1, int(round(delay_ms / 1000.0)))
            return f"Automatic recovery scheduled in ~{seconds}s ({attempts}/{total})."
        if state.get("exhausted"):
            return "Automatic recovery attempts were exhausted. Use Recover channel to try again manually."
        if attempts:
            return f"Automatic recovery attempt {attempts}/{total} is in progress."
        return ""

    def recovery_status_for_channel(self, node_name):
        if not node_name:
            return RecoveryStatus(node_name="", state="idle", retry_count=0)
        runtime = getattr(self, "runtime", None)
        fx_status = runtime.fx_status_for(node_name) if runtime is not None else None
        diagnostics_path = self._fx_status_diagnostics_path(fx_status)
        entry = self._auto_recovery_state.get(node_name) or {}
        timer = entry.get("timer")
        attempts = int(entry.get("attempts", 0))
        if entry.get("exhausted"):
            return RecoveryStatus(
                node_name=node_name,
                state="exhausted",
                retry_count=attempts,
                diagnostics_path=diagnostics_path,
            )
        if timer is not None and timer.isActive():
            remaining_ms = max(0, int(timer.remainingTime()))
            next_retry_at = time.time() + (remaining_ms / 1000.0)
            return RecoveryStatus(
                node_name=node_name,
                state="scheduled",
                retry_count=attempts,
                next_retry_at=next_retry_at,
                diagnostics_path=diagnostics_path,
            )
        if attempts:
            return RecoveryStatus(
                node_name=node_name,
                state="retrying",
                retry_count=attempts,
                diagnostics_path=diagnostics_path,
            )
        recent = getattr(self, "_recent_recovery_status", {}).get(node_name) or {}
        recent_status = recent.get("status")
        recent_at = float(recent.get("at", 0) or 0)
        if recent_status is not None and (time.time() - recent_at) < 90:
            return recent_status
        if getattr(fx_status, "state", "") == "degraded":
            return RecoveryStatus(
                node_name=node_name,
                state="retrying",
                retry_count=0,
                diagnostics_path=diagnostics_path,
            )
        return RecoveryStatus(
            node_name=node_name,
            state="idle",
            retry_count=0,
            diagnostics_path=diagnostics_path,
        )

    def channel_runtime_issue(self, node_name):
        view = self._runtime_view_state
        health = getattr(view, "health", {}) if view is not None else {}
        health_code = ((health or {}).get(node_name) or "").strip()
        runtime = getattr(self, "runtime", None)
        fx_status = runtime.fx_status_for(node_name) if runtime is not None else None
        fx_degraded = getattr(fx_status, "state", "") == "degraded"
        diagnostics_path = self._fx_status_diagnostics_path(fx_status)
        lines = []
        summary = ""
        if health_code:
            summary = self._format_runtime_health_code(health_code).rstrip(".")
            lines.append(f"Health: {self._format_runtime_health_code(health_code)}")
        if fx_degraded:
            if not summary:
                summary = "FX runtime degraded"
            formatted = self.format_fx_status_message(fx_status)
            if formatted:
                lines.append(formatted)
        recovery_note = self.fx_recovery_status_message(node_name)
        if recovery_note:
            lines.append(recovery_note)
        if diagnostics_path:
            lines.append(f"Diagnostics: {diagnostics_path}")
        degraded = bool(health_code) or fx_degraded
        if degraded:
            lines.append("Right-click for Retry FX Now and Open Diagnostics.")
        return {
            "degraded": degraded,
            "health_code": health_code,
            "summary": summary or "Runtime issue detected",
            "tooltip": "\n".join(line for line in lines if line),
            "diagnostics_path": diagnostics_path,
            "status": fx_status,
        }

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
        node_name = getattr(status, "node_name", "")
        if not node_name:
            return
        entry = self._ensure_auto_recovery_entry(node_name, getattr(status, "generation", 0))
        timer = entry.get("timer")
        if timer is not None and timer.isActive():
            return
        attempts = int(entry.get("attempts", 0))
        label = self._channel_label(node_name)
        if attempts >= len(self._AUTO_RECOVERY_DELAYS_MS):
            entry["exhausted"] = True
            self.status_lbl.setText(
                f"{label} is still degraded. Automatic recovery attempts are exhausted."
            )
            self._refresh_channel_runtime_status(node_name)
            return
        delay_ms = int(self._AUTO_RECOVERY_DELAYS_MS[attempts])
        timer = self._make_auto_recovery_timer()
        timer.timeout.connect(
            lambda node_name=node_name, generation=int(getattr(status, "generation", 0)): self._run_auto_recovery(node_name, generation)
        )
        entry["timer"] = timer
        entry["attempts"] = attempts + 1
        entry["last_delay_ms"] = delay_ms
        timer.start(delay_ms)
        self.status_lbl.setText(
            f"{label} degraded; retrying automatically in ~{max(1, int(round(delay_ms / 1000.0)))}s "
            f"({entry['attempts']}/{len(self._AUTO_RECOVERY_DELAYS_MS)})"
        )
        self._refresh_channel_runtime_status(node_name)

    def _run_auto_recovery(self, node_name, generation):
        entry = self._auto_recovery_state.get(node_name)
        if not entry or int(entry.get("generation", 0)) != int(generation or 0):
            return
        entry["timer"] = None
        current = self.runtime.fx_status_for(node_name)
        if getattr(current, "state", "") != "degraded":
            return
        current_generation = int(getattr(current, "generation", 0) or 0)
        if current_generation and int(generation or 0) and current_generation != int(generation or 0):
            return
        self.status_lbl.setText(f"Attempting automatic recovery for {self._channel_label(node_name)}...")
        self._refresh_channel_runtime_status(node_name)
        self.runtime.recover_channel(node_name)

    def _run_startup_preflight(self):
        """Check for required runtime binaries and surface a clear warning."""
        report = startup_preflight_report()
        self._startup_preflight = report
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
            QMessageBox.warning(self, "WaveLinux dependency check", msg)
        self._recover_unclean_runtime_state()
        self._write_runtime_pid()

    @staticmethod
    def _pid_is_alive(pid):
        try:
            os.kill(int(pid), 0)
        except (OSError, ValueError, TypeError):
            return False
        return True

    def _recover_unclean_runtime_state(self):
        pid_path = getattr(self, "_runtime_pid_path", "")
        if not pid_path or not os.path.exists(pid_path):
            return
        try:
            with open(pid_path, "r") as fh:
                previous_pid = fh.read().strip()
        except OSError:
            previous_pid = ""
        if not previous_pid or previous_pid == str(os.getpid()):
            return
        if self._pid_is_alive(previous_pid):
            return
        logging.warning(
            "Detected stale WaveLinux runtime from pid %s; resetting audio graph",
            previous_pid,
        )
        try:
            self.runtime.full_audio_reset_sync()
        except Exception as exc:
            logging.error(f"Startup stale-state reset failed: {exc}")

    def _write_runtime_pid(self):
        pid_path = getattr(self, "_runtime_pid_path", "")
        if not pid_path:
            return
        try:
            with open(pid_path, "w") as fh:
                fh.write(str(os.getpid()))
        except OSError as exc:
            logging.warning(f"Could not write runtime pid file {pid_path}: {exc}")

    def _clear_runtime_pid(self):
        pid_path = getattr(self, "_runtime_pid_path", "")
        if not pid_path:
            return
        try:
            if os.path.exists(pid_path):
                os.remove(pid_path)
        except OSError as exc:
            logging.warning(f"Could not clear runtime pid file {pid_path}: {exc}")

    def _setup_stress_control(self):
        if not stress_control_enabled():
            return
        try:
            from stress_control import StressControlServer

            socket_path = str(os.environ.get("WAVELINUX_STRESS_SOCKET_PATH", "") or "").strip() or None
            self._stress_control_server = StressControlServer(self, socket_path=socket_path)
            self._stress_control_server.start()
            logging.info(
                "Stress control enabled on %s",
                self._stress_control_server.socket_path,
            )
        except Exception as exc:
            logging.exception("Could not start stress control server: %s", exc)
            self._stress_control_server = None

    def _setup_feature_modules(self):
        ctx = AppContext(
            runtime=self.runtime,
            engine=self.engine,
            config_store=self,
            event_bus=self._event_bus,
            module_manager=None,
            diagnostics=getattr(self.runtime, "diagnostics", None),
            main_window=self,
        )
        manager = ModuleManager(
            ctx,
            feature_flags=self._feature_flags,
            health_bus=self._module_health_bus,
        )
        ctx.module_manager = manager
        self.module_manager = manager
        for module in (
            RuntimeModule(self),
            MixerUiModule(self),
            MeteringModule(self),
            AppRoutingModule(self),
            EffectsModule(self),
            DevicePolicyModule(self),
            SettingsUiModule(self),
            ScenesModule(self),
            UpdatesModule(self),
            HealthModule(self),
            StressControlModule(self),
        ):
            manager.register(module)

    def _start_feature_modules(self):
        manager = getattr(self, "module_manager", None)
        if manager is None:
            return
        manager.start_all()

    def _stop_stress_control(self):
        server = self.__dict__.get("_stress_control_server")
        if server is None:
            return
        try:
            server.stop()
        except Exception as exc:
            logging.warning("Could not stop stress control server cleanly: %s", exc)
        self._stress_control_server = None

    def _module_enabled(self, module_id):
        key = str(module_id or "").strip().lower()
        return (self.__dict__.get("_module_feature_enabled") or {}).get(key, True)

    def _set_feature_module_enabled(self, module_id, enabled, *, reason=""):
        key = str(module_id or "").strip().lower()
        previous = self._module_feature_enabled.get(key, True)
        self._module_feature_enabled[key] = bool(enabled)
        if previous == bool(enabled) and reason != "restore":
            return
        if key == "metering":
            if not enabled:
                self._stop_all_meters()
            else:
                self._restart_metering_module()
        elif key == "effects":
            if not enabled:
                self._disable_effects_module_runtime(reason=reason)
        elif key == "settings_ui":
            if not enabled:
                self._stress_close_settings()
        elif key == "mixer_ui":
            inputs_scroll = getattr(self, "inputs_scroll", None)
            if inputs_scroll is not None:
                inputs_scroll.setVisible(bool(enabled))
        elif key == "app_routing":
            self._set_app_routing_controls_enabled(bool(enabled))
        elif key == "updates":
            self._set_update_controls_enabled(bool(enabled))
        elif key == "health":
            system_tab = getattr(self, "_system_tab_widget", None)
            if system_tab is not None:
                system_tab.setEnabled(bool(enabled))
        elif key == "scenes":
            scenes_tab = getattr(self, "_scenes_tab_widget", None)
            if scenes_tab is not None:
                scenes_tab.setEnabled(bool(enabled))

    def _restart_metering_module(self):
        if not self._module_enabled("metering"):
            return
        view = getattr(self, "_runtime_view_state", None)
        if view is None:
            return
        self._refresh_runtime_view()

    def _disable_effects_module_runtime(self, *, reason=""):
        active_channels = set(getattr(self, "active_effects", {}).keys()) | set(
            getattr(self, "effect_params", {}).keys()
        )
        runtime = getattr(self, "runtime", None)
        if runtime is not None:
            for node_name in sorted(active_channels):
                runtime.clear_channel_fx(node_name)
        self.active_effects = {}
        if reason:
            logging.info("Effects module disabled: %s", reason)
        self.schedule_save()

    def _enable_effects_module_runtime(self):
        self._set_feature_module_enabled("effects", True, reason="restore")
        self._sync_runtime_persistent_state(immediate=True)
        self._request_runtime_refresh("effects-module-restore")

    def _set_app_routing_controls_enabled(self, enabled):
        for row in list(getattr(self, "app_widgets", {}).values()):
            try:
                row.combo.setEnabled(bool(enabled))
                row.manage_btn.setEnabled(bool(enabled))
                row.forget_btn.setEnabled(bool(enabled))
            except Exception:
                continue

    def _set_update_controls_enabled(self, enabled):
        for attr in (
            "_check_update_btn",
            "_download_update_btn",
            "_install_runtime_btn",
            "_rollback_update_btn",
            "_download_install_btn",
            "_install_current_btn",
            "_restore_backup_btn",
        ):
            widget = self.__dict__.get(attr)
            if widget is not None:
                widget.setEnabled(bool(enabled))

    def _module_health_issues(self):
        manager = self.__dict__.get("module_manager")
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

    def _stress_runtime_summary(self):
        default_sink = None
        default_source = None
        monitor_output = self._desired_mix_hw.get("Monitor")
        stream_output = self._desired_mix_hw.get("Stream")
        live_monitor_output = None
        live_stream_output = None
        selected_fx_source = None
        graph_nodes = []
        sink_inventory = []
        source_inventory = []
        try:
            with self.engine.session() as engine:
                snap = engine.create_snapshot(force=True)
                default_sink = engine.get_default_sink()
                default_source = engine.get_default_source()
                live_monitor_output = engine.get_live_mix_hardware_route("Monitor", snap=snap)
                live_stream_output = engine.get_live_mix_hardware_route("Stream", snap=snap)
                if self.selected_mic:
                    selected_fx_source = engine.get_channel_fx_source(self.selected_mic, snap=snap)
                if hasattr(engine, "stable_sink_inventory"):
                    sink_inventory = list(engine.stable_sink_inventory(snap=snap) or [])
                if hasattr(engine, "stable_source_inventory"):
                    source_inventory = list(engine.stable_source_inventory(snap=snap) or [])
                for node in getattr(snap, "nodes", []) or []:
                    node_name = str(getattr(node, "name", "") or "").strip()
                    if not node_name:
                        continue
                    if (
                        node_name.startswith("wavelinux")
                        or node_name.startswith("output.wavelinux")
                        or node_name.startswith("input.wavelinux")
                    ):
                        graph_nodes.append(node_name)
        except Exception:
            logging.exception("Could not build stress runtime summary")
        view = self.__dict__.get("_runtime_view_state")
        app_routes = {}
        app_summaries = []
        degraded_channels = []
        current_default_sink = default_sink
        current_default_source = default_source
        graph_blockers = {}
        if view is not None:
            current_default_sink = getattr(view, "default_sink", None) or current_default_sink
            current_default_source = getattr(view, "default_source", None) or current_default_source
            for app_view in getattr(view, "app_views", []) or []:
                app_summary = {
                    "app_id": str(getattr(app_view, "app_id", "") or ""),
                    "app_name": str(getattr(app_view, "app_name", "") or ""),
                    "resolved_app_id": str(getattr(app_view, "resolved_app_id", "") or ""),
                    "resolved_app_name": str(getattr(app_view, "resolved_app_name", "") or ""),
                    "identity_source": str(getattr(app_view, "identity_source", "") or ""),
                    "current_sink": getattr(app_view, "current_sink", None),
                    "current_volume": getattr(app_view, "current_volume", None),
                    "active_indices": list(getattr(app_view, "active_indices", []) or []),
                }
                app_summaries.append(app_summary)
                if app_summary["app_id"]:
                    app_routes[app_summary["app_id"]] = app_summary["current_sink"]
            degraded_channels = list(self._runtime_degraded_channels())
            graph_blockers = self._startup_graph_health_blockers(view=view)
        expected_default_source = selected_fx_source or self.selected_mic
        ready = bool(
            not self.__dict__.get("_runtime_stopped", False)
            and view is not None
            and graph_nodes
            and monitor_output
            and current_default_sink
            and not graph_blockers
            and (
                not expected_default_source
                or current_default_source == expected_default_source
            )
        )
        return {
            "running": not bool(self.__dict__.get("_runtime_stopped", False)),
            "ready": ready,
            "selected_mic": self.selected_mic,
            "selected_mic_fx_source": selected_fx_source,
            "expected_default_source": expected_default_source,
            "active_default_sink": current_default_sink,
            "active_default_source": current_default_source,
            "monitor_output": monitor_output,
            "stream_output": stream_output,
            "live_monitor_output": live_monitor_output,
            "live_stream_output": live_stream_output,
            "wave_modules_loaded": len(set(graph_nodes)),
            "graph_present": bool(graph_nodes),
            "graph_nodes": sorted(set(graph_nodes)),
            "degraded_channels": degraded_channels,
            "graph_blockers": dict(graph_blockers),
            "app_routes": app_routes,
            "apps": app_summaries,
            "modules": self._stress_list_modules(),
            "known_sinks": sink_inventory,
            "known_sources": source_inventory,
            "settings_tab": self._active_settings_tab_name(),
            "settings_visible": self._settings_dialog_visible(),
        }

    def _stress_health_summary(self):
        issues = self._collect_health_issues(
            preflight=self.__dict__.get("_startup_preflight") or startup_preflight_report(),
            state=install_state(),
        )
        return [
            {
                "code": issue.code,
                "severity": issue.severity,
                "title": issue.title,
                "detail": issue.detail,
                "primary_action": issue.primary_action,
                "secondary_action": issue.secondary_action,
                "context": dict(issue.context or {}),
            }
            for issue in issues
        ]

    def _stress_list_modules(self):
        manager = getattr(self, "module_manager", None)
        if manager is None:
            return []
        return [
            {
                "module_id": health.module_id,
                "state": health.state,
                "summary": health.summary,
                "issues": list(health.issues),
                "restartable": bool(health.restartable),
            }
            for health in manager.list_modules()
        ]

    def _stress_get_module_health(self, module_id):
        manager = getattr(self, "module_manager", None)
        if manager is None:
            return {}
        health = manager.module_health(str(module_id or ""))
        return {
            "module_id": health.module_id,
            "state": health.state,
            "summary": health.summary,
            "issues": list(health.issues),
            "restartable": bool(health.restartable),
        }

    def _stress_disable_module(self, module_id, *, reason="stress-disable"):
        manager = getattr(self, "module_manager", None)
        if manager is None:
            return {"accepted": False}
        manager.disable_module(str(module_id or ""), reason)
        return self._stress_get_module_health(module_id)

    def _stress_enable_module(self, module_id):
        manager = getattr(self, "module_manager", None)
        if manager is None:
            return {"accepted": False}
        manager.enable_module(str(module_id or ""))
        return self._stress_get_module_health(module_id)

    def _stress_restart_module(self, module_id, *, reason="stress-restart"):
        manager = getattr(self, "module_manager", None)
        if manager is None:
            return {"accepted": False}
        manager.restart_module(str(module_id or ""), reason)
        return self._stress_get_module_health(module_id)

    def _stress_list_known_sinks(self):
        summary = self._stress_runtime_summary()
        return list(summary.get("known_sinks") or [])

    def _stress_list_known_sources(self):
        summary = self._stress_runtime_summary()
        return list(summary.get("known_sources") or [])

    def _stress_set_monitor_output(self, sink_name, *, persist=True, include_summary=False):
        sink_name = str(sink_name or "").strip() or None
        self._set_mix_output_target(
            "Monitor",
            sink_name,
            persist=bool(persist),
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        if sink_name:
            self._record_preferred_monitor(sink_name, view=self.__dict__.get("_runtime_view_state"))
            self._schedule_monitor_route_followups(sink_name)
        if include_summary:
            return self._stress_runtime_summary()
        return {
            "monitor_output": sink_name,
            "requested": True,
        }

    def _stress_set_stream_output(self, sink_name, *, persist=True, include_summary=False):
        sink_name = str(sink_name or "").strip() or None
        self._set_mix_output_target(
            "Stream",
            sink_name,
            persist=bool(persist),
            update_combo=True,
            sync_runtime=False,
            sync_runtime_refresh=False,
        )
        if include_summary:
            return self._stress_runtime_summary()
        return {
            "stream_output": sink_name,
            "requested": True,
        }

    def _stress_set_selected_mic(self, mic_name, *, persist=True, include_summary=False):
        self._set_selected_mic_target(
            str(mic_name or "").strip() or None,
            record_preference=True,
            persist=bool(persist),
            request_refresh=True,
            view=self.__dict__.get("_runtime_view_state"),
        )
        if include_summary:
            return self._stress_runtime_summary()
        return {
            "selected_mic": self.selected_mic,
            "requested": True,
        }

    def _stress_open_settings_tab(self, tab_name):
        if not self._settings_dialog_visible():
            self._open_settings()
        tabs = self.__dict__.get("_settings_tabs")
        names = tuple(self.__dict__.get("_settings_tab_names", ()) or ())
        tab_name = str(tab_name or "").strip()
        if tabs is not None and tab_name:
            for index, name in enumerate(names):
                if str(name or "").strip().lower() == tab_name.lower():
                    tabs.setCurrentIndex(index)
                    break
        self._schedule_active_settings_tab_refresh(force=False)
        return {
            "active_tab": self._active_settings_tab_name(),
            "visible": self._settings_dialog_visible(),
        }

    def _stress_close_settings(self):
        dialog = self.__dict__.get("settings_dialog")
        if dialog is not None:
            dialog.hide()
        return {"visible": self._settings_dialog_visible()}

    def _open_settings(self):
        if not self._module_enabled("settings_ui"):
            QMessageBox.information(
                self,
                "Settings disabled",
                "The settings module is currently disabled for diagnostics.",
            )
            return
        was_visible = self._settings_dialog_visible()
        if not was_visible:
            self.settings_dialog.show()
        self.settings_dialog.raise_()
        if not was_visible:
            self._schedule_active_settings_tab_refresh(force=False)

    def _active_settings_tab_name(self):
        tabs = self.__dict__.get("_settings_tabs")
        if tabs is None:
            return ""
        try:
            index = tabs.currentIndex()
        except Exception:
            return ""
        names = self.__dict__.get("_settings_tab_names", ())
        if 0 <= index < len(names):
            return str(names[index] or "")
        try:
            return str(tabs.tabText(index) or "")
        except Exception:
            return ""

    def _refresh_settings_tab_by_name(self, tab_name):
        tab_name = str(tab_name or "").strip()
        if tab_name == "Hidden":
            self._refresh_hidden_list()
        elif tab_name == "Scenes":
            if not self._module_enabled("scenes"):
                return
            self._refresh_scenes_tab()
        elif tab_name == "Health":
            if not self._module_enabled("health"):
                return
            self._refresh_system_tab(preflight=self._startup_preflight)
        elif tab_name == "Advanced":
            self._refresh_advanced_tab()
        elif tab_name == "Updates":
            if not self._module_enabled("updates"):
                return
            self._refresh_update_tab()

    def _install_state_cache_is_stale(self, *, max_age_s=5.0):
        stamp = float(self.__dict__.get("_install_state_cache_at", 0.0) or 0.0)
        if stamp <= 0.0:
            return True
        return (time.monotonic() - stamp) >= max(0.0, float(max_age_s or 0.0))

    def _invalidate_install_state_cache(self):
        self._install_state_cache = None
        self._install_state_cache_at = 0.0

    def _cached_install_state(self, *, target_tabs=(), max_age_s=5.0, allow_async=True):
        state = self.__dict__.get("_install_state_cache")
        if (
            allow_async
            and "_install_state_refresh_queue" in self.__dict__
            and (state is None or self._install_state_cache_is_stale(max_age_s=max_age_s))
        ):
            self._schedule_install_state_refresh(
                target_tabs=target_tabs,
                force=(state is None),
            )
        return state

    def _schedule_install_state_refresh(self, *, target_tabs=(), force=False):
        pending_tabs = set(self.__dict__.get("_install_state_refresh_tabs", set()) or set())
        pending_tabs.update(
            str(tab_name or "").strip()
            for tab_name in (target_tabs or ())
            if str(tab_name or "").strip()
        )
        self._install_state_refresh_tabs = pending_tabs
        if self.__dict__.get("_install_state_refresh_inflight", False):
            return
        cached = self.__dict__.get("_install_state_cache")
        if not force and cached is not None and not self._install_state_cache_is_stale():
            return
        self._install_state_refresh_inflight = True
        poll_timer = self.__dict__.get("_install_state_refresh_poll_timer")
        if poll_timer is not None and not poll_timer.isActive():
            poll_timer.start()
        threading.Thread(
            target=self._load_install_state_refresh_worker,
            name="wavelinux-install-state",
            daemon=True,
        ).start()

    def _load_install_state_refresh_worker(self):
        queue_obj = self.__dict__.get("_install_state_refresh_queue")
        if queue_obj is None:
            return
        try:
            state = install_state()
        except Exception as exc:
            queue_obj.put(("error", str(exc)))
        else:
            queue_obj.put(("result", state))

    def _poll_install_state_refresh(self):
        queue_obj = self.__dict__.get("_install_state_refresh_queue")
        if queue_obj is None:
            return
        handled_result = False
        while True:
            try:
                kind, payload = queue_obj.get_nowait()
            except queue.Empty:
                break
            handled_result = True
            self._install_state_refresh_inflight = False
            if kind == "result":
                self._install_state_cache = payload
                self._install_state_cache_at = time.monotonic()
            else:
                logging.warning("Install-state refresh failed: %s", payload)
        if not handled_result and self.__dict__.get("_install_state_refresh_inflight", False):
            return
        timer = self.__dict__.get("_install_state_refresh_poll_timer")
        if timer is not None:
            timer.stop()
        if handled_result:
            self._apply_install_state_refresh()

    def _apply_install_state_refresh(self):
        state = self.__dict__.get("_install_state_cache")
        if state is None:
            return
        pending_tabs = set(self.__dict__.get("_install_state_refresh_tabs", set()) or set())
        self._install_state_refresh_tabs = set()
        if not self._settings_dialog_visible():
            return
        active_tab = self._active_settings_tab_name()
        if active_tab == "Updates" and "Updates" in pending_tabs:
            self._refresh_update_tab(state=state, allow_async=False)
        elif active_tab == "Health" and "Health" in pending_tabs:
            self._refresh_system_tab(
                preflight=self._startup_preflight,
                state=state,
                allow_async=False,
            )

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
        tab_name = str(tab_name or "").strip()
        if not tab_name:
            return
        refreshed = dict(self.__dict__.get("_settings_tab_last_refresh_at", {}) or {})
        refreshed[tab_name] = time.monotonic()
        self._settings_tab_last_refresh_at = refreshed

    def _mark_settings_tab_stale(self, tab_name):
        tab_name = str(tab_name or "").strip()
        if not tab_name:
            return
        refreshed = dict(self.__dict__.get("_settings_tab_last_refresh_at", {}) or {})
        refreshed.pop(tab_name, None)
        self._settings_tab_last_refresh_at = refreshed

    def _refresh_active_settings_tab(self, *, force=False):
        if not force and not self._settings_dialog_visible():
            return
        tab_name = self._active_settings_tab_name()
        if not self._settings_tab_refresh_is_stale(tab_name, force=force):
            return
        self._refresh_settings_tab_by_name(tab_name)
        self._mark_settings_tab_refreshed(tab_name)

    def _schedule_active_settings_tab_refresh(self, *, force=False):
        if not force and not self._settings_dialog_visible():
            return
        tab_name = self._active_settings_tab_name()
        if not tab_name:
            return
        if not self._settings_tab_refresh_is_stale(tab_name, force=force):
            return
        self._pending_settings_tab_refresh = tab_name
        timer = self.__dict__.get("_settings_tab_refresh_timer")
        if timer is not None:
            timer.start()
        else:
            self._apply_scheduled_settings_tab_refresh()

    def _apply_scheduled_settings_tab_refresh(self):
        if not self._settings_dialog_visible():
            return
        pending = str(self.__dict__.get("_pending_settings_tab_refresh", "") or "").strip()
        active = self._active_settings_tab_name()
        if pending and pending != active:
            return
        self._pending_settings_tab_refresh = ""
        self._refresh_active_settings_tab(force=False)

    def _on_settings_tab_changed(self, index):
        del index
        self._schedule_active_settings_tab_refresh(force=False)

    @staticmethod
    def _dedupe_names(values):
        seen = set()
        ordered = []
        for value in values or ():
            if not isinstance(value, str):
                continue
            clean = re.sub(r"\s+", " ", value).strip()
            if not clean or clean in seen:
                continue
            seen.add(clean)
            ordered.append(clean)
        return ordered

    @staticmethod
    def _scene_owner_from_key(key):
        if not isinstance(key, str) or "_" not in key:
            return str(key or "")
        return key.rsplit("_", 1)[0]

    @staticmethod
    def _normalize_mix_volume(value, default=1.0):
        try:
            numeric = float(value)
        except (TypeError, ValueError):
            numeric = float(default)
        return max(0.0, min(1.0, numeric))

    def _current_mix_master_volume(self, mix_name, default=1.0):
        desired = self.__dict__.get("_desired_mix_volumes", {}).get(mix_name)
        if desired is not None:
            return self._normalize_mix_volume(desired, default)
        slider_name = "mon_master_slider" if mix_name == "Monitor" else "str_master_slider"
        slider = self.__dict__.get(slider_name)
        if slider is not None and hasattr(slider, "value"):
            return self._normalize_mix_volume(slider.value() / 100.0, default)
        return self._normalize_mix_volume(default, default)

    def _set_mix_master_volume(self, mix_name, volume, *, persist=True, update_slider=False):
        normalized = self._normalize_mix_volume(volume)
        desired = self.__dict__.get("_desired_mix_volumes")
        if desired is None:
            self._desired_mix_volumes = {"Monitor": 1.0, "Stream": 1.0}
            desired = self._desired_mix_volumes
        desired[mix_name] = normalized
        if update_slider:
            slider_name = "mon_master_slider" if mix_name == "Monitor" else "str_master_slider"
            slider = self.__dict__.get(slider_name)
            if slider is not None and hasattr(slider, "setValue"):
                slider_value = int(round(normalized * 100))
                if not getattr(slider, "isSliderDown", lambda: False)():
                    slider.blockSignals(True)
                    slider.setValue(slider_value)
                    slider.blockSignals(False)
        if persist:
            self.schedule_save()

    def _capture_scene_snapshot(self):
        submixes = {}
        for key, value in (self.submix_state or {}).items():
            if not isinstance(key, str):
                continue
            if isinstance(value, dict):
                submixes[key] = {
                    "vol": float(value.get("vol", 1.0)),
                    "mute": bool(value.get("mute", False)),
                }
            elif key.endswith("_linked"):
                submixes[key] = bool(value)
        return {
            "saved_at": int(time.time()),
            "selected_mic": self.selected_mic or None,
            "selected_mic_id": self._stable_source_id_for_name(self.selected_mic) if self.selected_mic else "",
            "monitor_hw": self._desired_mix_hw.get("Monitor"),
            "monitor_hw_id": self._stable_sink_id_for_name(self._desired_mix_hw.get("Monitor")),
            "stream_hw": self._desired_mix_hw.get("Stream"),
            "monitor_mix_volume": self._current_mix_master_volume("Monitor"),
            "stream_mix_volume": self._current_mix_master_volume("Stream"),
            "virtual_channels": list(self._dedupe_names(self.virtual_channels)),
            "channel_order": self._dedupe_names(self.channel_order),
            "submixes": submixes,
            "app_routing": {
                k: v for k, v in (self.app_routing or {}).items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            "app_volumes": {
                k: v for k, v in (self._normalize_app_volume_prefs(
                    getattr(self, "app_volumes", {}),
                ) or {}).items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            "active_effects": {
                k: list(v) for k, v in (self.active_effects or {}).items()
                if isinstance(k, str) and isinstance(v, list)
            },
            "effect_params": {
                node_name: {
                    effect_id: dict(values)
                    for effect_id, values in (effects or {}).items()
                    if isinstance(effect_id, str) and isinstance(values, dict)
                }
                for node_name, effects in (self.effect_params or {}).items()
                if isinstance(node_name, str) and isinstance(effects, dict)
            },
        }

    @classmethod
    def _normalize_app_volume_prefs(cls, raw):
        if not isinstance(raw, dict):
            return {}
        cleaned = {}
        for app_id, value in raw.items():
            if not isinstance(app_id, str) or not PipeWireEngine.is_persistent_app_id(app_id):
                continue
            try:
                cleaned[app_id] = max(0.0, min(float(value), 1.0))
            except (TypeError, ValueError):
                continue
        return cleaned

    @classmethod
    def _normalize_scene_snapshot(cls, raw):
        if not isinstance(raw, dict):
            return None
        submixes = {}
        for key, value in (raw.get("submixes", {}) or {}).items():
            if not isinstance(key, str):
                continue
            if isinstance(value, dict):
                submixes[key] = {
                    "vol": float(value.get("vol", 1.0)),
                    "mute": bool(value.get("mute", False)),
                }
            elif key.endswith("_linked"):
                submixes[key] = bool(value)
        effect_params = {}
        for node_name, effects in (raw.get("effect_params", {}) or {}).items():
            if not isinstance(node_name, str) or not isinstance(effects, dict):
                continue
            clean_effects = {}
            for effect_id, values in effects.items():
                if isinstance(effect_id, str) and isinstance(values, dict):
                    clean_effects[effect_id] = {
                        str(param): float(val)
                        for param, val in values.items()
                        if isinstance(param, str) and isinstance(val, (int, float))
                    }
            effect_params[node_name] = clean_effects
        active_effects = {
            node_name: [str(effect_id) for effect_id in effects if isinstance(effect_id, str)]
            for node_name, effects in (raw.get("active_effects", {}) or {}).items()
            if isinstance(node_name, str) and isinstance(effects, list)
        }
        return {
            "saved_at": int(raw.get("saved_at") or time.time()),
            "selected_mic": raw.get("selected_mic") or None,
            "selected_mic_id": str(raw.get("selected_mic_id") or "").strip(),
            "monitor_hw": raw.get("monitor_hw"),
            "monitor_hw_id": str(raw.get("monitor_hw_id") or "").strip(),
            "stream_hw": raw.get("stream_hw"),
            "monitor_mix_volume": cls._normalize_mix_volume(raw.get("monitor_mix_volume", 1.0)),
            "stream_mix_volume": cls._normalize_mix_volume(raw.get("stream_mix_volume", 1.0)),
            "virtual_channels": cls._dedupe_names(raw.get("virtual_channels", []) or []),
            "channel_order": cls._dedupe_names(raw.get("channel_order", []) or []),
            "submixes": submixes,
            "app_routing": {
                k: v for k, v in (raw.get("app_routing", {}) or {}).items()
                if isinstance(k, str) and PipeWireEngine.is_persistent_app_id(k)
            },
            "app_volumes": cls._normalize_app_volume_prefs(raw.get("app_volumes", {})),
            "active_effects": active_effects,
            "effect_params": effect_params,
        }

    @classmethod
    def _normalize_scene_library(cls, raw):
        if not isinstance(raw, dict):
            return {}
        scenes = {}
        for name, snapshot in raw.items():
            if not isinstance(name, str):
                continue
            clean_name = re.sub(r"\s+", " ", name).strip()
            if not clean_name:
                continue
            normalized = cls._normalize_scene_snapshot(snapshot)
            if normalized is not None:
                scenes[clean_name] = normalized
        return scenes

    def _selected_scene_name(self):
        combo = getattr(self, "_scene_combo", None)
        if combo is None or combo.count() <= 0:
            return ""
        return (combo.currentData() or combo.currentText() or "").strip()

    def _scene_summary_text(self, snapshot):
        snapshot = self._normalize_scene_snapshot(snapshot)
        if not snapshot:
            return "No saved scenes yet."
        saved_at = int(snapshot.get("saved_at") or 0)
        stamp = time.strftime("%Y-%m-%d %H:%M", time.localtime(saved_at)) if saved_at else "unknown"
        selected_mic = snapshot.get("selected_mic") or "system default mic"
        return (
            f"Saved {stamp} · mic: {selected_mic} · "
            f"channels: {len(snapshot.get('virtual_channels', []))} · "
            f"routes: {len(snapshot.get('app_routing', {}))} · "
            f"FX chains: {len(snapshot.get('active_effects', {}))}"
        )

    def _on_scene_selection_change(self, _index):
        self._refresh_scenes_tab()

    def _refresh_scenes_tab(self, selected_name=None):
        if not self._module_enabled("scenes"):
            return
        combo = getattr(self, "_scene_combo", None)
        summary_lbl = getattr(self, "_scene_summary_lbl", None)
        if combo is None or summary_lbl is None:
            return
        current = (selected_name or combo.currentData() or combo.currentText() or "").strip()
        names = list(self.scenes.keys())
        combo.blockSignals(True)
        combo.clear()
        for name in names:
            combo.addItem(name, name)
        if current:
            idx = combo.findData(current)
            if idx >= 0:
                combo.setCurrentIndex(idx)
        combo.blockSignals(False)
        selected = self._selected_scene_name()
        snapshot = self.scenes.get(selected)
        summary_lbl.setText(self._scene_summary_text(snapshot))
        has_scene = bool(snapshot)
        for attr in (
            "_apply_scene_btn",
            "_overwrite_scene_btn",
            "_rename_scene_btn",
            "_delete_scene_btn",
        ):
            btn = getattr(self, attr, None)
            if btn is not None:
                btn.setEnabled(has_scene)
        self._mark_settings_tab_refreshed("Scenes")

    def _apply_scene_snapshot(self, snapshot, *, scene_name=""):
        snapshot = self._normalize_scene_snapshot(snapshot)
        if snapshot is None:
            return False
        scene_virtuals = list(snapshot.get("virtual_channels", []))
        existing_virtuals = [name for name in self.virtual_channels if name not in scene_virtuals]
        self.virtual_channels = scene_virtuals + existing_virtuals
        for name in scene_virtuals:
            self.runtime.ensure_virtual_channel_sync(name, refresh=False)

        selected_mic = (
            self._resolve_hardware_source_name(snapshot.get("selected_mic_id"))
            or self._resolve_hardware_source_name(snapshot.get("selected_mic"))
            or snapshot.get("selected_mic")
            or None
        )
        if selected_mic:
            self._set_selected_mic_target(
                selected_mic,
                record_preference=True,
                persist=False,
                request_refresh=False,
                view=self.__dict__.get("_runtime_view_state"),
            )
        else:
            self.selected_mic = None
            self._mic_selection_initialized = False

        scene_nodes = set(snapshot.get("channel_order", []) or [])
        scene_nodes.update(snapshot.get("active_effects", {}).keys())
        scene_nodes.update(snapshot.get("effect_params", {}).keys())
        scene_nodes.update(
            self._scene_owner_from_key(key)
            for key in (snapshot.get("submixes", {}) or {}).keys()
        )
        if selected_mic:
            scene_nodes.add(selected_mic)

        self.submix_state = {
            key: value
            for key, value in self.submix_state.items()
            if self._scene_owner_from_key(key) not in scene_nodes
        }
        self.submix_state.update(snapshot.get("submixes", {}))

        self.active_effects = {
            key: value
            for key, value in self.active_effects.items()
            if key not in scene_nodes
        }
        self.active_effects.update(snapshot.get("active_effects", {}))

        self.effect_params = {
            key: value
            for key, value in self.effect_params.items()
            if key not in scene_nodes
        }
        self.effect_params.update(snapshot.get("effect_params", {}))

        self.app_routing = dict(snapshot.get("app_routing", {}))
        self.app_volumes = self._normalize_app_volume_prefs(snapshot.get("app_volumes", {}))
        scene_order = list(snapshot.get("channel_order", []))
        self.channel_order = scene_order + [
            name for name in self.channel_order
            if name not in scene_order
        ]
        self._set_mix_master_volume(
            "Monitor",
            snapshot.get("monitor_mix_volume", 1.0),
            persist=False,
            update_slider=True,
        )
        self._set_mix_master_volume(
            "Stream",
            snapshot.get("stream_mix_volume", 1.0),
            persist=False,
            update_slider=True,
        )
        self._set_mix_output_target(
            "Monitor",
            (
                self._resolve_hardware_sink_name(snapshot.get("monitor_hw_id"))
                or self._resolve_hardware_sink_name(snapshot.get("monitor_hw"))
                or snapshot.get("monitor_hw")
            ),
            persist=False,
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        self._record_preferred_monitor(self.__dict__.get("_desired_mix_hw", {}).get("Monitor"), view=self.__dict__.get("_runtime_view_state"))
        self._set_mix_output_target(
            "Stream",
            snapshot.get("stream_hw"),
            persist=False,
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        self._sync_runtime_persistent_state(immediate=True)
        self.schedule_save()
        if hasattr(self, "_refresh_hidden_list"):
            self._refresh_hidden_list()
        if hasattr(self, "_refresh_advanced_tab"):
            self._refresh_advanced_tab()
        if hasattr(self, "_refresh_scenes_tab"):
            self._refresh_scenes_tab(scene_name or self._selected_scene_name())
        if hasattr(self, "_refresh"):
            self._refresh()
        label = scene_name or "scene"
        self.status_lbl.setText(f"Applied {label}")
        return True

    def _save_current_scene_as(self):
        current_name = self._selected_scene_name()
        text, ok = QInputDialog.getText(
            self.settings_dialog,
            "Save Scene",
            "Scene name:",
            text=current_name,
        )
        if not ok:
            return
        name = re.sub(r"\s+", " ", text).strip()
        if not name:
            return
        if name in self.scenes:
            yn = QMessageBox.question(
                self.settings_dialog,
                "Overwrite Scene",
                f"Replace the existing scene '{name}'?",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if yn != QMessageBox.StandardButton.Yes:
                return
        self.scenes[name] = self._capture_scene_snapshot()
        self.save_config()
        self._refresh_scenes_tab(name)
        self.status_lbl.setText(f"Saved scene: {name}")

    def _overwrite_selected_scene(self):
        name = self._selected_scene_name()
        if not name:
            return
        self.scenes[name] = self._capture_scene_snapshot()
        self.save_config()
        self._refresh_scenes_tab(name)
        self.status_lbl.setText(f"Updated scene: {name}")

    def _apply_selected_scene(self):
        name = self._selected_scene_name()
        if not name:
            return
        self._apply_scene_snapshot(self.scenes.get(name), scene_name=name)

    def _rename_selected_scene(self):
        current = self._selected_scene_name()
        if not current:
            return
        text, ok = QInputDialog.getText(
            self.settings_dialog,
            "Rename Scene",
            "Scene name:",
            text=current,
        )
        if not ok:
            return
        new_name = re.sub(r"\s+", " ", text).strip()
        if not new_name or new_name == current:
            return
        if new_name in self.scenes:
            yn = QMessageBox.question(
                self.settings_dialog,
                "Replace Scene",
                f"Replace the existing scene '{new_name}'?",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if yn != QMessageBox.StandardButton.Yes:
                return
        snapshot = self.scenes.pop(current)
        self.scenes[new_name] = snapshot
        self.save_config()
        self._refresh_scenes_tab(new_name)
        self.status_lbl.setText(f"Renamed scene: {new_name}")

    def _delete_selected_scene(self):
        name = self._selected_scene_name()
        if not name:
            return
        yn = QMessageBox.question(
            self.settings_dialog,
            "Delete Scene",
            f"Delete the scene '{name}'?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        self.scenes.pop(name, None)
        self.save_config()
        self._refresh_scenes_tab()
        self.status_lbl.setText(f"Deleted scene: {name}")

    def _preferred_setup_mic(self):
        if self.selected_mic:
            return self.selected_mic
        view = self.__dict__.get("_runtime_view_state")
        default_source = getattr(view, "default_source", None) if view is not None else None
        if default_source:
            resolved = self._resolve_hardware_source_name(default_source)
            if resolved:
                return resolved
        mics = list(getattr(view, "mic_inputs", []) or []) if view is not None else []
        if mics:
            return getattr(mics[0], "name", "") or None
        return self._resolve_startup_mic_target()

    @staticmethod
    def _sink_stable_id_from_row(row):
        return str(getattr(row, "stable_id", "") or "").strip()

    @staticmethod
    def _source_stable_id_from_row(row):
        return str(getattr(row, "stable_id", "") or "").strip()

    def _hardware_sink_rows(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        return [
            sink for sink in (getattr(view, "sinks", []) or [])
            if not getattr(sink, "is_internal", False)
            and not str(getattr(sink, "name", "") or "").startswith("wavelinux_")
        ]

    def _hardware_source_rows(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        return list(getattr(view, "mic_inputs", []) or [])

    def _sink_row_for_name(self, sink_name, view=None):
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            return None
        for sink in self._hardware_sink_rows(view):
            if str(getattr(sink, "name", "") or "").strip() == sink_name:
                return sink
        return None

    def _source_row_for_name(self, source_name, view=None):
        source_name = str(source_name or "").strip()
        if not source_name:
            return None
        for source in self._hardware_source_rows(view):
            if str(getattr(source, "name", "") or "").strip() == source_name:
                return source
        return None

    def _sink_row_for_stable_id(self, stable_id, view=None):
        stable_id = str(stable_id or "").strip().lower()
        if not stable_id:
            return None
        for sink in self._hardware_sink_rows(view):
            if self._sink_stable_id_from_row(sink).lower() == stable_id:
                return sink
        return None

    def _source_row_for_stable_id(self, stable_id, view=None):
        stable_id = str(stable_id or "").strip().lower()
        if not stable_id:
            return None
        for source in self._hardware_source_rows(view):
            if self._source_stable_id_from_row(source).lower() == stable_id:
                return source
        return None

    def _display_name_for_sink_name(self, sink_name, view=None):
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            return ""
        row = self._sink_row_for_name(sink_name, view=view)
        if row is not None:
            return str(getattr(row, "display_name", "") or sink_name)
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "display_name_for_sink"):
            return engine.display_name_for_sink(sink_name)
        return sink_name

    def _stable_sink_id_for_name(self, sink_name):
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "stable_sink_id"):
            return engine.stable_sink_id(sink_name)
        return PipeWireEngine.stable_sink_id(sink_name)

    def _stable_source_id_for_name(self, source_name):
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "stable_source_id"):
            return engine.stable_source_id(source_name)
        return PipeWireEngine._stable_device_id_from_props(
            "source",
            source_name,
            {},
            source=True,
        )

    def _resolve_hardware_sink_name(self, sink_name):
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "resolve_hardware_sink_name"):
            return engine.resolve_hardware_sink_name(sink_name)
        return str(sink_name or "").strip() or None

    def _resolve_hardware_source_name(self, source_name):
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "resolve_hardware_source_name"):
            return engine.resolve_hardware_source_name(source_name)
        return str(source_name or "").strip() or None

    def _display_name_for_source_name(self, source_name, view=None):
        source_name = str(source_name or "").strip()
        if not source_name:
            return ""
        row = self._source_row_for_name(source_name, view=view)
        if row is not None:
            return str(getattr(row, "label", "") or getattr(row, "description", "") or source_name)
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "display_name_for_source"):
            return engine.display_name_for_source(source_name) or source_name
        return PipeWireEngine.friendly_name(source_name) or source_name

    def _visible_default_sink(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        default_sink = getattr(view, "default_sink", None) if view is not None else None
        engine = self.__dict__.get("engine")
        if not default_sink and engine is not None and hasattr(engine, "get_default_sink"):
            default_sink = engine.get_default_sink()
        if not default_sink:
            return None
        resolved = self._resolve_hardware_sink_name(default_sink)
        if resolved and (view is None or self._sink_row_for_name(resolved, view=view)):
            return resolved
        return None

    def _visible_default_source(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        default_source = getattr(view, "default_source", None) if view is not None else None
        engine = self.__dict__.get("engine")
        if not default_source and engine is not None and hasattr(engine, "get_default_source"):
            default_source = engine.get_default_source()
        if not default_source:
            return None
        resolved = self._resolve_hardware_source_name(default_source)
        if resolved and (view is None or self._source_row_for_name(resolved, view=view)):
            return resolved
        return None

    def _resolve_startup_monitor_target(self, view=None):
        default_sink = self._visible_default_sink(view=view)
        if default_sink:
            return default_sink
        rows = self._hardware_sink_rows(view=view)
        if rows:
            return str(getattr(rows[0], "name", "") or "") or None
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "stable_sink_inventory"):
            inventory = list(engine.stable_sink_inventory() or [])
            if inventory:
                return str(inventory[0].get("name") or "") or None
        return None

    def _resolve_startup_mic_target(self, view=None):
        default_source = self._visible_default_source(view=view)
        if default_source:
            return default_source
        rows = self._hardware_source_rows(view=view)
        if rows:
            return str(getattr(rows[0], "name", "") or "") or None
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "stable_source_inventory"):
            inventory = list(engine.stable_source_inventory() or [])
            if inventory:
                return str(inventory[0].get("name") or "") or None
        return None

    def _record_preferred_monitor(self, sink_name, *, view=None):
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            self._preferred_monitor_hw_id = ""
            self._preferred_monitor_hw_name = ""
            self._active_monitor_fallback = False
            self._restorable_monitor_hw_id = ""
            self._restorable_monitor_hw_name = ""
            return
        resolved = self._resolve_hardware_sink_name(sink_name) or sink_name
        row = self._sink_row_for_name(resolved, view=view)
        stable_id = self._sink_stable_id_from_row(row)
        if not stable_id:
            stable_id = self._stable_sink_id_for_name(resolved)
        self._preferred_monitor_hw_name = resolved
        self._preferred_monitor_hw_id = stable_id
        self._last_good_monitor_hw_name = resolved
        self._last_good_monitor_hw_id = stable_id
        self._active_monitor_fallback = False
        self._restorable_monitor_hw_id = ""
        self._restorable_monitor_hw_name = ""

    def _record_preferred_mic(self, source_name, *, view=None):
        source_name = str(source_name or "").strip()
        if not source_name:
            self._preferred_selected_mic_id = ""
            self._preferred_selected_mic_name = ""
            self._active_mic_fallback = False
            self._restorable_selected_mic_id = ""
            self._restorable_selected_mic_name = ""
            return
        resolved = self._resolve_hardware_source_name(source_name) or source_name
        row = self._source_row_for_name(resolved, view=view)
        stable_id = self._source_stable_id_from_row(row)
        if not stable_id:
            stable_id = self._stable_source_id_for_name(resolved)
        self._preferred_selected_mic_name = resolved
        self._preferred_selected_mic_id = stable_id
        self._last_good_selected_mic_name = resolved
        self._last_good_selected_mic_id = stable_id
        self._active_mic_fallback = False
        self._restorable_selected_mic_id = ""
        self._restorable_selected_mic_name = ""

    def _resolve_monitor_fallback(self, view=None):
        rows = self._hardware_sink_rows(view=view)
        if not rows:
            return None
        default_sink = self._visible_default_sink(view=view)
        if default_sink:
            return default_sink
        last_good_monitor_hw_id = self.__dict__.get("_last_good_monitor_hw_id", "")
        if last_good_monitor_hw_id:
            row = self._sink_row_for_stable_id(last_good_monitor_hw_id, view=view)
            if row is not None:
                return getattr(row, "name", None)
        return getattr(rows[0], "name", None)

    def _resolve_mic_fallback(self, view=None):
        rows = self._hardware_source_rows(view=view)
        if not rows:
            return None
        default_source = self._visible_default_source(view=view)
        if default_source:
            return default_source
        last_good_selected_mic_id = self.__dict__.get("_last_good_selected_mic_id", "")
        if last_good_selected_mic_id:
            row = self._source_row_for_stable_id(last_good_selected_mic_id, view=view)
            if row is not None:
                return getattr(row, "name", None)
        return getattr(rows[0], "name", None)

    def _normalize_effect_request_for_node(self, node_name, active_effects, params_map):
        wanted = [str(fid) for fid in list(active_effects or []) if fid]
        normalized = {
            str(fid): dict(values or {})
            for fid, values in dict(params_map or {}).items()
        }
        return wanted, normalized

    def _normalize_loaded_effect_state(self):
        normalized_effects = {}
        normalized_params = {}
        node_names = set((self.active_effects or {}).keys()) | set((self.effect_params or {}).keys())
        for node_name in node_names:
            wanted, params_map = self._normalize_effect_request_for_node(
                node_name,
                (self.active_effects or {}).get(node_name, []),
                (self.effect_params or {}).get(node_name, {}),
            )
            if wanted:
                normalized_effects[node_name] = wanted
            if params_map:
                normalized_params[node_name] = params_map
        self.active_effects = normalized_effects
        self.effect_params = normalized_params

    def _set_selected_mic_target(self, mic_name, *, record_preference=False, persist=True, request_refresh=True, view=None):
        mic_name = str(mic_name or "").strip() or None
        if mic_name != self.__dict__.get("selected_mic"):
            self._last_selected_mic_change_at = time.monotonic()
        self.selected_mic = mic_name
        self._mic_selection_initialized = bool(mic_name)
        runtime = getattr(self, "runtime", None)
        if runtime is not None:
            self._suppress_pactl_events_for(1.5)
            runtime.set_selected_mic(mic_name)
        if record_preference and mic_name:
            self._record_preferred_mic(mic_name, view=view)
        if persist:
            self.schedule_save()
        if request_refresh:
            self._request_runtime_refresh("selected-mic-change")

    def _restore_preferred_monitor(self):
        target = str(
            self.__dict__.get("_preferred_monitor_hw_name", "")
            or self.__dict__.get("_restorable_monitor_hw_name", "")
            or ""
        ).strip()
        if not target:
            return
        resolved = self._resolve_hardware_sink_name(
            self.__dict__.get("_preferred_monitor_hw_id", "") or target
        ) or self._resolve_hardware_sink_name(target)
        if not resolved:
            QMessageBox.information(
                self.settings_dialog,
                "Monitor device unavailable",
                "WaveLinux could not find the preferred monitor device right now.",
            )
            return
        self._set_mix_output_target(
            "Monitor",
            resolved,
            persist=True,
            update_combo=True,
            sync_runtime=True,
        )
        self._record_preferred_monitor(resolved, view=self.__dict__.get("_runtime_view_state"))
        self.status_lbl.setText(f"Restored monitor device: {self._display_name_for_sink_name(resolved)}")
        self._refresh_system_tab(preflight=self._startup_preflight)

    def _restore_preferred_mic(self):
        target = str(
            self.__dict__.get("_preferred_selected_mic_name", "")
            or self.__dict__.get("_restorable_selected_mic_name", "")
            or ""
        ).strip()
        if not target:
            return
        resolved = self._resolve_hardware_source_name(
            self.__dict__.get("_preferred_selected_mic_id", "") or target
        ) or self._resolve_hardware_source_name(target)
        if not resolved:
            QMessageBox.information(
                self.settings_dialog,
                "Microphone unavailable",
                "WaveLinux could not find the preferred microphone right now.",
            )
            return
        self._set_selected_mic_target(
            resolved,
            record_preference=True,
            persist=True,
            request_refresh=True,
            view=self.__dict__.get("_runtime_view_state"),
        )
        self.status_lbl.setText(f"Restored microphone device: {self._display_name_for_source_name(resolved)}")
        self._refresh_system_tab(preflight=self._startup_preflight)

    def _reconcile_device_policy(self, view=None):
        if not self._module_enabled("device_policy"):
            return False
        view = view or self.__dict__.get("_runtime_view_state")
        if view is None:
            return False
        changed = False
        preferred_monitor_hw_name = self.__dict__.get("_preferred_monitor_hw_name", "")
        preferred_monitor_hw_id = self.__dict__.get("_preferred_monitor_hw_id", "")
        active_monitor_fallback = bool(self.__dict__.get("_active_monitor_fallback", False))
        preferred_selected_mic_name = self.__dict__.get("_preferred_selected_mic_name", "")
        preferred_selected_mic_id = self.__dict__.get("_preferred_selected_mic_id", "")
        active_mic_fallback = bool(self.__dict__.get("_active_mic_fallback", False))

        desired_mix_hw = self.__dict__.get("_desired_mix_hw", {}) or {}
        active_monitor = desired_mix_hw.get("Monitor") or getattr(view.mixes.get("Monitor"), "hardware_sink", None)
        resolved_monitor = self._resolve_hardware_sink_name(active_monitor) if active_monitor else None
        active_monitor_row = self._sink_row_for_name(resolved_monitor or active_monitor, view=view)
        if active_monitor_row is not None:
            active_monitor_name = str(getattr(active_monitor_row, "name", "") or "")
            active_monitor_id = self._sink_stable_id_from_row(active_monitor_row)
            self._last_good_monitor_hw_name = active_monitor_name
            self._last_good_monitor_hw_id = active_monitor_id
            if not preferred_monitor_hw_name:
                self._preferred_monitor_hw_name = active_monitor_name
                self._preferred_monitor_hw_id = active_monitor_id
                preferred_monitor_hw_name = active_monitor_name
                preferred_monitor_hw_id = active_monitor_id
            preferred_monitor_row = self._sink_row_for_stable_id(preferred_monitor_hw_id, view=view)
            if preferred_monitor_row is not None and active_monitor_name == getattr(preferred_monitor_row, "name", None):
                self._active_monitor_fallback = False
                self._restorable_monitor_hw_id = ""
                self._restorable_monitor_hw_name = ""
            elif active_monitor_fallback and preferred_monitor_row is not None:
                self._restorable_monitor_hw_id = preferred_monitor_hw_id
                self._restorable_monitor_hw_name = getattr(preferred_monitor_row, "name", "") or preferred_monitor_hw_name
        elif active_monitor:
            fallback_monitor = self._resolve_monitor_fallback(view=view)
            if fallback_monitor and fallback_monitor != active_monitor:
                if not active_monitor_fallback:
                    if not preferred_monitor_hw_name:
                        self._preferred_monitor_hw_name = str(active_monitor)
                        preferred_monitor_hw_name = str(active_monitor)
                    if not preferred_monitor_hw_id:
                        self._preferred_monitor_hw_id = self._stable_sink_id_for_name(self._preferred_monitor_hw_name)
                        preferred_monitor_hw_id = self._preferred_monitor_hw_id
                    self._restorable_monitor_hw_name = preferred_monitor_hw_name
                    self._restorable_monitor_hw_id = preferred_monitor_hw_id
                self._active_monitor_fallback = True
                self._set_mix_output_target(
                    "Monitor",
                    fallback_monitor,
                    persist=True,
                    update_combo=True,
                    sync_runtime=True,
                )
                changed = True

        active_mic = str(self.__dict__.get("selected_mic") or "").strip()
        active_mic_row = self._source_row_for_name(active_mic, view=view)
        if active_mic_row is not None:
            active_mic_name = str(getattr(active_mic_row, "name", "") or "")
            active_mic_id = self._source_stable_id_from_row(active_mic_row)
            self._last_good_selected_mic_name = active_mic_name
            self._last_good_selected_mic_id = active_mic_id
            if not preferred_selected_mic_name:
                self._preferred_selected_mic_name = active_mic_name
                self._preferred_selected_mic_id = active_mic_id
                preferred_selected_mic_name = active_mic_name
                preferred_selected_mic_id = active_mic_id
            preferred_mic_row = self._source_row_for_stable_id(preferred_selected_mic_id, view=view)
            if preferred_mic_row is not None and active_mic_name == getattr(preferred_mic_row, "name", None):
                self._active_mic_fallback = False
                self._restorable_selected_mic_id = ""
                self._restorable_selected_mic_name = ""
            elif active_mic_fallback and preferred_mic_row is not None:
                self._restorable_selected_mic_id = preferred_selected_mic_id
                self._restorable_selected_mic_name = getattr(preferred_mic_row, "name", "") or preferred_selected_mic_name
        else:
            fallback_mic = self._resolve_mic_fallback(view=view)
            if fallback_mic and fallback_mic != active_mic:
                if not active_mic_fallback:
                    if not preferred_selected_mic_name:
                        self._preferred_selected_mic_name = active_mic
                        preferred_selected_mic_name = active_mic
                    if not preferred_selected_mic_id:
                        self._preferred_selected_mic_id = self._stable_source_id_for_name(self._preferred_selected_mic_name)
                        preferred_selected_mic_id = self._preferred_selected_mic_id
                    self._restorable_selected_mic_name = preferred_selected_mic_name
                    self._restorable_selected_mic_id = preferred_selected_mic_id
                self._active_mic_fallback = True
                self._set_selected_mic_target(
                    fallback_mic,
                    record_preference=False,
                    persist=True,
                    request_refresh=True,
                    view=view,
                )
                changed = True

        return changed

    def _device_health_issues(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        issues = []
        desired_mix_hw = self.__dict__.get("_desired_mix_hw", {}) or {}
        active_monitor = str(desired_mix_hw.get("Monitor") or "").strip()
        active_mic = str(self.__dict__.get("selected_mic") or "").strip()
        if self.__dict__.get("_active_monitor_fallback", False):
            issues.append(HealthIssue(
                code="device.monitor_fallback_active",
                severity="warning",
                title="Monitor output is running on a fallback device",
                detail=(
                    f"WaveLinux is routing Monitor to {self._display_name_for_sink_name(active_monitor, view=view) or active_monitor} "
                    f"because the preferred device {self._display_name_for_sink_name(self.__dict__.get('_preferred_monitor_hw_name', ''), view=view) or self.__dict__.get('_preferred_monitor_hw_name', '') or 'is unavailable'}."
                ),
                primary_action=(
                    "Restore monitor device"
                    if self.__dict__.get("_restorable_monitor_hw_name", "")
                    else "Re-run device reconcile"
                ),
                secondary_action="Re-run device reconcile" if self.__dict__.get("_restorable_monitor_hw_name", "") else "",
                context={
                    "active_sink": active_monitor,
                    "preferred_sink": self.__dict__.get("_preferred_monitor_hw_name", ""),
                    "restorable_sink": self.__dict__.get("_restorable_monitor_hw_name", ""),
                },
            ))
        if self.__dict__.get("_active_monitor_fallback", False) and self.__dict__.get("_restorable_monitor_hw_name", ""):
            issues.append(HealthIssue(
                code="device.monitor_preferred_restorable",
                severity="info",
                title="Preferred monitor device is available again",
                detail=(
                    f"{self._display_name_for_sink_name(self.__dict__.get('_restorable_monitor_hw_name', ''), view=view) or self.__dict__.get('_restorable_monitor_hw_name', '')} "
                    "has returned. WaveLinux will not switch automatically."
                ),
                primary_action="Restore monitor device",
                secondary_action="Re-run device reconcile",
                context={"sink_name": self.__dict__.get("_restorable_monitor_hw_name", "")},
            ))
        if self.__dict__.get("_active_mic_fallback", False):
            issues.append(HealthIssue(
                code="device.mic_fallback_active",
                severity="warning",
                title="Microphone is running on a fallback device",
                detail=(
                    f"WaveLinux is using {self._display_name_for_source_name(active_mic, view=view) or active_mic} "
                    f"because the preferred microphone {self._display_name_for_source_name(self.__dict__.get('_preferred_selected_mic_name', ''), view=view) or self.__dict__.get('_preferred_selected_mic_name', '') or 'is unavailable'}."
                ),
                primary_action=(
                    "Restore microphone device"
                    if self.__dict__.get("_restorable_selected_mic_name", "")
                    else "Re-run device reconcile"
                ),
                secondary_action="Re-run device reconcile" if self.__dict__.get("_restorable_selected_mic_name", "") else "",
                context={
                    "active_source": active_mic,
                    "preferred_source": self.__dict__.get("_preferred_selected_mic_name", ""),
                    "restorable_source": self.__dict__.get("_restorable_selected_mic_name", ""),
                },
            ))
        if self.__dict__.get("_active_mic_fallback", False) and self.__dict__.get("_restorable_selected_mic_name", ""):
            issues.append(HealthIssue(
                code="device.mic_preferred_restorable",
                severity="info",
                title="Preferred microphone is available again",
                detail=(
                    f"{self._display_name_for_source_name(self.__dict__.get('_restorable_selected_mic_name', ''), view=view) or self.__dict__.get('_restorable_selected_mic_name', '')} "
                    "has returned. WaveLinux will not switch automatically."
                ),
                primary_action="Restore microphone device",
                secondary_action="Re-run device reconcile",
                context={"source_name": self.__dict__.get("_restorable_selected_mic_name", "")},
            ))
        stream_target = str(desired_mix_hw.get("Stream") or "").strip()
        if stream_target and not self._resolve_hardware_sink_name(stream_target):
            issues.append(HealthIssue(
                code="device.stream_target_missing",
                severity="warning",
                title="Stream output target is unavailable",
                detail=(
                    f"WaveLinux cannot currently resolve the explicit Stream target "
                    f"{self._display_name_for_sink_name(stream_target, view=view) or stream_target}. "
                    "Stream does not auto-follow Monitor."
                ),
                primary_action="Re-run device reconcile",
                secondary_action="Open diagnostics",
                context={"sink_name": stream_target},
            ))
        return issues

    def _apply_quick_start_template(self, template_id):
        template = _QUICK_START_TEMPLATES.get(template_id)
        if template is None:
            return False
        template_channels = list(template.get("channels", []) or [])
        self.virtual_channels = self._dedupe_names(template_channels + list(self.virtual_channels))
        template_order = []
        for display_name in template_channels:
            _, safe = PipeWireEngine._sanitize_channel_name(display_name)
            template_order.append(f"wavelinux_{safe}")
        self.channel_order = self._dedupe_names(template_order + list(self.channel_order))
        for display_name in template_channels:
            self.runtime.ensure_virtual_channel_sync(display_name, refresh=False)

        selected_mic = self._preferred_setup_mic()
        if selected_mic:
            self._set_selected_mic_target(
                selected_mic,
                record_preference=True,
                persist=False,
                request_refresh=False,
                view=self.__dict__.get("_runtime_view_state"),
            )
            self.active_effects[selected_mic] = list(template.get("mic_effects", []) or [])

        default_sink = self._resolve_startup_monitor_target()
        if default_sink:
            self._set_mix_output_target(
                "Monitor",
                default_sink,
                persist=False,
                update_combo=True,
                sync_runtime=True,
                sync_runtime_refresh=False,
            )
            self._record_preferred_monitor(default_sink, view=self.__dict__.get("_runtime_view_state"))
        self._selected_setup_template = template_id
        self._onboarding_completed = True
        self._show_first_run_setup = False
        self._sync_runtime_persistent_state(immediate=True)
        self.schedule_save()
        self._refresh()
        self.status_lbl.setText(f"Applied quick start: {template['title']}")
        return True

    def _open_quick_start_setup(self):
        first_run = bool(self._show_first_run_setup and not self._onboarding_completed)
        dlg = QDialog(self)
        dlg.setWindowTitle("Quick Start Setup")
        dlg.setMinimumWidth(480)
        dlg.setStyleSheet(STYLESHEET)
        layout = QVBoxLayout(dlg)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(12)

        title = QLabel("QUICK START")
        title.setObjectName("sectionLabel")
        layout.addWidget(title)

        intro = QLabel(
            "Pick a starter template. You can run this again later from Settings to reshape channels and default mic FX."
        )
        intro.setWordWrap(True)
        intro.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        layout.addWidget(intro)

        combo = QComboBox()
        for template_id, meta in _QUICK_START_TEMPLATES.items():
            combo.addItem(meta["title"], template_id)
        if self._selected_setup_template:
            idx = combo.findData(self._selected_setup_template)
            if idx >= 0:
                combo.setCurrentIndex(idx)
        layout.addWidget(combo)

        desc_lbl = QLabel()
        desc_lbl.setWordWrap(True)
        desc_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        layout.addWidget(desc_lbl)

        def _sync_desc():
            template = _QUICK_START_TEMPLATES.get(combo.currentData(), {})
            channels = ", ".join(template.get("channels", []) or []) or "None"
            mic_fx = ", ".join(template.get("mic_effects", []) or []) or "None"
            desc_lbl.setText(
                f"{template.get('description', '').strip()}\n\n"
                f"Starter channels: {channels}\n"
                f"Mic FX: {mic_fx}"
            )

        combo.currentIndexChanged.connect(_sync_desc)
        _sync_desc()

        buttons = QDialogButtonBox()
        apply_btn = buttons.addButton("Apply Template", QDialogButtonBox.ButtonRole.AcceptRole)
        skip_text = "Skip for now" if first_run else "Cancel"
        skip_btn = buttons.addButton(skip_text, QDialogButtonBox.ButtonRole.RejectRole)
        buttons.accepted.connect(dlg.accept)
        buttons.rejected.connect(dlg.reject)
        layout.addWidget(buttons)

        apply_btn.setDefault(True)
        skip_btn.setAutoDefault(False)

        if dlg.exec() != QDialog.DialogCode.Accepted:
            if first_run:
                self._onboarding_completed = True
                self._show_first_run_setup = False
                self.save_config()
                self.status_lbl.setText("Quick start skipped")
            return
        if self._apply_quick_start_template(combo.currentData()):
            self.save_config()

    def _check_for_updates(self):
        self._check_update_btn.setEnabled(False)
        self._update_status_lbl.setText("Checking for updates…")
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

        # Cancel any in-flight checker so we don't end up with two
        # threads racing into the same queue.
        prev = getattr(self, '_updater', None)
        if prev is not None:
            prev.cancel()

        self._updater = UpdateChecker()
        self._updater.check()
        # Poll the result queue every 200 ms on the Qt thread — avoids
        # calling QTimer.singleShot from a non-Qt thread which is unreliable.
        # Reuse the existing timer instead of constructing a new one each
        # press; otherwise a check / download / re-check sequence stacks
        # multiple QTimers all firing the same slot.
        timer = getattr(self, '_update_poll_timer', None)
        if timer is None:
            timer = QTimer(self)
            timer.setInterval(200)
            timer.timeout.connect(self._poll_updater)
            self._update_poll_timer = timer
        else:
            timer.stop()
        timer.start()

    def _poll_updater(self):
        """Drain the updater queue on the main thread."""
        updater = getattr(self, '_updater', None)
        if updater is None:
            return
        while True:
            item = updater.poll()
            if item is None:
                break
            kind = item[0]
            if kind == 'result':
                self._update_poll_timer.stop()
                self._handle_update_result(item[1])
            elif kind == 'error':
                self._update_poll_timer.stop()
                self._handle_update_error(item[1])

    def _handle_update_result(self, release_info):
        self._check_update_btn.setEnabled(True)
        if not isinstance(release_info, VerifiedReleaseInfo):
            raise TypeError("Expected VerifiedReleaseInfo from update checker")
        latest_tag = str(release_info.version or "").strip()
        self._pending_verified_release = release_info
        self._pending_update_url = release_info.release_url or release_page_url()
        self._pending_update_asset_url = release_info.asset_url or ""
        self._pending_update_asset_name = release_info.asset_name or ""
        self._last_update_check_at = time.time()
        self._last_update_issue = None
        current = _parse_version(APP_VERSION)
        latest  = _parse_version(latest_tag)
        mode, _description, guidance = self._runtime_mode_detail()
        if latest > current:
            self._pending_update_tag = latest_tag
            if self._pending_update_asset_url and mode.allows_self_update:
                self._last_update_attempt_result = f"Verified update available: v{latest_tag}"
                self._update_status_lbl.setText(
                    f"Verified update available: v{latest_tag}  (current: v{APP_VERSION})"
                )
                self._update_status_lbl.setStyleSheet("color: #00d4aa; font-size: 12px; font-weight: bold;")
                self._show_notification(
                    "WaveLinux Update Available",
                    f"Version {latest_tag} is available. Open Settings → Updates to install it.",
                )
            elif self._pending_update_asset_url:
                self._last_update_attempt_result = (
                    f"Verified release v{latest_tag} is available; update this install through your package manager."
                )
                self._update_status_lbl.setText(
                    f"Verified release v{latest_tag} is available. {guidance}"
                )
                self._update_status_lbl.setStyleSheet("color: #d28b26; font-size: 12px;")
            else:
                self._last_update_attempt_result = f"Verified release v{latest_tag} has no eligible AppImage asset."
                self._update_status_lbl.setText(
                    f"Version {latest_tag} is available, but the signed manifest exposes no eligible AppImage asset."
                )
                self._update_status_lbl.setStyleSheet("color: #d28b26; font-size: 12px;")
        else:
            self._last_update_attempt_result = f"Already on the latest verified release: v{APP_VERSION}"
            self._update_status_lbl.setText(f"You're up to date on the latest verified release! (v{APP_VERSION})")
            self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
            self._pending_update_tag = None
            self._pending_update_asset_url = ""
            self._pending_update_asset_name = ""
        self._refresh_update_tab()
        self._refresh_system_tab()

    def _handle_update_error(self, payload):
        self._check_update_btn.setEnabled(True)
        payload = dict(payload or {})
        self._last_update_issue = payload
        self._pending_update_url = str(payload.get("release_url") or release_page_url())
        self._last_update_attempt_result = f"Update check failed: {payload.get('message') or 'unknown error'}"
        message = str(payload.get("message") or "unknown error")
        code = str(payload.get("code") or "")
        style = (
            "color: #d28b26; font-size: 12px;"
            if code in {"update.manifest_missing", "update.asset_missing"}
            else "color: #e05050; font-size: 12px;"
        )
        self._update_status_lbl.setText(f"Update check failed: {message}")
        self._update_status_lbl.setStyleSheet(style)
        self._refresh_update_tab()
        self._refresh_system_tab()

    def _open_release_page(self):
        url = getattr(self, "_pending_update_url", None) or release_page_url()
        QDesktopServices.openUrl(QUrl(url))

    def _download_and_install_update(self):
        mode, _description, guidance = self._runtime_mode_detail()
        if not mode.allows_self_update:
            QMessageBox.information(
                self.settings_dialog,
                "Package-managed install",
                "WaveLinux detected a package-managed install for this runtime.\n\n"
                f"{guidance}",
            )
            return
        progress = self.__dict__.get("_update_progress")
        if progress is not None:
            progress.setVisible(True)
            progress.setRange(0, 0)
            progress.setFormat("Checking latest release…")
        self._download_update_btn.setEnabled(False)
        self._check_update_btn.setEnabled(False)
        self._update_status_lbl.setText("Checking latest verified AppImage release…")
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

        prev = self.__dict__.get("_update_installer")
        if prev is not None:
            prev.cancel()

        # Do not trust a cached release object for a "latest" install.
        # The app can stay open across a new GitHub release; if we reuse a
        # stale cached same-version asset here, the updater just reinstalls the
        # currently running AppImage instead of the new release.
        self._update_installer = AppImageUpdateInstaller()
        self._update_installer.install(release_info=None)

        timer = self.__dict__.get("_update_install_poll_timer")
        if timer is None:
            timer = QTimer(self)
            timer.setInterval(200)
            timer.timeout.connect(self._poll_update_installer)
            self._update_install_poll_timer = timer
        else:
            timer.stop()
        timer.start()

    def _poll_update_installer(self):
        installer = getattr(self, "_update_installer", None)
        if installer is None:
            return
        while True:
            item = installer.poll()
            if item is None:
                break
            kind = item[0]
            if kind == "progress":
                self._handle_update_install_progress(item[1], item[2], item[3])
            elif kind == "status":
                self._update_status_lbl.setText(str(item[1]))
                self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
            elif kind == "installed":
                self._update_install_poll_timer.stop()
                self._handle_update_install_success(item[1], item[2])
            elif kind == "error":
                self._update_install_poll_timer.stop()
                self._handle_update_install_error(item[1])

    def _handle_update_install_progress(self, asset_name, downloaded, total):
        progress = getattr(self, "_update_progress", None)
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
            self._update_status_lbl.setText(
                f"Downloading {asset_name or 'AppImage'}… {percent}%"
            )
        else:
            self._update_status_lbl.setText(
                f"Downloading {asset_name or 'AppImage'}…"
            )
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

    def _handle_update_install_success(self, result, release_info):
        self._download_update_btn.setEnabled(True)
        self._check_update_btn.setEnabled(True)
        rollback_btn = getattr(self, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(True)
        progress = getattr(self, "_update_progress", None)
        if progress is not None:
            progress.setVisible(False)
            progress.setRange(0, 100)
            progress.setValue(0)
        if not isinstance(release_info, VerifiedReleaseInfo):
            raise TypeError("Expected VerifiedReleaseInfo from installer")
        tag = str(release_info.version or "").strip()
        self._pending_verified_release = release_info
        self._pending_update_tag = tag or None
        self._pending_update_url = release_info.release_url or release_page_url()
        self._pending_update_asset_url = release_info.asset_url or ""
        self._pending_update_asset_name = release_info.asset_name or ""
        self._last_update_check_at = time.time()
        self._last_update_issue = None
        version_text = f"v{tag}" if tag else "the latest version"
        self._last_update_attempt_result = (
            f"Installed verified {version_text} to {result.appimage_path}"
            + (f" with backup at {result.backup_path}" if result.backup_path else "")
        )
        self._update_status_lbl.setText(
            f"Installed verified {version_text} to {result.appimage_path}. Restart WaveLinux to run it."
        )
        self._update_status_lbl.setStyleSheet("color: #00d4aa; font-size: 12px; font-weight: bold;")
        self._refresh_update_tab()
        self._refresh_system_tab()

        install_message = (
            "WaveLinux downloaded, verified, and installed the latest AppImage.\n\n"
            f"Installed AppImage: {result.appimage_path}\n"
            + (
                f"Previous AppImage backup: {result.backup_path}\n"
                if result.backup_path else ""
            )
            + f"Launcher: {result.wrapper_path}\n\n"
            "Restart into the updated AppImage now?"
        )

        yn = QMessageBox.question(
            self.settings_dialog,
            "Update installed",
            install_message,
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn == QMessageBox.StandardButton.Yes:
            self._restart_with_command([result.appimage_path])

    def _handle_update_install_error(self, payload):
        self._download_update_btn.setEnabled(True)
        self._check_update_btn.setEnabled(True)
        rollback_btn = getattr(self, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(True)
        progress = getattr(self, "_update_progress", None)
        if progress is not None:
            progress.setVisible(False)
            progress.setRange(0, 100)
            progress.setValue(0)
        payload = dict(payload or {})
        self._last_update_issue = payload
        self._pending_update_url = str(payload.get("release_url") or release_page_url())
        self._last_update_attempt_result = f"Update install failed: {payload.get('message') or 'unknown error'}"
        code = str(payload.get("code") or "")
        self._update_status_lbl.setText(
            f"Update install failed: {payload.get('message') or 'unknown error'}"
        )
        self._update_status_lbl.setStyleSheet(
            "color: #d28b26; font-size: 12px;"
            if code in {"update.manifest_missing", "update.asset_missing"}
            else "color: #e05050; font-size: 12px;"
        )
        self._refresh_update_tab()
        self._refresh_system_tab()

    def _restore_previous_appimage(self):
        mode, _description, guidance = self._runtime_mode_detail()
        if mode.kind == "package":
            QMessageBox.information(
                self.settings_dialog,
                "Package-managed install",
                "WaveLinux detected a package-managed install for this runtime.\n\n"
                f"{guidance}",
            )
            return
        backup_path = installed_appimage_backup_path()
        if not os.path.exists(backup_path):
            QMessageBox.information(
                self.settings_dialog,
                "No backup AppImage",
                f"No previous AppImage backup exists at:\n{backup_path}",
            )
            return

        progress = getattr(self, "_update_progress", None)
        if progress is not None:
            progress.setVisible(True)
            progress.setRange(0, 0)
            progress.setFormat("Preparing rollback…")
        self._download_update_btn.setEnabled(False)
        self._check_update_btn.setEnabled(False)
        rollback_btn = getattr(self, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(False)
        self._update_status_lbl.setText("Restoring previous AppImage backup…")
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

        try:
            result = restore_previous_install()
        except UpdateError as exc:
            self._handle_update_restore_error(exc.as_payload())
            return
        except Exception as exc:
            self._handle_update_restore_error(
                UpdateError("update.rollback_failed", str(exc)).as_payload()
            )
            return

        self._handle_update_restore_success(result)

    def _handle_update_restore_success(self, result):
        self._download_update_btn.setEnabled(True)
        self._check_update_btn.setEnabled(True)
        rollback_btn = getattr(self, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(True)
        progress = getattr(self, "_update_progress", None)
        if progress is not None:
            progress.setVisible(False)
            progress.setRange(0, 100)
            progress.setValue(0)
        if not isinstance(result, UpdateRollbackResult):
            raise TypeError("Expected UpdateRollbackResult from rollback")
        version_text = f"v{result.restored_version}" if result.restored_version else "the previous version"
        self._last_update_issue = None
        self._last_update_attempt_result = (
            f"Restored previous AppImage {version_text} from {result.backup_path}"
        )
        self._update_status_lbl.setText(
            f"Restored previous AppImage {version_text} to {result.appimage_path}. Restart WaveLinux to run it."
        )
        self._update_status_lbl.setStyleSheet("color: #00d4aa; font-size: 12px; font-weight: bold;")
        self._refresh_update_tab()
        self._refresh_system_tab()

        yn = QMessageBox.question(
            self.settings_dialog,
            "Previous AppImage restored",
            "WaveLinux restored the previous AppImage backup.\n\n"
            f"Backup: {result.backup_path}\n"
            f"Installed AppImage: {result.appimage_path}\n"
            f"Launcher: {result.wrapper_path}\n\n"
            "Restart into the restored AppImage now?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn == QMessageBox.StandardButton.Yes:
            self._restart_with_command([result.appimage_path])

    def _handle_update_restore_error(self, payload):
        self._download_update_btn.setEnabled(True)
        self._check_update_btn.setEnabled(True)
        rollback_btn = getattr(self, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setEnabled(True)
        progress = getattr(self, "_update_progress", None)
        if progress is not None:
            progress.setVisible(False)
            progress.setRange(0, 100)
            progress.setValue(0)
        payload = dict(payload or {})
        self._last_update_issue = payload
        self._pending_update_url = str(payload.get("release_url") or release_page_url())
        self._last_update_attempt_result = f"Rollback failed: {payload.get('message') or 'unknown error'}"
        self._update_status_lbl.setText(
            f"Rollback failed: {payload.get('message') or 'unknown error'}"
        )
        self._update_status_lbl.setStyleSheet("color: #e05050; font-size: 12px;")
        self._refresh_update_tab()
        self._refresh_system_tab()
        QMessageBox.warning(
            self.settings_dialog,
            "Rollback failed",
            str(payload.get("message") or "WaveLinux could not restore the previous AppImage."),
        )

    def _install_current_runtime_launcher(self):
        mode = self._current_runtime_mode()
        try:
            if mode.kind == "appimage":
                result = install_current_appimage()
                title = "AppImage installed"
                message = (
                    "WaveLinux installed this AppImage for desktop use.\n\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}"
                )
            elif mode.kind == "bundle":
                result = install_current_bundle()
                title = "Local build launcher installed"
                message = (
                    "WaveLinux installed a launcher for this local bundled build.\n\n"
                    f"Binary: {result.bundle_path}\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}"
                )
            elif mode.kind == "source":
                result = install_current_source_checkout()
                title = "Source launcher installed"
                message = (
                    "WaveLinux installed a launcher for this source checkout.\n\n"
                    f"Source: {result.source_dir}\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}"
                )
            else:
                QMessageBox.information(
                    self.settings_dialog,
                    "Launcher install unavailable",
                    "This runtime mode does not support installing the current binary as a desktop launcher.",
                )
                return
        except Exception as exc:
            QMessageBox.warning(
                self.settings_dialog,
                "Launcher install failed",
                str(exc),
            )
            return
        self._refresh_update_tab()
        QMessageBox.information(
            self.settings_dialog,
            title,
            message,
        )
        self._refresh_system_tab()

    def _repair_installed_launchers(self):
        state = install_state()
        mode = self._current_runtime_mode()
        try:
            if mode.kind == "source":
                result = repair_current_source_checkout_launchers()
                removed = len(result.removed_entries)
                msg = (
                    "WaveLinux rebuilt the source-checkout desktop launcher.\n\n"
                    f"Source: {result.source_dir}\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}\n"
                    f"Removed stale launchers: {removed}"
                )
            elif mode.kind == "bundle":
                result = repair_current_bundle_launchers()
                removed = len(result.removed_entries)
                msg = (
                    "WaveLinux rebuilt the local bundled-build desktop launcher.\n\n"
                    f"Binary: {result.bundle_path}\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}\n"
                    f"Removed stale launchers: {removed}"
                )
            elif state.wrapper_mode == "source":
                if state.wrapper_source_dir and os.path.isfile(os.path.join(state.wrapper_source_dir, "main.py")):
                    result = repair_current_source_checkout_launchers()
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
                    result = repair_bundle_launchers(bundle_exec)
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
                result = repair_installed_appimage_launchers()
                removed = len(result.removed_entries)
                msg = (
                    "WaveLinux repaired the canonical desktop launcher files.\n\n"
                    f"Launcher: {result.wrapper_path}\n"
                    f"Desktop file: {result.desktop_path}\n"
                    f"Removed stale launchers: {removed}"
                )
            elif is_running_in_appimage():
                result = install_current_appimage()
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
                self.settings_dialog,
                "Launcher repair failed",
                str(exc),
            )
            return
        self._refresh_system_tab()
        self._refresh_update_tab()
        QMessageBox.information(
            self.settings_dialog,
            "Desktop launchers repaired",
            msg,
        )

    def _restart_app(self):
        self._restart_with_command(launch_command())

    def _restart_with_command(self, command):
        self.save_config()
        self._shutting_down = True
        self._clear_runtime_pid()
        self.runtime.cleanup_sync()
        self.runtime.shutdown()
        os.execv(command[0], command + sys.argv[1:])

    def _check_for_updates_bg(self):
        """Silent background check 30 s after startup."""
        prev = getattr(self, '_bg_updater', None)
        if prev is not None:
            prev.cancel()

        self._bg_updater = UpdateChecker()
        self._bg_updater.check()
        # Reuse the bg poll timer if one already exists rather than
        # constructing a fresh QTimer each call.
        timer = getattr(self, '_bg_poll_timer', None)
        if timer is None:
            timer = QTimer(self)
            timer.setInterval(500)
            timer.timeout.connect(self._poll_bg_updater)
            self._bg_poll_timer = timer
        else:
            timer.stop()
        timer.start()

    def _poll_bg_updater(self):
        updater = getattr(self, '_bg_updater', None)
        if updater is None:
            return
        item = updater.poll()
        if item is None:
            return
        self._bg_poll_timer.stop()
        if item[0] == 'result':
            info = item[1]
            if not isinstance(info, VerifiedReleaseInfo):
                return
            tag = str(info.version or "").strip()
            mode, _description, guidance = self._runtime_mode_detail()
            self._pending_verified_release = info
            self._pending_update_url = info.release_url or release_page_url()
            self._pending_update_asset_url = info.asset_url or ""
            self._pending_update_asset_name = info.asset_name or ""
            self._last_update_check_at = time.time()
            self._last_update_issue = None
            if _parse_version(tag) > _parse_version(APP_VERSION):
                self._pending_update_tag = tag
                self._show_notification(
                    "WaveLinux Update Available",
                    (
                        f"Version {tag} is available. Open Settings → Updates to install it."
                        if self._pending_update_asset_url and mode.allows_self_update
                        else (
                            f"Version {tag} is available. {guidance}"
                            if self._pending_update_asset_url
                            else f"Version {tag} is available. Open Settings → Updates for details."
                        )
                    ),
                )
        elif item[0] == 'error':
            self._last_update_issue = dict(item[1] or {})
            self._pending_update_url = str(self._last_update_issue.get("release_url") or release_page_url())
            self._last_update_attempt_result = f"Background update check failed: {self._last_update_issue.get('message') or 'unknown error'}"

    def _refresh_update_tab(self, *, state=None, allow_async=True):
        if not self._module_enabled("updates"):
            return
        btn = getattr(self, "_install_runtime_btn", None)
        state = state or self._cached_install_state(
            target_tabs=("Updates",),
            max_age_s=5.0,
            allow_async=allow_async,
        )
        state_ready = state is not None
        mode, description, guidance = self._runtime_mode_detail()
        backup_path = getattr(
            state,
            "installed_appimage_backup_path",
            installed_appimage_backup_path(),
        )
        backup_exists = bool(
            getattr(state, "installed_appimage_backup_exists", os.path.exists(backup_path))
        )
        launcher_targets_active = (
            self._launcher_targets_active_runtime(state=state, mode=mode)
            if state_ready else None
        )
        if btn is not None:
            if mode.kind == "appimage":
                btn.setVisible(True)
                btn.setText(
                    "Reinstall This AppImage"
                    if state_ready and state.installed_appimage_exists
                    else "Install This AppImage"
                )
                btn.setToolTip("Install the currently running AppImage into ~/.local/bin and refresh its desktop launcher.")
            elif mode.kind == "bundle":
                btn.setVisible(True)
                btn.setText(
                    "Reinstall This Local Build"
                    if state_ready
                    and state.wrapper_mode == "bundle"
                    and getattr(state, "wrapper_bundle_exec", None) == mode.running_path
                    else "Install This Local Build"
                )
                btn.setToolTip("Install or refresh the desktop launcher for the current local bundled WaveLinux binary.")
            elif mode.kind == "source":
                btn.setVisible(True)
                btn.setText("Reinstall This Source Checkout")
                btn.setToolTip("Install or refresh the desktop launcher for the current source checkout.")
            else:
                btn.setVisible(False)
        rollback_btn = getattr(self, "_rollback_update_btn", None)
        if rollback_btn is not None:
            rollback_btn.setVisible(mode.kind in {"appimage", "source", "bundle"})
            rollback_btn.setEnabled(mode.kind in {"appimage", "source", "bundle"} and backup_exists)
            rollback_btn.setToolTip(
                (
                    f"Restore the previous installed AppImage backup from {backup_path}."
                    if backup_exists else
                    "No previous AppImage backup is available to restore."
                )
            )
        download_btn = getattr(self, "_download_update_btn", None)
        if download_btn is not None:
            pending_tag = getattr(self, "_pending_update_tag", None)
            pending_asset_url = getattr(self, "_pending_update_asset_url", "") or ""
            if not mode.allows_self_update:
                download_btn.setText("Use Package Manager to Update")
                download_btn.setEnabled(False)
                download_btn.setToolTip(guidance)
            elif pending_tag and _parse_version(pending_tag) > _parse_version(APP_VERSION):
                download_btn.setText(f"Download && Install v{pending_tag}")
                download_btn.setEnabled(bool(pending_asset_url))
                if not pending_asset_url:
                    download_btn.setToolTip(
                        "The latest signed release manifest does not expose an eligible x86_64 AppImage asset."
                    )
                else:
                    download_btn.setToolTip(
                        "Download the latest verified AppImage from GitHub and replace the installed desktop build."
                    )
            else:
                download_btn.setText("Download && Install Latest AppImage")
                download_btn.setEnabled(True)
                download_btn.setToolTip(
                    "Fetch the latest verified GitHub AppImage and install it to ~/.local/bin/WaveLinux.AppImage."
                )
        policy_lbl = getattr(self, "_update_policy_lbl", None)
        if policy_lbl is not None:
            policy_lbl.setText(description)
        info_lbl = getattr(self, "_install_state_lbl", None)
        if info_lbl is not None:
            lines = []
            lines.append(f"Current running version: v{APP_VERSION}")
            lines.append(f"Runtime mode: {mode.kind}")
            if not state_ready:
                lines.append("Install state: refreshing in background…")
            elif state.wrapper_mode == "source":
                lines.append(
                    "Installed launcher mode: source checkout"
                    + (
                        f" ({state.wrapper_source_dir})"
                        if state.wrapper_source_dir else ""
                    )
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
            if state_ready:
                if state.running_appimage_path:
                    lines.append(f"Running AppImage: {state.running_appimage_path}")
                elif getattr(sys, "frozen", False):
                    lines.append(f"Running binary: {os.path.abspath(sys.executable)}")
                else:
                    lines.append(f"Running from source: {resource_path('main.py')}")
                lines.append(
                    "Installed AppImage: "
                    + (state.installed_appimage_path if state.installed_appimage_exists else "not installed")
                )
                lines.append(
                    "Backup AppImage: "
                    + (backup_path if backup_exists else "not available")
                )
                lines.append(
                    "Desktop launcher: "
                    + (
                        (state.desktop_exec_target or state.desktop_path)
                        if state.desktop_exists
                        else "not installed"
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
            info_lbl.setText("\n".join(lines))
        warning_lbl = getattr(self, "_install_warning_lbl", None)
        if warning_lbl is not None:
            warning_lbl.setVisible(bool(state_ready and state.warnings))
            warning_lbl.setText("\n".join(getattr(state, "warnings", ()) or ()))
        note_lbl = getattr(self, "_update_note_lbl", None)
        if note_lbl is not None:
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
        repair_btn = getattr(self, "_repair_launcher_btn", None)
        if repair_btn is not None:
            needs_repair = bool(
                state_ready
                and (
                    state.warnings
                    or state.stale_launcher_entries
                    or (
                    state.wrapper_mode == "appimage" and state.installed_appimage_exists
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
            repair_btn.setEnabled(needs_repair or is_running_in_appimage())
        self._mark_settings_tab_refreshed("Updates")

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
        return runtime_mode()

    def _launcher_targets_active_runtime(self, *, state=None, mode=None):
        state = state or install_state()
        mode = mode or self._current_runtime_mode()
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

    def _runtime_mode_detail(self):
        mode = self._current_runtime_mode()
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

    def _running_binary_path(self, state):
        if state.running_appimage_path:
            return state.running_appimage_path
        return self._current_runtime_mode().running_path

    def _update_issue_title(self, code):
        titles = {
            "update.manifest_missing": "Signed release manifest unavailable",
            "update.signature_invalid": "Release signature verification failed",
            "update.asset_missing": "Verified AppImage asset unavailable",
            "update.checksum_mismatch": "Downloaded AppImage checksum mismatch",
            "update.smoke_test_failed": "Downloaded AppImage failed validation",
            "update.rollback_failed": "Previous AppImage restore failed",
        }
        return titles.get(code, "Update verification issue")

    def _health_issue_for_runtime_detail(self, detail):
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

    def _health_issue_for_channel(self, node_name, *, recovered=False):
        label = self._channel_label(node_name)
        recovery = self.recovery_status_for_channel(node_name)
        issue = self.channel_runtime_issue(node_name)
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
            detail_lines.extend(
                line.strip() for line in tooltip.splitlines() if line.strip()
            )
        if recovery.state == "scheduled" and recovery.next_retry_at:
            detail_lines.append(
                f"Next retry around {self._format_timestamp(recovery.next_retry_at)}."
            )
        elif recovery.state == "retrying":
            detail_lines.append(
                f"Recovery attempt count: {recovery.retry_count}."
            )
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

    def _collect_health_issues(self, *, preflight=None, state=None):
        preflight = preflight or startup_preflight_report()
        state = state or install_state()
        mode = self._current_runtime_mode()
        wrapper_mode = getattr(state, "wrapper_mode", "unknown")
        wrapper_source_dir = getattr(state, "wrapper_source_dir", None)
        wrapper_bundle_exec = getattr(state, "wrapper_bundle_exec", None)
        warnings = tuple(getattr(state, "warnings", ()) or ())
        issues = [
            self._health_issue_for_runtime_detail(detail)
            for detail in preflight.get("issue_details", [])
        ]

        expects_appimage_install = (
            mode.kind == "appimage"
            or wrapper_mode == "appimage"
            or (getattr(state, "desktop_exists", False) and wrapper_mode not in {"source", "bundle"})
            or bool(getattr(state, "stale_launcher_entries", ()))
        )
        if expects_appimage_install and getattr(state, "appimage_missing", False):
            detail = (
                f"No installed AppImage was found at {state.installed_appimage_path}. "
                "WaveLinux can still run from source or a package manager, but one-click "
                "AppImage restart/update flows install into that path."
            )
            issues.append(HealthIssue(
                code="install.appimage_missing",
                severity="warning",
                title="Installed AppImage missing",
                detail=detail,
                primary_action="Open Releases Page",
                secondary_action="Re-run check",
                context={"path": state.installed_appimage_path},
            ))
        if getattr(state, "wrapper_mismatch", False):
            issues.append(HealthIssue(
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
            ))
        if (
            wrapper_mode == "source"
            and any("source wrapper points at a missing WaveLinux checkout" in warning for warning in warnings)
        ):
            issues.append(HealthIssue(
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
            ))
        if (
            wrapper_mode == "bundle"
            and any("bundle launcher points at a missing WaveLinux binary" in warning for warning in warnings)
        ):
            issues.append(HealthIssue(
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
            ))
        if (
            mode.kind == "source"
            and wrapper_mode == "source"
            and wrapper_source_dir
            and os.path.abspath(wrapper_source_dir) != os.path.abspath(os.path.dirname(mode.running_path))
        ):
            issues.append(HealthIssue(
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
            ))
        if (
            mode.kind == "bundle"
            and wrapper_mode == "bundle"
            and wrapper_bundle_exec
            and os.path.abspath(wrapper_bundle_exec) != os.path.abspath(mode.running_path)
        ):
            issues.append(HealthIssue(
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
            ))
        if (
            mode.kind == "appimage"
            and wrapper_mode == "appimage"
            and getattr(state, "running_appimage_path", None)
            and getattr(state, "installed_appimage_exists", False)
            and os.path.abspath(state.running_appimage_path) != os.path.abspath(state.installed_appimage_path)
        ):
            issues.append(HealthIssue(
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
            ))
        backup_path = getattr(state, "installed_appimage_backup_path", installed_appimage_backup_path())
        backup_exists = bool(getattr(state, "installed_appimage_backup_exists", os.path.exists(backup_path)))
        if backup_exists:
            issues.append(HealthIssue(
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
            ))
        if getattr(state, "desktop_mismatch", False) or getattr(state, "stale_launcher_entries", ()):
            stale_paths = "\n".join(entry.path for entry in getattr(state, "stale_launcher_entries", ()))
            detail = (
                f"The canonical desktop entry at {state.desktop_path} does not match the "
                "current install state."
            )
            if stale_paths:
                detail += f"\nStale launchers:\n{stale_paths}"
            issues.append(HealthIssue(
                code="install.desktop_stale",
                severity="warning",
                title="Desktop launchers need repair",
                detail=detail,
                primary_action="Repair launchers",
                secondary_action="Re-run check",
                context={"desktop_path": state.desktop_path},
            ))

        update_issue = dict(getattr(self, "_last_update_issue", {}) or {})
        if update_issue:
            code = str(update_issue.get("code") or "update.asset_missing")
            severity = (
                "warning"
                if code in {"update.manifest_missing", "update.asset_missing"}
                else "error"
            )
            issues.append(HealthIssue(
                code=code,
                severity=severity,
                title=self._update_issue_title(code),
                detail=str(update_issue.get("message") or "WaveLinux detected an update verification issue."),
                primary_action="Retry update check",
                secondary_action="Open Releases Page",
                context=update_issue,
            ))

        issues.extend(self._device_health_issues(self.__dict__.get("_runtime_view_state")))
        issues.extend(self._module_health_issues())

        degraded = set(self._runtime_degraded_channels())
        for node_name in sorted(degraded):
            issues.append(self._health_issue_for_channel(node_name))

        for node_name, payload in sorted((self._recent_recovery_status or {}).items()):
            if node_name in degraded:
                continue
            if (time.time() - float(payload.get("at", 0) or 0)) >= 90:
                continue
            issues.append(self._health_issue_for_channel(node_name, recovered=True))

        return issues

    def _run_health_issue_action(self, issue, action):
        action = str(action or "").strip()
        if not action:
            return
        if action == "Re-run check":
            self._rerun_system_check()
            return
        if action == "Repair launchers":
            self._repair_installed_launchers()
            return
        if action == "Open Releases Page":
            self._open_release_page()
            return
        if action == "Retry update check":
            self._check_for_updates()
            return
        if action == "Re-run device reconcile":
            self._reconcile_device_policy()
            self._request_runtime_refresh("device-reconcile")
            return
        if action == "Restore Previous AppImage":
            self._restore_previous_appimage()
            return
        if action == "Restore monitor device":
            self._restore_preferred_monitor()
            return
        if action == "Restore microphone device":
            self._restore_preferred_mic()
            return
        if action == "Recover channel":
            self.recover_channel(str(issue.context.get("node_name") or ""))
            return
        if action == "Open diagnostics":
            self.open_channel_diagnostics(str(issue.context.get("node_name") or ""))
            return
        if action == "Restart module":
            module_id = str(issue.context.get("module_id") or "").strip()
            if module_id and getattr(self, "module_manager", None) is not None:
                self.module_manager.restart_module(module_id, "health-action")
                self._request_runtime_refresh(f"module-restart:{module_id}")
                self._schedule_active_settings_tab_refresh(force=True)
            return
        if action == "Enable module":
            module_id = str(issue.context.get("module_id") or "").strip()
            if module_id and getattr(self, "module_manager", None) is not None:
                self.module_manager.enable_module(module_id)
                self._request_runtime_refresh(f"module-enable:{module_id}")
                self._schedule_active_settings_tab_refresh(force=True)
            return

    def _render_health_cards(self, issues):
        layout = getattr(self, "_health_cards_layout", None)
        if layout is None:
            return
        self._clear_layout(layout)
        if not issues:
            issues = [HealthIssue(
                code="health.ok",
                severity="ok",
                title="No active issues detected",
                detail="Host runtime checks, install state, runtime recovery, and updater status all look healthy.",
            )]
        for issue in issues:
            card = HealthCard(self._health_cards_container)
            card.configure(
                issue,
                primary_handler=(
                    lambda checked=False, issue=issue: self._run_health_issue_action(issue, issue.primary_action)
                ) if issue.primary_action else None,
                secondary_handler=(
                    lambda checked=False, issue=issue: self._run_health_issue_action(issue, issue.secondary_action)
                ) if issue.secondary_action else None,
            )
            layout.addWidget(card)
        layout.addStretch(1)

    def _open_diagnostics_folder(self):
        path = self._diagnostics_root_path()
        if os.path.isdir(path) and QDesktopServices.openUrl(QUrl.fromLocalFile(path)):
            self.status_lbl.setText("Opened diagnostics folder")
            return
        QMessageBox.information(
            self,
            "Diagnostics Folder",
            f"WaveLinux stores runtime diagnostics here:\n{path}",
        )

    def _refresh_system_tab(self, *, preflight=None, state=None, allow_async=True):
        if not self._module_enabled("health"):
            return
        preflight = preflight or startup_preflight_report()
        self._startup_preflight = preflight
        state = state or self._cached_install_state(
            target_tabs=("Health",),
            max_age_s=5.0,
            allow_async=allow_async,
        )
        state_ready = state is not None
        backup_path = getattr(
            state,
            "installed_appimage_backup_path",
            installed_appimage_backup_path(),
        )
        backup_exists = bool(
            getattr(state, "installed_appimage_backup_exists", os.path.exists(backup_path))
        )
        launcher_targets_active = (
            self._launcher_targets_active_runtime(state=state)
            if state_ready else None
        )

        summary_lbl = getattr(self, "_system_summary_lbl", None)
        runtime_lbl = getattr(self, "_system_runtime_lbl", None)
        if not all((summary_lbl, runtime_lbl)):
            return
        if state_ready:
            issues = self._collect_health_issues(preflight=preflight, state=state)
        else:
            issues = self._device_health_issues(self.__dict__.get("_runtime_view_state"))
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
            summary_lbl.setText("WaveLinux health looks good. Host runtime, install state, recovery, and updater status are healthy.")
            summary_lbl.setStyleSheet("color: #00d4aa; font-size: 13px; font-weight: bold;")

        runtime_lines = [
            f"Current running version: v{APP_VERSION}",
            f"Running binary: {self._running_binary_path(state) if state_ready else self._current_runtime_mode().running_path}",
            "Installed AppImage: "
            + (
                state.installed_appimage_path
                if state_ready and state.installed_appimage_exists
                else ("not installed" if state_ready else "refreshing…")
            ),
            "Backup AppImage: "
            + (backup_path if backup_exists else "not available"),
            "Desktop launcher target: "
            + ((state.desktop_exec_target or "not installed") if state_ready else "refreshing…"),
            "Wrapper target: "
            + ((state.wrapper_target or "not installed") if state_ready else "refreshing…"),
            "Launcher targets active runtime: "
            + (
                "n/a" if launcher_targets_active is None
                else ("yes" if launcher_targets_active else "no")
            ),
            "Current system default sink: "
            + (getattr(self._runtime_view_state, "default_sink", None) or self.engine.get_default_sink() or "unknown"),
            "Current system default source: "
            + (getattr(self._runtime_view_state, "default_source", None) or self.engine.get_default_source() or "unknown"),
            "Host tools present: " + ", ".join(sorted(cmd for cmd, present in preflight["deps"].items() if present)),
            f"LADSPA plugins detected: {len(getattr(self.engine, 'ladspa_plugins', set()) or set())}",
            f"Last successful update check: {self._format_timestamp(self._last_update_check_at)}",
            f"Last update attempt result: {self._last_update_attempt_result}",
            f"Diagnostics directory: {self._diagnostics_root_path()}",
        ]
        runtime_lbl.setText("\n".join(runtime_lines))
        self._render_health_cards(issues)

        repair_btn = getattr(self, "_repair_launcher_btn", None)
        if repair_btn is not None:
            repair_btn.setEnabled(
                bool(
                    state_ready
                    and (state.stale_launcher_entries or state.wrapper_mismatch or state.desktop_mismatch)
                )
                or is_running_in_appimage()
            )
        recover_btn = getattr(self, "_health_recover_btn", None)
        if recover_btn is not None:
            degraded = len(self._runtime_degraded_channels())
            recover_btn.setEnabled(degraded > 0)
            recover_btn.setText(
                f"Recover degraded channels ({degraded})" if degraded else "Recover degraded channels"
            )
        self._mark_settings_tab_refreshed("Health")

    def _rerun_system_check(self):
        self._refresh_system_tab()
        self._refresh_update_tab()
        QMessageBox.information(
            self.settings_dialog,
            "Health check complete",
            "WaveLinux refreshed its host-runtime, install-state, recovery, and updater checks.",
        )

    def _refresh_advanced_tab(self):
        # Called when the dialog opens so values reflect any recent change.
        if hasattr(self, 'prune_spin'):
            self.prune_spin.blockSignals(True)
            self.prune_spin.setValue(self.app_prune_days)
            self.prune_spin.blockSignals(False)
        if hasattr(self, 'autostart_check'):
            self.autostart_check.blockSignals(True)
            self.autostart_check.setChecked(self.is_autostart_enabled())
            self.autostart_check.blockSignals(False)
        if hasattr(self, 'restore_forgotten_btn'):
            n = len(self.forgotten_apps)
            self.restore_forgotten_btn.setEnabled(n > 0)
            self.restore_forgotten_btn.setText(
                f"Restore forgotten apps ({n})" if n
                else "Restore forgotten apps"
            )
        if hasattr(self, 'recover_degraded_btn'):
            n = len(self._runtime_degraded_channels())
            self.recover_degraded_btn.setEnabled(n > 0)
            self.recover_degraded_btn.setText(
                f"Recover degraded channels ({n})" if n
                else "Recover degraded channels"
            )
        self._mark_settings_tab_refreshed("Advanced")

    def _restore_forgotten_apps(self):
        """Clear the persistent ✕ blocklist so apps the user has clicked
        the ✕ on can be re-discovered. Doesn't restore their saved
        volume / destination — those were dropped by `forget_app` and
        the user will need to re-pick a sink once the row reappears."""
        if not self.forgotten_apps:
            return
        n = len(self.forgotten_apps)
        yn = QMessageBox.question(
            self.settings_dialog, "Restore forgotten apps",
            f"Clear the blocklist of {n} forgotten app(s)? They will "
            f"reappear in the routing tab the next time they make sound.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        self.forgotten_apps.clear()
        self.save_config()
        self._refresh_advanced_tab()
        self._refresh()

    def _on_prune_days_change(self, value):
        self.app_prune_days = int(value)
        self.schedule_save()

    def _forget_all_offline(self):
        """One-shot: drop every saved app preference whose app isn't
        currently making sound. Refreshes the panel immediately."""
        active_ids = {
            app_id for app_id in self.app_widgets
            if self.app_widgets[app_id]._active_indices
        }
        remembered_ids = (
            set(getattr(self, "app_routing", {}).keys())
            | set(getattr(self, "app_volumes", {}).keys())
            | set(getattr(self, "app_last_seen", {}).keys())
        )
        to_forget = [app_id for app_id in remembered_ids if app_id not in active_ids]
        if not to_forget:
            QMessageBox.information(self.settings_dialog, "Forget offline apps",
                                    "No offline apps to forget.")
            return
        yn = QMessageBox.question(
            self.settings_dialog, "Forget offline apps",
            f"Drop saved routing and volume settings for {len(to_forget)} offline app(s)?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        for app_id in to_forget:
            self.app_routing.pop(app_id, None)
            self.app_volumes.pop(app_id, None)
            self.app_last_seen.pop(app_id, None)
            self.forgotten_apps.add(app_id)
            widget = self.app_widgets.pop(app_id, None)
            if widget is not None:
                widget.setParent(None)
                widget.deleteLater()
        self.save_config()
        self._refresh()

    def _on_emergency_reset(self):
        yn = QMessageBox.warning(
            self.settings_dialog, "Emergency Reset",
            "Unload ALL WaveLinux audio modules and rebuild from config? "
            "Use this if your audio has wedged.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn == QMessageBox.StandardButton.Yes:
            self.runtime.full_audio_reset_sync()
            self.load_config()
            self._refresh()

    def _export_runtime_diagnostics(self):
        path = self.runtime.export_diagnostics()
        QMessageBox.information(
            self,
            "Diagnostics Exported",
            f"Saved runtime diagnostics to:\n{path}",
        )

    def open_channel_diagnostics(self, node_name):
        if not node_name:
            return
        issue = self.channel_runtime_issue(node_name)
        path = (issue.get("diagnostics_path") or "").strip()
        if not path:
            path = self.runtime.export_diagnostics(f"channel-diagnostics:{node_name}")
        if not path:
            QMessageBox.information(
                self,
                "Diagnostics Unavailable",
                "WaveLinux could not locate or export diagnostics for that channel.",
            )
            return
        if os.path.exists(path) and QDesktopServices.openUrl(QUrl.fromLocalFile(path)):
            self.status_lbl.setText(f"Opened diagnostics for {self._channel_label(node_name)}")
            return
        QMessageBox.information(
            self,
            "Diagnostics Path",
            f"WaveLinux saved diagnostics for {self._channel_label(node_name)} here:\n{path}",
        )

    def _runtime_degraded_channels(self):
        view = self._runtime_view_state
        if view is None:
            return []
        health = getattr(view, "health", {}) or {}
        return sorted(name for name, state in health.items() if state)

    def _recover_all_degraded_channels(self):
        degraded = self._runtime_degraded_channels()
        if not degraded:
            QMessageBox.information(self, "Recovery", "No degraded channels detected.")
            return
        yn = QMessageBox.question(
            self.settings_dialog,
            "Recover degraded channels",
            f"Attempt recovery for {len(degraded)} degraded channel(s)?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        for node_name in degraded:
            self._clear_auto_recovery_state(node_name)
        self.runtime.recover_channels(degraded)
        self.status_lbl.setText(f"Recovering {len(degraded)} channel(s)...")
        self._refresh_advanced_tab()

    def _refresh_hidden_list(self):
        """Rebuild the hidden channels list in the Settings dialog."""
        # Clear existing items
        while self.hidden_list_layout.count():
            item = self.hidden_list_layout.takeAt(0)
            w = item.widget()
            if w:
                w.deleteLater()

        if not self.hidden_nodes:
            empty_lbl = QLabel("No hidden channels")
            empty_lbl.setStyleSheet("color: #5a5a72; font-size: 11px; font-style: italic;")
            self.hidden_list_layout.addWidget(empty_lbl)
            self._mark_settings_tab_refreshed("Hidden")
            return

        for node_name in sorted(self.hidden_nodes):
            row = QHBoxLayout()
            friendly = node_name.replace('wavelinux_', '').replace('_', ' ').title()
            lbl = QLabel(friendly)
            lbl.setStyleSheet("color: #e0e0ee; font-size: 12px;")
            row.addWidget(lbl, 1)

            unhide_btn = QPushButton("👁  Show")
            unhide_btn.setObjectName("addBtn")
            unhide_btn.setMinimumWidth(96)
            unhide_btn.setMinimumHeight(30)
            unhide_btn.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
            unhide_btn.clicked.connect(lambda checked, nn=node_name: self._unhide_from_settings(nn))
            row.addWidget(unhide_btn)

            row_widget = QWidget()
            row_widget.setLayout(row)
            self.hidden_list_layout.addWidget(row_widget)
        self._mark_settings_tab_refreshed("Hidden")

    def _unhide_from_settings(self, node_name):
        self.unhide_node(node_name)
        self._refresh_hidden_list()

    def schedule_save(self):
        timer = self.__dict__.get("_save_timer")
        if timer is not None:
            timer.start(500)

    def _flush_pending_ui_state(self):
        master_timer = self.__dict__.get("_master_commit_timer")
        if master_timer is not None and master_timer.isActive():
            master_timer.stop()
            self._commit_master_vols()

        for strip in self.__dict__.get("channel_widgets", {}).values():
            if hasattr(strip, "flush_pending_state"):
                strip.flush_pending_state()

        for row in self.__dict__.get("app_widgets", {}).values():
            if hasattr(row, "flush_pending_state"):
                row.flush_pending_state()

    def _virtual_channel_specs(self):
        specs = {}
        for display_name in self.virtual_channels:
            clean, safe = PipeWireEngine._sanitize_channel_name(display_name)
            specs[f"wavelinux_{safe}"] = clean
        return specs

    def _sync_runtime_persistent_state(self, *, immediate=False):
        monitor_hw = self._desired_mix_hw.get("Monitor")
        stream_hw = self._desired_mix_hw.get("Stream")
        if monitor_hw is None and hasattr(self, "mon_out_combo"):
            monitor_hw = self.mon_out_combo.currentData()
        if stream_hw is None and hasattr(self, "str_out_combo"):
            stream_hw = self.str_out_combo.currentData()
        self._suppress_pactl_events_for(1.5)
        self.runtime.sync_persistent_state(
            selected_mic=self.selected_mic,
            submix_state=self.submix_state,
            active_effects=self.active_effects,
            effect_params=self.effect_params,
            app_routing=dict(self.app_routing),
            app_volumes=self._normalize_app_volume_prefs(
                getattr(self, "app_volumes", {}),
            ),
            virtual_channels=self._virtual_channel_specs(),
            monitor_hw=monitor_hw,
            stream_hw=stream_hw,
            monitor_mix_volume=self._current_mix_master_volume("Monitor"),
            stream_mix_volume=self._current_mix_master_volume("Stream"),
            apply_now=bool(immediate),
        )

    @staticmethod
    def _normalize_app_identity_overrides(raw):
        return PipeWireEngine._normalize_identity_override_map(raw)

    @staticmethod
    def _normalize_app_label_overrides(raw):
        return PipeWireEngine._normalize_label_override_map(raw)

    def _set_engine_identity_overrides(self):
        engine = self.__dict__.get("engine")
        if engine is None or not hasattr(engine, "set_app_identity_overrides"):
            return
        engine.set_app_identity_overrides(
            self.__dict__.get("app_identity_overrides", {}),
            self.__dict__.get("app_label_overrides", {}),
        )

    def _identity_dialog_parent(self):
        return getattr(self, "settings_dialog", None) or self

    def _all_scene_app_ids(self):
        app_ids = set()
        for snapshot in (self.scenes or {}).values():
            if not isinstance(snapshot, dict):
                continue
            for mapping_name in ("app_routing", "app_volumes"):
                mapping = snapshot.get(mapping_name, {}) or {}
                if not isinstance(mapping, dict):
                    continue
                for app_id in mapping.keys():
                    if PipeWireEngine.is_persistent_app_id(app_id):
                        app_ids.add(app_id)
        return app_ids

    def _known_persistent_app_ids(self):
        app_ids = set()
        sources = (
            set(getattr(self, "app_routing", {}).keys())
            | set(getattr(self, "app_volumes", {}).keys())
            | set(getattr(self, "app_last_seen", {}).keys())
            | set(getattr(self, "app_display_names", {}).keys())
            | set(getattr(self, "forgotten_apps", set()))
            | set(self.__dict__.get("app_identity_overrides", {}).keys())
            | set(self.__dict__.get("app_identity_overrides", {}).values())
            | set(self.__dict__.get("app_label_overrides", {}).keys())
            | self._all_scene_app_ids()
        )
        for app_id in sources:
            if PipeWireEngine.is_persistent_app_id(app_id):
                app_ids.add(app_id)
        view = getattr(self, "_runtime_view_state", None)
        for app_view in getattr(view, "app_views", []) or []:
            app_id = getattr(app_view, "app_id", "")
            if PipeWireEngine.is_persistent_app_id(app_id):
                app_ids.add(app_id)
        app_ids.discard(PipeWireEngine.SYSTEM_SOUNDS_BUCKET)
        return app_ids

    def _override_sources_for_target(self, target_app_id, *, exclude_source=None):
        sources = []
        for source_app_id, mapped_target in self.__dict__.get("app_identity_overrides", {}).items():
            if mapped_target != target_app_id:
                continue
            if exclude_source and source_app_id == exclude_source:
                continue
            sources.append(source_app_id)
        return sorted(set(sources))

    def _app_id_has_runtime_or_saved_references(self, app_id):
        if not app_id:
            return False
        if app_id in getattr(self, "app_routing", {}):
            return True
        if app_id in getattr(self, "app_volumes", {}):
            return True
        if app_id in getattr(self, "app_last_seen", {}):
            return True
        if app_id in getattr(self, "forgotten_apps", set()):
            return True
        for snapshot in (self.scenes or {}).values():
            if not isinstance(snapshot, dict):
                continue
            if app_id in (snapshot.get("app_routing", {}) or {}):
                return True
            if app_id in (snapshot.get("app_volumes", {}) or {}):
                return True
        return False

    def _cleanup_orphaned_custom_identity(self, app_id):
        if not isinstance(app_id, str) or not app_id.startswith("custom:"):
            return
        if self._override_sources_for_target(app_id):
            return
        if self._app_id_has_runtime_or_saved_references(app_id):
            return
        self.app_label_overrides.pop(app_id, None)
        self.app_display_names.pop(app_id, None)

    def _allocate_custom_app_id(self, label, *, keep_existing=""):
        base_id = PipeWireEngine._make_app_route_key("custom", label)
        if keep_existing and keep_existing.startswith("custom:"):
            base_id = keep_existing
        if not base_id:
            base_id = "custom:app"
        candidate = base_id
        known = self._known_persistent_app_ids()
        suffix = 2
        while candidate in known and candidate != keep_existing:
            candidate = f"{base_id}-{suffix}"
            suffix += 1
        return candidate

    def _migrate_scene_library_app_identity(self, source_app_id, target_app_id):
        if not source_app_id or not target_app_id or source_app_id == target_app_id:
            return False
        changed = False
        for snapshot in (self.scenes or {}).values():
            if not isinstance(snapshot, dict):
                continue
            for mapping_name in ("app_routing", "app_volumes"):
                mapping = snapshot.get(mapping_name, {}) or {}
                if source_app_id not in mapping:
                    continue
                if target_app_id not in mapping:
                    mapping[target_app_id] = mapping[source_app_id]
                mapping.pop(source_app_id, None)
                changed = True
        return changed

    def _migrate_app_identity_state(self, source_app_id, target_app_id):
        if not source_app_id or not target_app_id or source_app_id == target_app_id:
            return False
        changed = False
        if source_app_id in self.app_routing:
            if target_app_id not in self.app_routing:
                self.app_routing[target_app_id] = self.app_routing[source_app_id]
            self.app_routing.pop(source_app_id, None)
            changed = True
        if source_app_id in self.app_volumes:
            if target_app_id not in self.app_volumes:
                self.app_volumes[target_app_id] = self.app_volumes[source_app_id]
            self.app_volumes.pop(source_app_id, None)
            changed = True
        if source_app_id in self.app_last_seen:
            source_seen = int(self.app_last_seen.pop(source_app_id))
            target_seen = int(self.app_last_seen.get(target_app_id, 0) or 0)
            self.app_last_seen[target_app_id] = max(source_seen, target_seen)
            changed = True
        source_label = self.app_display_names.pop(source_app_id, None)
        if target_app_id in self.app_label_overrides:
            self.app_display_names[target_app_id] = self.app_label_overrides[target_app_id]
            changed = True
        elif source_label and target_app_id not in self.app_display_names:
            self.app_display_names[target_app_id] = source_label
            changed = True
        if source_app_id in self.forgotten_apps:
            self.forgotten_apps.discard(source_app_id)
            self.forgotten_apps.add(target_app_id)
            changed = True
        if self._migrate_scene_library_app_identity(source_app_id, target_app_id):
            changed = True
        self._cleanup_orphaned_custom_identity(source_app_id)
        return changed

    def _display_name_for_app_id(self, app_id, fallback=None):
        override = self.__dict__.get("app_label_overrides", {}).get(app_id)
        if override:
            self.app_display_names[app_id] = override
            return override
        if fallback:
            self.app_display_names[app_id] = fallback
            return fallback
        cached = self.app_display_names.get(app_id)
        if cached:
            return cached
        return PipeWireEngine.display_name_for_app_id(app_id)

    def _app_identity_context(self, app_view_or_row):
        app_id = str(getattr(app_view_or_row, "app_id", "") or "").strip()
        app_name = str(getattr(app_view_or_row, "app_name", "") or "").strip()
        resolved_app_id = str(
            getattr(app_view_or_row, "resolved_app_id", "") or app_id
        ).strip()
        resolved_app_name = str(
            getattr(app_view_or_row, "resolved_app_name", "") or app_name
        ).strip()
        reset_source_app_id = str(
            getattr(app_view_or_row, "reset_source_app_id", "") or ""
        ).strip()
        sources_for_target = self._override_sources_for_target(app_id)
        if not reset_source_app_id and len(sources_for_target) == 1:
            reset_source_app_id = sources_for_target[0]
        label_override_active = app_id in self.__dict__.get("app_label_overrides", {})
        manual_override_active = bool(
            getattr(app_view_or_row, "manual_override_active", False)
            or getattr(app_view_or_row, "override_applied", False)
            or bool(reset_source_app_id)
            or label_override_active
        )
        source_app_id = ""
        if (
            resolved_app_id
            and resolved_app_id != PipeWireEngine.SYSTEM_SOUNDS_BUCKET
            and PipeWireEngine.is_persistent_app_id(resolved_app_id)
        ):
            source_app_id = resolved_app_id
        elif (
            app_id
            and app_id != PipeWireEngine.SYSTEM_SOUNDS_BUCKET
            and PipeWireEngine.is_persistent_app_id(app_id)
        ):
            source_app_id = app_id
        display_name = self._display_name_for_app_id(
            app_id,
            app_name or resolved_app_name,
        )
        return {
            "app_id": app_id,
            "app_name": display_name,
            "resolved_app_id": resolved_app_id,
            "resolved_app_name": resolved_app_name,
            "source_app_id": source_app_id,
            "reset_source_app_id": reset_source_app_id,
            "manual_override_active": manual_override_active,
            "identity_source": str(getattr(app_view_or_row, "identity_source", "") or ""),
            "override_applied": bool(getattr(app_view_or_row, "override_applied", False)),
        }

    def _migrate_legacy_app_identity(self, app_id, display_name):
        if not app_id or not display_name:
            return False
        changed = False
        if app_id not in self.app_routing and display_name in self.app_routing:
            self.app_routing[app_id] = self.app_routing.pop(display_name)
            changed = True
        if app_id not in self.app_last_seen and display_name in self.app_last_seen:
            self.app_last_seen[app_id] = self.app_last_seen.pop(display_name)
            changed = True
        if app_id not in self.app_volumes and display_name in self.app_volumes:
            self.app_volumes[app_id] = self.app_volumes.pop(display_name)
            changed = True
        if app_id not in self.forgotten_apps and display_name in self.forgotten_apps:
            self.forgotten_apps.discard(display_name)
            self.forgotten_apps.add(app_id)
            changed = True
        if app_id != display_name and display_name in self.app_display_names:
            legacy_label = self.app_display_names.pop(display_name, None)
            if legacy_label and app_id not in self.app_display_names:
                self.app_display_names[app_id] = legacy_label
            changed = True
        if self.app_display_names.get(app_id) != display_name:
            self.app_display_names[app_id] = display_name
            changed = True
        return changed

    def _apply_app_identity_changes(self, status_message):
        self._set_engine_identity_overrides()
        self._sync_runtime_persistent_state(immediate=True)
        self.save_config()
        runtime = getattr(self, "runtime", None)
        if runtime is not None and hasattr(runtime, "refresh_now"):
            runtime.refresh_now("app-identity-change")
        if hasattr(self, "status_lbl"):
            self.status_lbl.setText(status_message)
        if hasattr(self, "_refresh"):
            self._refresh()

    def _pin_app_identity(self, app_view_or_row):
        ctx = self._app_identity_context(app_view_or_row)
        source_app_id = ctx["source_app_id"]
        if not source_app_id:
            QMessageBox.information(
                self._identity_dialog_parent(),
                "Pin App Identity",
                "WaveLinux needs a stable app signature before it can pin this stream.",
            )
            return False

        current_label = self._display_name_for_app_id(
            ctx["app_id"],
            ctx["app_name"] or ctx["resolved_app_name"],
        )
        label, ok = QInputDialog.getText(
            self._identity_dialog_parent(),
            "Pin / Rename App",
            "Display label:",
            text=current_label,
        )
        if not ok:
            return False
        label = PipeWireEngine._sanitize_app_label(label)
        if not label:
            QMessageBox.information(
                self._identity_dialog_parent(),
                "Pin / Rename App",
                "Enter a non-empty app label.",
            )
            return False

        current_app_id = ctx["app_id"]
        manual_override_active = ctx["manual_override_active"]
        if current_app_id.startswith("custom:") or manual_override_active:
            target_app_id = current_app_id
        elif current_app_id.startswith(("app:", "snap:")):
            target_app_id = current_app_id
        else:
            target_app_id = self._allocate_custom_app_id(label)

        if target_app_id != current_app_id:
            self._migrate_app_identity_state(current_app_id, target_app_id)
            self.app_identity_overrides[source_app_id] = target_app_id
        elif (
            source_app_id != current_app_id
            and PipeWireEngine.is_persistent_app_id(source_app_id)
            and manual_override_active
        ):
            self.app_identity_overrides[source_app_id] = current_app_id

        self.app_label_overrides[target_app_id] = label
        self.app_display_names[target_app_id] = label
        if target_app_id != current_app_id:
            self.app_display_names.pop(current_app_id, None)
        self._apply_app_identity_changes(f"Pinned app identity: {label}")
        return True

    def _merge_app_identity(self, app_view_or_row):
        ctx = self._app_identity_context(app_view_or_row)
        source_app_id = ctx["source_app_id"]
        if not source_app_id:
            QMessageBox.information(
                self._identity_dialog_parent(),
                "Merge App Identity",
                "WaveLinux needs a stable app signature before it can merge this stream.",
            )
            return False

        candidate_ids = sorted(
            (
                app_id for app_id in self._known_persistent_app_ids()
                if app_id != ctx["app_id"]
                and app_id != PipeWireEngine.SYSTEM_SOUNDS_BUCKET
                and PipeWireEngine.is_persistent_app_id(app_id)
            ),
            key=lambda app_id: (
                self._display_name_for_app_id(app_id).lower(),
                app_id,
            ),
        )
        if not candidate_ids:
            QMessageBox.information(
                self._identity_dialog_parent(),
                "Merge App Identity",
                "No other saved app identities are available to merge into yet.",
            )
            return False

        labels = [
            f"{self._display_name_for_app_id(app_id)} [{app_id}]"
            for app_id in candidate_ids
        ]
        selection, ok = QInputDialog.getItem(
            self._identity_dialog_parent(),
            "Merge Into Existing App",
            "Route this app identity into:",
            labels,
            0,
            False,
        )
        if not ok or not selection:
            return False
        target_app_id = candidate_ids[labels.index(selection)]
        current_app_id = ctx["app_id"]

        self.app_identity_overrides[source_app_id] = target_app_id
        if current_app_id != target_app_id:
            self._migrate_app_identity_state(current_app_id, target_app_id)
        self._cleanup_orphaned_custom_identity(current_app_id)
        target_label = self.app_label_overrides.get(target_app_id)
        if target_label:
            self.app_display_names[target_app_id] = target_label
        self._apply_app_identity_changes(
            f"Merged app identity into {self._display_name_for_app_id(target_app_id)}"
        )
        return True

    def _reset_app_identity_override(self, app_view_or_row):
        ctx = self._app_identity_context(app_view_or_row)
        current_app_id = ctx["app_id"]
        source_app_id = ctx["reset_source_app_id"]
        had_source_override = bool(
            source_app_id
            and self.app_identity_overrides.get(source_app_id) == current_app_id
        )
        had_label_override = current_app_id in self.app_label_overrides
        if not had_source_override and not had_label_override:
            return False

        if had_source_override:
            self.app_identity_overrides.pop(source_app_id, None)
            remaining_sources = self._override_sources_for_target(
                current_app_id,
                exclude_source=source_app_id,
            )
            if current_app_id.startswith("custom:") and not remaining_sources:
                self._migrate_app_identity_state(current_app_id, source_app_id)
                self.app_label_overrides.pop(current_app_id, None)
                self.app_display_names.pop(current_app_id, None)
                self.app_display_names[source_app_id] = PipeWireEngine.display_name_for_app_id(
                    source_app_id,
                )
                self._cleanup_orphaned_custom_identity(current_app_id)
        elif had_label_override:
            self.app_label_overrides.pop(current_app_id, None)
            self.app_display_names[current_app_id] = PipeWireEngine.display_name_for_app_id(
                current_app_id,
            )

        self._apply_app_identity_changes("Reset app identity to automatic detection")
        return True

    def _any_slider_dragging(self):
        """True if any slider thumb is being held — refresh defers when
        true to avoid stutter from mid-drag subprocess calls."""
        for s in (getattr(self, 'mon_master_slider', None),
                  getattr(self, 'str_master_slider', None)):
            if s is not None and s.isSliderDown():
                return True
        for strip in self.channel_widgets.values():
            for s in (getattr(strip, 'mon_slider', None),
                      getattr(strip, 'str_slider', None)):
                if s is not None and s.isSliderDown():
                    return True
        for row in self.app_widgets.values():
            s = getattr(row, 'vol_slider', None)
            if s is not None and s.isSliderDown():
                return True
        return False

    def recover_channel(self, node_name):
        if not node_name:
            return
        self._clear_auto_recovery_state(node_name)
        self.runtime.recover_channel(node_name)

    def _start_event_subscriber(self):
        """Run `pactl subscribe` under a QProcess so external mute/volume
        changes (pavucontrol, media keys) refresh within ~150ms."""
        self._event_proc = QProcess(self)
        self._event_proc.setProcessChannelMode(QProcess.ProcessChannelMode.MergedChannels)
        self._event_proc.readyReadStandardOutput.connect(self._on_pactl_event)
        self._event_proc.errorOccurred.connect(self._on_event_proc_error)
        self._event_proc.finished.connect(self._on_event_proc_finished)
        try:
            self._event_proc.start("pactl", ["subscribe"])
        except Exception as e:
            logging.warning(f"pactl subscribe unavailable: {e} — falling back to poll")

    def _on_pactl_event(self):
        """Debounce relevant external graph changes, but ignore noisy
        internal churn like source-output moves and module bookkeeping."""
        try:
            payload = bytes(self._event_proc.readAllStandardOutput()).decode(
                "utf-8", "replace"
            )
        except Exception:
            payload = ""
        if self._pactl_events_suppressed():
            return
        if payload and not self._should_refresh_for_pactl_event(payload):
            return
        self._event_refresh_timer.start()
        if payload and self._should_schedule_settle_refresh_for_pactl_event(payload):
            settle_timer = (
                self.__dict__.get("_device_settle_refresh_timer")
                or self.__dict__.get("_hotplug_refresh_timer")
            )
            if settle_timer is not None:
                settle_timer.start()
        if payload and self._should_schedule_bluetooth_settle_refresh_for_pactl_event(payload):
            bluetooth_timer = self.__dict__.get("_bluetooth_refresh_timer")
            if bluetooth_timer is not None:
                bluetooth_timer.start()

    def _on_event_proc_error(self, err):
        if self._shutting_down:
            return
        logging.warning(f"pactl subscribe error: {err}")
        self._schedule_audio_server_recovery()

    def _on_event_proc_finished(self, exit_code, exit_status):
        if self._shutting_down:
            return
        logging.warning(
            "pactl subscribe exited (code=%s, status=%s)",
            exit_code,
            exit_status,
        )
        self._schedule_audio_server_recovery()

    def _schedule_audio_server_recovery(self):
        if self._shutting_down:
            return
        self._bluetooth_profile_reassert_retries = max(
            int(self.__dict__.get("_bluetooth_profile_reassert_retries", 0) or 0),
            6,
        )
        reconnect_scheduled = False
        if not self._selected_mic_uses_bluetooth_input():
            reconnect_scheduled = self._schedule_known_bluetooth_monitor_reconnect(
                disconnect_first=False,
                settle_delay_ms=250,
            )
        restart_timer = self.__dict__.get("_event_proc_restart_timer")
        if restart_timer is not None:
            restart_timer.start()
        event_timer = self.__dict__.get("_event_refresh_timer")
        if event_timer is not None:
            event_timer.start()
        bluetooth_timer = self.__dict__.get("_bluetooth_refresh_timer")
        if bluetooth_timer is not None:
            if reconnect_scheduled:
                bluetooth_timer.start(600)
            else:
                bluetooth_timer.start()

    def _restart_event_subscriber_if_needed(self):
        if self._shutting_down:
            return
        proc = self.__dict__.get("_event_proc")
        if proc is not None and proc.state() != QProcess.ProcessState.NotRunning:
            return
        if proc is not None:
            try:
                proc.deleteLater()
            except Exception:
                pass
            self._event_proc = None
        self._start_event_subscriber()

    @staticmethod
    def _preferred_bluetooth_playback_profile_name(profiles):
        available = [
            str(profile_name or "").strip()
            for profile_name in (profiles or [])
            if str(profile_name or "").strip()
        ]
        if "a2dp-sink" in available:
            return "a2dp-sink"
        for profile_name in available:
            if profile_name.startswith("a2dp-sink"):
                return profile_name
        return ""

    @staticmethod
    def _bluetooth_mac_from_card_name(card_name):
        raw = str(card_name or "").strip()
        if raw.startswith("bluez_card."):
            raw = raw.split(".", 1)[1]
        match = re.search(r"([0-9A-Fa-f]{2}(?:[_:-][0-9A-Fa-f]{2}){5})", raw)
        if not match:
            return ""
        return match.group(1).replace("_", ":").replace("-", ":").upper()

    @staticmethod
    def _run_bluetoothctl_commands(*commands, timeout=8):
        script = "\n".join(str(cmd) for cmd in commands if str(cmd or "").strip()) + "\nquit\n"
        if not script.strip():
            return None
        return subprocess.run(
            ["bluetoothctl"],
            input=script,
            capture_output=True,
            text=True,
            timeout=timeout,
        )

    def _complete_bluetooth_reconnect(self, mac):
        mac = str(mac or "").strip().upper()
        pending = self.__dict__.setdefault("_pending_bluetooth_reconnect_macs", set())
        try:
            if self._shutting_down or not mac:
                return
            self._run_bluetoothctl_commands(f"connect {mac}", timeout=10)
        except Exception as exc:
            logging.warning("Bluetooth reconnect failed for %s: %s", mac, exc)
        finally:
            pending.discard(mac)
            timer = self.__dict__.get("_bluetooth_refresh_timer")
            if timer is not None and not self._shutting_down:
                timer.start()

    def _schedule_bluetooth_reconnect_mac(
        self,
        mac,
        *,
        disconnect_first=True,
        settle_delay_ms=900,
    ):
        mac = str(mac or "").strip().upper()
        if not mac or self._shutting_down:
            return False
        pending = self.__dict__.setdefault("_pending_bluetooth_reconnect_macs", set())
        if mac in pending:
            return False
        pending.add(mac)
        if disconnect_first:
            try:
                self._run_bluetoothctl_commands(f"disconnect {mac}", timeout=8)
            except Exception as exc:
                logging.warning("Bluetooth disconnect failed for %s: %s", mac, exc)
        delay_ms = max(0, int(settle_delay_ms or 0))
        QTimer.singleShot(delay_ms, lambda mac=mac: self._complete_bluetooth_reconnect(mac))
        logging.info(
            "Scheduled Bluetooth reconnect for %s (disconnect_first=%s, delay_ms=%s)",
            mac,
            disconnect_first,
            delay_ms,
        )
        return True

    def _schedule_bluetooth_reconnect(
        self,
        card_name,
        *,
        disconnect_first=True,
        settle_delay_ms=900,
    ):
        return self._schedule_bluetooth_reconnect_mac(
            self._bluetooth_mac_from_card_name(card_name),
            disconnect_first=disconnect_first,
            settle_delay_ms=settle_delay_ms,
        )

    def _known_bluetooth_target_macs(self):
        candidates = [
            self.__dict__.get("_desired_mix_hw", {}).get("Monitor", ""),
            self.__dict__.get("_preferred_monitor_hw_name", ""),
            self.__dict__.get("_preferred_monitor_hw_id", ""),
            self.__dict__.get("_restorable_monitor_hw_name", ""),
            self.__dict__.get("_restorable_monitor_hw_id", ""),
        ]
        macs = []
        seen = set()
        for candidate in candidates:
            mac = self._bluetooth_mac_from_card_name(candidate)
            if not mac or mac in seen:
                continue
            seen.add(mac)
            macs.append(mac)
        return macs

    def _selected_mic_uses_bluetooth_input(self):
        selected_mic = str(self.__dict__.get("selected_mic", "") or "").strip().lower()
        if selected_mic.startswith("bluez_input."):
            return True
        view = self.__dict__.get("_runtime_view_state")
        runtime_selected = str(getattr(view, "selected_mic", "") or "").strip().lower()
        return runtime_selected.startswith("bluez_input.")

    def _schedule_known_bluetooth_monitor_reconnect(
        self,
        *,
        disconnect_first,
        settle_delay_ms,
    ):
        reconnect_scheduled = False
        for mac in self._known_bluetooth_target_macs():
            reconnect_scheduled = (
                self._schedule_bluetooth_reconnect_mac(
                    mac,
                    disconnect_first=disconnect_first,
                    settle_delay_ms=settle_delay_ms,
                )
                or reconnect_scheduled
            )
        return reconnect_scheduled

    def _has_bluetooth_playback_cards(self):
        engine = self.__dict__.get("engine")
        if engine is None or not hasattr(engine, "list_cards"):
            return False
        try:
            cards = list(engine.list_cards() or [])
        except Exception:
            return False
        return any(
            str((card or {}).get("name") or "").strip().startswith("bluez_card.")
            for card in cards
        )

    def _reassert_bluetooth_playback_profile(self):
        if self._shutting_down:
            return False, False
        engine = self.__dict__.get("engine")
        if engine is None:
            return False, False
        try:
            if hasattr(engine, "lock_bluetooth_to_a2dp"):
                engine.lock_bluetooth_to_a2dp()
        except Exception as exc:
            logging.warning("Failed to re-lock Bluetooth autoswitch: %s", exc)
        if self._selected_mic_uses_bluetooth_input():
            return False, False
        if not hasattr(engine, "list_cards") or not hasattr(engine, "set_card_profile"):
            return False, False
        changed = False
        retry_needed = False
        try:
            cards = list(engine.list_cards() or [])
        except Exception as exc:
            logging.warning("Failed to inspect Bluetooth cards after server churn: %s", exc)
            return False, False
        bluetooth_cards = [
            card
            for card in cards
            if str((card or {}).get("name") or "").strip().startswith("bluez_card.")
        ]
        retries_left = int(self.__dict__.get("_bluetooth_profile_reassert_retries", 0) or 0)
        if not bluetooth_cards and retries_left > 0:
            reconnect_scheduled = self._schedule_known_bluetooth_monitor_reconnect(
                disconnect_first=False,
                settle_delay_ms=250,
            )
            if reconnect_scheduled:
                return False, True
        for card in cards:
            card_name = str((card or {}).get("name") or "").strip()
            if not card_name.startswith("bluez_card."):
                continue
            active_profile = str((card or {}).get("active_profile") or "").strip()
            if (
                active_profile in {"", "off", "headset-head-unit", "headset-head-unit-cvsd"}
                and retries_left > 0
            ):
                self._schedule_bluetooth_reconnect(card_name)
                retry_needed = True
            target_profile = self._preferred_bluetooth_playback_profile_name(
                [
                    profile.get("name")
                    for profile in ((card or {}).get("profiles") or [])
                    if bool(profile.get("available"))
                ]
            )
            if active_profile.startswith("a2dp-sink"):
                continue
            if not target_profile:
                retry_needed = retry_needed or active_profile in {
                    "",
                    "off",
                    "headset-head-unit",
                    "headset-head-unit-cvsd",
                }
                if retry_needed:
                    self._schedule_bluetooth_reconnect(card_name)
                continue
            try:
                changed = bool(engine.set_card_profile(card_name, target_profile)) or changed
                retry_needed = True
            except Exception as exc:
                logging.warning(
                    "Failed to restore Bluetooth playback profile on %s: %s",
                    card_name,
                    exc,
                )
                retry_needed = True
        return changed, retry_needed

    def _prime_bluetooth_playback_profile(self):
        if self._shutting_down or self._selected_mic_uses_bluetooth_input():
            return False
        if not self._has_bluetooth_playback_cards():
            return False
        self._bluetooth_profile_reassert_retries = max(
            int(self.__dict__.get("_bluetooth_profile_reassert_retries", 0) or 0),
            4,
        )
        changed, retry_needed = self._reassert_bluetooth_playback_profile()
        if changed:
            self._request_runtime_refresh("startup-bt-profile")
        timer = self.__dict__.get("_bluetooth_refresh_timer")
        if timer is not None and hasattr(timer, "start"):
            timer.start()
        return changed or retry_needed

    def _handle_bluetooth_settle_refresh(self):
        if self._shutting_down:
            return
        self._restart_event_subscriber_if_needed()
        _, retry_needed = self._reassert_bluetooth_playback_profile()
        self._request_runtime_refresh("bluetooth-settle")
        retries_left = int(self.__dict__.get("_bluetooth_profile_reassert_retries", 0) or 0)
        if retry_needed and retries_left > 0:
            self._bluetooth_profile_reassert_retries = retries_left - 1
            timer = self.__dict__.get("_bluetooth_refresh_timer")
            if timer is not None:
                timer.start(600)
        else:
            self._bluetooth_profile_reassert_retries = 0

    @staticmethod
    def _should_refresh_for_pactl_event(payload):
        refresh_targets = {"sink", "source", "sink-input", "server", "card", "client"}
        ignored_targets = {"source-output", "module"}
        saw_any = False
        for line in payload.splitlines():
            text = line.strip().lower()
            if not text:
                continue
            match = re.search(r"\bon\s+([a-z-]+)\b", text)
            if not match:
                continue
            saw_any = True
            target = match.group(1)
            if target in ignored_targets:
                continue
            if target in refresh_targets:
                return True
        return not saw_any

    @staticmethod
    def _should_schedule_settle_refresh_for_pactl_event(payload):
        settle_targets = {"sink", "source", "server", "card"}
        for line in payload.splitlines():
            text = line.strip().lower()
            if not text:
                continue
            match = re.search(r"\bon\s+([a-z-]+)\b", text)
            if not match:
                continue
            target = match.group(1)
            if target in settle_targets:
                return True
        return False

    @staticmethod
    def _should_schedule_bluetooth_settle_refresh_for_pactl_event(payload):
        structural_targets = {"sink", "source", "server", "card"}
        for line in payload.splitlines():
            text = line.strip().lower()
            if not text or "bluez" not in text:
                continue
            match = re.search(r"\bon\s+([a-z-]+)\b", text)
            if not match:
                continue
            if match.group(1) in structural_targets:
                return True
        return False

    def _setup_ui(self):
        build_main_window(self)

    def _mixer_panel_controller(self):
        controller = self.__dict__.get("_mixer_panel")
        if controller is None:
            controller = MixerPanelController(self)
            self._mixer_panel = controller
        return controller

    def _app_routing_panel_controller(self):
        controller = self.__dict__.get("_app_routing_panel")
        if controller is None:
            controller = AppRoutingPanelController(self)
            self._app_routing_panel = controller
        return controller

    def _clear_layout(self, layout):
        while layout.count():
            item = layout.takeAt(0)
            w = item.widget()
            if w:
                w.setParent(None)
                w.deleteLater()
            elif item.layout():
                self._clear_layout(item.layout())

    # ── Responsive strip scaling ────────────────────────────────────

    _MIN_STRIP_W = MixerPanelController._MIN_STRIP_W
    _MAX_STRIP_W = MixerPanelController._MAX_STRIP_W
    _MIN_SLIDER_H = MixerPanelController._MIN_SLIDER_H
    _MAX_SLIDER_H = MixerPanelController._MAX_SLIDER_H
    _SLIDER_WIDTH_SCALE_CAP = MixerPanelController._SLIDER_WIDTH_SCALE_CAP
    _STRIP_CARD_GAP = MixerPanelController._STRIP_CARD_GAP
    _STRIP_ROW_MARGIN = MixerPanelController._STRIP_ROW_MARGIN
    _STRIP_MIN_OUTER_MARGIN = MixerPanelController._STRIP_MIN_OUTER_MARGIN
    _STRIP_MAX_OUTER_MARGIN = MixerPanelController._STRIP_MAX_OUTER_MARGIN
    _STRIP_MIN_INNER_SPACING = MixerPanelController._STRIP_MIN_INNER_SPACING
    _STRIP_MAX_INNER_SPACING = MixerPanelController._STRIP_MAX_INNER_SPACING
    _STRIP_MIN_FADER_SPACING = MixerPanelController._STRIP_MIN_FADER_SPACING
    _STRIP_MAX_FADER_SPACING = MixerPanelController._STRIP_MAX_FADER_SPACING
    _STRIP_MIN_PEAK_HEIGHT = MixerPanelController._STRIP_MIN_PEAK_HEIGHT
    _STRIP_MAX_PEAK_HEIGHT = MixerPanelController._STRIP_MAX_PEAK_HEIGHT
    _STRIP_MIN_LINK_SIZE = MixerPanelController._STRIP_MIN_LINK_SIZE
    _STRIP_MAX_LINK_SIZE = MixerPanelController._STRIP_MAX_LINK_SIZE
    _STRIP_MIN_MUTE_SIZE = MixerPanelController._STRIP_MIN_MUTE_SIZE
    _STRIP_MAX_MUTE_SIZE = MixerPanelController._STRIP_MAX_MUTE_SIZE
    _STRIP_MIN_MIC_GAIN_HEIGHT = MixerPanelController._STRIP_MIN_MIC_GAIN_HEIGHT
    _STRIP_MAX_MIC_GAIN_HEIGHT = MixerPanelController._STRIP_MAX_MIC_GAIN_HEIGHT

    def eventFilter(self, obj, event):
        self._mixer_panel_controller().handle_event_filter(obj, event)
        return super().eventFilter(obj, event)

    # Worst-case non-slider chrome budget for the strip row. This is used
    # to derive the vertical slider budget from the actual viewport height
    # before any clipping occurs.
    _STRIP_NON_SLIDER_HEIGHT_BUDGET = MixerPanelController._STRIP_NON_SLIDER_HEIGHT_BUDGET
    _STRIP_MIN_TOTAL_HEIGHT = MixerPanelController._STRIP_MIN_TOTAL_HEIGHT

    def _compute_mixer_strip_metrics(self, strips=None) -> MixerStripMetrics:
        return self._mixer_panel_controller().compute_strip_metrics(strips)

    def _measure_strip_heights(self, metrics, strips) -> int:
        return self._mixer_panel_controller().measure_strip_heights(metrics, strips)

    def _apply_mixer_strip_metrics(self, metrics, strips) -> None:
        self._mixer_panel_controller().apply_strip_metrics(metrics, strips)

    def resizeEvent(self, event):
        out = super().resizeEvent(event)
        if hasattr(self, "inputs_scroll"):
            self._rescale_strips()
        return out

    def _rescale_strips(self):
        self._mixer_panel_controller().rescale_strips()

    def _refresh(self):
        """Update UI to match PipeWire state without destroying everything.
        Skip when minimised to tray unless the settings dialog is open
        (app routing list needs to stay live when the user has it open)."""
        hidden_to_tray = self.tray is not None and not self.isVisible()
        settings_open  = self._settings_dialog_visible()
        if hidden_to_tray and not settings_open:
            return

        # Defer refresh while a slider is dragging — the next subscribe
        # event or backstop tick picks up missed state.
        if self._any_slider_dragging():
            self._event_refresh_timer.start()  # try again shortly
            return

        self.runtime.refresh_now("ui-refresh")
        if self._runtime_view_state is None:
            self.status_lbl.setText("PipeWire syncing...")



    def _on_master_vol_change(self, mix_name, value):
        # 40ms debounce — same reasoning as the per-strip faders.
        if '_pending_master_vol' not in self.__dict__:
            self._pending_master_vol = {}
            self._master_commit_timer = QTimer(self)
            self._master_commit_timer.setSingleShot(True)
            self._master_commit_timer.setInterval(40)
            self._master_commit_timer.timeout.connect(self._commit_master_vols)
        normalized = self._normalize_mix_volume(value / 100.0)
        self._pending_master_vol[mix_name] = normalized
        self._set_mix_master_volume(
            mix_name,
            normalized,
            persist=True,
            update_slider=False,
        )
        self._master_commit_timer.start()

    def _commit_master_vols(self):
        """Fire all pending master-fader writes in one pass."""
        if '_pending_master_vol' not in self.__dict__:
            return
        pending = self._pending_master_vol
        self._pending_master_vol = {}
        for mix_name, vol in pending.items():
            self.runtime.set_mix_volume(mix_name, vol)

    def _set_mix_output_target(self, mix_name, hw_sink_name, *, persist=True, update_combo=False,
                               sync_runtime=False, sync_runtime_refresh=True):
        self._desired_mix_hw[mix_name] = hw_sink_name
        runtime = getattr(self, "runtime", None)
        if runtime is not None:
            self._suppress_pactl_events_for(1.0)
            if sync_runtime and hasattr(runtime, "set_mix_hardware_route_sync"):
                runtime.set_mix_hardware_route_sync(
                    mix_name,
                    hw_sink_name,
                    refresh=bool(sync_runtime_refresh),
                )
            else:
                runtime.set_mix_hardware_route(mix_name, hw_sink_name)
        if update_combo:
            combo_name = "mon_out_combo" if mix_name == "Monitor" else "str_out_combo"
            combo = getattr(self, combo_name, None)
            if combo is not None and hasattr(combo, "blockSignals"):
                combo.blockSignals(True)
                try:
                    idx = combo.findData(hw_sink_name) if hasattr(combo, "findData") else -1
                    if idx >= 0 and hasattr(combo, "setCurrentIndex"):
                        combo.setCurrentIndex(idx)
                finally:
                    combo.blockSignals(False)
        if persist:
            self.schedule_save()

    def _on_mix_out_change(self, mix_name, hw_sink_name):
        self._set_mix_output_target(
            mix_name,
            hw_sink_name,
            persist=True,
            update_combo=False,
            sync_runtime=(mix_name == "Monitor"),
            sync_runtime_refresh=(mix_name != "Monitor"),
        )
        if mix_name == "Monitor" and hw_sink_name:
            self._record_preferred_monitor(hw_sink_name, view=self.__dict__.get("_runtime_view_state"))
            self._schedule_monitor_route_followups(hw_sink_name)

    def _schedule_monitor_route_followups(self, hw_sink_name):
        """Reassert app routing after a manual Monitor output switch.

        Changing the default sink can trigger a delayed pulse-side move of
        active app streams onto the hardware sink after the initial sync
        route already completed. Schedule the existing settle refresh passes
        so those streams get moved back onto their WaveLinux buses without
        doing more synchronous work on the UI thread.
        """
        settle_timer = (
            self.__dict__.get("_device_settle_refresh_timer")
            or self.__dict__.get("_hotplug_refresh_timer")
        )
        if settle_timer is not None and hasattr(settle_timer, "start"):
            settle_timer.start()
        reassert_timer = self.__dict__.get("_monitor_route_reassert_timer")
        if reassert_timer is not None and hasattr(reassert_timer, "start"):
            reassert_timer.start()
        target = str(hw_sink_name or "").strip().lower()
        stable_id = ""
        if target and not target.startswith("bt:"):
            try:
                stable_id = str(self._stable_sink_id_for_name(hw_sink_name) or "").strip().lower()
            except Exception:
                stable_id = ""
        if "bluez_output." not in target and not target.startswith("bt:") and not stable_id.startswith("bt:"):
            return
        bluetooth_timer = self.__dict__.get("_bluetooth_refresh_timer")
        if bluetooth_timer is not None and hasattr(bluetooth_timer, "start"):
            bluetooth_timer.start()
        bluetooth_reassert_timer = self.__dict__.get("_monitor_route_bluetooth_reassert_timer")
        if bluetooth_reassert_timer is not None and hasattr(bluetooth_reassert_timer, "start"):
            bluetooth_reassert_timer.start()

    def _reassert_persistent_state_after_monitor_switch(self, reason):
        if bool(self.__dict__.get("_shutting_down", False)):
            return
        self._request_runtime_refresh(reason)

    def _apply_pending_clipguard_migration(self):
        """Rewrite the legacy master-bus `clipguard: true` flag as a
        per-mic `limiter` on `selected_mic`'s active_effects. Idempotent
        — once cleared, refresh ticks won't re-add the limiter even if
        the user removed it."""
        mic = self.selected_mic
        if not mic:
            return
        chain = list(self.active_effects.get(mic, []))
        if 'limiter' not in chain:
            chain.append('limiter')
            self.active_effects[mic] = chain
            logging.info(
                f"Migrated master-bus clipguard=true → per-mic limiter on {mic}"
            )
            self._sync_runtime_persistent_state(immediate=True)
            self.schedule_save()
        self._pending_clipguard_migration = False

    def _sync_mic_picker(self, mics, default_src=None):
        self._mixer_panel_controller().sync_mic_picker(mics, default_src=default_src)

    def _input_display_sort_key(self, node_name, *, is_mic=False):
        return self._mixer_panel_controller().input_display_sort_key(node_name, is_mic=is_mic)

    def _sorted_input_nodes(self, mic_nodes, virtual_nodes):
        return self._mixer_panel_controller().sorted_input_nodes(mic_nodes, virtual_nodes)

    def _refresh_runtime_view(self):
        view = self._runtime_view_state
        if view is None:
            self.status_lbl.setText("PipeWire syncing...")
            return
        self._mixer_panel_controller().refresh_view(view)
        self._app_routing_panel_controller().refresh_view(view)

        if not getattr(view, "health", {}):
            self.status_lbl.setText(
                f"PipeWire connected · {getattr(view, 'node_count', 0)} nodes · "
                f"{getattr(view, 'app_count', 0)} apps"
            )

    def _on_mic_input_change(self, idx):
        """Persist a mic-picker change and request an immediate runtime
        reconcile without forcing a synchronous full UI refresh."""
        new_mic = self.mic_in_combo.itemData(idx)
        if new_mic == self.selected_mic:
            return
        self._set_selected_mic_target(
            new_mic,
            record_preference=True,
            persist=True,
            request_refresh=True,
            view=self.__dict__.get("_runtime_view_state"),
        )

    def _on_add_channel(self):
        text, ok = QInputDialog.getText(self, "Add Virtual Channel", "Channel Name:")
        if not (ok and text):
            return
        clean = re.sub(r'\s+', ' ', text).strip()
        if not clean:
            return
        self.runtime.ensure_virtual_channel_sync(clean)
        if clean not in self.virtual_channels:
            self.virtual_channels.append(clean)
            self.save_config()
        self._refresh()

    def _remove_sink(self, sink_name):
        self.runtime.remove_virtual_channel_sync(sink_name)
        # Drop whichever display-name entry maps to this sink_name.
        for display in list(self.virtual_channels):
            _, safe = PipeWireEngine._sanitize_channel_name(display)
            if f"wavelinux_{safe}" == sink_name or display == sink_name:
                self.virtual_channels.remove(display)
                break
        self.save_config()
        self._refresh()

    # `_on_clipguard_toggle` was the master-bus Clipguard handler. Removed
    # along with the button when Clipguard moved to the per-channel chain.
    # Migration of the saved `clipguard: true` flag happens in load_config.


    # Wave Link-style starter channels seeded on first install so users
    # don't open the app to an empty mixer. First-run only — deletions
    # aren't undone on subsequent launches.
    _DEFAULT_CHANNELS = ("Music", "Game", "Browser", "Voice Chat", "System")

    def _seed_default_channels(self):
        """Create the starter virtual channels and persist the resulting
        list. Called only when no config file exists yet."""
        for name in self._DEFAULT_CHANNELS:
            created = self.runtime.ensure_virtual_channel_sync(name)
            if created is not None:
                if name not in self.virtual_channels:
                    self.virtual_channels.append(name)

    def _serialize_config(self):
        scenes = self.__dict__.get('scenes', {})
        desired_mix_hw = self.__dict__.get("_desired_mix_hw", {}) or {}
        return {
            'schema_version': 1,
            'monitor_hw': desired_mix_hw.get("Monitor"),
            'stream_hw': desired_mix_hw.get("Stream"),
            'preferred_monitor_hw_id': self.__dict__.get('_preferred_monitor_hw_id', ""),
            'preferred_monitor_hw_name': self.__dict__.get('_preferred_monitor_hw_name', ""),
            'preferred_selected_mic_id': self.__dict__.get('_preferred_selected_mic_id', ""),
            'preferred_selected_mic_name': self.__dict__.get('_preferred_selected_mic_name', ""),
            'monitor_mix_volume': self._current_mix_master_volume("Monitor"),
            'stream_mix_volume': self._current_mix_master_volume("Stream"),
            'channels': list(self.virtual_channels),
            'scenes': self._normalize_scene_library(scenes),
            'onboarding_completed': bool(self.__dict__.get('_onboarding_completed', True)),
            'quick_start_template': self.__dict__.get('_selected_setup_template', ""),
            'selected_mic': self.selected_mic,
            'submixes': dict(self.submix_state),
            'hidden': sorted(self.hidden_nodes),
            'app_routing': {
                k: v for k, v in self.app_routing.items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            'app_volumes': {
                k: v for k, v in self._normalize_app_volume_prefs(
                    getattr(self, "app_volumes", {}),
                ).items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            'channel_order': list(self.channel_order),
            'effect_params': self.effect_params,
            'active_effects': self.active_effects,
            'app_last_seen': {
                k: v for k, v in self.app_last_seen.items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            'app_display_names': {
                k: v for k, v in self.app_display_names.items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            'app_identity_overrides': self._normalize_app_identity_overrides(
                self.__dict__.get("app_identity_overrides", {}),
            ),
            'app_label_overrides': self._normalize_app_label_overrides(
                self.__dict__.get("app_label_overrides", {}),
            ),
            'app_prune_days': self.app_prune_days,
            'forgotten_apps': sorted(
                name for name in self.forgotten_apps
                if PipeWireEngine.is_persistent_app_id(name)
            ),
        }

    @staticmethod
    def _write_config_file(path, payload):
        tmp = path + '.tmp'
        with open(tmp, 'w') as f:
            json.dump(payload, f, indent=4)
        os.replace(tmp, path)

    def _apply_config_dict(self, conf, *, remove_missing_virtuals=False):
        if not isinstance(conf, dict):
            raise ValueError("WaveLinux config must be a JSON object.")
        runtime = getattr(self, "runtime", None)
        engine = getattr(self, "engine", None)
        previous_virtuals = list(self.__dict__.get("virtual_channels", []) or [])
        self.submix_state = self._migrate_submix_state(conf.get('submixes', {}))
        self.hidden_nodes = self._migrate_hidden_nodes(conf.get('hidden', []))
        self.app_routing = {
            k: v for k, v in (conf.get('app_routing', {}) or {}).items()
            if PipeWireEngine.is_persistent_app_id(k)
        }
        self.app_volumes = self._normalize_app_volume_prefs(conf.get('app_volumes', {}))
        self.app_last_seen = {
            k: int(v) for k, v in (conf.get('app_last_seen', {}) or {}).items()
            if isinstance(k, str) and isinstance(v, (int, float))
            and PipeWireEngine.is_persistent_app_id(k)
        }
        self.app_display_names = {
            k: v for k, v in (conf.get('app_display_names', {}) or {}).items()
            if isinstance(k, str) and isinstance(v, str) and PipeWireEngine.is_persistent_app_id(k)
        }
        self.app_identity_overrides = self._normalize_app_identity_overrides(
            conf.get('app_identity_overrides', {}),
        )
        self.app_label_overrides = self._normalize_app_label_overrides(
            conf.get('app_label_overrides', {}),
        )
        self.app_prune_days = int(conf.get('app_prune_days', self.app_prune_days) or 14)
        self.forgotten_apps = {
            name for name in (conf.get('forgotten_apps', []) or [])
            if isinstance(name, str) and PipeWireEngine.is_persistent_app_id(name)
        }
        for name in list(self.app_routing.keys()):
            if PipeWireEngine.name_matches_host(name):
                self.app_routing.pop(name, None)
        for name in list(self.app_volumes.keys()):
            if PipeWireEngine.name_matches_host(name):
                self.app_volumes.pop(name, None)
        for name in list(self.app_last_seen.keys()):
            if PipeWireEngine.name_matches_host(name):
                self.app_last_seen.pop(name, None)
                self.app_display_names.pop(name, None)
        for app_id in (
            set(self.app_routing)
            | set(self.app_volumes)
            | set(self.app_last_seen)
            | set(self.forgotten_apps)
            | set(self.app_identity_overrides.values())
            | set(self.app_label_overrides.keys())
        ):
            self.app_display_names.setdefault(
                app_id,
                PipeWireEngine.display_name_for_app_id(app_id),
            )
        for app_id, label in self.app_label_overrides.items():
            self.app_display_names[app_id] = label
        self._set_engine_identity_overrides()
        self._prune_stale_apps()
        self.virtual_channels = self._dedupe_names(conf.get('channels', []) or [])
        self.scenes = self._normalize_scene_library(conf.get('scenes', {}))
        self._onboarding_completed = bool(conf.get('onboarding_completed', True))
        self._selected_setup_template = str(conf.get('quick_start_template') or "")
        self.channel_order = self._dedupe_names(conf.get('channel_order', []) or [])
        self.selected_mic = None
        self._mic_selection_initialized = False
        self._preferred_monitor_hw_id = str(
            conf.get('preferred_monitor_hw_id')
            or self._stable_sink_id_for_name(conf.get('monitor_hw'))
            or ""
        ).strip()
        self._preferred_monitor_hw_name = str(
            conf.get('preferred_monitor_hw_name')
            or conf.get('monitor_hw')
            or ""
        ).strip()
        self._preferred_selected_mic_id = str(
            conf.get('preferred_selected_mic_id')
            or self._stable_source_id_for_name(conf.get('selected_mic'))
            or ""
        ).strip()
        self._preferred_selected_mic_name = str(
            conf.get('preferred_selected_mic_name')
            or conf.get('selected_mic')
            or ""
        ).strip()
        self._restorable_monitor_hw_id = ""
        self._restorable_monitor_hw_name = ""
        self._restorable_selected_mic_id = ""
        self._restorable_selected_mic_name = ""
        self._active_monitor_fallback = False
        self._active_mic_fallback = False
        self._set_mix_master_volume(
            "Monitor",
            conf.get('monitor_mix_volume', 1.0),
            persist=False,
            update_slider=True,
        )
        self._set_mix_master_volume(
            "Stream",
            conf.get('stream_mix_volume', 1.0),
            persist=False,
            update_slider=True,
        )
        self.effect_params = conf.get('effect_params', {}) or {}
        self.active_effects = {
            k: list(v) for k, v in (conf.get('active_effects', {}) or {}).items()
            if isinstance(v, list)
        }

        _comp_key_remap = {
            'threshold_db':  'Threshold level (dB)',
            'ratio':         'Ratio (1:n)',
            'attack_ms':     'Attack time (ms)',
            'release_ms':    'Release time (ms)',
            'makeup_gain_db':'Makeup gain (dB)',
        }
        for _node_params in self.effect_params.values():
            _comp = _node_params.get('compressor')
            if not isinstance(_comp, dict):
                continue
            for _old, _new in _comp_key_remap.items():
                if _old in _comp and _new not in _comp:
                    _comp[_new] = _comp.pop(_old)
                elif _old in _comp:
                    _comp.pop(_old, None)
        self._normalize_loaded_effect_state()

        if runtime is not None:
            runtime.ensure_output_mix_sync("Monitor", refresh=False)
            runtime.ensure_output_mix_sync("Stream", refresh=False)

        self._pending_clipguard_migration = bool(conf.get('clipguard'))
        if self._pending_clipguard_migration and self.selected_mic:
            self._apply_pending_clipguard_migration()

        startup_mic = self._resolve_startup_mic_target()
        if startup_mic:
            self._set_selected_mic_target(
                startup_mic,
                record_preference=True,
                persist=False,
                request_refresh=False,
                view=self.__dict__.get("_runtime_view_state"),
            )

        mon_hw = self._resolve_startup_monitor_target()
        str_hw = conf.get('stream_hw')
        self._set_mix_output_target(
            "Monitor",
            mon_hw,
            persist=False,
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        self._record_preferred_monitor(mon_hw, view=self.__dict__.get("_runtime_view_state"))
        self._set_mix_output_target(
            "Stream",
            str_hw,
            persist=False,
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        if remove_missing_virtuals and runtime is not None:
            for name in previous_virtuals:
                if name in self.virtual_channels:
                    continue
                _, safe = PipeWireEngine._sanitize_channel_name(name)
                runtime.remove_virtual_channel_sync(f"wavelinux_{safe}", refresh=False)
        if runtime is not None:
            for name in self.virtual_channels:
                runtime.ensure_virtual_channel_sync(name, refresh=False)
        # Materialize config-backed virtual sinks before the immediate
        # startup reconcile. Otherwise startup can spend several seconds
        # bringing up the selected mic FX/default-source path while the
        # app buses do not exist yet, and routed playback opens into a
        # silent graph.
        self._sync_runtime_persistent_state(immediate=True)
        if runtime is not None:
            self._request_runtime_refresh("post-config-virtual-sync")
        config = self._serialize_config()
        event_bus = self.__dict__.get("_event_bus")
        if event_bus is not None:
            event_bus.publish(ConfigChanged(config=config))
        manager = self.__dict__.get("module_manager")
        if manager is not None:
            manager.on_config_changed(config)

    def load_config(self):
        if not os.path.exists(self.config_path):
            # First launch — set up the standard mixes, route Monitor to
            # the system default, and seed starter channels.
            self._onboarding_completed = False
            self._selected_setup_template = ""
            self._show_first_run_setup = True
            self.runtime.ensure_output_mix_sync("Monitor", refresh=False)
            self.runtime.ensure_output_mix_sync("Stream", refresh=False)
            startup_mic = self._resolve_startup_mic_target()
            if startup_mic:
                self._set_selected_mic_target(
                    startup_mic,
                    record_preference=True,
                    persist=False,
                    request_refresh=False,
                    view=self.__dict__.get("_runtime_view_state"),
                )
            def_sink = self._resolve_startup_monitor_target()
            if def_sink:
                self._set_mix_output_target(
                    "Monitor",
                    def_sink,
                    persist=False,
                    update_combo=True,
                    sync_runtime=True,
                    sync_runtime_refresh=False,
                )
                self._record_preferred_monitor(def_sink, view=self.__dict__.get("_runtime_view_state"))
            self._seed_default_channels()
            self._sync_runtime_persistent_state(immediate=True)
            self.save_config()
            return

        try:
            with open(self.config_path, 'r') as f:
                conf = json.load(f)
            self._apply_config_dict(conf, remove_missing_virtuals=True)
        except Exception as e:
            logging.error(f"Error loading config: {e}")

    @staticmethod
    def _migrate_submix_state(raw):
        """Drop entries keyed by ephemeral pw_id (`42_Monitor`). Current
        keys are `<node.name>_Monitor`/`_Stream` so they survive a
        PipeWire restart. Only drops keys whose prefix int-parses AND
        whose suffix is exactly `_Monitor`/`_Stream` so other suffixes
        (`_linked` etc.) and numeric-looking node.names aren't touched."""
        if not isinstance(raw, dict):
            return {}
        clean = {}
        legacy_suffixes = ('_Monitor', '_Stream')
        for key, val in raw.items():
            if not isinstance(key, str) or '_' not in key:
                continue
            if key.endswith(legacy_suffixes):
                prefix = key.rsplit('_', 1)[0]
                try:
                    int(prefix)
                    # Legacy {pw_id}_{Monitor|Stream} key — drop.
                    continue
                except ValueError:
                    pass
            clean[key] = val
        return clean

    @staticmethod
    def _migrate_hidden_nodes(raw):
        """Only keep entries that look like node.names (non-empty strings).
        Old configs stored ints; those are worthless across restarts."""
        if not isinstance(raw, (list, set, tuple)):
            return set()
        return {entry for entry in raw if isinstance(entry, str) and entry}

    def save_config(self):
        # `clipguard` (legacy master-bus flag) is not written — it's
        # been replaced by the per-mic `limiter` effect.
        try:
            self._flush_pending_ui_state()
            config = self._serialize_config()
            self._write_config_file(self.config_path, config)
            event_bus = self.__dict__.get("_event_bus")
            if event_bus is not None:
                event_bus.publish(ConfigChanged(config=config))
            manager = self.__dict__.get("module_manager")
            if manager is not None:
                manager.on_config_changed(config)
        except Exception as e:
            logging.error(f"Error saving config: {e}")

    def _backup_current_config(self):
        if not os.path.exists(self.config_path):
            return ""
        stamp = time.strftime("%Y%m%d-%H%M%S")
        backup_path = f"{self.config_path}.{stamp}.bak"
        shutil.copy2(self.config_path, backup_path)
        return backup_path

    def _export_full_config(self):
        default_name = os.path.join(
            os.path.dirname(self.config_path),
            f"wavelinux-export-{time.strftime('%Y%m%d-%H%M%S')}.json",
        )
        path, _ = QFileDialog.getSaveFileName(
            self.settings_dialog,
            "Export WaveLinux Config",
            default_name,
            "JSON Files (*.json)",
        )
        if not path:
            return
        if not path.lower().endswith(".json"):
            path += ".json"
        try:
            self._write_config_file(path, self._serialize_config())
        except Exception as exc:
            QMessageBox.warning(
                self.settings_dialog,
                "Config export failed",
                str(exc),
            )
            return
        self.status_lbl.setText("Config exported")
        QMessageBox.information(
            self.settings_dialog,
            "Config exported",
            f"Saved WaveLinux config to:\n{path}",
        )

    def _import_full_config(self):
        path, _ = QFileDialog.getOpenFileName(
            self.settings_dialog,
            "Import WaveLinux Config",
            os.path.dirname(self.config_path),
            "JSON Files (*.json)",
        )
        if not path:
            return
        yn = QMessageBox.question(
            self.settings_dialog,
            "Import WaveLinux Config",
            "Replace the current WaveLinux configuration with the selected file?\n\n"
            "This overwrites scenes, routing, FX, and saved app state.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        try:
            with open(path, "r") as fh:
                payload = json.load(fh)
            backup_path = self._backup_current_config()
            self._apply_config_dict(payload, remove_missing_virtuals=True)
            self.save_config()
        except Exception as exc:
            QMessageBox.warning(
                self.settings_dialog,
                "Config import failed",
                str(exc),
            )
            return
        self._refresh_scenes_tab()
        self._refresh_hidden_list()
        self._refresh_advanced_tab()
        self._refresh_system_tab()
        self._refresh_update_tab()
        self._refresh()
        self.status_lbl.setText("Config imported")
        msg = f"Imported WaveLinux config from:\n{path}"
        if backup_path:
            msg += f"\n\nBackup of the previous config:\n{backup_path}"
        QMessageBox.information(
            self.settings_dialog,
            "Config imported",
            msg,
        )



    def forget_app(self, app_id):
        """Drop all state for an app and add it to the persistent
        `forgotten_apps` blocklist. Recover via Settings → Advanced →
        'Restore forgotten apps' or by editing config.json."""
        # System Sounds is a permanent built-in entry — never let it
        # land in the blocklist, even if something calls forget on it.
        if app_id == PipeWireEngine.SYSTEM_SOUNDS_BUCKET:
            return
        self.app_routing.pop(app_id, None)
        self.app_volumes.pop(app_id, None)
        self.app_last_seen.pop(app_id, None)
        self.app_display_names.pop(app_id, None)
        self.forgotten_apps.add(app_id)
        widget = self.app_widgets.pop(app_id, None)
        if widget is not None:
            widget.setParent(None)
            widget.deleteLater()
        self.save_config()
        self._refresh()

    def _prune_stale_apps(self):
        """Drop saved app_routing / app_last_seen entries we haven't
        seen in `app_prune_days`. Last-seen stamps are refreshed every
        tick the app is active, so quietly-running apps keep their slot."""
        if self.app_prune_days <= 0:
            return
        cutoff = int(time.time()) - self.app_prune_days * 24 * 3600
        stale_routed = [
            name for name in (
                set(self.app_routing.keys())
                | set(getattr(self, "app_volumes", {}).keys())
            )
            if self.app_last_seen.get(name) is not None
            and self.app_last_seen.get(name, 0) < cutoff
        ]
        for name in stale_routed:
            self.app_routing.pop(name, None)
            self.app_volumes.pop(name, None)
            self.app_last_seen.pop(name, None)
            if name not in self.forgotten_apps:
                self.app_display_names.pop(name, None)
        # Apps without a saved routing also get reaped past the cutoff.
        stale_seen = [
            name for name, ts in list(self.app_last_seen.items())
            if ts < cutoff
        ]
        for name in stale_seen:
            self.app_last_seen.pop(name, None)
            if (
                name not in self.app_routing
                and name not in getattr(self, "app_volumes", {})
                and name not in self.forgotten_apps
            ):
                self.app_display_names.pop(name, None)
        total = len(stale_routed) + len(stale_seen)
        if total:
            logging.info(
                f"Pruned {total} stale app entries "
                f"({len(stale_routed)} routed, {len(stale_seen)} seen-only)"
            )

    # ── Channel reorder / rename ──────────────────────────────────

    def move_channel(self, node_name, delta):
        """Move a channel left (-1) or right (+1) in the persistent order."""
        order = list(self.channel_order)
        if node_name not in order:
            order.append(node_name)
        # Append currently-visible names that aren't in the order yet.
        visible_names = [s.node_name for s in self.channel_widgets.values() if s.node_name]
        for nm in visible_names:
            if nm not in order:
                order.append(nm)

        idx = order.index(node_name)
        new_idx = max(0, min(len(order) - 1, idx + delta))
        if new_idx == idx:
            return
        order.pop(idx)
        order.insert(new_idx, node_name)
        self.channel_order = order
        self.save_config()
        self._relayout_channel_strips()

    def _relayout_channel_strips(self):
        """Re-home existing ChannelStrips in `channel_order`."""
        self._mixer_panel_controller().relayout_channel_strips()

    def rename_channel(self, old_node_name):
        """Rename a user-created virtual channel. Hardware mic
        node.names aren't ours to rename."""
        if not old_node_name.startswith("wavelinux_"):
            QMessageBox.information(self, "Rename",
                                    "Only virtual channels can be renamed.")
            return
        old_display = old_node_name.replace("wavelinux_", "").replace("_", " ").title()
        new_name, ok = QInputDialog.getText(
            self, "Rename Channel", "New name:", text=old_display,
        )
        if not (ok and new_name):
            return
        cleaned = re.sub(r"\s+", " ", new_name).strip()
        if not cleaned:
            return
        new_sink = self.runtime.rename_virtual_channel_sync(old_node_name, cleaned)
        if not new_sink:
            QMessageBox.warning(self, "Rename", "Could not rename the channel.")
            return

        # Migrate persistent state that keys by node.name.
        self._rekey_state(old_node_name, new_sink)
        # Replace the display name in virtual_channels too.
        for i, name in enumerate(self.virtual_channels):
            _, safe = PipeWireEngine._sanitize_channel_name(name)
            if f"wavelinux_{safe}" == old_node_name:
                self.virtual_channels[i] = cleaned
                break
        else:
            self.virtual_channels.append(cleaned)

        # Drop the widget so _refresh re-creates it with the new label.
        for pw_id, widget in list(self.channel_widgets.items()):
            if widget.node_name == old_node_name:
                self.input_layout.removeWidget(widget)
                widget.setParent(None)
                widget.deleteLater()
                del self.channel_widgets[pw_id]
                meter = self.meters.pop(pw_id, None)
                if meter is not None:
                    meter.stop()
        self.save_config()
        self._refresh()

    def _rekey_state(self, old_name, new_name):
        """Migrate everything keyed by node.name when a channel is renamed."""
        # `_linked` lives in submix_state with the same `<node>_<suffix>`
        # key shape as Monitor/Stream/gain, so it must migrate too —
        # otherwise the Link toggle silently resets after a rename.
        for suffix in ("Monitor", "Stream", "gain", "linked"):
            k_old = f"{old_name}_{suffix}"
            if k_old in self.submix_state:
                self.submix_state[f"{new_name}_{suffix}"] = self.submix_state.pop(k_old)
        if old_name in self.hidden_nodes:
            self.hidden_nodes.discard(old_name)
            self.hidden_nodes.add(new_name)
        if old_name in self.effect_params:
            self.effect_params[new_name] = self.effect_params.pop(old_name)
        if old_name in self.active_effects:
            self.active_effects[new_name] = self.active_effects.pop(old_name)
        if old_name in self.channel_order:
            i = self.channel_order.index(old_name)
            self.channel_order[i] = new_name



    # ── Autostart ─────────────────────────────────────────────────

    @property
    def autostart_path(self):
        return os.path.expanduser(f"~/.config/autostart/{DESKTOP_FILENAME}")

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
        exec_cmd = desktop_exec_command()
        contents = (
            "[Desktop Entry]\n"
            "Type=Application\n"
            "Name=WaveLinux\n"
            f"Exec={exec_cmd}\n"
            "Icon=wavelinux\n"
            "X-GNOME-Autostart-enabled=true\n"
            "NoDisplay=false\n"
        )
        with open(path, "w") as f:
            f.write(contents)

    # ── Card profiles ─────────────────────────────────────────────

    def _open_card_profiles(self):
        dlg = CardProfileDialog(self.engine, self.runtime, self)
        dlg.exec()

    def _show_notification(self, title, body):
        """Tray bubble + log entry. Used for update-available notices and
        any other one-shot user-visible message. No-op when no tray is
        available, so we never crash a headless run."""
        if self.tray is not None and self.tray.isVisible():
            try:
                self.tray.showMessage(title, body, self.tray_icon_obj, 3000)
            except Exception:
                pass
        logging.info(f"{title}: {body}")

    def _notify_hotplug(self, node_names, *, added):
        """Show a tray bubble (if available) so the user knows WaveLinux saw
        a device come or go — the refresh loop already rebuilds routes, but
        silence here is confusing."""
        view = self.__dict__.get("_runtime_view_state")
        pretty_names = []
        for node_name in list(node_names)[:3]:
            source_label = self._display_name_for_source_name(node_name, view=view)
            if source_label and source_label != node_name:
                pretty_names.append(source_label)
                continue
            sink_label = self._display_name_for_sink_name(node_name, view=view)
            if sink_label and sink_label != node_name:
                pretty_names.append(sink_label)
                continue
            pretty_names.append(
                PipeWireEngine.friendly_name(
                    node_name.replace("wavelinux_", "").replace("_", " ")
                )
            )
        pretty = ", ".join(
            pretty_names
        )
        suffix = "" if len(node_names) <= 3 else f" (+{len(node_names) - 3} more)"
        if added:
            title, body = "Device connected", f"{pretty}{suffix}"
        else:
            title, body = "Device disconnected", f"{pretty}{suffix}"
        self._show_notification(title, body)

    def hide_node(self, node_name):
        self.hidden_nodes.add(node_name)
        self.schedule_save()
        self._refresh()

    def unhide_node(self, node_name):
        self.hidden_nodes.discard(node_name)
        self.schedule_save()
        self._refresh()



    def _setup_tray(self):
        self.tray = None
        if not QSystemTrayIcon.isSystemTrayAvailable():
            logging.info("No system tray available; closing the window will quit.")
            return

        self.tray = QSystemTrayIcon(self)
        self.tray.setIcon(self.tray_icon_obj)

        menu = QMenu()
        show_act = QAction("Show WaveLinux", self)
        show_act.triggered.connect(self.showNormal)
        menu.addAction(show_act)

        profiles_act = QAction("Sound Card Profiles…", self)
        profiles_act.triggered.connect(self._open_card_profiles)
        menu.addAction(profiles_act)

        self.autostart_act = QAction("Start at login", self)
        self.autostart_act.setCheckable(True)
        self.autostart_act.setChecked(self.is_autostart_enabled())
        self.autostart_act.toggled.connect(self.set_autostart)
        menu.addAction(self.autostart_act)

        menu.addSeparator()
        quit_act = QAction("Quit WaveLinux", self)
        quit_act.triggered.connect(self._request_quit_app)
        menu.addAction(quit_act)

        self.tray.setContextMenu(menu)
        self.tray.activated.connect(self._on_tray_activated)
        self.tray.show()

    def _on_tray_activated(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            if self.isVisible():
                self.hide()
            else:
                self.showNormal()

    def hideEvent(self, event):
        super().hideEvent(event)
        if self.tray is not None and not self.isVisible():
            self._stop_all_meters()

    def showEvent(self, event):
        super().showEvent(event)
        QTimer.singleShot(0, self._refresh)

    def _request_quit_app(self):
        if getattr(self, "_quit_in_progress", False):
            return
        self._quit_in_progress = True
        self._shutting_down = True
        self._suppress_pactl_events_for(3.0)
        if hasattr(self, "status_lbl"):
            self.status_lbl.setText("Shutting down WaveLinux...")
        self._close_open_dialogs_for_quit()
        if self.tray is not None:
            self.tray.hide()
        self.setEnabled(False)
        QTimer.singleShot(0, self._quit_app)

    def closeEvent(self, event):
        """Minimize to tray when one is available; otherwise actually quit."""
        if getattr(self, "_quit_in_progress", False):
            event.accept()
            return
        if self.tray is not None and self.tray.isVisible():
            event.ignore()
            self.hide()
            return
        self._request_quit_app()
        event.accept()

    def _close_open_dialogs_for_quit(self):
        app = QApplication.instance()
        if app is None:
            return
        for widget in list(app.topLevelWidgets()):
            if widget is None or widget is self:
                continue
            if isinstance(widget, QDialog):
                try:
                    widget.hide()
                    widget.done(QDialog.DialogCode.Rejected)
                    widget.close()
                except Exception:
                    pass

    def _stop_all_meters(self):
        self._mixer_panel_controller().stop_all_meters()

    def _quit_app(self):
        """Cleanly save state, unload all modules, and exit."""
        if getattr(self, "_runtime_stopped", False):
            QApplication.instance().quit()
            return
        logging.info("Shutting down WaveLinux...")
        self._shutting_down = True
        manager = getattr(self, "module_manager", None)
        if manager is not None:
            manager.stop_all("app-quit")
        self.refresh_timer.stop()
        self._save_timer.stop()
        self._event_refresh_timer.stop()
        self._event_proc_restart_timer.stop()
        self._device_settle_refresh_timer.stop()
        self._bluetooth_refresh_timer.stop()
        runtime_view_timer = getattr(self, "_runtime_view_refresh_timer", None)
        if runtime_view_timer is not None:
            runtime_view_timer.stop()
        mic_cutover_timer = getattr(self, "_mic_cutover_refresh_timer", None)
        if mic_cutover_timer is not None:
            mic_cutover_timer.stop()
        # Stop every parec meter subprocess.
        self._stop_all_meters()
        # Flush any pending slider writes before we tear down the engine.
        self.save_config()
        proc = getattr(self, "_event_proc", None)
        if proc is not None and proc.state() != QProcess.ProcessState.NotRunning:
            proc.terminate()
            if not proc.waitForFinished(300):
                proc.kill()
                proc.waitForFinished(500)
        self._clear_runtime_pid()
        self.runtime.full_audio_reset_sync(refresh=False)
        self.runtime.shutdown()
        self._runtime_stopped = True
        logging.info("Audio reset complete. Exiting.")
        QApplication.instance().quit()

    def _cleanup_before_exit(self):
        self._stop_stress_control()
        self._clear_runtime_pid()
        if not getattr(self, "_runtime_stopped", False):
            self.runtime.cleanup_sync()


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
