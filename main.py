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
from types import SimpleNamespace

from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QLabel, QSlider, QPushButton, QFrame, QScrollArea, QDialog,
    QDialogButtonBox, QComboBox, QMessageBox, QSystemTrayIcon,
    QMenu, QInputDialog, QProgressBar, QSizePolicy, QTabWidget,
    QFileDialog,
    QSpinBox, QCheckBox
)
from PyQt6.QtCore import Qt, QTimer, QLockFile, QProcess, pyqtSignal, QObject, QEvent, QUrl
from PyQt6.QtGui import QFont, QIcon, QAction, QDesktopServices

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
from pipewire_engine import PipeWireEngine
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

import struct

APP_VERSION = "2.0.8"
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


class MeterWorker(QObject):
    """Emit normalized channel peaks from `parec`."""

    peak = pyqtSignal(float)

    def __init__(self, source_name, parent=None):
        super().__init__(parent)
        self.source_name = source_name
        self._proc = None
        self._sample_rate = 24000
        self._frame_hz = 20
        self._sample_bytes = self._sample_rate * 2 // self._frame_hz
        self._decode_queue = queue.Queue(maxsize=6)
        self._decode_stop = threading.Event()
        self._decode_thread = None

    @staticmethod
    def _frame_peak(frame, last_peak):
        count = len(frame) // 2
        if count <= 0:
            return last_peak
        samples = struct.unpack(f"<{count}h", frame)
        peak_int = max((abs(s) for s in samples), default=0)
        normalized = peak_int / 32768.0
        if normalized >= last_peak:
            return normalized
        return max(normalized, last_peak * 0.6)

    def start(self):
        if self._proc is not None:
            return
        self._decode_stop.clear()
        self._drain_decode_queue()
        if self._decode_thread is None or not self._decode_thread.is_alive():
            self._decode_thread = threading.Thread(
                target=self._decode_loop,
                daemon=True,
                name=f"meter:{self.source_name}",
            )
            self._decode_thread.start()
        self._proc = QProcess(self)
        self._proc.setProcessChannelMode(QProcess.ProcessChannelMode.SeparateChannels)
        self._proc.readyReadStandardOutput.connect(self._on_bytes)
        args = [
            f"--device={self.source_name}",
            f"--rate={self._sample_rate}",
            "--format=s16le",
            "--channels=1",
            "--raw",
            "--latency-msec=50",
        ]
        self._proc.start("parec", args)

    def stop(self):
        if self._proc is None:
            return
        try:
            self._proc.kill()
            self._proc.waitForFinished(200)
        except Exception:
            pass
        self._proc = None
        self._decode_stop.set()
        try:
            self._decode_queue.put_nowait(None)
        except queue.Full:
            try:
                self._decode_queue.get_nowait()
            except queue.Empty:
                pass
            try:
                self._decode_queue.put_nowait(None)
            except queue.Full:
                pass
        thread = self._decode_thread
        if thread is not None and thread.is_alive():
            thread.join(timeout=0.3)
        self._decode_thread = None
        self._drain_decode_queue()

    def _on_bytes(self):
        if self._proc is None:
            return
        chunk = bytes(self._proc.readAllStandardOutput())
        if not chunk:
            return
        try:
            self._decode_queue.put_nowait(chunk)
        except queue.Full:
            try:
                self._decode_queue.get_nowait()
            except queue.Empty:
                pass
            try:
                self._decode_queue.put_nowait(chunk)
            except queue.Full:
                pass

    def _decode_loop(self):
        buf = bytearray()
        last_peak = 0.0
        while not self._decode_stop.is_set():
            try:
                chunk = self._decode_queue.get(timeout=0.2)
            except queue.Empty:
                continue
            if chunk is None:
                break
            buf.extend(chunk)
            while len(buf) >= self._sample_bytes:
                frame = bytes(buf[:self._sample_bytes])
                del buf[:self._sample_bytes]
                last_peak = self._frame_peak(frame, last_peak)
                self.peak.emit(min(last_peak, 1.0))

    def _drain_decode_queue(self):
        while True:
            try:
                self._decode_queue.get_nowait()
            except queue.Empty:
                return


# ── ALSA card / profile picker ────────────────────────────────────
class CardProfileDialog(QDialog):
    """Lets the user pick an ALSA card profile (Analog Stereo vs Pro
    Audio, etc.) directly from WaveLinux so they don't have to drop
    into pavucontrol."""

    def __init__(self, engine, runtime=None, parent=None):
        super().__init__(parent)
        self.engine = engine
        self.runtime = runtime
        self.setWindowTitle("Sound Card Profiles")
        self.setMinimumWidth(520)
        self.setStyleSheet(STYLESHEET)
        self._combos = []   # (card_name, combo)

        self._layout = QVBoxLayout(self)
        self._layout.setSpacing(12)
        self._layout.setContentsMargins(20, 20, 20, 20)

        title = QLabel("🎛 Sound Card Profiles")
        title.setStyleSheet("font-size: 18px; font-weight: bold; color: #fff;")
        self._layout.addWidget(title)

        desc = QLabel("Pick the ALSA profile for each card — e.g. Analog Stereo "
                      "for headphones or Pro Audio for interfaces with many channels.")
        desc.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        desc.setWordWrap(True)
        self._layout.addWidget(desc)

        # Placeholder while cards load on background thread.
        self._loading_lbl = QLabel("Loading sound cards…")
        self._loading_lbl.setStyleSheet("color: #6b6b82; padding: 24px;")
        self._loading_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._layout.addWidget(self._loading_lbl)

        self._cards_container = QWidget()
        self._cards_layout = QVBoxLayout(self._cards_container)
        self._cards_layout.setContentsMargins(0, 0, 0, 0)
        self._cards_layout.setSpacing(10)
        self._cards_container.setVisible(False)
        self._layout.addWidget(self._cards_container)

        self._btns = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Apply | QDialogButtonBox.StandardButton.Close
        )
        self._btns.button(QDialogButtonBox.StandardButton.Apply).clicked.connect(self._apply)
        self._btns.button(QDialogButtonBox.StandardButton.Apply).setEnabled(False)
        self._btns.button(QDialogButtonBox.StandardButton.Close).clicked.connect(self.accept)
        self._layout.addWidget(self._btns)

        # Load cards on a background thread so __init__ doesn't block.
        self._card_queue = queue.SimpleQueue()
        self._card_poll = QTimer(self)
        self._card_poll.setInterval(30)
        self._card_poll.timeout.connect(self._poll_cards)
        self._card_poll.start()
        threading.Thread(target=self._load_cards_bg, daemon=True).start()

    def _load_cards_bg(self):
        try:
            cards = self.engine.list_cards()
            self._card_queue.put(('ok', cards))
        except Exception as e:
            self._card_queue.put(('error', str(e)))

    def _poll_cards(self):
        try:
            msg = self._card_queue.get_nowait()
        except queue.Empty:
            return
        self._card_poll.stop()
        self._loading_lbl.setVisible(False)

        status, data = msg
        if status == 'error' or not data:
            empty = QLabel("No cards reported by PipeWire.")
            empty.setStyleSheet("color: #6b6b82; padding: 24px;")
            self._cards_layout.addWidget(empty)
        else:
            for card in data:
                row = QFrame()
                row.setObjectName("fxItemFrame")
                row.setStyleSheet(
                    "QFrame#fxItemFrame {"
                    " background: rgba(255,255,255,0.03);"
                    " border: 1px solid rgba(255,255,255,0.06);"
                    " border-radius: 10px; padding: 10px; }"
                )
                rlay = QVBoxLayout(row)
                name_lbl = QLabel(card.get('description') or card['name'])
                name_lbl.setStyleSheet("color: #e0e0ee; font-weight: bold;")
                rlay.addWidget(name_lbl)
                subtle = QLabel(card['name'])
                subtle.setStyleSheet("color: #6b6b82; font-size: 10px;")
                rlay.addWidget(subtle)

                combo = QComboBox()
                for prof in card['profiles']:
                    label = prof['description']
                    if not prof['available']:
                        label += "  (unavailable)"
                    combo.addItem(label, prof['name'])
                    if not prof['available']:
                        item = combo.model().item(combo.count() - 1)
                        if item is not None:
                            item.setEnabled(False)
                idx = combo.findData(card['active_profile'])
                if idx >= 0:
                    combo.setCurrentIndex(idx)
                rlay.addWidget(combo)
                self._cards_layout.addWidget(row)
                self._combos.append((card['name'], combo))

        self._cards_container.setVisible(True)
        self._btns.button(QDialogButtonBox.StandardButton.Apply).setEnabled(bool(self._combos))

    def _apply(self):
        for card_name, combo in self._combos:
            target = combo.currentData()
            if target:
                if self.runtime is not None:
                    self.runtime.set_card_profile(card_name, target)
                else:
                    threading.Thread(
                        target=self.engine.set_card_profile,
                        args=(card_name, target),
                        daemon=True,
                    ).start()


# ── FX Selection Dialog ───────────────────────────────────────────
class FXSelectionDialog(QDialog):
    _TOGGLE_STYLE = (
        "QPushButton[role=\"fxToggle\"] {"
        " background: #1a1a28; color: #8b8b9e;"
        " border: 1px solid rgba(255,255,255,0.12);"
        " border-radius: 6px; font-weight: bold; padding: 4px 0; }"
        "QPushButton[role=\"fxToggle\"]:hover {"
        " border-color: rgba(0,229,255,0.4); }"
        "QPushButton[role=\"fxToggle\"]:checked {"
        " background: #00e5ff; color: #0d0d14;"
        " border-color: #00e5ff; }"
        "QPushButton[role=\"fxToggle\"]:disabled {"
        " background: #14141e; color: #555568;"
        " border-color: rgba(255,255,255,0.06); }"
    )
    _TOGGLE_FAILED_STYLE = (
        "QPushButton[role=\"fxToggle\"] {"
        " background: #2a1a22; color: #ff6a8a;"
        " border: 1px solid #ff6a8a;"
        " border-radius: 6px; font-weight: bold; padding: 4px 0; }"
    )

    def __init__(self, node_id, node_name, capture_target, engine, runtime=None, parent=None):
        super().__init__(parent)
        self.node_id = str(node_id)
        self.node_name = node_name
        # The PipeWire source name the FX chain's first stage should pull
        # from. For mics that's the mic's own node.name; for virtual sinks
        # it's `<sink>.monitor`. Determined by the channel strip and passed
        # in so the engine doesn't need to guess about media class.
        self.capture_target = capture_target
        self.engine = engine
        self.runtime = runtime
        self.setWindowTitle("Channel Effects")
        self.setMinimumWidth(400)
        self.setStyleSheet(STYLESHEET)

        # Per-(effect_id) widgets we need to touch from _on_toggle / slider changes.
        self._param_sliders = {}   # effect_id -> {param_key: (slider, value_lbl)}
        self._param_frames  = {}   # effect_id -> QFrame holding the param rows
        self._toggle_btns   = {}   # effect_id -> QPushButton

        # Saved intent (which toggles were last ON) so the dialog still
        # ticks them when the chain isn't currently running — the chain
        # may have died from a PipeWire restart or stage crash. Keep the
        # ORDERED list separate from the membership set: re-spawning from
        # a set discards the user's saved chain order and may flip the
        # canonical signal flow on rebuild.
        win = self._main_window_static(parent)
        self._main_win = win
        self._saved_effects_list = list(
            win.active_effects.get(self.node_name, []) if win else []
        )
        self._saved_effects = set(self._saved_effects_list)
        self._effect_defs = list(self.engine.get_available_effects())
        saved_params = (
            win.effect_params.get(self.node_name, {}) if win else {}
        ) or {}
        self._saved_effect_params = {
            effect_id: dict(values)
            for effect_id, values in saved_params.items()
            if isinstance(values, dict)
        }
        self._effect_available = {
            fx["id"]: self.engine.effect_available(fx["id"])
            for fx in self._effect_defs
        }
        self._effect_ui_meta = {
            fx["id"]: {
                "params": self.engine.get_effect_params(fx["id"]),
                "help": self.engine.get_effect_help(fx["id"]),
                "presets": self.engine.get_effect_presets(fx["id"]),
            }
            for fx in self._effect_defs
        }

        self._pending_fx_generation = 0
        self._runtime_inflight = False
        self._pending_close = False
        self._initial_effects_list = list(self._saved_effects_list)
        self._initial_effect_params = {
            effect_id: dict(values)
            for effect_id, values in self._saved_effect_params.items()
        }
        self._inflight_effects_list = []
        self._inflight_effect_params = {}
        if self.runtime is not None:
            self.runtime.fx_status_changed.connect(self._on_runtime_fx_status)

        self._param_timer = QTimer(self)
        self._param_timer.setSingleShot(True)
        self._param_timer.setInterval(120)
        self._param_timer.timeout.connect(self._commit_live_patch)

        root = QVBoxLayout(self)
        root.setSpacing(12)
        root.setContentsMargins(20, 20, 20, 20)

        title = QLabel("✨ Channel Effects")
        title.setStyleSheet("font-size: 18px; font-weight: bold; color: #fff;")
        root.addWidget(title)

        desc = QLabel("Processing is live per-channel. Parameters save automatically.")
        desc.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        root.addWidget(desc)

        self._status_lbl = QLabel("")
        self._status_lbl.setWordWrap(True)
        self._status_lbl.setVisible(False)
        self._status_lbl.setStyleSheet("color: #ffb26a; font-size: 11px;")
        root.addWidget(self._status_lbl)

        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.Shape.NoFrame)
        inner = QWidget()
        scroll_layout = QVBoxLayout(inner)
        scroll_layout.setSpacing(10)

        for fx in self._effect_defs:
            scroll_layout.addWidget(self._build_effect_card(fx))

        scroll_layout.addStretch()
        scroll.setWidget(inner)
        root.addWidget(scroll, 1)

        self.close_btn = QPushButton("Apply")
        self.close_btn.setObjectName("addBtn")
        self.close_btn.clicked.connect(self._on_done)
        root.addWidget(self.close_btn)

        # Use runtime controller status instead of probing engine state for
        # every effect while the dialog is opening.
        self._refresh_toggle_status()

    @staticmethod
    def _main_window_static(parent):
        """Walk up to find the WaveLinuxWindow. Static because we need it
        before instance state is fully wired."""
        p = parent
        while p is not None and not hasattr(p, 'effect_params'):
            p = p.parent()
        return p

    def _on_done(self):
        if self._param_timer.isActive():
            self._param_timer.stop()
            self._commit_live_patch()
        if self._runtime_inflight:
            self.close_btn.setText("Applying...")
            self.close_btn.setEnabled(False)
            self._pending_close = True
            return

        self.accept()

    # ── Card construction ──────────────────────────────────────────

    def _build_effect_card(self, fx):
        fid = fx['id']
        frame = QFrame()
        frame.setObjectName("fxItemFrame")
        frame.setStyleSheet(
            "QFrame#fxItemFrame {"
            " background: rgba(255,255,255,0.03);"
            " border: 1px solid rgba(255,255,255,0.06);"
            " border-radius: 10px; padding: 10px; }"
        )
        vlay = QVBoxLayout(frame)

        header = QHBoxLayout()
        icon_lbl = QLabel(fx['icon'])
        icon_lbl.setStyleSheet("font-size: 20px;")
        header.addWidget(icon_lbl)

        text_col = QVBoxLayout()
        name_lbl = QLabel(fx['name'])
        name_lbl.setStyleSheet("color: #e0e0ee; font-weight: bold; font-size: 13px;")
        text_col.addWidget(name_lbl)
        info_lbl = QLabel(fx['desc'])
        info_lbl.setStyleSheet("color: #6b6b82; font-size: 10px;")
        text_col.addWidget(info_lbl)
        header.addLayout(text_col, 1)

        toggle_btn = QPushButton()
        toggle_btn.setCheckable(True)
        toggle_btn.setProperty("role", "fxToggle")
        toggle_btn.setStyleSheet(self._TOGGLE_STYLE)
        active = fid in self._saved_effects
        toggle_btn.setChecked(active)
        toggle_btn.setFixedWidth(60)

        available = self._effect_available.get(fid, False)
        if not available:
            toggle_btn.setText("N/A")
            toggle_btn.setEnabled(False)
            toggle_btn.setToolTip(
                "LADSPA plugin not installed.\n"
                "rnnoise needs librnnoise_ladspa; "
                "compressor/gate/limiter need swh-plugins. "
                "highpass uses PipeWire's built-in biquad."
            )
            info_lbl.setStyleSheet("color: #ff6a8a; font-size: 10px;")
            info_lbl.setText(fx['desc'] + " — plugin missing")
        else:
            toggle_btn.setText("ON" if active else "OFF")
            toggle_btn.clicked.connect(
                lambda checked, fid=fid: self._on_toggle(fid)
            )

        header.addWidget(toggle_btn)
        self._toggle_btns[fid] = toggle_btn
        vlay.addLayout(header)

        # Help text (plain-English description of what this effect does)
        # and preset buttons live together in the expanding panel.
        meta = self._effect_ui_meta.get(fid, {})
        params = meta.get("params", [])
        help_text = meta.get("help", "")
        presets = meta.get("presets", [])

        if params or help_text or presets:
            param_frame = QFrame()
            param_frame.setStyleSheet("background: transparent;")
            pf_layout = QVBoxLayout(param_frame)
            pf_layout.setContentsMargins(16, 6, 4, 2)
            pf_layout.setSpacing(6)

            if help_text:
                help_lbl = QLabel(help_text)
                help_lbl.setObjectName("effectHelp")
                help_lbl.setWordWrap(True)
                help_lbl.setStyleSheet(
                    "color: #8b8b9e; font-size: 11px;"
                    " background: rgba(0,229,255,0.04);"
                    " border-left: 2px solid rgba(0,229,255,0.3);"
                    " padding: 6px 8px; border-radius: 4px;"
                )
                pf_layout.addWidget(help_lbl)

            if presets:
                preset_row = QHBoxLayout()
                preset_row.setSpacing(6)
                label_lbl = QLabel("Presets:")
                label_lbl.setStyleSheet("color: #6b6b82; font-size: 10px; font-weight: 700;")
                preset_row.addWidget(label_lbl)
                for pname, pvalues in presets:
                    btn = QPushButton(pname)
                    btn.setObjectName("presetBtn")
                    btn.setCursor(Qt.CursorShape.PointingHandCursor)
                    btn.clicked.connect(
                        lambda checked, fid=fid, pv=pvalues: self._apply_preset(fid, pv)
                    )
                    preset_row.addWidget(btn)
                preset_row.addStretch()
                pf_layout.addLayout(preset_row)

            if params:
                stored = self._current_params(fid)
                self._param_sliders[fid] = {}
                for key, label, pmin, pmax, default, suffix in params:
                    row = QHBoxLayout()
                    lbl = QLabel(label)
                    lbl.setStyleSheet("color: #a0a0b8; font-size: 11px; min-width: 80px;")
                    row.addWidget(lbl)

                    slider = QSlider(Qt.Orientation.Horizontal)
                    slider.setRange(0, 1000)
                    val = float(stored.get(key, default))
                    frac = 0.0 if pmax == pmin else (val - pmin) / (pmax - pmin)
                    slider.setValue(max(0, min(1000, int(round(frac * 1000)))))
                    slider.setProperty("pmin", pmin)
                    slider.setProperty("pmax", pmax)
                    slider.setProperty("pkey", key)
                    slider.setProperty("psuffix", suffix)
                    row.addWidget(slider, 1)

                    value_lbl = QLabel(self._fmt_value(val, suffix))
                    value_lbl.setStyleSheet("color: #e0e0ee; font-size: 11px; min-width: 64px;")
                    value_lbl.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
                    row.addWidget(value_lbl)

                    slider.valueChanged.connect(
                        lambda v, fid=fid, s=slider, lbl=value_lbl: self._on_param_changed(fid, s, lbl)
                    )
                    self._param_sliders[fid][key] = (slider, value_lbl)
                    pf_layout.addLayout(row)

            param_frame.setVisible(active and available)
            self._param_frames[fid] = param_frame
            vlay.addWidget(param_frame)

        return frame

    @staticmethod
    def _fmt_value(val, suffix):
        if abs(val) >= 10:
            return f"{val:.0f}{suffix}"
        return f"{val:.2f}{suffix}"

    def _current_params(self, effect_id):
        """Look up the cached saved effect params for this node."""
        return dict(self._saved_effect_params.get(effect_id, {}))

    def _main_window(self):
        """Walk up to the WaveLinuxWindow (only place that has effect_params)."""
        return self._main_win or self._main_window_static(self.parent())

    def _collect_params(self, effect_id):
        out = {}
        for key, (slider, _lbl) in self._param_sliders.get(effect_id, {}).items():
            pmin = float(slider.property("pmin"))
            pmax = float(slider.property("pmax"))
            val = pmin + (pmax - pmin) * (slider.value() / 1000.0)
            out[key] = val
        return out

    # Toggle, preset pick, and slider drag all feed the live-patch path.
    # The dialog debounces those edits so rapid churn stays last-write-wins.

    def _active_effect_ids(self):
        """Return the effect ids whose toggle buttons are currently ON,
        in the order they appear in the engine's available list."""
        wanted = []
        for fx in self._effect_defs:
            fid = fx['id']
            btn = self._toggle_btns.get(fid)
            if btn is not None and btn.isChecked() and btn.isEnabled():
                wanted.append(fid)
        return wanted

    def _all_params_map(self):
        """Snapshot every effect's current slider values (including OFF
        effects) so toggling back ON resumes the user's last tweak."""
        out = {}
        for fid in self._param_sliders.keys():
            out[fid] = self._collect_params(fid)
        return out

    def _has_pending_changes(self, wanted=None, params_map=None):
        wanted = list(wanted if wanted is not None else self._active_effect_ids())
        params_map = dict(params_map if params_map is not None else self._all_params_map())
        normalized = {
            fid: dict(values)
            for fid, values in params_map.items()
            if values
        }
        return (
            wanted != list(self._initial_effects_list)
            or normalized != self._initial_effect_params
        )

    def _queue_live_patch(self):
        if self.runtime is None:
            return
        self._param_timer.start()

    def _commit_live_patch(self):
        if self.runtime is None:
            return
        if self._runtime_inflight:
            self._param_timer.start()
            return
        wanted = self._active_effect_ids()
        params_map = self._all_params_map()
        if not self._has_pending_changes(wanted, params_map):
            return
        self._save_chain_state(wanted, params_map)
        self._start_fx_worker(wanted, params_map)

    def _start_fx_worker(self, wanted, params_map):
        self._inflight_effects_list = list(wanted or [])
        self._inflight_effect_params = {
            fid: dict(vals)
            for fid, vals in (params_map or {}).items()
            if vals
        }
        if wanted:
            self._pending_fx_generation = self.runtime.set_channel_fx(
                self.node_name, self.capture_target, wanted, params_map
            )
        else:
            self._pending_fx_generation = self.runtime.clear_channel_fx(
                self.node_name
            )
        self._runtime_inflight = True

    def _on_runtime_fx_status(self, status):
        if getattr(status, "node_name", None) != self.node_name:
            return
        if self._pending_fx_generation and status.generation:
            if status.generation < self._pending_fx_generation:
                return
        self._runtime_inflight = status.state in {"building", "cutover_pending", "clearing"}
        if status.state == "degraded":
            recovery_note = ""
            win = self._main_window()
            if hasattr(win, "fx_recovery_status_message"):
                recovery_note = (win.fx_recovery_status_message(self.node_name) or "").strip()
            status_text = (status.message or "").strip()
            if hasattr(win, "format_fx_status_message"):
                status_text = (win.format_fx_status_message(status) or "").strip()
            parts = [
                "WaveLinux could not apply one or more effects.",
                status_text,
            ]
            if recovery_note:
                parts.append(recovery_note)
            self._status_lbl.setText("\n\n".join(part for part in parts if part))
            self._status_lbl.setVisible(True)
        elif status.state in {"active", "idle"}:
            self._status_lbl.clear()
            self._status_lbl.setVisible(False)
        if status.state not in {"building", "cutover_pending", "clearing"}:
            self.close_btn.setText("Apply")
            self.close_btn.setEnabled(True)
            self._refresh_toggle_status()
            if status.state in {"active", "idle"}:
                self._initial_effects_list = list(self._inflight_effects_list)
                self._initial_effect_params = {
                    fid: dict(vals)
                    for fid, vals in self._inflight_effect_params.items()
                }
                has_more_changes = self._has_pending_changes()
                if has_more_changes:
                    self._queue_live_patch()
            if self._pending_close and status.state in {"active", "idle"} and not has_more_changes:
                self._pending_close = False
                self.accept()
            elif status.state == "degraded":
                self._pending_close = False

    def reject(self):
        if self._param_timer.isActive():
            self._param_timer.stop()
            self._commit_live_patch()
        if self._runtime_inflight:
            self._pending_close = True
            self.close_btn.setText("Applying...")
            self.close_btn.setEnabled(False)
            return
        super().reject()

    def closeEvent(self, event):
        if self.runtime is None:
            super().closeEvent(event)
            return
        try:
            self.runtime.fx_status_changed.disconnect(self._on_runtime_fx_status)
        except TypeError:
            pass
        super().closeEvent(event)

    def _refresh_toggle_status(self):
        """Annotate toggles from runtime status without blocking on engine probes."""
        if self.runtime is None:
            for btn in self._toggle_btns.values():
                if btn.isEnabled():
                    btn.setStyleSheet(self._TOGGLE_STYLE)
                    btn.setToolTip("")
            return
        status = self.runtime.fx_status_for(self.node_name)
        degraded = getattr(status, "state", "") == "degraded"
        message = (status.message or "").strip()
        for fid, btn in self._toggle_btns.items():
            if not btn.isEnabled():
                continue
            if degraded and btn.isChecked():
                btn.setStyleSheet(self._TOGGLE_FAILED_STYLE)
                btn.setToolTip(message or "FX chain degraded. Re-apply or recover the channel.")
            else:
                btn.setStyleSheet(self._TOGGLE_STYLE)
                btn.setToolTip("")

    def _save_chain_state(self, effects, params_map):
        """Mirror chain into main-window state: `active_effects` (ordered
        ids) and `effect_params` (slider positions)."""
        win = self._main_window()
        if win is None:
            return
        if effects:
            win.active_effects[self.node_name] = list(effects)
        else:
            win.active_effects.pop(self.node_name, None)
        # Save params for OFF effects too so re-enable resumes the tweak.
        # Drop the per-effect dict when its slider stash is empty, and
        # drop the whole node entry when nothing is left — the previous
        # version's `if not stash` check ran AFTER unconditionally writing
        # every key, so it never fired.
        if params_map:
            stash = win.effect_params.setdefault(self.node_name, {})
            for fid, vals in params_map.items():
                if vals:
                    stash[fid] = dict(vals)
                else:
                    stash.pop(fid, None)
            if not stash:
                win.effect_params.pop(self.node_name, None)
        self._saved_effect_params = {
            fid: dict(vals) for fid, vals in params_map.items() if vals
        }
        if hasattr(win, 'schedule_save'):
            win.schedule_save()
        elif hasattr(win, 'save_config'):
            win.save_config()

    # ── Handlers ───────────────────────────────────────────────────

    def _on_toggle(self, effect_id):
        btn = self._toggle_btns[effect_id]
        frame = self._param_frames.get(effect_id)
        btn.setText("ON" if btn.isChecked() else "OFF")
        if frame:
            frame.setVisible(btn.isChecked())
        self._queue_live_patch()

    def _apply_preset(self, effect_id, values):
        """Snap the effect's sliders to a labelled preset and live-patch it."""
        sliders = self._param_sliders.get(effect_id, {})
        for key, (slider, value_lbl) in sliders.items():
            if key not in values:
                continue
            pmin = float(slider.property("pmin"))
            pmax = float(slider.property("pmax"))
            target = float(values[key])
            frac = 0.0 if pmax == pmin else (target - pmin) / (pmax - pmin)
            slider.blockSignals(True)
            slider.setValue(max(0, min(1000, int(round(frac * 1000)))))
            slider.blockSignals(False)
            suffix = slider.property("psuffix") or ""
            value_lbl.setText(self._fmt_value(target, suffix))
        self._queue_live_patch()

    def _on_param_changed(self, effect_id, slider, value_lbl):
        pmin = float(slider.property("pmin"))
        pmax = float(slider.property("pmax"))
        suffix = slider.property("psuffix") or ""
        val = pmin + (pmax - pmin) * (slider.value() / 1000.0)
        value_lbl.setText(self._fmt_value(val, suffix))
        self._queue_live_patch()


# ── Channel Strip Widget ───────────────────────────────────────────
class ChannelStrip(QFrame):
    """A single mixer channel: icon, name, vertical fader, mute, FX."""

    def __init__(self, node_id, node_name, name, ch_type, icon, engine, parent=None):
        super().__init__(parent)
        self.setObjectName("channelStrip")
        self.setFixedWidth(self._MAX_W)
        self.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self.node_id = node_id          # PipeWire numeric id (ephemeral)
        self.node_name = node_name      # PipeWire node.name (stable across restarts)
        self.ch_name = name
        self.ch_type = ch_type
        self.engine = engine
        self.is_mic = ch_type.lower() == "microphone"
        self._muted = False
        self._mon_muted = False
        self._str_muted = False
        self._main_win = None
        self._last_rendered_state = None
        self._last_fx_indicator_active = None
        self._last_runtime_issue_active = None

        # Slider-commit debouncers — UI moves at full Qt rate but only
        # the final value reaches `pactl set-sink-input-volume`.
        self._mon_commit_timer = QTimer(self)
        self._mon_commit_timer.setSingleShot(True)
        self._mon_commit_timer.setInterval(40)
        self._mon_commit_timer.timeout.connect(self._commit_mon_vol)
        self._str_commit_timer = QTimer(self)
        self._str_commit_timer.setSingleShot(True)
        self._str_commit_timer.setInterval(40)
        self._str_commit_timer.timeout.connect(self._commit_str_vol)
        self._src_commit_timer = QTimer(self)
        self._src_commit_timer.setSingleShot(True)
        self._src_commit_timer.setInterval(40)
        self._src_commit_timer.timeout.connect(self._commit_src_vol)
        # Pending values held until the timer fires.
        self._pending_mon_vol = None
        self._pending_str_vol = None
        self._pending_src_vol = None
        self._src_muted = False

        layout = QVBoxLayout(self)
        self._root_layout = layout
        layout.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        layout.setContentsMargins(8, 8, 8, 8)
        layout.setSpacing(4)

        # Icon + optional FX indicator. Other actions live in right-click.
        head_row = QHBoxLayout()
        head_row.setContentsMargins(0, 0, 0, 0)
        self.health_indicator = QLabel("!")
        self.health_indicator.setObjectName("healthIndicator")
        self.health_indicator.setToolTip("Runtime issue detected — right-click for recovery tools")
        self.health_indicator.setVisible(False)
        head_row.addWidget(self.health_indicator)
        head_row.addStretch()
        icon_lbl = QLabel(icon)
        icon_lbl.setObjectName("channelIcon")
        head_row.addWidget(icon_lbl)
        head_row.addStretch()
        self.fx_indicator = QLabel("✨")
        self.fx_indicator.setObjectName("fxIndicator")
        self.fx_indicator.setToolTip("Effect active — right-click → Effects to edit")
        self.fx_indicator.setVisible(False)
        head_row.addWidget(self.fx_indicator)
        layout.addLayout(head_row)

        # Right-click menu — where reorder / hide / rename / effects / remove live.
        self.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        self.customContextMenuRequested.connect(self._show_context_menu)

        # Name (click to rename for virtual channels; mics keep their alsa name).
        self.name_lbl = QLabel(name)
        self.name_lbl.setObjectName("channelName")
        self.name_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.name_lbl.setWordWrap(True)
        self.name_lbl.setMinimumHeight(24)  # allow 2 lines
        layout.addWidget(self.name_lbl)

        layout.addSpacing(4)

        # Peak meter — placed between the name and the faders so it
        # reads as "this channel's level", not "this fader's level".
        self.peak_bar = QProgressBar()
        self.peak_bar.setObjectName("peakBar")
        self.peak_bar.setRange(0, 1000)
        self.peak_bar.setTextVisible(False)
        self.peak_bar.setFixedHeight(5)
        self.peak_bar.setValue(0)
        layout.addWidget(self.peak_bar)

        self.src_slider = None
        self.src_vol_lbl = None
        if self.is_mic:
            src_box = QVBoxLayout()
            src_box.setContentsMargins(0, 2, 0, 2)
            src_box.setSpacing(2)
            src_head = QHBoxLayout()
            src_head.setContentsMargins(0, 0, 0, 0)
            src_label = QLabel("MIC")
            src_label.setObjectName("mixTagMic")
            src_head.addWidget(src_label)
            src_head.addStretch()
            self.src_vol_lbl = QLabel("100%")
            self.src_vol_lbl.setAlignment(Qt.AlignmentFlag.AlignRight)
            self.src_vol_lbl.setObjectName("volumeLabel")
            src_head.addWidget(self.src_vol_lbl)
            src_box.addLayout(src_head)
            self.src_slider = QSlider(Qt.Orientation.Horizontal)
            self.src_slider.setRange(0, 100)
            self.src_slider.setValue(100)
            self.src_slider.setToolTip("Hardware mic gain")
            self.src_slider.valueChanged.connect(self._on_src_vol)
            src_box.addWidget(self.src_slider)
            layout.addLayout(src_box)

        # Link button: when on, the two mix faders move together.
        link_row = QHBoxLayout()
        link_row.addStretch()
        self.link_btn = QPushButton("🔗")
        self.link_btn.setObjectName("linkBtn")
        self.link_btn.setCheckable(True)
        self.link_btn.setFixedSize(24, 24)
        self.link_btn.setToolTip("Link the Monitor and Stream faders")
        self.link_btn.clicked.connect(self._on_link_toggle)
        link_row.addWidget(self.link_btn)
        link_row.addStretch()
        layout.addLayout(link_row)

        # Two-column fader layout (Headphones + Stream).
        faders_row = QHBoxLayout()
        faders_row.setSpacing(10)
        self._faders_row = faders_row

        mon_col = QVBoxLayout()
        mon_col.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        mon_label = QLabel("MON")
        mon_label.setObjectName("mixTagMon")
        mon_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        mon_col.addWidget(mon_label)
        self.mon_vol_lbl = QLabel("100%")
        self.mon_vol_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.mon_vol_lbl.setObjectName("volumeLabel")
        mon_col.addWidget(self.mon_vol_lbl)
        self.mon_slider = QSlider(Qt.Orientation.Vertical)
        self.mon_slider.setRange(0, 100)
        self.mon_slider.setValue(100)
        self.mon_slider.setMinimumHeight(140)
        self.mon_slider.valueChanged.connect(self._on_mon_vol)
        mon_col.addWidget(self.mon_slider, 1, Qt.AlignmentFlag.AlignHCenter)
        self.mon_mute = QPushButton("🎧")
        self.mon_mute.setObjectName("muteBtn")
        self.mon_mute.setFixedSize(28, 28)
        self.mon_mute.setToolTip("Mute in Monitor mix")
        self.mon_mute.clicked.connect(self._on_mon_mute)
        mon_col.addWidget(self.mon_mute, 0, Qt.AlignmentFlag.AlignHCenter)
        faders_row.addLayout(mon_col)

        str_col = QVBoxLayout()
        str_col.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        str_label = QLabel("STR")
        str_label.setObjectName("mixTagStr")
        str_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        str_col.addWidget(str_label)
        self.str_vol_lbl = QLabel("100%")
        self.str_vol_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.str_vol_lbl.setObjectName("volumeLabel")
        str_col.addWidget(self.str_vol_lbl)
        self.str_slider = QSlider(Qt.Orientation.Vertical)
        self.str_slider.setRange(0, 100)
        self.str_slider.setValue(100)
        self.str_slider.setMinimumHeight(140)
        self.str_slider.valueChanged.connect(self._on_str_vol)
        str_col.addWidget(self.str_slider, 1, Qt.AlignmentFlag.AlignHCenter)
        self.str_mute = QPushButton("📡")
        self.str_mute.setObjectName("muteBtn")
        self.str_mute.setFixedSize(28, 28)
        self.str_mute.setToolTip("Mute in Stream mix")
        self.str_mute.clicked.connect(self._on_str_mute)
        str_col.addWidget(self.str_mute, 0, Qt.AlignmentFlag.AlignHCenter)
        faders_row.addLayout(str_col)

        layout.addLayout(faders_row, 1)

        # Hidden no-op kept so callers that touch `strip.fx_btn.setProperty(...)`
        # don't fail. The real FX entry point is the right-click menu.
        self.fx_btn = QPushButton()
        self.fx_btn.setVisible(False)

    _MIN_W = 120
    _MAX_W = 280
    _MIN_SLIDER_H = 80
    _MAX_SLIDER_H = 200
    _VERT_SLIDER_END_PAD = 12
    _STRIP_HEIGHT_PAD = 12
    _WIDTH_SCALE_CAP = 180

    def apply_scale(self, width: int, slider_h: int, *, target_height: int | None = None):
        """Resize this strip to `width` px and return the desired card height."""
        self.setFixedWidth(width)
        width_t = (width - self._MIN_W) / (self._MAX_W - self._MIN_W)
        width_t = max(0.0, min(1.0, width_t))
        control_width_t = (
            (min(width, self._WIDTH_SCALE_CAP) - self._MIN_W)
            / (self._WIDTH_SCALE_CAP - self._MIN_W)
        )
        control_width_t = max(0.0, min(1.0, control_width_t))
        height_t = (slider_h - self._MIN_SLIDER_H) / (self._MAX_SLIDER_H - self._MIN_SLIDER_H)
        height_t = max(0.0, min(1.0, height_t))
        t = min(control_width_t, height_t)
        margin = max(3, int(3 + t * 5))
        self._root_layout.setContentsMargins(margin, margin, margin, margin)
        self._root_layout.setSpacing(max(2, int(2 + t * 4)))
        spacing = max(6, int(6 + t * 4))
        self._faders_row.setSpacing(spacing)
        slider_widget_h = slider_h + self._VERT_SLIDER_END_PAD
        self.mon_slider.setFixedHeight(slider_widget_h)
        self.str_slider.setFixedHeight(slider_widget_h)
        self.name_lbl.setFixedHeight(24)
        peak_h = max(4, int(4 + t * 2))
        self.peak_bar.setFixedHeight(peak_h)
        link_size = max(20, int(20 + t * 4))
        self.link_btn.setFixedSize(link_size, link_size)
        mute_size = max(24, int(24 + t * 4))
        self.mon_mute.setFixedSize(mute_size, mute_size)
        self.str_mute.setFixedSize(mute_size, mute_size)
        self._root_layout.activate()
        desired_h = self._root_layout.sizeHint().height() + self._STRIP_HEIGHT_PAD
        self.setFixedHeight(target_height if target_height is not None else desired_h)
        return desired_h

    def _stash_submix(self, mix_name, vol, mute):
        win = self._main_window()
        if not hasattr(win, 'submix_state') or not self.node_name:
            return
        win.submix_state[f"{self.node_name}_{mix_name}"] = {'vol': vol, 'mute': mute}
        if hasattr(win, 'schedule_save'):
            win.schedule_save()

    def _on_link_toggle(self):
        linked = self.link_btn.isChecked()
        if linked:
            self.str_slider.setValue(self.mon_slider.value())
        # Persist regardless of direction so unlink isn't silently lost.
        self._save_link_state(linked)

    def _save_link_state(self, linked):
        win = self._main_window()
        if hasattr(win, 'submix_state') and self.node_name:
            win.submix_state[f"{self.node_name}_linked"] = bool(linked)
            if hasattr(win, 'schedule_save'):
                win.schedule_save()

    def _on_mon_vol(self, value):
        # Visual update is immediate; engine write is debounced 40ms.
        self.mon_vol_lbl.setText(f"{value}%")
        self._pending_mon_vol = value
        self._mon_commit_timer.start()
        if self.link_btn.isChecked() and self.str_slider.value() != value:
            self.str_slider.setValue(value)

    def _on_src_vol(self, value):
        if self.src_vol_lbl is not None:
            self.src_vol_lbl.setText(f"{value}%")
        self._pending_src_vol = value
        self._src_commit_timer.start()

    def _on_str_vol(self, value):
        self.str_vol_lbl.setText(f"{value}%")
        self._pending_str_vol = value
        self._str_commit_timer.start()
        if self.link_btn.isChecked() and self.mon_slider.value() != value:
            self.mon_slider.setValue(value)

    def _commit_mon_vol(self):
        """Fire the deferred Monitor-fader write."""
        v = self._pending_mon_vol
        self._pending_mon_vol = None
        if v is None or not self.node_id:
            return
        win = self._main_window()
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            return
        runtime.set_submix_state(
            self.node_id,
            "Monitor",
            v / 100.0,
            self._mon_muted,
            node_name=self.node_name,
        )
        self._stash_submix("Monitor", v / 100.0, self._mon_muted)

    def _commit_str_vol(self):
        v = self._pending_str_vol
        self._pending_str_vol = None
        if v is None or not self.node_id:
            return
        win = self._main_window()
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            return
        runtime.set_submix_state(
            self.node_id,
            "Stream",
            v / 100.0,
            self._str_muted,
            node_name=self.node_name,
        )
        self._stash_submix("Stream", v / 100.0, self._str_muted)

    def _commit_src_vol(self):
        v = self._pending_src_vol
        self._pending_src_vol = None
        if v is None or not self.node_name:
            return
        win = self._main_window()
        runtime = getattr(win, "runtime", None)
        if runtime is None or not hasattr(runtime, "set_source_volume"):
            return
        runtime.set_source_volume(self.node_name, v / 100.0)

    def flush_pending_state(self):
        for timer, commit in (
            (getattr(self, "_mon_commit_timer", None), self._commit_mon_vol),
            (getattr(self, "_str_commit_timer", None), self._commit_str_vol),
            (getattr(self, "_src_commit_timer", None), self._commit_src_vol),
        ):
            if timer is not None and timer.isActive():
                timer.stop()
                commit()

    def _apply_mute_style(self, btn, muted):
        if btn == self.mon_mute:
            icon = "🎧"
        else:
            icon = "📡"
        target_text = "🔇" if muted else icon
        target_state = "true" if muted else "false"
        if btn.text() == target_text and btn.property("muted") == target_state:
            return
        btn.setText(target_text)
        btn.setProperty("muted", target_state)
        btn.style().unpolish(btn)
        btn.style().polish(btn)

    def _on_mon_mute(self):
        self._mon_muted = not self._mon_muted
        if self.node_id:
            win = self._main_window()
            runtime = getattr(win, "runtime", None)
            if runtime is None:
                return
            runtime.set_submix_state(
                self.node_id,
                "Monitor",
                self.mon_slider.value() / 100.0,
                self._mon_muted,
                node_name=self.node_name,
            )
            self._stash_submix("Monitor", self.mon_slider.value() / 100.0, self._mon_muted)
        self._apply_mute_style(self.mon_mute, self._mon_muted)

    def _on_str_mute(self):
        self._str_muted = not self._str_muted
        if self.node_id:
            win = self._main_window()
            runtime = getattr(win, "runtime", None)
            if runtime is None:
                return
            runtime.set_submix_state(
                self.node_id,
                "Stream",
                self.str_slider.value() / 100.0,
                self._str_muted,
                node_name=self.node_name,
            )
            self._stash_submix("Stream", self.str_slider.value() / 100.0, self._str_muted)
        self._apply_mute_style(self.str_mute, self._str_muted)

    def fx_capture_target(self):
        """Source the FX chain's first stage pulls from. Mics → mic
        node.name; virtual sinks → `<sink>.monitor`."""
        if self.is_mic:
            return self.node_name
        return f"{self.node_name}.monitor"

    def _main_window(self):
        win = self._main_win
        if win is None:
            win = self.window()
            self._main_win = win
        return win

    def _on_fx_toggle(self):
        """Open the effects dialog and refresh the ✨ indicator."""
        win = self._main_window()
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            QMessageBox.warning(
                self,
                "Audio runtime unavailable",
                "WaveLinux's audio runtime is not available, so channel "
                "effects cannot be edited right now.",
            )
            return
        dlg = FXSelectionDialog(
            self.node_id, self.node_name, self.fx_capture_target(),
            self.engine, runtime, self,
        )
        dlg.exec()
        self._refresh_fx_indicator()

    def _refresh_fx_indicator(self, active=None):
        if not self.node_name:
            self.fx_indicator.setVisible(False)
            self._last_fx_indicator_active = False
            return
        if active is None:
            win = self._main_window()
            if hasattr(win, "active_effects"):
                active = bool(win.active_effects.get(self.node_name))
            else:
                active = False
        visible = bool(active)
        if visible == self._last_fx_indicator_active:
            return
        self.fx_indicator.setVisible(visible)
        self._last_fx_indicator_active = visible

    def set_runtime_issue(self, active, message=""):
        active = bool(active)
        if active != self._last_runtime_issue_active:
            self.health_indicator.setVisible(active)
            self.setProperty("degraded", "true" if active else "false")
            self.style().unpolish(self)
            self.style().polish(self)
            self._last_runtime_issue_active = active
        tip = message or "Runtime issue detected — right-click for recovery tools."
        self.health_indicator.setToolTip(tip if active else "")

    def _show_context_menu(self, pos):
        """Right-click menu for channel-strip actions."""
        win = self._main_window()
        runtime = getattr(win, "runtime", None)
        menu = QMenu(self)
        menu.setStyleSheet(
            "QMenu { background: #1a1a28; color: #e0e0ee;"
            " border: 1px solid rgba(255,255,255,0.1); border-radius: 6px; padding: 4px; }"
            "QMenu::item { padding: 6px 20px; }"
            "QMenu::item:selected { background: rgba(0,229,255,0.15); }"
            "QMenu::separator { background: rgba(255,255,255,0.08); height: 1px; margin: 4px 6px; }"
        )

        fx_act = menu.addAction("✨ Effects…")
        fx_act.triggered.connect(self._on_fx_toggle)

        if self.node_name:
            issue = (
                win.channel_runtime_issue(self.node_name)
                if hasattr(win, "channel_runtime_issue")
                else {"degraded": False}
            )
            if issue.get("degraded"):
                summary = issue.get("summary") or "Runtime issue detected."
                status_act = menu.addAction(summary)
                status_act.setEnabled(False)
                retry_act = menu.addAction("Retry FX Now")
                retry_act.triggered.connect(self._request_recover)
                diag_act = menu.addAction("Open Diagnostics")
                diag_act.triggered.connect(self._open_diagnostics)
                menu.addSeparator()

        # Carla VST/LV2 host — only shown when `carla` is on $PATH.
        if shutil.which("carla"):
            vst_act = menu.addAction("🎹 Open VST plugin (Carla)…")
            vst_act.triggered.connect(self._launch_carla)

        menu.addSeparator()

        move_left = menu.addAction("◀ Move Left")
        move_left.triggered.connect(lambda: self._request_move(-1))
        move_right = menu.addAction("▶ Move Right")
        move_right.triggered.connect(lambda: self._request_move(1))

        menu.addSeparator()

        if self.ch_type.lower() == "virtual":
            rename_act = menu.addAction("✏️ Rename…")
            rename_act.triggered.connect(self._request_rename)
            remove_act = menu.addAction("❌ Remove Channel")
            remove_act.triggered.connect(self._request_remove)

        hide_act = menu.addAction("👁 Hide")
        hide_act.triggered.connect(self._request_hide)

        menu.exec(self.mapToGlobal(pos))

    def _request_remove(self):
        """Remove a virtual channel (only valid for ch_type == 'virtual')."""
        win = self._main_window()
        if hasattr(win, "_remove_sink") and self.node_name:
            win._remove_sink(self.node_name)

    def _launch_carla(self):
        """Spawn Carla. The user wires plugin I/O in Carla itself —
        WaveLinux doesn't supervise the process."""
        try:
            subprocess.Popen(["carla"])
        except FileNotFoundError:
            QMessageBox.information(
                self, "Carla not found",
                "Install Carla from your distro or upstream package source to host VST3 / "
                "LV2 plugins. WaveLinux bridges to Carla rather than hosting "
                "those plugin formats directly."
            )

    def _request_hide(self):
        """Request parent window to hide this channel."""
        win = self._main_window()
        if hasattr(win, 'hide_node') and self.node_name:
            win.hide_node(self.node_name)

    def _request_move(self, delta):
        win = self._main_window()
        if hasattr(win, 'move_channel') and self.node_name:
            win.move_channel(self.node_name, delta)

    def _request_recover(self):
        win = self._main_window()
        if hasattr(win, 'recover_channel') and self.node_name:
            win.recover_channel(self.node_name)

    def _open_diagnostics(self):
        win = self._main_window()
        if hasattr(win, "open_channel_diagnostics") and self.node_name:
            win.open_channel_diagnostics(self.node_name)

    def _request_rename(self):
        win = self._main_window()
        if hasattr(win, 'rename_channel') and self.node_name:
            win.rename_channel(self.node_name)

    def on_peak(self, peak_01):
        """Receive a 0..1 peak from the MeterWorker and drive the bar."""
        self.peak_bar.setValue(int(max(0.0, min(peak_01, 1.0)) * 1000))

    def update_from_node(self, mon_vol, mon_mute, str_vol, str_mute, is_hidden,
                         source_vol=1.0, source_mute=False):
        """Update strip UI from stored state. `is_hidden` is informational;
        the parent window controls visibility, not this widget."""
        self._mon_muted = mon_mute
        self._str_muted = str_mute
        self._src_muted = bool(source_mute)
        mon_pct = int(mon_vol * 100)
        str_pct = int(str_vol * 100)
        src_pct = int(source_vol * 100)
        win = self._main_window()
        is_linked = False
        if hasattr(win, 'submix_state') and self.node_name:
            is_linked = bool(win.submix_state.get(f"{self.node_name}_linked", False))
        state = (
            mon_pct,
            bool(mon_mute),
            str_pct,
            bool(str_mute),
            bool(is_linked),
            src_pct if self.is_mic else None,
            bool(source_mute) if self.is_mic else None,
        )
        if state == self._last_rendered_state:
            return

        if self.mon_slider.value() != mon_pct:
            self.mon_slider.blockSignals(True)
            self.mon_slider.setValue(mon_pct)
            self.mon_slider.blockSignals(False)
        if self.str_slider.value() != str_pct:
            self.str_slider.blockSignals(True)
            self.str_slider.setValue(str_pct)
            self.str_slider.blockSignals(False)
        if self.src_slider is not None and self.src_slider.value() != src_pct:
            self.src_slider.blockSignals(True)
            self.src_slider.setValue(src_pct)
            self.src_slider.blockSignals(False)

        if self.link_btn.isChecked() != is_linked:
            self.link_btn.blockSignals(True)
            self.link_btn.setChecked(is_linked)
            self.link_btn.blockSignals(False)

        mon_text = f"{mon_pct}%"
        if self.mon_vol_lbl.text() != mon_text:
            self.mon_vol_lbl.setText(mon_text)
        str_text = f"{str_pct}%"
        if self.str_vol_lbl.text() != str_text:
            self.str_vol_lbl.setText(str_text)
        if self.src_vol_lbl is not None:
            src_text = f"{src_pct}%"
            if self.src_vol_lbl.text() != src_text:
                self.src_vol_lbl.setText(src_text)
        if self.src_slider is not None:
            tip = "Hardware mic gain"
            if source_mute:
                tip += " (currently muted at the source)"
            if self.src_slider.toolTip() != tip:
                self.src_slider.setToolTip(tip)

        self._apply_mute_style(self.mon_mute, mon_mute)
        self._apply_mute_style(self.str_mute, str_mute)
        self._last_rendered_state = state


# ── App Routing Row ────────────────────────────────────────────────
class AppRoutingRow(QWidget):
    """A row showing an app name and a dropdown to choose which sink it goes to."""

    _GENERIC_APP_ICON_CANDIDATES = [
        "audio-x-generic",
        "multimedia-player",
        "applications-multimedia",
    ]
    _SYSTEM_ICON_CANDIDATES = [
        "preferences-system-sound",
        "audio-volume-high",
        "audio-card",
    ]

    def __init__(self, app_id, display_name, engine, sinks, main_win=None, parent=None):
        super().__init__(parent)
        self.engine = engine
        self.app_id = app_id
        self.app_name = display_name
        self.resolved_app_id = app_id
        self.resolved_app_name = display_name
        self.identity_source = ""
        self.override_applied = False
        self.manual_override_active = False
        self.reset_source_app_id = ""
        self._main_win = main_win
        self._active_indices = [] # Current sink-input indices for this app
        self._last_active_state = None
        self._last_sink_selection = object()

        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 4, 0, 4)
        layout.setAlignment(Qt.AlignmentFlag.AlignVCenter)

        self.icon_lbl = QLabel("🎵")
        self.icon_lbl.setObjectName("channelIcon")
        self.icon_lbl.setFixedSize(28, 24)
        self.icon_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(self.icon_lbl)
        
        self.name_lbl = QLabel(display_name)
        self.name_lbl.setObjectName("appName")
        self.name_lbl.setMinimumWidth(150)
        self.name_lbl.setAlignment(Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft)
        layout.addWidget(self.name_lbl)
        
        layout.addStretch()

        # Direct App Volume Control
        self.vol_slider = QSlider(Qt.Orientation.Horizontal)
        self.vol_slider.setRange(0, 100)
        self.vol_slider.setFixedWidth(100)
        self.vol_slider.setValue(100)
        self.vol_slider.valueChanged.connect(self._on_vol_change)
        
        self.vol_lbl = QLabel("Vol:")
        self.vol_lbl.setObjectName("volumeLabel")
        self.vol_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(self.vol_lbl)
        layout.addWidget(self.vol_slider)
        
        layout.addSpacing(10)
        
        self.combo = QComboBox()
        self.combo.setFixedWidth(220)
        self.combo.currentIndexChanged.connect(self._on_route_change)
        layout.addWidget(self.combo)

        self.manage_btn = QPushButton("⋯")
        self.manage_btn.setObjectName("forgetBtn")
        self.manage_btn.setFixedWidth(26)
        self.manage_btn.setToolTip("Pin, merge, or reset this app identity")
        self.manage_btn.clicked.connect(self._show_identity_menu)
        layout.addWidget(self.manage_btn)

        self.forget_btn = QPushButton("✕")
        self.forget_btn.setObjectName("forgetBtn")
        self.forget_btn.setFixedWidth(26)
        self.forget_btn.setToolTip("Forget this app so it stops showing up in the list")
        self.forget_btn.clicked.connect(self._on_forget)
        layout.addWidget(self.forget_btn)

        # 40ms volume-write debouncer (same reasoning as the channel strip).
        self._pending_app_vol = None
        self._app_commit_timer = QTimer(self)
        self._app_commit_timer.setSingleShot(True)
        self._app_commit_timer.setInterval(40)
        self._app_commit_timer.timeout.connect(self._commit_app_vol)

        self.update_state(display_name, [], sinks, None)

    def _set_icon_candidates(self, icon_candidates, *, is_system):
        ordered = []
        seen = set()
        for candidate in list(icon_candidates or []) + (
            self._SYSTEM_ICON_CANDIDATES if is_system else self._GENERIC_APP_ICON_CANDIDATES
        ):
            key = str(candidate or "").strip()
            if not key or key in seen:
                continue
            seen.add(key)
            ordered.append(key)
        for candidate in ordered:
            icon = QIcon.fromTheme(candidate)
            if icon.isNull():
                continue
            self.icon_lbl.clear()
            self.icon_lbl.setPixmap(icon.pixmap(20, 20))
            self.icon_lbl.setToolTip(candidate)
            return
        self.icon_lbl.clear()
        self.icon_lbl.setText("🔔" if is_system else "🎵")
        self.icon_lbl.setToolTip("")

    def _on_vol_change(self, value):
        self._pending_app_vol = value
        self._app_commit_timer.start()

    def _commit_app_vol(self):
        v = self._pending_app_vol
        self._pending_app_vol = None
        if v is None:
            return
        normalized = max(0.0, min(float(v) / 100.0, 1.0))
        win = self._main_win
        if win is not None:
            app_volumes = getattr(win, "app_volumes", None)
            if (
                app_volumes is not None
                and PipeWireEngine.is_persistent_app_id(self.app_id)
                and app_volumes.get(self.app_id) != normalized
            ):
                app_volumes[self.app_id] = normalized
                win._sync_runtime_persistent_state()
                win.schedule_save()
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            return
        for idx in self._active_indices:
            runtime.set_app_volume(idx, normalized)

    def flush_pending_state(self):
        timer = getattr(self, "_app_commit_timer", None)
        if timer is not None and timer.isActive():
            timer.stop()
            self._commit_app_vol()

    def _on_route_change(self, idx):
        sink_name = self.combo.itemData(idx)
        win = self._main_win
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            return
        runtime.set_app_route(self.app_id, sink_name)
        if win is not None:
            win.app_routing[self.app_id] = sink_name
            win.save_config()

    def _on_forget(self):
        """Permanently remove this app from the routing list."""
        win = self._main_win
        if win is not None:
            win.forget_app(self.app_id)

    def _identity_source_app_id(self):
        if PipeWireEngine.is_persistent_app_id(self.resolved_app_id):
            return self.resolved_app_id
        if PipeWireEngine.is_persistent_app_id(self.app_id):
            return self.app_id
        return ""

    def _show_identity_menu(self):
        win = self._main_win
        if win is None:
            return
        menu = QMenu(self)
        persistent_source = self._identity_source_app_id()
        is_system = self.app_id == PipeWireEngine.SYSTEM_SOUNDS_BUCKET
        can_pin_merge = bool(persistent_source) and not is_system
        can_reset = bool(self.manual_override_active) and not is_system

        if not can_pin_merge:
            info_act = menu.addAction(
                "WaveLinux needs a stable app signature before it can pin or merge this stream."
            )
            info_act.setEnabled(False)
            menu.addSeparator()

        pin_act = menu.addAction("Pin / Rename App…")
        pin_act.setEnabled(can_pin_merge)
        pin_act.triggered.connect(lambda checked=False: win._pin_app_identity(self))

        merge_act = menu.addAction("Merge Into Existing App…")
        merge_act.setEnabled(can_pin_merge)
        merge_act.triggered.connect(lambda checked=False: win._merge_app_identity(self))

        reset_act = menu.addAction("Reset to Auto Detection")
        reset_act.setEnabled(can_reset)
        reset_act.triggered.connect(lambda checked=False: win._reset_app_identity_override(self))

        menu.exec(self.manage_btn.mapToGlobal(self.manage_btn.rect().bottomLeft()))

    def update_state(self, display_name, active_indices, sinks, current_sink,
                     current_volume=None, saved_volume=None,
                     resolved_app_id=None, resolved_app_name=None,
                     identity_source="", override_applied=False,
                     manual_override_active=False, reset_source_app_id="",
                     icon_candidates=None):
        self.app_name = display_name or self.app_name
        self._active_indices = active_indices
        self.resolved_app_id = str(resolved_app_id or self.app_id)
        self.resolved_app_name = str(resolved_app_name or self.app_name)
        self.identity_source = str(identity_source or "")
        self.override_applied = bool(override_applied)
        self.manual_override_active = bool(manual_override_active)
        self.reset_source_app_id = str(reset_source_app_id or "")
        is_active = len(active_indices) > 0
        is_system = (self.app_id == PipeWireEngine.SYSTEM_SOUNDS_BUCKET)

        derived_icon_candidates = list(icon_candidates or [])
        for app_token, app_label in (
            (self.app_id, self.app_name),
            (self.resolved_app_id, self.resolved_app_name),
        ):
            for candidate in PipeWireEngine.theme_icon_candidates_for_app_id(
                app_token,
                fallback_name=app_label,
            ):
                if candidate not in derived_icon_candidates:
                    derived_icon_candidates.append(candidate)
        self._set_icon_candidates(derived_icon_candidates, is_system=is_system)
        label_text = (
            f"{self.app_name} (Idle)" if (is_system and not is_active)
            else f"{self.app_name} (Offline)" if not is_active
            else self.app_name
        )
        active_state = (label_text, is_active, is_system)
        if active_state != self._last_active_state:
            self.name_lbl.setText(label_text)
            self.vol_slider.setEnabled(True)
            self.vol_lbl.setStyleSheet("" if is_active else "color: #666;")
            self.name_lbl.setStyleSheet("" if is_active else "color: #888;")
            self._last_active_state = active_state

        self.forget_btn.setVisible(not is_system)
        self.manage_btn.setVisible(not is_system)
        if not is_system:
            self.manage_btn.setEnabled(True)
            self.manage_btn.setToolTip("Pin, merge, or reset this app identity")
            self.forget_btn.setEnabled(True)
            self.forget_btn.setToolTip(
                "Permanently hide this app from the routing list. "
                "Drops its saved volume / destination too."
            )
        tooltip_lines = [
            f"Canonical app ID: {self.app_id}",
            f"Resolved app ID: {self.resolved_app_id or 'n/a'}",
            "Identity source: " + (self.identity_source or "n/a"),
            "Manual override active: " + ("yes" if self.manual_override_active else "no"),
        ]
        tooltip = "\n".join(tooltip_lines)
        self.name_lbl.setToolTip(tooltip)
        self.manage_btn.setToolTip("Pin, merge, or reset this app identity\n\n" + tooltip)

        vol = current_volume
        if vol is None:
            vol = saved_volume
        if vol is None and is_active:
            vol = self.engine.get_sink_input_volume(active_indices[0])
        if vol is not None and not self.vol_slider.isSliderDown():
            vol_pct = int(vol * 100)
            if self.vol_slider.value() != vol_pct:
                self.vol_slider.blockSignals(True)
                self.vol_slider.setValue(vol_pct)
                self.vol_slider.blockSignals(False)

        # Combo: hardware sinks + user-created WaveLinux channels.
        # Internal mix/source nodes stay hidden. Rebuild only when the
        # sink list actually changes (cached via a fingerprint).
        if not self.combo.view().isVisible():
            sink_fp = tuple(
                (s.get('name'), s.get('display_name')) if isinstance(s, dict)
                else (getattr(s, 'name', None), getattr(s, 'display_name', None))
                for s in sinks
            )
            if getattr(self, '_combo_sink_fp', None) != sink_fp:
                self._combo_sink_fp = sink_fp
                self.combo.blockSignals(True)
                curr_data = self.combo.currentData()
                self.combo.clear()
                self.combo.addItem("System Default", None)
                for s in sinks:
                    if isinstance(s, dict):
                        name = s['name']
                        display_name = s.get('display_name')
                    else:
                        name = getattr(s, 'name', None)
                        display_name = getattr(s, 'display_name', None)
                    if name is None:
                        continue
                    if name.startswith('wavelinux_mix_') or name.startswith('wavelinux_src_'):
                        continue
                    if name.endswith('.monitor'):
                        continue
                    if name.startswith('wavelinux_'):
                        pretty = name.replace('wavelinux_', '').replace('_', ' ').title()
                        display = display_name or pretty
                    else:
                        display = display_name or self.engine.display_name_for_sink(name)
                    self.combo.addItem(display, name)

                target_sink = current_sink or curr_data
                idx = self.combo.findData(target_sink)
                if idx >= 0:
                    self.combo.setCurrentIndex(idx)
                self.combo.blockSignals(False)
                self._last_sink_selection = target_sink
            elif current_sink != self._last_sink_selection:
                # Sink list unchanged but the current selection may have
                # changed — sync the combobox cheaply.
                idx = self.combo.findData(current_sink)
                if idx >= 0 and idx != self.combo.currentIndex():
                    self.combo.blockSignals(True)
                    self.combo.setCurrentIndex(idx)
                    self.combo.blockSignals(False)
                self._last_sink_selection = current_sink


class HealthCard(QFrame):
    """Small status card used by the Health settings tab."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setObjectName("healthCard")
        self.setProperty("severity", "info")
        layout = QVBoxLayout(self)
        layout.setContentsMargins(12, 12, 12, 12)
        layout.setSpacing(8)

        head = QHBoxLayout()
        head.setContentsMargins(0, 0, 0, 0)
        head.setSpacing(8)

        self.badge_lbl = QLabel("INFO")
        self.badge_lbl.setObjectName("healthBadge")
        self.badge_lbl.setProperty("severity", "info")
        head.addWidget(self.badge_lbl, 0, Qt.AlignmentFlag.AlignTop)

        self.title_lbl = QLabel()
        self.title_lbl.setObjectName("healthTitle")
        self.title_lbl.setWordWrap(True)
        head.addWidget(self.title_lbl, 1)
        layout.addLayout(head)

        self.detail_lbl = QLabel()
        self.detail_lbl.setObjectName("healthDetail")
        self.detail_lbl.setWordWrap(True)
        layout.addWidget(self.detail_lbl)

        action_row = QHBoxLayout()
        action_row.setContentsMargins(0, 0, 0, 0)
        action_row.setSpacing(8)
        self.primary_btn = QPushButton()
        self.primary_btn.setObjectName("showHiddenBtn")
        action_row.addWidget(self.primary_btn)
        self.secondary_btn = QPushButton()
        self.secondary_btn.setObjectName("showHiddenBtn")
        action_row.addWidget(self.secondary_btn)
        action_row.addStretch()
        layout.addLayout(action_row)

    def configure(self, issue: HealthIssue, *, primary_handler=None,
                  secondary_handler=None):
        severity = (issue.severity or "info").strip().lower()
        badge_text = severity.upper()
        self.setProperty("severity", severity)
        self.badge_lbl.setText(badge_text)
        self.badge_lbl.setProperty("severity", severity)
        self.title_lbl.setText(issue.title or issue.code)
        self.detail_lbl.setText(issue.detail or "")
        self._refresh_style()

        self._configure_button(
            self.primary_btn,
            issue.primary_action,
            primary_handler,
        )
        self._configure_button(
            self.secondary_btn,
            issue.secondary_action,
            secondary_handler,
        )

    @staticmethod
    def _disconnect_button(button):
        try:
            button.clicked.disconnect()
        except TypeError:
            pass

    def _configure_button(self, button, text, handler):
        self._disconnect_button(button)
        text = str(text or "").strip()
        visible = bool(text and handler is not None)
        button.setVisible(visible)
        if not visible:
            return
        button.setText(text)
        button.clicked.connect(handler)

    def _refresh_style(self):
        for widget in (self, self.badge_lbl):
            widget.style().unpolish(widget)
            widget.style().polish(widget)


# ── Main Window ────────────────────────────────────────────────────
class WaveLinuxWindow(QMainWindow):
    _AUTO_RECOVERY_DELAYS_MS = (1500, 5000)

    def __init__(self):
        super().__init__()
        self.setWindowTitle("WaveLinux")
        
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
        self._hotplug_refresh_timer = QTimer(self)
        self._hotplug_refresh_timer.setSingleShot(True)
        self._hotplug_refresh_timer.setInterval(1800)
        self._hotplug_refresh_timer.timeout.connect(
            lambda: self._request_runtime_refresh("hotplug-settle")
        )

        self._setup_ui()
        self._run_startup_preflight()
        self.load_config()
        self._refresh()
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

    def _on_runtime_view_state(self, view_state):
        self._runtime_view_state = view_state
        health = getattr(view_state, "health", {}) or {}
        pending_ops = getattr(view_state, "pending_operations", {}) or {}
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
        hidden_to_tray = self.tray is not None and not self.isVisible()
        settings_open = self._settings_dialog_visible()
        if settings_open:
            self._refresh_advanced_tab()
            self._refresh_system_tab(preflight=self._startup_preflight)
        if hidden_to_tray and not settings_open:
            return
        if self._any_slider_dragging():
            return
        self._refresh_runtime_view()

    def _request_runtime_refresh(self, reason=""):
        runtime = getattr(self, "runtime", None)
        if runtime is not None and hasattr(runtime, "refresh_now"):
            runtime.refresh_now(reason or "runtime-refresh")

    def _on_runtime_fx_status(self, status):
        node_name = getattr(status, "node_name", "")
        state = getattr(status, "state", "")
        if state in {"building", "cutover_pending", "clearing"}:
            self._cancel_auto_recovery_timer(node_name)
        elif state in {"active", "idle"}:
            self._clear_auto_recovery_state(node_name)
            if node_name and not getattr(self, "_shutting_down", False):
                self._request_runtime_refresh(f"fx-status:{state}:{node_name}")
        if state == "degraded":
            self.status_lbl.setText(
                self.format_fx_status_message(status) or "FX runtime degraded"
            )
            self._schedule_auto_recovery(status)
        if self._settings_dialog_visible():
            self._refresh_system_tab(preflight=self._startup_preflight)
        self._refresh_channel_runtime_status(node_name)

    def _settings_dialog_visible(self):
        dialog = self.__dict__.get("settings_dialog")
        return bool(dialog is not None and dialog.isVisible())

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

    def _open_settings(self):
        self._refresh_scenes_tab()
        self._refresh_hidden_list()
        self._refresh_system_tab()
        self._refresh_advanced_tab()
        self._refresh_update_tab()
        self.settings_dialog.show()
        self.settings_dialog.raise_()

    def _build_system_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(12)

        title = QLabel("HEALTH")
        title.setObjectName("sectionLabel")
        layout.addWidget(title)

        self._system_summary_lbl = QLabel()
        self._system_summary_lbl.setWordWrap(True)
        self._system_summary_lbl.setStyleSheet("color: #e0e0ee; font-size: 13px; font-weight: bold;")
        layout.addWidget(self._system_summary_lbl)

        self._system_runtime_lbl = QLabel()
        self._system_runtime_lbl.setWordWrap(True)
        self._system_runtime_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        layout.addWidget(self._system_runtime_lbl)

        btn_row = QHBoxLayout()
        self._rerun_system_check_btn = QPushButton("Re-run System Check")
        self._rerun_system_check_btn.setObjectName("showHiddenBtn")
        self._rerun_system_check_btn.clicked.connect(self._rerun_system_check)
        btn_row.addWidget(self._rerun_system_check_btn)

        self._repair_launcher_btn = QPushButton("Repair Desktop Launchers")
        self._repair_launcher_btn.setObjectName("showHiddenBtn")
        self._repair_launcher_btn.clicked.connect(self._repair_installed_launchers)
        btn_row.addWidget(self._repair_launcher_btn)

        self._health_recover_btn = QPushButton("Recover degraded channels")
        self._health_recover_btn.setObjectName("showHiddenBtn")
        self._health_recover_btn.clicked.connect(self._recover_all_degraded_channels)
        btn_row.addWidget(self._health_recover_btn)

        self._health_diag_btn = QPushButton("Open diagnostics folder")
        self._health_diag_btn.setObjectName("showHiddenBtn")
        self._health_diag_btn.clicked.connect(self._open_diagnostics_folder)
        btn_row.addWidget(self._health_diag_btn)

        self._health_restart_btn = QPushButton("Restart WaveLinux")
        self._health_restart_btn.setObjectName("showHiddenBtn")
        self._health_restart_btn.clicked.connect(self._restart_app)
        btn_row.addWidget(self._health_restart_btn)

        btn_row.addStretch()
        layout.addLayout(btn_row)

        self._health_cards_scroll = QScrollArea()
        self._health_cards_scroll.setWidgetResizable(True)
        self._health_cards_scroll.setFrameShape(QFrame.Shape.NoFrame)
        self._health_cards_scroll.setStyleSheet("background: transparent;")
        self._health_cards_container = QWidget()
        self._health_cards_layout = QVBoxLayout(self._health_cards_container)
        self._health_cards_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        self._health_cards_layout.setContentsMargins(0, 0, 0, 0)
        self._health_cards_layout.setSpacing(10)
        self._health_cards_scroll.setWidget(self._health_cards_container)
        layout.addWidget(self._health_cards_scroll, 1)

        return tab

    def _build_scenes_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(12)

        title = QLabel("SCENES")
        title.setObjectName("sectionLabel")
        layout.addWidget(title)

        desc = QLabel(
            "Save a full routing snapshot and restore it later. Scenes capture "
            "virtual channels, output targets, app routing, levels, FX chains, "
            "and effect parameters."
        )
        desc.setWordWrap(True)
        desc.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        layout.addWidget(desc)

        pick_row = QHBoxLayout()
        pick_row.addWidget(QLabel("Saved scene:"))
        self._scene_combo = QComboBox()
        self._scene_combo.currentIndexChanged.connect(self._on_scene_selection_change)
        pick_row.addWidget(self._scene_combo, 1)
        layout.addLayout(pick_row)

        self._scene_summary_lbl = QLabel("No saved scenes yet.")
        self._scene_summary_lbl.setWordWrap(True)
        self._scene_summary_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        layout.addWidget(self._scene_summary_lbl)

        btn_row = QHBoxLayout()
        self._apply_scene_btn = QPushButton("Apply Scene")
        self._apply_scene_btn.setObjectName("addBtn")
        self._apply_scene_btn.clicked.connect(self._apply_selected_scene)
        btn_row.addWidget(self._apply_scene_btn)

        self._save_scene_btn = QPushButton("Save Current As…")
        self._save_scene_btn.setObjectName("showHiddenBtn")
        self._save_scene_btn.clicked.connect(self._save_current_scene_as)
        btn_row.addWidget(self._save_scene_btn)

        self._overwrite_scene_btn = QPushButton("Update Selected")
        self._overwrite_scene_btn.setObjectName("showHiddenBtn")
        self._overwrite_scene_btn.clicked.connect(self._overwrite_selected_scene)
        btn_row.addWidget(self._overwrite_scene_btn)

        btn_row.addStretch()
        layout.addLayout(btn_row)

        edit_row = QHBoxLayout()
        self._rename_scene_btn = QPushButton("Rename")
        self._rename_scene_btn.setObjectName("showHiddenBtn")
        self._rename_scene_btn.clicked.connect(self._rename_selected_scene)
        edit_row.addWidget(self._rename_scene_btn)

        self._delete_scene_btn = QPushButton("Delete")
        self._delete_scene_btn.setObjectName("removeBtn")
        self._delete_scene_btn.clicked.connect(self._delete_selected_scene)
        edit_row.addWidget(self._delete_scene_btn)

        edit_row.addStretch()
        layout.addLayout(edit_row)
        layout.addStretch(1)
        return tab

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
            "monitor_hw": self._desired_mix_hw.get("Monitor"),
            "stream_hw": self._desired_mix_hw.get("Stream"),
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
            "monitor_hw": raw.get("monitor_hw"),
            "stream_hw": raw.get("stream_hw"),
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

    def _apply_scene_snapshot(self, snapshot, *, scene_name=""):
        snapshot = self._normalize_scene_snapshot(snapshot)
        if snapshot is None:
            return False
        scene_virtuals = list(snapshot.get("virtual_channels", []))
        existing_virtuals = [name for name in self.virtual_channels if name not in scene_virtuals]
        self.virtual_channels = scene_virtuals + existing_virtuals
        for name in scene_virtuals:
            self.runtime.ensure_virtual_channel_sync(name)

        selected_mic = snapshot.get("selected_mic") or None
        self.selected_mic = selected_mic

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
        self._desired_mix_hw["Monitor"] = snapshot.get("monitor_hw")
        self._desired_mix_hw["Stream"] = snapshot.get("stream_hw")
        self.runtime.set_mix_hardware_route("Monitor", snapshot.get("monitor_hw"))
        self.runtime.set_mix_hardware_route("Stream", snapshot.get("stream_hw"))
        self._sync_runtime_persistent_state()
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
        view = getattr(self, "_runtime_view_state", None)
        default_source = getattr(view, "default_source", None) if view is not None else None
        if default_source:
            return default_source
        mics = list(getattr(view, "mic_inputs", []) or []) if view is not None else []
        if mics:
            return getattr(mics[0], "name", "") or None
        engine = getattr(self, "engine", None)
        if engine is not None and hasattr(engine, "get_default_source"):
            return engine.get_default_source()
        return None

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
            self.runtime.ensure_virtual_channel_sync(display_name)

        selected_mic = self._preferred_setup_mic()
        if selected_mic:
            self.selected_mic = selected_mic
            self.runtime.set_selected_mic(selected_mic)
            self.active_effects[selected_mic] = list(template.get("mic_effects", []) or [])

        default_sink = self.engine.get_default_sink() if hasattr(self.engine, "get_default_sink") else None
        if default_sink:
            self._set_mix_output_target("Monitor", default_sink, persist=False, update_combo=True)
        self._selected_setup_template = template_id
        self._onboarding_completed = True
        self._show_first_run_setup = False
        self._sync_runtime_persistent_state()
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

    def _build_advanced_tab(self):
        """Settings → Advanced tab. Each control writes through to
        config.json immediately."""
        tab = QWidget()
        layout = QVBoxLayout(tab)
        layout.setContentsMargins(8, 8, 8, 8)
        layout.setSpacing(12)

        def _heading(text):
            lbl = QLabel(text)
            lbl.setObjectName("sectionLabel")
            layout.addWidget(lbl)

        # App prune cutoff
        _heading("APP CLEANUP")
        prune_row = QHBoxLayout()
        prune_row.addWidget(QLabel("Forget offline apps after (days):"))
        self.prune_spin = QSpinBox()
        self.prune_spin.setRange(1, 365)
        self.prune_spin.setValue(self.app_prune_days)
        self.prune_spin.valueChanged.connect(self._on_prune_days_change)
        prune_row.addWidget(self.prune_spin)
        prune_row.addStretch()
        layout.addLayout(prune_row)

        forget_all_btn = QPushButton("Forget all offline apps now")
        forget_all_btn.setObjectName("removeBtn")
        forget_all_btn.clicked.connect(self._forget_all_offline)
        layout.addWidget(forget_all_btn, alignment=Qt.AlignmentFlag.AlignLeft)

        # Recovery for the ✕ blocklist (otherwise the only way out is
        # editing config.json by hand). Enabled state is set by
        # `_refresh_advanced_tab`.
        self.restore_forgotten_btn = QPushButton("Restore forgotten apps")
        self.restore_forgotten_btn.setObjectName("showHiddenBtn")
        self.restore_forgotten_btn.setToolTip(
            "Clear the per-app ✕ blocklist so apps you've previously "
            "forgotten can show up in the routing tab again."
        )
        self.restore_forgotten_btn.clicked.connect(self._restore_forgotten_apps)
        layout.addWidget(self.restore_forgotten_btn, alignment=Qt.AlignmentFlag.AlignLeft)

        # Startup / tray
        _heading("STARTUP & TRAY")
        self.autostart_check = QCheckBox("Start WaveLinux at login")
        self.autostart_check.setChecked(self.is_autostart_enabled())
        self.autostart_check.toggled.connect(self.set_autostart)
        layout.addWidget(self.autostart_check)

        quick_start_btn = QPushButton("Quick Start Setup…")
        quick_start_btn.setObjectName("showHiddenBtn")
        quick_start_btn.clicked.connect(self._open_quick_start_setup)
        layout.addWidget(quick_start_btn, alignment=Qt.AlignmentFlag.AlignLeft)

        profiles_btn = QPushButton("Sound Card Profiles…")
        profiles_btn.setObjectName("showHiddenBtn")
        profiles_btn.clicked.connect(self._open_card_profiles)
        layout.addWidget(profiles_btn, alignment=Qt.AlignmentFlag.AlignLeft)

        _heading("CONFIG")
        config_btn_row = QHBoxLayout()
        import_config_btn = QPushButton("Import Full Config…")
        import_config_btn.setObjectName("showHiddenBtn")
        import_config_btn.setToolTip(
            "Replace the current WaveLinux configuration with a saved JSON export."
        )
        import_config_btn.clicked.connect(self._import_full_config)
        config_btn_row.addWidget(import_config_btn)

        export_config_btn = QPushButton("Export Full Config…")
        export_config_btn.setObjectName("showHiddenBtn")
        export_config_btn.setToolTip(
            "Save the current WaveLinux configuration, scenes, routing, and FX state to JSON."
        )
        export_config_btn.clicked.connect(self._export_full_config)
        config_btn_row.addWidget(export_config_btn)
        config_btn_row.addStretch()
        layout.addLayout(config_btn_row)

        # LADSPA / diagnostics
        _heading("DIAGNOSTICS")
        probed = len(self.engine.ladspa_plugins)
        ladspa_lbl = QLabel(
            f"LADSPA plugins detected: {probed}\n"
            f"Paths searched: $LADSPA_PATH + standard host LADSPA directories.\n"
            f"AppImage bundled LADSPA is disabled by default; opt in with "
            f"WAVELINUX_ENABLE_BUNDLED_LADSPA=1."
        )
        ladspa_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        ladspa_lbl.setWordWrap(True)
        layout.addWidget(ladspa_lbl)

        export_diag_btn = QPushButton("Export Runtime Diagnostics")
        export_diag_btn.setObjectName("showHiddenBtn")
        export_diag_btn.clicked.connect(self._export_runtime_diagnostics)
        layout.addWidget(export_diag_btn, alignment=Qt.AlignmentFlag.AlignLeft)

        self.recover_degraded_btn = QPushButton("Recover degraded channels")
        self.recover_degraded_btn.setObjectName("showHiddenBtn")
        self.recover_degraded_btn.setToolTip(
            "Request runtime recovery for each channel currently "
            "marked degraded."
        )
        self.recover_degraded_btn.clicked.connect(self._recover_all_degraded_channels)
        layout.addWidget(self.recover_degraded_btn, alignment=Qt.AlignmentFlag.AlignLeft)

        emergency_btn = QPushButton("Emergency Reset (unload all WaveLinux modules)")
        emergency_btn.setObjectName("removeBtn")
        emergency_btn.clicked.connect(self._on_emergency_reset)
        layout.addWidget(emergency_btn, alignment=Qt.AlignmentFlag.AlignLeft)

        layout.addStretch(1)
        return tab

    # ── Update tab ────────────────────────────────────────────────

    def _build_update_tab(self):
        tab = QWidget()
        layout = QVBoxLayout(tab)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(12)

        ver_lbl = QLabel(f"Current running version: <b>{APP_VERSION}</b>")
        ver_lbl.setStyleSheet("color: #e0e0ee; font-size: 13px;")
        layout.addWidget(ver_lbl)

        self._update_status_lbl = QLabel("Click 'Check for Updates' to see if a newer version is available.")
        self._update_status_lbl.setWordWrap(True)
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
        layout.addWidget(self._update_status_lbl)

        self._update_policy_lbl = QLabel()
        self._update_policy_lbl.setWordWrap(True)
        self._update_policy_lbl.setStyleSheet("color: #5a5a72; font-size: 11px;")
        layout.addWidget(self._update_policy_lbl)

        btn_row = QHBoxLayout()
        self._check_update_btn = QPushButton("Check for Updates")
        self._check_update_btn.setObjectName("showHiddenBtn")
        self._check_update_btn.clicked.connect(self._check_for_updates)
        btn_row.addWidget(self._check_update_btn)

        self._open_release_btn = QPushButton("Open Releases Page")
        self._open_release_btn.setObjectName("addBtn")
        self._open_release_btn.clicked.connect(self._open_release_page)
        btn_row.addWidget(self._open_release_btn)

        self._download_update_btn = QPushButton("Download && Install Latest AppImage")
        self._download_update_btn.setObjectName("showHiddenBtn")
        self._download_update_btn.clicked.connect(self._download_and_install_update)
        btn_row.addWidget(self._download_update_btn)

        self._install_runtime_btn = QPushButton()
        self._install_runtime_btn.setObjectName("showHiddenBtn")
        self._install_runtime_btn.clicked.connect(self._install_current_runtime_launcher)
        btn_row.addWidget(self._install_runtime_btn)

        self._rollback_update_btn = QPushButton("Restore Previous AppImage")
        self._rollback_update_btn.setObjectName("showHiddenBtn")
        self._rollback_update_btn.clicked.connect(self._restore_previous_appimage)
        btn_row.addWidget(self._rollback_update_btn)

        btn_row.addStretch()
        layout.addLayout(btn_row)

        self._install_state_lbl = QLabel()
        self._install_state_lbl.setWordWrap(True)
        self._install_state_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        layout.addWidget(self._install_state_lbl)

        self._install_warning_lbl = QLabel()
        self._install_warning_lbl.setWordWrap(True)
        self._install_warning_lbl.setStyleSheet("color: #d28b26; font-size: 11px;")
        layout.addWidget(self._install_warning_lbl)

        self._update_progress = QProgressBar()
        self._update_progress.setVisible(False)
        self._update_progress.setTextVisible(True)
        self._update_progress.setRange(0, 100)
        self._update_progress.setValue(0)
        layout.addWidget(self._update_progress)

        self._update_note_lbl = QLabel()
        self._update_note_lbl.setWordWrap(True)
        self._update_note_lbl.setStyleSheet("color: #5a5a72; font-size: 11px;")
        layout.addWidget(self._update_note_lbl)

        layout.addStretch(1)
        return tab

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
        progress = getattr(self, "_update_progress", None)
        if progress is not None:
            progress.setVisible(True)
            progress.setRange(0, 0)
            progress.setFormat("Preparing update…")
        self._download_update_btn.setEnabled(False)
        self._check_update_btn.setEnabled(False)
        self._update_status_lbl.setText("Preparing AppImage download…")
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

        prev = getattr(self, "_update_installer", None)
        if prev is not None:
            prev.cancel()

        release_info = getattr(self, "_pending_verified_release", None)
        self._update_installer = AppImageUpdateInstaller()
        self._update_installer.install(release_info=release_info)

        timer = getattr(self, "_update_install_poll_timer", None)
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

    def _refresh_update_tab(self):
        btn = getattr(self, "_install_runtime_btn", None)
        state = install_state()
        mode, description, guidance = self._runtime_mode_detail()
        backup_path = getattr(state, "installed_appimage_backup_path", installed_appimage_backup_path())
        backup_exists = bool(getattr(state, "installed_appimage_backup_exists", os.path.exists(backup_path)))
        launcher_targets_active = self._launcher_targets_active_runtime(state=state, mode=mode)
        if btn is not None:
            if mode.kind == "appimage":
                btn.setVisible(True)
                btn.setText(
                    "Reinstall This AppImage" if state.installed_appimage_exists else "Install This AppImage"
                )
                btn.setToolTip("Install the currently running AppImage into ~/.local/bin and refresh its desktop launcher.")
            elif mode.kind == "bundle":
                btn.setVisible(True)
                btn.setText(
                    "Reinstall This Local Build"
                    if state.wrapper_mode == "bundle" and getattr(state, "wrapper_bundle_exec", None) == mode.running_path
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
            if state.wrapper_mode == "source":
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
            warning_lbl.setVisible(bool(state.warnings))
            warning_lbl.setText("\n".join(state.warnings))
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
            repair_btn.setEnabled(needs_repair or is_running_in_appimage())

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
        if action == "Restore Previous AppImage":
            self._restore_previous_appimage()
            return
        if action == "Recover channel":
            self.recover_channel(str(issue.context.get("node_name") or ""))
            return
        if action == "Open diagnostics":
            self.open_channel_diagnostics(str(issue.context.get("node_name") or ""))
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

    def _refresh_system_tab(self, *, preflight=None, state=None):
        preflight = preflight or startup_preflight_report()
        self._startup_preflight = preflight
        state = state or install_state()
        backup_path = getattr(state, "installed_appimage_backup_path", installed_appimage_backup_path())
        backup_exists = bool(getattr(state, "installed_appimage_backup_exists", os.path.exists(backup_path)))
        launcher_targets_active = self._launcher_targets_active_runtime(state=state)

        summary_lbl = getattr(self, "_system_summary_lbl", None)
        runtime_lbl = getattr(self, "_system_runtime_lbl", None)
        if not all((summary_lbl, runtime_lbl)):
            return
        issues = self._collect_health_issues(preflight=preflight, state=state)
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
            f"Running binary: {self._running_binary_path(state)}",
            "Installed AppImage: "
            + (state.installed_appimage_path if state.installed_appimage_exists else "not installed"),
            "Backup AppImage: "
            + (backup_path if backup_exists else "not available"),
            "Desktop launcher target: " + (state.desktop_exec_target or "not installed"),
            "Wrapper target: " + (state.wrapper_target or "not installed"),
            "Launcher targets active runtime: "
            + (
                "n/a" if launcher_targets_active is None
                else ("yes" if launcher_targets_active else "no")
            ),
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
                bool(state.stale_launcher_entries or state.wrapper_mismatch or state.desktop_mismatch)
                or is_running_in_appimage()
            )
        recover_btn = getattr(self, "_health_recover_btn", None)
        if recover_btn is not None:
            degraded = len(self._runtime_degraded_channels())
            recover_btn.setEnabled(degraded > 0)
            recover_btn.setText(
                f"Recover degraded channels ({degraded})" if degraded else "Recover degraded channels"
            )

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
        self._refresh_update_tab()

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

    def _sync_runtime_persistent_state(self):
        monitor_hw = self._desired_mix_hw.get("Monitor")
        stream_hw = self._desired_mix_hw.get("Stream")
        if monitor_hw is None and hasattr(self, "mon_out_combo"):
            monitor_hw = self.mon_out_combo.currentData()
        if stream_hw is None and hasattr(self, "str_out_combo"):
            stream_hw = self.str_out_combo.currentData()
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
        self._sync_runtime_persistent_state()
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
        if payload and not self._should_refresh_for_pactl_event(payload):
            return
        self._event_refresh_timer.start()
        if payload and self._should_schedule_settle_refresh_for_pactl_event(payload):
            self._hotplug_refresh_timer.start()

    def _on_event_proc_error(self, err):
        if self._shutting_down:
            return
        logging.warning(f"pactl subscribe error: {err}")

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

    def _setup_ui(self):
        self._setup_tray()
        central_scroll = QScrollArea()
        central_scroll.setWidgetResizable(True)
        central_scroll.setObjectName("centralScroll")
        self.setCentralWidget(central_scroll)

        central = QWidget()
        central.setObjectName("central")
        central_scroll.setWidget(central)
        
        root = QVBoxLayout(central)
        root.setContentsMargins(0, 0, 0, 0)
        root.setSpacing(0)

        # ── Header ──
        header = QFrame()
        header.setObjectName("header")
        h_layout = QHBoxLayout(header)
        h_layout.setContentsMargins(0, 0, 0, 0)

        logo_col = QVBoxLayout()
        logo_col.setSpacing(0)
        logo_lbl = QLabel("🌊  WaveLinux")
        logo_lbl.setObjectName("logoLabel")
        logo_lbl.setAlignment(Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft)
        logo_col.addWidget(logo_lbl)
        sub_lbl = QLabel("PipeWire Audio Router")
        sub_lbl.setObjectName("subtitleLabel")
        sub_lbl.setAlignment(Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft)
        logo_col.addWidget(sub_lbl)
        h_layout.addLayout(logo_col)

        h_layout.addStretch()

        self.settings_btn = QPushButton("⚙️ Settings")
        self.settings_btn.setObjectName("showHiddenBtn")
        self.settings_btn.setToolTip("Open App Routing settings")
        self.settings_btn.clicked.connect(self._open_settings)
        h_layout.addWidget(self.settings_btn)

        self.add_btn = QPushButton("+ Add Channel")
        self.add_btn.setObjectName("addBtn")
        self.add_btn.clicked.connect(self._on_add_channel)
        h_layout.addWidget(self.add_btn)

        root.addWidget(header)

        # ── Body (Inputs) ──
        body = QVBoxLayout()
        body.setContentsMargins(20, 6, 20, 0)
        body.setSpacing(0)

        input_lbl = QLabel("AUDIO SOURCES")
        input_lbl.setObjectName("sectionLabel")
        body.addWidget(input_lbl)

        # Inputs row — horizontally scrolling so the strips stay usable
        # below 1200px window width. `setWidgetResizable(False)` so the
        # inner widget keeps its natural size (strip count × ~160px).
        self.inputs_scroll = QScrollArea()
        self.inputs_scroll.setObjectName("inputsScroll")
        self.inputs_scroll.setWidgetResizable(False)
        self.inputs_scroll.setFrameShape(QFrame.Shape.NoFrame)
        self.inputs_scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.inputs_scroll.setVerticalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.inputs_container = QWidget()
        self.inputs_container.setObjectName("inputsContainer")
        self.input_layout = QHBoxLayout(self.inputs_container)
        self.input_layout.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignTop)
        self.input_layout.setContentsMargins(4, 4, 4, 4)
        self.input_layout.setSpacing(10)
        self.inputs_scroll.setWidget(self.inputs_container)
        self.inputs_scroll.viewport().installEventFilter(self)
        body.addWidget(self.inputs_scroll, 1)

        root.addLayout(body, 1)

        # ── Outputs & App Routing ──
        bottom_widget = QWidget()
        bottom_outer = QVBoxLayout(bottom_widget)
        bottom_outer.setContentsMargins(0, 0, 0, 0)
        bottom_outer.setSpacing(0)

        bottom_container = QHBoxLayout()
        bottom_container.setContentsMargins(20, 4, 20, 4)
        bottom_container.setSpacing(20)
        
        # Outputs Assignment Panel
        out_frame = QFrame()
        out_frame.setObjectName("routingPanel")
        o_layout = QVBoxLayout(out_frame)
        o_layout.setContentsMargins(12, 8, 12, 8)
        o_title = QLabel("MASTER")
        o_title.setObjectName("sectionLabel")
        o_layout.addWidget(o_title)
        o_layout.addSpacing(4)

        # Mic picker — single-mic mode. Per-mic state is keyed by
        # node.name so it survives swaps. Labelled "Microphone Input"
        # rather than "Microphone Source" for less-technical users.
        mic_row = QHBoxLayout()
        mic_lbl = QLabel("🎤 Microphone Input")
        mic_lbl.setObjectName("masterMixLabel")
        self.mic_in_combo = QComboBox()
        self.mic_in_combo.setToolTip("Pick which physical mic the mixer uses")
        self.mic_in_combo.currentIndexChanged.connect(self._on_mic_input_change)
        mic_row.addWidget(mic_lbl)
        mic_row.addWidget(self.mic_in_combo, 1)
        o_layout.addLayout(mic_row)
        o_layout.addSpacing(4)

        # Monitor output — labelled "Monitor" instead of "Headphones"
        # since many users monitor through speakers.
        mon_row = QHBoxLayout()
        mon_lbl = QLabel("🎧 Monitor Output")
        mon_lbl.setObjectName("masterMixLabel")
        self.mon_out_combo = QComboBox()
        self.mon_out_combo.setToolTip("Pick the physical output you listen on (headphones / speakers)")
        self.mon_out_combo.currentIndexChanged.connect(
            lambda idx: self._on_mix_out_change("Monitor", self.mon_out_combo.itemData(idx))
        )
        mon_row.addWidget(mon_lbl)
        mon_row.addWidget(self.mon_out_combo, 1)
        o_layout.addLayout(mon_row)

        self.mon_master_slider = QSlider(Qt.Orientation.Horizontal)
        self.mon_master_slider.setRange(0, 100)
        self.mon_master_slider.setFixedHeight(20)
        self.mon_master_slider.valueChanged.connect(lambda v: self._on_master_vol_change("Monitor", v))
        o_layout.addWidget(self.mon_master_slider)
        o_layout.addSpacing(6)

        # Stream — fixed to the virtual `WaveLinux-Stream` device so OBS
        # has one stable source to pick (matching Wave Link).
        str_row = QHBoxLayout()
        str_lbl = QLabel("📡 Stream")
        str_lbl.setObjectName("masterMixLabel")
        str_row.addWidget(str_lbl)
        self.str_out_label = QLabel("OBS input: WaveLinux-Stream")
        self.str_out_label.setObjectName("streamHintLabel")
        str_row.addWidget(self.str_out_label, 1)
        # Kept for save/load compatibility; hidden.
        self.str_out_combo = QComboBox()
        self.str_out_combo.hide()
        o_layout.addLayout(str_row)

        str_master_row = QHBoxLayout()
        self.str_master_slider = QSlider(Qt.Orientation.Horizontal)
        self.str_master_slider.setRange(0, 100)
        self.str_master_slider.setFixedHeight(20)
        self.str_master_slider.valueChanged.connect(lambda v: self._on_master_vol_change("Stream", v))
        str_master_row.addWidget(self.str_master_slider, 1)

        # Clipguard lives on the per-channel FX chain (`limiter` effect),
        # not the master Stream bus. The master row is just fader + OBS hint.
        o_layout.addLayout(str_master_row)

        bottom_container.addWidget(out_frame, 1)

        # Settings dialog — tabbed (Apps / Hidden / Scenes / Health / Advanced / Updates).
        self.settings_dialog = QDialog(self)
        self.settings_dialog.setWindowTitle("WaveLinux Settings")
        self.settings_dialog.setMinimumSize(640, 480)
        self.settings_dialog.setStyleSheet(STYLESHEET)
        sd_layout = QVBoxLayout(self.settings_dialog)
        sd_layout.setContentsMargins(16, 16, 16, 16)
        tabs = QTabWidget(self.settings_dialog)
        sd_layout.addWidget(tabs)

        # — Apps tab —
        apps_tab = QWidget()
        apps_layout = QVBoxLayout(apps_tab)
        apps_layout.setContentsMargins(8, 8, 8, 8)
        r_title = QLabel("APP ROUTING")
        r_title.setObjectName("sectionLabel")
        apps_layout.addWidget(r_title)
        self.routing_scroll = QScrollArea()
        self.routing_scroll.setWidgetResizable(True)
        self.routing_scroll.setFrameShape(QFrame.Shape.NoFrame)
        self.routing_scroll.setStyleSheet("background: transparent;")
        self.routing_container = QWidget()
        self.routing_layout = QVBoxLayout(self.routing_container)
        self.routing_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        self.routing_layout.setContentsMargins(0, 0, 0, 0)
        self.routing_scroll.setWidget(self.routing_container)
        apps_layout.addWidget(self.routing_scroll, 1)
        tabs.addTab(apps_tab, "Apps")

        # — Hidden channels tab —
        hidden_tab = QWidget()
        hidden_layout = QVBoxLayout(hidden_tab)
        hidden_layout.setContentsMargins(8, 8, 8, 8)
        hidden_title = QLabel("HIDDEN CHANNELS")
        hidden_title.setObjectName("sectionLabel")
        hidden_layout.addWidget(hidden_title)
        self.hidden_list_container = QWidget()
        self.hidden_list_layout = QVBoxLayout(self.hidden_list_container)
        self.hidden_list_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        self.hidden_list_layout.setContentsMargins(0, 0, 0, 0)
        self.hidden_list_layout.setSpacing(4)
        hidden_layout.addWidget(self.hidden_list_container, 1)
        tabs.addTab(hidden_tab, "Hidden")

        # — Scenes tab —
        tabs.addTab(self._build_scenes_tab(), "Scenes")

        # — Health tab —
        tabs.addTab(self._build_system_tab(), "Health")

        # — Advanced tab —
        tabs.addTab(self._build_advanced_tab(), "Advanced")

        # — Updates tab —
        tabs.addTab(self._build_update_tab(), "Updates")

        bottom_outer.addLayout(bottom_container)

        # ── Status Bar ──
        status = QFrame()
        status.setObjectName("statusBar")
        s_layout = QHBoxLayout(status)
        s_layout.setContentsMargins(0, 0, 0, 0)
        dot = QLabel("●")
        dot.setObjectName("statusDot")
        s_layout.addWidget(dot)
        self.status_lbl = QLabel("PipeWire connected")
        self.status_lbl.setObjectName("statusLabel")
        s_layout.addWidget(self.status_lbl)
        s_layout.addStretch()
        bottom_outer.addWidget(status)

        root.addWidget(bottom_widget, 0)

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

    _MIN_STRIP_W = 120
    _MAX_STRIP_W = 280
    _MIN_SLIDER_H = 80
    _MAX_SLIDER_H = 200
    _SLIDER_WIDTH_SCALE_CAP = 180

    def eventFilter(self, obj, event):
        if obj is self.inputs_scroll.viewport() and event.type() == QEvent.Type.Resize:
            self._rescale_strips()
        return super().eventFilter(obj, event)

    # Approximate fixed-height overhead in a strip (icon row, name label,
    # peak bar, link row, MON/STR labels, %, mute buttons, margins/spacing)
    # — everything that isn't the slider. Used to back-compute how much
    # vertical room is left for the sliders.
    _STRIP_VERT_OVERHEAD = 170
    _STRIP_MIN_VIEWPORT_H = 180
    _STRIP_MAX_VIEWPORT_H = 620

    def resizeEvent(self, event):
        out = super().resizeEvent(event)
        if hasattr(self, "inputs_scroll"):
            self._rescale_strips()
        return out

    def _rescale_strips(self):
        strips = list(self.channel_widgets.values())
        n = len(strips)
        if n == 0:
            return
        # Horizontal sizing
        avail_w = self.inputs_scroll.viewport().width()
        spacing = self.input_layout.spacing()
        margins = (self.input_layout.contentsMargins().left()
                   + self.input_layout.contentsMargins().right())
        space = avail_w - spacing * (n - 1) - margins
        ideal_w = space // n if n > 0 else self._MAX_STRIP_W
        if ideal_w >= self._MIN_STRIP_W:
            strip_w = min(ideal_w, self._MAX_STRIP_W)
            self.inputs_scroll.setHorizontalScrollBarPolicy(
                Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        else:
            strip_w = self._MIN_STRIP_W
            self.inputs_scroll.setHorizontalScrollBarPolicy(
                Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        # Slider height is the min of the width-driven cap and the
        # height-driven cap so the strip fits in both axes before
        # scrolling kicks in.
        width_t = (
            (min(strip_w, self._SLIDER_WIDTH_SCALE_CAP) - self._MIN_STRIP_W)
            / (self._SLIDER_WIDTH_SCALE_CAP - self._MIN_STRIP_W)
        )
        width_t = max(0.0, min(1.0, width_t))
        slider_h_w = int(self._MIN_SLIDER_H + width_t * (self._MAX_SLIDER_H - self._MIN_SLIDER_H))
        avail_h = self.inputs_scroll.viewport().height()
        height_t = (
            (avail_h - self._STRIP_MIN_VIEWPORT_H)
            / (self._STRIP_MAX_VIEWPORT_H - self._STRIP_MIN_VIEWPORT_H)
        )
        height_t = max(0.0, min(1.0, height_t))
        slider_h_h = int(
            self._MIN_SLIDER_H
            + height_t * (self._MAX_SLIDER_H - self._MIN_SLIDER_H)
        )
        slider_h = max(self._MIN_SLIDER_H,
                       min(self._MAX_SLIDER_H, slider_h_w, slider_h_h))
        desired_heights = [
            strip.apply_scale(strip_w, slider_h)
            for strip in strips
        ]
        target_strip_h = max(desired_heights) if desired_heights else 0
        for strip in strips:
            strip.apply_scale(strip_w, slider_h, target_height=target_strip_h)
        self.inputs_container.adjustSize()

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
        if not hasattr(self, '_pending_master_vol'):
            self._pending_master_vol = {}
            self._master_commit_timer = QTimer(self)
            self._master_commit_timer.setSingleShot(True)
            self._master_commit_timer.setInterval(40)
            self._master_commit_timer.timeout.connect(self._commit_master_vols)
        self._pending_master_vol[mix_name] = value / 100.0
        self._master_commit_timer.start()

    def _commit_master_vols(self):
        """Fire all pending master-fader writes in one pass."""
        if not hasattr(self, '_pending_master_vol'):
            return
        pending = self._pending_master_vol
        self._pending_master_vol = {}
        for mix_name, vol in pending.items():
            self.runtime.set_mix_volume(mix_name, vol)

    def _set_mix_output_target(self, mix_name, hw_sink_name, *, persist=True, update_combo=False,
                               sync_runtime=False):
        self._desired_mix_hw[mix_name] = hw_sink_name
        runtime = getattr(self, "runtime", None)
        if runtime is not None:
            if sync_runtime and hasattr(runtime, "set_mix_hardware_route_sync"):
                runtime.set_mix_hardware_route_sync(mix_name, hw_sink_name)
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
        self._set_mix_output_target(mix_name, hw_sink_name, persist=True, update_combo=False)

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
            self._sync_runtime_persistent_state()
            self.schedule_save()
        self._pending_clipguard_migration = False

    def _sync_mic_picker(self, mics, default_src=None):
        """Refresh the master mic combo. Cheap on each tick — only
        rebuilds items when the mic list changes. Falls back to
        `pactl get-default-source` when nothing's saved yet."""
        combo = self.mic_in_combo
        mic_names = {m.name for m in mics}
        if self.selected_mic and self.selected_mic in mic_names:
            self._mic_selection_initialized = True

        # Resolve the mic only during the initial selection pass. Once the
        # user has an active mic locked, later hotplug churn should not
        # silently promote another device into the selection.
        if mics and not getattr(self, "_mic_selection_initialized", False):
            if default_src is None:
                default_src = (
                    self.engine.get_default_source()
                    if hasattr(self.engine, 'get_default_source') else None
                )
            if self.selected_mic in mic_names:
                self._mic_selection_initialized = True
            else:
                if default_src and default_src in mic_names:
                    self.selected_mic = default_src
                else:
                    self.selected_mic = mics[0].name
                self.runtime.set_selected_mic(self.selected_mic)
                self._mic_selection_initialized = True
                self.schedule_save()

        mic_fp = tuple((m.name, m.description or '') for m in mics)
        if self.__dict__.get('_mic_combo_fp') != mic_fp:
            self._mic_combo_fp = mic_fp
            combo.blockSignals(True)
            combo.clear()
            for m in mics:
                label = PipeWireEngine.friendly_name(getattr(m, 'description', None)) or m.name
                combo.addItem(label, m.name)
            if not mics:
                combo.addItem("(no microphone detected)", None)
            combo.blockSignals(False)

        # Sync selection to the saved mic, if it's in the combo.
        idx = combo.findData(self.selected_mic)
        if idx >= 0 and combo.currentIndex() != idx:
            combo.blockSignals(True)
            combo.setCurrentIndex(idx)
            combo.blockSignals(False)

    def _refresh_runtime_view(self):
        view = self._runtime_view_state
        if view is None:
            self.status_lbl.setText("PipeWire syncing...")
            return

        mics = list(getattr(view, "mic_inputs", []) or [])
        virtuals = list(getattr(view, "virtual_channels", []) or [])
        self._sync_mic_picker(mics, default_src=getattr(view, "default_source", None))
        if getattr(self, '_pending_clipguard_migration', False) and self.selected_mic:
            self._apply_pending_clipguard_migration()

        present_names = set(getattr(view, "present_node_names", set()) or set())
        if self._known_node_names:
            added = present_names - self._known_node_names
            removed = self._known_node_names - present_names
            if added:
                self._notify_hotplug(added, added=True)
            if removed:
                self._notify_hotplug(removed, added=False)
        self._known_node_names = present_names

        current_node_ids = set()
        visible_strip_ids = []
        input_layout_changed = False
        selected_mic_node = next((m for m in mics if m.name == self.selected_mic), None)
        shown_inputs = []
        if selected_mic_node is not None:
            shown_inputs.append(selected_mic_node)
        shown_inputs.extend(virtuals)
        order_index = {nm: i for i, nm in enumerate(self.channel_order)}
        sorted_nodes = sorted(
            shown_inputs,
            key=lambda n: order_index.get(n.name, len(order_index) + 1),
        )

        for node in sorted_nodes:
            pw_id = str(node.node_id)
            current_node_ids.add(pw_id)
            nname = node.name
            is_hidden = nname in self.hidden_nodes
            if is_hidden and not self.show_hidden:
                if pw_id in self.channel_widgets:
                    strip = self.channel_widgets[pw_id]
                    if not strip.isHidden():
                        strip.hide()
                        input_layout_changed = True
                meter = self.meters.pop(pw_id, None)
                if meter is not None:
                    meter.stop()
                continue
            visible_strip_ids.append(pw_id)

            mon_key = f"{nname}_Monitor"
            str_key = f"{nname}_Stream"
            fresh_mon = mon_key not in self.submix_state
            fresh_str = str_key not in self.submix_state
            if fresh_mon:
                self.submix_state[mon_key] = {
                    'vol': float(node.monitor_volume),
                    'mute': bool(node.monitor_mute),
                }
            if fresh_str:
                self.submix_state[str_key] = {
                    'vol': float(node.stream_volume),
                    'mute': bool(node.stream_mute),
                }
            if fresh_mon or fresh_str:
                self.schedule_save()

            if pw_id not in self.channel_widgets:
                strip = ChannelStrip(
                    pw_id,
                    nname,
                    node.label,
                    node.channel_type,
                    node.icon,
                    self.engine,
                )
                self.channel_widgets[pw_id] = strip
                self.input_layout.addWidget(strip)
                input_layout_changed = True

            strip = self.channel_widgets[pw_id]
            strip.node_id = pw_id
            if strip.isHidden():
                strip.show()
                input_layout_changed = True
            strip.update_from_node(
                float(node.monitor_volume),
                bool(node.monitor_mute),
                float(node.stream_volume),
                bool(node.stream_mute),
                is_hidden,
                source_vol=float(getattr(node, "source_volume", 1.0)),
                source_mute=bool(getattr(node, "source_mute", False)),
            )
            strip._refresh_fx_indicator(active=getattr(node, "fx_running", False))
            issue = self.channel_runtime_issue(nname)
            strip.set_runtime_issue(issue["degraded"], issue["tooltip"])
            strip.setToolTip(issue["tooltip"] if issue["degraded"] else "")

            meter = self.meters.get(pw_id)
            meter_source = node.meter_source
            if meter is None or meter.source_name != meter_source:
                if meter is not None:
                    meter.stop()
                meter = MeterWorker(meter_source, self)
                meter.peak.connect(strip.on_peak)
                meter.start()
                self.meters[pw_id] = meter

        for stale in [pid for pid in self.channel_widgets if pid not in current_node_ids]:
            widget = self.channel_widgets.pop(stale)
            widget.setParent(None)
            widget.deleteLater()
            meter = self.meters.pop(stale, None)
            if meter is not None:
                meter.stop()
            input_layout_changed = True

        visible_strip_sig = tuple(visible_strip_ids)
        if input_layout_changed or visible_strip_sig != self._visible_strip_ids:
            self._visible_strip_ids = visible_strip_sig
            self._rescale_strips()
            self.inputs_container.adjustSize()

        sink_rows = [
            {'name': sink.name, 'display_name': sink.display_name}
            for sink in getattr(view, "sinks", []) or []
        ]
        apps_by_id = {
            app.app_id: app for app in (getattr(view, "app_views", []) or [])
        }
        now = int(time.time())
        identity_migrated = False
        for app in apps_by_id.values():
            identity_migrated |= self._migrate_legacy_app_identity(app.app_id, app.app_name)
            if app.active_indices:
                self.app_last_seen[app.app_id] = now
        cutoff = int(time.time()) - max(1, self.app_prune_days) * 24 * 3600
        recently_seen = {
            app_id for app_id, ts in self.app_last_seen.items()
            if ts >= cutoff
        }
        all_display_app_ids = (
            set(apps_by_id.keys())
            | set(self.app_routing.keys())
            | set(getattr(self, "app_volumes", {}).keys())
            | recently_seen
            | {PipeWireEngine.SYSTEM_SOUNDS_BUCKET}
        )
        all_display_app_ids -= self.forgotten_apps
        sys_bucket = PipeWireEngine.SYSTEM_SOUNDS_BUCKET
        ordered_display_apps = sorted(
            all_display_app_ids,
            key=lambda app_id: (
                0 if app_id == sys_bucket else 1,
                self._display_name_for_app_id(
                    app_id,
                    apps_by_id.get(app_id).app_name if app_id in apps_by_id else None,
                ).lower(),
            ),
        )
        routing_layout_changed = tuple(ordered_display_apps) != self._app_widget_order
        for app_id in ordered_display_apps:
            runtime_app = apps_by_id.get(app_id)
            active_indices = list(runtime_app.active_indices) if runtime_app is not None else []
            if runtime_app is not None:
                row_identity = runtime_app
            else:
                reset_sources = self._override_sources_for_target(app_id)
                row_identity = SimpleNamespace(
                    app_id=app_id,
                    app_name=self._display_name_for_app_id(app_id),
                    resolved_app_id=reset_sources[0] if len(reset_sources) == 1 else app_id,
                    resolved_app_name=self._display_name_for_app_id(
                        reset_sources[0] if len(reset_sources) == 1 else app_id,
                    ),
                    identity_source="remembered",
                    override_applied=bool(reset_sources),
                    manual_override_active=bool(reset_sources or app_id in self.app_label_overrides),
                    reset_source_app_id=reset_sources[0] if len(reset_sources) == 1 else "",
                )
            display_name = self._display_name_for_app_id(
                app_id,
                getattr(row_identity, "app_name", None),
            )
            ctx = self._app_identity_context(row_identity)
            preferred_sink = self.app_routing.get(app_id)
            live_sink = runtime_app.current_sink if runtime_app is not None else None
            current_sink = preferred_sink or live_sink
            current_volume = runtime_app.current_volume if runtime_app is not None else None
            saved_volume = getattr(self, "app_volumes", {}).get(app_id)
            if app_id not in self.app_widgets:
                row = AppRoutingRow(app_id, display_name, self.engine, sink_rows, main_win=self)
                self.app_widgets[app_id] = row
                self.routing_layout.addWidget(row)
                routing_layout_changed = True
            self.app_widgets[app_id].update_state(
                display_name,
                active_indices,
                sink_rows,
                current_sink,
                current_volume=current_volume,
                saved_volume=saved_volume,
                resolved_app_id=ctx["resolved_app_id"],
                resolved_app_name=ctx["resolved_app_name"],
                identity_source=ctx["identity_source"],
                override_applied=ctx["override_applied"],
                manual_override_active=ctx["manual_override_active"],
                reset_source_app_id=ctx["reset_source_app_id"],
                icon_candidates=list(getattr(row_identity, "icon_candidates", []) or []),
            )
        for app_id in list(self.app_widgets.keys()):
            if app_id not in all_display_app_ids:
                self.app_widgets[app_id].setParent(None)
                self.app_widgets[app_id].deleteLater()
                del self.app_widgets[app_id]
                routing_layout_changed = True
        if routing_layout_changed:
            self._app_widget_order = tuple(ordered_display_apps)
            self.routing_container.updateGeometry()
            self.routing_container.adjustSize()
        if identity_migrated:
            self._sync_runtime_persistent_state()
            self.schedule_save()

        combo = self.mon_out_combo
        if not combo.view().isVisible():
            current_hw = getattr(view.mixes.get("Monitor"), "hardware_sink", None)
            desired_hw = self._desired_mix_hw.get("Monitor")
            current_data = combo.currentData()
            combo_updated = False
            sink_fp = tuple(
                (sink.name, sink.display_name)
                for sink in (getattr(view, "sinks", []) or [])
                if not sink.is_internal and not sink.name.startswith("wavelinux_")
            )
            if sink_fp != self._monitor_sink_fp:
                self._monitor_sink_fp = sink_fp
                combo.blockSignals(True)
                combo.clear()
                combo.addItem("None", None)
                for sink_name, display_name in sink_fp:
                    combo.addItem(display_name, sink_name)
                idx = combo.findData(desired_hw)
                if idx < 0:
                    idx = combo.findData(current_data)
                if idx < 0 and desired_hw is None:
                    idx = combo.findData(current_hw)
                if idx >= 0:
                    combo.setCurrentIndex(idx)
                    combo_updated = True
                combo.blockSignals(False)
            elif desired_hw != current_data:
                idx = combo.findData(desired_hw)
                if idx >= 0 and idx != combo.currentIndex():
                    combo.blockSignals(True)
                    combo.setCurrentIndex(idx)
                    combo.blockSignals(False)
                    combo_updated = True

        mon_mix = view.mixes.get("Monitor")
        if mon_mix and not self.mon_master_slider.isSliderDown():
            self.mon_master_slider.blockSignals(True)
            self.mon_master_slider.setValue(int(mon_mix.master_volume * 100))
            self.mon_master_slider.blockSignals(False)
        str_mix = view.mixes.get("Stream")
        if str_mix and not self.str_master_slider.isSliderDown():
            self.str_master_slider.blockSignals(True)
            self.str_master_slider.setValue(int(str_mix.master_volume * 100))
            self.str_master_slider.blockSignals(False)

        if not getattr(view, "health", {}):
            self.status_lbl.setText(
                f"PipeWire connected · {getattr(view, 'node_count', 0)} nodes · "
                f"{getattr(view, 'app_count', 0)} apps"
            )

    def _on_mic_input_change(self, idx):
        """Persist a mic-picker change and refresh immediately so the
        strip swaps to the new mic on the same tick."""
        new_mic = self.mic_in_combo.itemData(idx)
        if new_mic == self.selected_mic:
            return
        self.selected_mic = new_mic
        self._mic_selection_initialized = True
        self.runtime.set_selected_mic(new_mic)
        self.schedule_save()
        self._refresh()

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
        return {
            'schema_version': 1,
            'monitor_hw': self._desired_mix_hw.get("Monitor"),
            'stream_hw': self._desired_mix_hw.get("Stream"),
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
        self.selected_mic = conf.get('selected_mic') or None
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

        if runtime is not None:
            runtime.ensure_output_mix_sync("Monitor")
            runtime.ensure_output_mix_sync("Stream")

        self._pending_clipguard_migration = bool(conf.get('clipguard'))
        if self._pending_clipguard_migration and self.selected_mic:
            self._apply_pending_clipguard_migration()

        if remove_missing_virtuals and runtime is not None:
            for name in previous_virtuals:
                if name in self.virtual_channels:
                    continue
                _, safe = PipeWireEngine._sanitize_channel_name(name)
                runtime.remove_virtual_channel_sync(f"wavelinux_{safe}")
        if runtime is not None:
            for name in self.virtual_channels:
                runtime.ensure_virtual_channel_sync(name)

        default_sink = (
            engine.get_default_sink()
            if engine is not None and hasattr(engine, "get_default_sink")
            else None
        )
        mon_hw = conf.get('monitor_hw') or default_sink
        str_hw = conf.get('stream_hw')
        self._set_mix_output_target(
            "Monitor", mon_hw, persist=False, update_combo=True, sync_runtime=True
        )
        self._set_mix_output_target(
            "Stream", str_hw, persist=False, update_combo=True, sync_runtime=True
        )
        self._sync_runtime_persistent_state()

    def load_config(self):
        if not os.path.exists(self.config_path):
            # First launch — set up the standard mixes, route Monitor to
            # the system default, and seed starter channels.
            self._onboarding_completed = False
            self._selected_setup_template = ""
            self._show_first_run_setup = True
            self.runtime.ensure_output_mix_sync("Monitor")
            self.runtime.ensure_output_mix_sync("Stream")
            def_sink = self.engine.get_default_sink()
            if def_sink:
                self._set_mix_output_target(
                    "Monitor", def_sink, persist=False, update_combo=True, sync_runtime=True
                )
            self._seed_default_channels()
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
            self._write_config_file(self.config_path, self._serialize_config())
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
        widgets = list(self.channel_widgets.values())
        for w in widgets:
            self.input_layout.removeWidget(w)
        name_to_widget = {w.node_name: w for w in widgets if w.node_name}
        for nm in self.channel_order:
            w = name_to_widget.pop(nm, None)
            if w is not None:
                self.input_layout.addWidget(w)
        # Anything not in channel_order goes to the end.
        for w in name_to_widget.values():
            self.input_layout.addWidget(w)
        self._rescale_strips()
        self.inputs_container.adjustSize()

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
        pretty = ", ".join(
            PipeWireEngine.friendly_name(n.replace("wavelinux_", "").replace("_", " "))
            for n in list(node_names)[:3]
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
        quit_act.triggered.connect(self._quit_app)
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

    def closeEvent(self, event):
        """Minimize to tray when one is available; otherwise actually quit."""
        if self.tray is not None and self.tray.isVisible():
            event.ignore()
            self.hide()
            return
        self._quit_app()
        event.accept()

    def _stop_all_meters(self):
        for meter in list(self.meters.values()):
            meter.stop()
        self.meters.clear()

    def _quit_app(self):
        """Cleanly save state, unload all modules, and exit."""
        logging.info("Shutting down WaveLinux...")
        self._shutting_down = True
        self.refresh_timer.stop()
        self._save_timer.stop()
        self._event_refresh_timer.stop()
        # Stop every parec meter subprocess.
        self._stop_all_meters()
        # Flush any pending slider writes before we tear down the engine.
        self.save_config()
        proc = getattr(self, "_event_proc", None)
        if proc is not None and proc.state() != QProcess.ProcessState.NotRunning:
            proc.kill()
            proc.waitForFinished(500)
        self._clear_runtime_pid()
        self.runtime.full_audio_reset_sync()
        self.runtime.shutdown()
        logging.info("Audio reset complete. Exiting.")
        QApplication.instance().quit()

    def _cleanup_before_exit(self):
        self._clear_runtime_pid()
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
