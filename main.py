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
import tempfile
import threading
import queue
import hashlib
import urllib.request
import urllib.error

from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QLabel, QSlider, QPushButton, QFrame, QScrollArea, QDialog,
    QDialogButtonBox, QComboBox, QMessageBox, QSystemTrayIcon,
    QMenu, QInputDialog, QProgressBar, QSizePolicy, QTabWidget,
    QSpinBox, QCheckBox
)
from PyQt6.QtCore import Qt, QTimer, QLockFile, QProcess, pyqtSignal, QObject, QEvent
from PyQt6.QtGui import QFont, QIcon, QAction

from audio_runtime import AudioRuntimeAdapter, AudioRuntimeController
from pipewire_engine import PipeWireEngine
from wavelinux_theme import STYLESHEET

import struct

APP_VERSION = "2.0.0"
_GITHUB_OWNER = "excalprimeacct-gif"
_GITHUB_REPO  = "WaveLinux"
_UPDATE_FILES = ["main.py", "pipewire_engine.py", "wavelinux_theme.py"]
_RUNTIME_DEPS = ["pactl", "pw-dump", "wpctl", "parec", "pipewire"]


def _parse_version(v):
    """Return a comparable tuple from a semver string like '1.2.3' or 'v1.2.3'."""
    v = v.lstrip('v').strip()
    try:
        return tuple(int(x) for x in v.split('.'))
    except ValueError:
        return (0,)


class UpdateChecker:
    """Background updater using queue polling from the Qt thread."""

    _API_URL  = (f"https://api.github.com/"
                 f"repos/{_GITHUB_OWNER}/{_GITHUB_REPO}/releases/latest")
    _RAW_BASE = (f"https://raw.githubusercontent.com/"
                 f"{_GITHUB_OWNER}/{_GITHUB_REPO}")
    _CHECKSUMS_FILE = "sha256sums.txt"

    def __init__(self, app_dir):
        self._app_dir = app_dir
        self._q       = queue.SimpleQueue()
        self._cancel  = threading.Event()

    def check(self):
        self._cancel.clear()
        threading.Thread(target=self._do_check, daemon=True).start()

    def download(self, tag):
        self._cancel.clear()
        threading.Thread(target=self._do_download, args=(tag,), daemon=True).start()

    def cancel(self):
        self._cancel.set()

    def poll(self):
        """Return next queued item or None. Always called from the Qt thread."""
        try:
            return self._q.get_nowait()
        except queue.Empty:
            return None

    def _fetch_json(self, url):
        req = urllib.request.Request(url, headers={"User-Agent": "WaveLinux-Updater"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode())

    def _do_check(self):
        try:
            data = self._fetch_json(self._API_URL)
            tag = data.get("tag_name", "").lstrip('v')
            if not tag:
                self._q.put(('error', "GitHub returned no release tag — has a release been published yet?"))
                return
            self._q.put(('result', tag))
        except urllib.error.HTTPError as e:
            if e.code == 404:
                self._q.put(('error', "No releases published yet on GitHub."))
            else:
                self._q.put(('error', f"HTTP {e.code}: {e.reason}"))
        except urllib.error.URLError as e:
            self._q.put(('error', f"Network error: {e.reason}"))
        except Exception as e:
            self._q.put(('error', f"Check failed: {e}"))

    def _do_download(self, tag):
        tmp_dir = tempfile.mkdtemp(prefix="wavelinux-update-")
        try:
            expected = self._fetch_checksums(tag)
            if not expected:
                self._q.put(('error', "Release is missing sha256sums.txt; refusing insecure update."))
                return
            for filename in _UPDATE_FILES:
                if self._cancel.is_set():
                    self._q.put(('cancelled',))
                    return
                url = f"{self._RAW_BASE}/v{tag}/{filename}"
                dest = os.path.join(tmp_dir, filename)
                self._download_file(url, dest, filename)
                got = self._sha256_file(dest)
                want = expected.get(filename)
                if not want:
                    self._q.put(('error', f"sha256sums.txt missing entry for {filename}; refusing update."))
                    return
                if got.lower() != want.lower():
                    self._q.put(('error', f"Checksum mismatch for {filename}; refusing update."))
                    return
                if self._cancel.is_set():
                    self._q.put(('cancelled',))
                    return

            for filename in _UPDATE_FILES:
                src = os.path.join(tmp_dir, filename)
                dst = os.path.join(self._app_dir, filename)
                if not os.path.exists(src):
                    continue
                try:
                    if os.path.exists(dst):
                        shutil.copy2(dst, dst + ".bak")
                    os.replace(src, dst)
                except OSError as e:
                    self._q.put(('error', f"Could not replace {filename}: {e}"))
                    return

            self._q.put(('progress', '__done__', 0, 0))
        except Exception as e:
            self._q.put(('error', f"Download failed: {e}"))
        finally:
            shutil.rmtree(tmp_dir, ignore_errors=True)

    def _fetch_checksums(self, tag):
        url = f"{self._RAW_BASE}/v{tag}/{self._CHECKSUMS_FILE}"
        req = urllib.request.Request(url, headers={"User-Agent": "WaveLinux-Updater"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
        out = {}
        for line in raw.splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) < 2:
                continue
            digest = parts[0].strip()
            filename = parts[-1].strip().lstrip("*")
            if filename in _UPDATE_FILES:
                out[filename] = digest
        return out

    @staticmethod
    def _sha256_file(path):
        h = hashlib.sha256()
        with open(path, "rb") as fh:
            while True:
                chunk = fh.read(65536)
                if not chunk:
                    break
                h.update(chunk)
        return h.hexdigest()

    def _download_file(self, url, dest, filename):
        req = urllib.request.Request(url, headers={"User-Agent": "WaveLinux-Updater"})
        with urllib.request.urlopen(req, timeout=30) as resp:
            total = int(resp.headers.get("Content-Length", 0))
            done  = 0
            with open(dest, "wb") as fh:
                while not self._cancel.is_set():
                    chunk = resp.read(65536)
                    if not chunk:
                        break
                    fh.write(chunk)
                    done += len(chunk)
                    self._q.put(('progress', filename, done, total))


class MeterWorker(QObject):
    """Emit normalized channel peaks from `parec`."""

    peak = pyqtSignal(float)

    def __init__(self, source_name, parent=None):
        super().__init__(parent)
        self.source_name = source_name
        self._proc = None
        self._buf = bytearray()
        self._sample_rate = 24000
        self._frame_hz = 20
        self._sample_bytes = self._sample_rate * 2 // self._frame_hz
        self._last_peak = 0.0

    def start(self):
        if self._proc is not None:
            return
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
        self._buf.clear()

    def _on_bytes(self):
        if self._proc is None:
            return
        chunk = bytes(self._proc.readAllStandardOutput())
        if not chunk:
            return
        self._buf.extend(chunk)
        # Process the buffer in window-sized frames.
        while len(self._buf) >= self._sample_bytes:
            frame = bytes(self._buf[:self._sample_bytes])
            del self._buf[:self._sample_bytes]
            count = len(frame) // 2
            if count == 0:
                continue
            samples = struct.unpack(f"<{count}h", frame)
            peak_int = max((abs(s) for s in samples), default=0)
            normalized = peak_int / 32768.0
            # Simple release envelope so the meter doesn't flicker.
            if normalized >= self._last_peak:
                self._last_peak = normalized
            else:
                self._last_peak = max(normalized, self._last_peak * 0.6)
            self.peak.emit(min(self._last_peak, 1.0))


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
        if self.runtime is not None:
            self.runtime.fx_status_changed.connect(self._on_runtime_fx_status)

        # Debounce param-driven rebuilds — 150ms feels live but doesn't
        # respawn the chain on every pixel of slider drag.
        self._param_timer = QTimer(self)
        self._param_timer.setSingleShot(True)
        self._param_timer.setInterval(150)
        self._param_timer.timeout.connect(self._rebuild_chain)

        root = QVBoxLayout(self)
        root.setSpacing(12)
        root.setContentsMargins(20, 20, 20, 20)

        title = QLabel("✨ Channel Effects")
        title.setStyleSheet("font-size: 18px; font-weight: bold; color: #fff;")
        root.addWidget(title)

        desc = QLabel("Processing is applied per-channel. Parameters save automatically.")
        desc.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        root.addWidget(desc)

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
        # Flush any pending debounced rebuild so a slider tweak that
        # finished within the last 150 ms isn't lost on close.
        if self._param_timer.isActive():
            self._param_timer.stop()
            self._rebuild_chain()

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
                "rnnoise needs noise-suppression-for-voice (AUR); "
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

    # Toggle, preset pick, and slider drag all funnel through
    # `_rebuild_chain` to respawn the channel's filter-chain with the
    # current ON-set + parameter values.

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

    def _rebuild_chain(self):
        """Spawn / replace the channel's FX chain to match the dialog's
        current toggle + slider state, then ask the main window to re-route
        submix loopbacks through the new bus."""
        if self.runtime is None:
            QMessageBox.warning(
                self,
                "Audio runtime unavailable",
                "WaveLinux's audio runtime is not available, so effects "
                "cannot be applied right now.",
            )
            return
        wanted = self._active_effect_ids()
        params_map = self._all_params_map()
        
        # Persist on the main-window state objects so a restart re-applies.
        self._save_chain_state(wanted, params_map)
        
        self._start_fx_worker(wanted, params_map)

    def _start_fx_worker(self, wanted, params_map):
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
            QMessageBox.warning(
                self,
                "Effects rebuild failed",
                "WaveLinux could not apply one or more effects.\n\n"
                f"{status.message}"
            )
        if status.state not in {"building", "cutover_pending", "clearing"}:
            self.close_btn.setText("Apply")
            self.close_btn.setEnabled(True)
            self._refresh_toggle_status()
            if self._pending_close:
                self._pending_close = False
                self.accept()

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
        # Debounced — _on_done flushes any pending rebuild on close.
        self._param_timer.start()

    def _apply_preset(self, effect_id, values):
        """Snap the effect's sliders to a labelled preset and respawn."""
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
        # Debounced — _on_done flushes any pending rebuild on close.
        self._param_timer.start()

    def _on_param_changed(self, effect_id, slider, value_lbl):
        pmin = float(slider.property("pmin"))
        pmax = float(slider.property("pmax"))
        suffix = slider.property("psuffix") or ""
        val = pmin + (pmax - pmin) * (slider.value() / 1000.0)
        value_lbl.setText(self._fmt_value(val, suffix))
        # Debounced — _on_done flushes any pending rebuild on close.
        self._param_timer.start()


# ── Channel Strip Widget ───────────────────────────────────────────
class ChannelStrip(QFrame):
    """A single mixer channel: icon, name, vertical fader, mute, FX."""

    def __init__(self, node_id, node_name, name, ch_type, icon, engine, parent=None):
        super().__init__(parent)
        self.setObjectName("channelStrip")
        self.setFixedWidth(self._MAX_W)
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
        # Pending values held until the timer fires.
        self._pending_mon_vol = None
        self._pending_str_vol = None

        layout = QVBoxLayout(self)
        layout.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        layout.setContentsMargins(8, 8, 8, 8)
        layout.setSpacing(4)

        # Icon + optional FX indicator. Other actions live in right-click.
        head_row = QHBoxLayout()
        head_row.setContentsMargins(0, 0, 0, 0)
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
        name_lbl = QLabel(name)
        name_lbl.setObjectName("channelName")
        name_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        name_lbl.setWordWrap(True)
        name_lbl.setMinimumHeight(24)  # allow 2 lines
        layout.addWidget(name_lbl)

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
        mon_col.addWidget(self.mon_slider, 1)
        self.mon_mute = QPushButton("🎧")
        self.mon_mute.setObjectName("muteBtn")
        self.mon_mute.setFixedSize(28, 28)
        self.mon_mute.setToolTip("Mute in Monitor mix")
        self.mon_mute.clicked.connect(self._on_mon_mute)
        mon_col.addWidget(self.mon_mute)
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
        str_col.addWidget(self.str_slider, 1)
        self.str_mute = QPushButton("📡")
        self.str_mute.setObjectName("muteBtn")
        self.str_mute.setFixedSize(28, 28)
        self.str_mute.setToolTip("Mute in Stream mix")
        self.str_mute.clicked.connect(self._on_str_mute)
        str_col.addWidget(self.str_mute)
        faders_row.addLayout(str_col)

        layout.addLayout(faders_row, 1)

        # Hidden no-op kept so callers that touch `strip.fx_btn.setProperty(...)`
        # don't fail. The real FX entry point is the right-click menu.
        self.fx_btn = QPushButton()
        self.fx_btn.setVisible(False)

    _MIN_W = 120
    _MAX_W = 180
    _MIN_SLIDER_H = 80
    _MAX_SLIDER_H = 140

    def apply_scale(self, width: int, slider_h: int):
        """Resize this strip to `width` px and set slider minimum height."""
        self.setFixedWidth(width)
        t = (width - self._MIN_W) / (self._MAX_W - self._MIN_W)
        t = max(0.0, min(1.0, t))
        margin = max(4, int(4 + t * 4))
        self.layout().setContentsMargins(margin, margin, margin, margin)
        spacing = max(6, int(6 + t * 4))
        self._faders_row.setSpacing(spacing)
        self.mon_slider.setMinimumHeight(slider_h)
        self.str_slider.setMinimumHeight(slider_h)

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
            view = getattr(win, "_runtime_view_state", None)
            health = getattr(view, "health", {}) if view is not None else {}
            node_health = (health or {}).get(self.node_name)
            fx_status = runtime.fx_status_for(self.node_name) if runtime is not None else None
            is_degraded = bool(node_health) or getattr(fx_status, "state", "") == "degraded"
            if is_degraded:
                recover_act = menu.addAction("🛠 Recover Channel")
                recover_act.triggered.connect(self._request_recover)
                if node_health:
                    recover_act.setToolTip(f"Current health error: {node_health}")
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
                "Install Carla (e.g. `sudo pacman -S carla`) to host VST3 / "
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

    def _request_rename(self):
        win = self._main_window()
        if hasattr(win, 'rename_channel') and self.node_name:
            win.rename_channel(self.node_name)

    def on_peak(self, peak_01):
        """Receive a 0..1 peak from the MeterWorker and drive the bar."""
        self.peak_bar.setValue(int(max(0.0, min(peak_01, 1.0)) * 1000))

    def update_from_node(self, mon_vol, mon_mute, str_vol, str_mute, is_hidden):
        """Update strip UI from stored state. `is_hidden` is informational;
        the parent window controls visibility, not this widget."""
        self._mon_muted = mon_mute
        self._str_muted = str_mute
        mon_pct = int(mon_vol * 100)
        str_pct = int(str_vol * 100)
        win = self._main_window()
        is_linked = False
        if hasattr(win, 'submix_state') and self.node_name:
            is_linked = bool(win.submix_state.get(f"{self.node_name}_linked", False))
        state = (mon_pct, bool(mon_mute), str_pct, bool(str_mute), bool(is_linked))
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

        self._apply_mute_style(self.mon_mute, mon_mute)
        self._apply_mute_style(self.str_mute, str_mute)
        self._last_rendered_state = state


# ── App Routing Row ────────────────────────────────────────────────
class AppRoutingRow(QWidget):
    """A row showing an app name and a dropdown to choose which sink it goes to."""

    def __init__(self, app_id, display_name, engine, sinks, main_win=None, parent=None):
        super().__init__(parent)
        self.engine = engine
        self.app_id = app_id
        self.app_name = display_name
        self._main_win = main_win
        self._active_indices = [] # Current sink-input indices for this app
        self._last_active_state = None
        self._last_sink_selection = object()

        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 4, 0, 4)
        layout.setAlignment(Qt.AlignmentFlag.AlignVCenter)

        self.icon_lbl = QLabel("🎵")
        self.icon_lbl.setObjectName("channelIcon")
        self.icon_lbl.setFixedWidth(28)
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

    def _on_vol_change(self, value):
        self._pending_app_vol = value
        self._app_commit_timer.start()

    def _commit_app_vol(self):
        v = self._pending_app_vol
        self._pending_app_vol = None
        if v is None:
            return
        runtime = getattr(self._main_win, "runtime", None)
        if runtime is None:
            return
        for idx in self._active_indices:
            runtime.set_app_volume(idx, v / 100.0)

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

    def update_state(self, display_name, active_indices, sinks, current_sink, current_volume=None):
        self.app_name = display_name or self.app_name
        self._active_indices = active_indices
        is_active = len(active_indices) > 0
        is_system = (self.app_id == PipeWireEngine.SYSTEM_SOUNDS_BUCKET)

        if is_system:
            self.icon_lbl.setText("🔔")
        label_text = (
            f"{self.app_name} (Idle)" if (is_system and not is_active)
            else f"{self.app_name} (Offline)" if not is_active
            else self.app_name
        )
        active_state = (label_text, is_active, is_system)
        if active_state != self._last_active_state:
            self.name_lbl.setText(label_text)
            self.vol_slider.setEnabled(is_active)
            self.vol_lbl.setStyleSheet("" if is_active else "color: #666;")
            self.name_lbl.setStyleSheet("" if is_active else "color: #888;")
            self._last_active_state = active_state

        self.forget_btn.setVisible(not is_system)
        if not is_system:
            self.forget_btn.setEnabled(True)
            self.forget_btn.setToolTip(
                "Permanently hide this app from the routing list. "
                "Drops its saved volume / destination too."
            )

        if is_active:
            vol = current_volume
            if vol is None:
                vol = self.engine.get_sink_input_volume(active_indices[0])
            vol_pct = int(vol * 100)
            if self.vol_slider.value() != vol_pct:
                self.vol_slider.blockSignals(True)
                self.vol_slider.setValue(vol_pct)
                self.vol_slider.blockSignals(False)

        # Combo: hardware sinks + user-created WaveLinux channels (starred).
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
                        display = display_name or f"{pretty} STAR"
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


# ── Main Window ────────────────────────────────────────────────────
class WaveLinuxWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("WaveLinux")
        
        self.resize(1200, 720)
        
        # Set app icon and tray icon.
        assets_dir = os.path.dirname(os.path.abspath(__file__))
        app_icon_path = os.path.join(assets_dir, "icon.png")
        tray_icon_path = os.path.join(assets_dir, "tray_icon.png")

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
        # ── State ──
        self.channel_widgets = {}   # node_id -> ChannelStrip
        self.app_widgets = {}       # app_id -> AppRoutingRow
        self.submix_state = {}      # "node_id_MixName" -> {'vol': 1.0, 'mute': False}
        self.app_routing = {}       # app_id -> sink_name (persistent)
        self.app_last_seen = {}     # app_id -> epoch seconds (for stale prune)
        self.app_display_names = {} # app_id -> last known display label
        self.app_prune_days = 14    # forget routing entries not seen in this many days
        # ✕'d apps. Consulted in `_refresh` BEFORE the row is built so
        # the ✕ button sticks across re-syntheses.
        self.forgotten_apps = set()  # {app_id}
        self.virtual_channels = []   # list of display names
        # Single-mic mode: one mic strip at a time, picked from the master
        # combo. None → resolved to `pactl get-default-source` on first refresh.
        self.selected_mic = None
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
        self.config_path = os.path.expanduser("~/.config/wavelinux/config.json")
        os.makedirs(os.path.dirname(self.config_path), exist_ok=True)
        
        # Backstop poll. `pactl subscribe` drives most refreshes; this
        # only fires when an event was missed.
        self.refresh_timer = QTimer(self)
        self.refresh_timer.timeout.connect(self._refresh)

        # Coalesce rapid save requests (slider drags).
        self._save_timer = QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.timeout.connect(self.save_config)

        # Coalesce rapid refresh requests — pactl-subscribe storms can
        # fire 5+ events per operation.
        self._event_refresh_timer = QTimer(self)
        self._event_refresh_timer.setSingleShot(True)
        self._event_refresh_timer.setInterval(150)
        self._event_refresh_timer.timeout.connect(self._refresh)

        self._setup_ui()
        self._run_startup_preflight()
        self.load_config()
        self._refresh()
        # 5s backstop interval — subscribe-driven refreshes carry the
        # real-time signal; this just catches missed events.
        self.refresh_timer.start(5000)
        self._start_event_subscriber()
        self._pending_update_tag = None
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
        settings_open = (
            getattr(self, "settings_dialog", None)
            and self.settings_dialog.isVisible()
        )
        if settings_open:
            self._refresh_advanced_tab()
        if hidden_to_tray and not settings_open:
            return
        if self._any_slider_dragging():
            return
        self._refresh_runtime_view()

    def _on_runtime_fx_status(self, status):
        if getattr(status, "state", "") == "degraded":
            self.status_lbl.setText(status.message or "FX runtime degraded")

    def _run_startup_preflight(self):
        """Check for required runtime binaries and surface a clear warning."""
        missing = [cmd for cmd in _RUNTIME_DEPS if shutil.which(cmd) is None]
        if not missing:
            return
        msg = (
            "Missing required audio/runtime tools:\n"
            f"  {', '.join(missing)}\n\n"
            "WaveLinux can start, but routing/meter/update features may fail.\n"
            "Install PipeWire + WirePlumber + PulseAudio compatibility tools first."
        )
        logging.warning(msg.replace("\n", " "))
        QMessageBox.warning(self, "WaveLinux dependency check", msg)

    def _open_settings(self):
        self._refresh_hidden_list()
        self._refresh_advanced_tab()
        self.settings_dialog.show()
        self.settings_dialog.raise_()

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

        profiles_btn = QPushButton("Sound Card Profiles…")
        profiles_btn.setObjectName("showHiddenBtn")
        profiles_btn.clicked.connect(self._open_card_profiles)
        layout.addWidget(profiles_btn, alignment=Qt.AlignmentFlag.AlignLeft)

        # LADSPA / diagnostics
        _heading("DIAGNOSTICS")
        probed = len(self.engine.ladspa_plugins)
        ladspa_lbl = QLabel(
            f"LADSPA plugins detected: {probed}\n"
            f"Paths searched: $LADSPA_PATH + /usr/lib/ladspa, /usr/lib64/ladspa, "
            f"/usr/local/lib/ladspa, /usr/lib/x86_64-linux-gnu/ladspa, "
            f"~/.ladspa, ~/.local/lib/ladspa."
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

        ver_lbl = QLabel(f"Current version: <b>{APP_VERSION}</b>")
        ver_lbl.setStyleSheet("color: #e0e0ee; font-size: 13px;")
        layout.addWidget(ver_lbl)

        self._update_status_lbl = QLabel("Click 'Check for Updates' to see if a newer version is available.")
        self._update_status_lbl.setWordWrap(True)
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
        layout.addWidget(self._update_status_lbl)

        self._update_progress = QProgressBar()
        self._update_progress.setRange(0, 100)
        self._update_progress.setValue(0)
        self._update_progress.setVisible(False)
        self._update_progress.setTextVisible(True)
        self._update_progress.setStyleSheet(
            "QProgressBar { background: #1a1a2e; border: 1px solid #3a3a5c;"
            " border-radius: 4px; color: #e0e0ee; text-align: center; }"
            "QProgressBar::chunk { background: #00d4aa; border-radius: 3px; }"
        )
        layout.addWidget(self._update_progress)

        btn_row = QHBoxLayout()
        self._check_update_btn = QPushButton("Check for Updates")
        self._check_update_btn.setObjectName("showHiddenBtn")
        self._check_update_btn.clicked.connect(self._check_for_updates)
        btn_row.addWidget(self._check_update_btn)

        self._apply_update_btn = QPushButton("Download and Apply Update")
        self._apply_update_btn.setObjectName("addBtn")
        self._apply_update_btn.setVisible(False)
        self._apply_update_btn.clicked.connect(self._download_and_apply_update)
        btn_row.addWidget(self._apply_update_btn)

        btn_row.addStretch()
        layout.addLayout(btn_row)

        note = QLabel(
            "Updates replace main.py, pipewire_engine.py, and wavelinux_theme.py.\n"
            "For safety, the release must include sha256sums.txt and all file checksums must match.\n"
            "The old files are backed up with a .bak extension.\n"
            "A restart is required after applying an update."
        )
        note.setWordWrap(True)
        note.setStyleSheet("color: #5a5a72; font-size: 11px;")
        layout.addWidget(note)

        layout.addStretch(1)
        return tab

    def _check_for_updates(self):
        self._check_update_btn.setEnabled(False)
        self._apply_update_btn.setVisible(False)
        self._update_status_lbl.setText("Checking for updates…")
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
        self._update_progress.setVisible(False)

        # Cancel any in-flight checker so we don't end up with two
        # threads racing into the same queue.
        prev = getattr(self, '_updater', None)
        if prev is not None:
            prev.cancel()

        app_dir = os.path.dirname(os.path.abspath(__file__))
        self._updater = UpdateChecker(app_dir=app_dir)
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
            elif kind == 'progress':
                _, filename, done, total = item
                self._handle_update_progress(filename, done, total)
            elif kind == 'cancelled':
                self._update_poll_timer.stop()
                self._handle_update_cancelled()

    def _handle_update_result(self, latest_tag):
        self._check_update_btn.setEnabled(True)
        current = _parse_version(APP_VERSION)
        latest  = _parse_version(latest_tag)
        if latest > current:
            self._update_status_lbl.setText(
                f"Update available: v{latest_tag}  (current: v{APP_VERSION})"
            )
            self._update_status_lbl.setStyleSheet("color: #00d4aa; font-size: 12px; font-weight: bold;")
            self._apply_update_btn.setVisible(True)
            self._pending_update_tag = latest_tag
            self._show_notification(
                "WaveLinux Update Available",
                f"Version {latest_tag} is available. Open Settings → Updates to apply.",
            )
        else:
            self._update_status_lbl.setText(f"You're up to date! (v{APP_VERSION})")
            self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
            self._apply_update_btn.setVisible(False)


    def _handle_update_cancelled(self):
        self._check_update_btn.setEnabled(True)
        self._apply_update_btn.setEnabled(True)
        self._update_progress.setVisible(False)
        self._update_status_lbl.setText("Update cancelled.")
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")

    def _handle_update_error(self, message):
        self._check_update_btn.setEnabled(True)
        self._apply_update_btn.setVisible(False)
        self._update_progress.setVisible(False)
        self._update_status_lbl.setText(f"Error: {message}")
        self._update_status_lbl.setStyleSheet("color: #e05050; font-size: 12px;")

    def _download_and_apply_update(self):
        tag = getattr(self, '_pending_update_tag', None)
        if not tag:
            return
        updater = getattr(self, '_updater', None)
        if updater is None:
            app_dir = os.path.dirname(os.path.abspath(__file__))
            updater = UpdateChecker(app_dir=app_dir)
            self._updater = updater
        self._apply_update_btn.setEnabled(False)
        self._check_update_btn.setEnabled(False)
        self._update_progress.setValue(0)
        self._update_progress.setVisible(True)
        self._update_status_lbl.setText("Downloading update…")
        self._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
        updater.download(tag)
        # Reuse the poll timer for download progress
        if not getattr(self, '_update_poll_timer', None):
            self._update_poll_timer = QTimer(self)
            self._update_poll_timer.setInterval(200)
            self._update_poll_timer.timeout.connect(self._poll_updater)
        self._update_poll_timer.start()

    def _handle_update_progress(self, filename, done, total):
        if filename == "__done__":
            self._update_progress.setValue(100)
            self._update_progress.setVisible(False)
            self._apply_update_btn.setVisible(False)
            self._check_update_btn.setEnabled(True)
            self._update_status_lbl.setText(
                "Update applied! Restart WaveLinux for the new version to take effect."
            )
            self._update_status_lbl.setStyleSheet("color: #00d4aa; font-size: 12px; font-weight: bold;")
            yn = QMessageBox.question(
                self.settings_dialog, "Update Applied",
                "Update applied successfully. Restart WaveLinux now?",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if yn == QMessageBox.StandardButton.Yes:
                self._restart_app()
            return
        pct = int(done * 100 / total) if total > 0 else 0
        self._update_progress.setValue(pct)
        self._update_status_lbl.setText(f"Downloading {os.path.basename(filename)}… {pct}%")

    def _restart_app(self):
        self.save_config()
        self.runtime.cleanup_sync()
        self.runtime.shutdown()
        os.execv(sys.executable, [sys.executable] + sys.argv)

    def _check_for_updates_bg(self):
        """Silent background check 30 s after startup."""
        prev = getattr(self, '_bg_updater', None)
        if prev is not None:
            prev.cancel()

        app_dir = os.path.dirname(os.path.abspath(__file__))
        self._bg_updater = UpdateChecker(app_dir=app_dir)
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
            tag = item[1]
            if _parse_version(tag) > _parse_version(APP_VERSION):
                self._pending_update_tag = tag
                self._show_notification(
                    "WaveLinux Update Available",
                    f"Version {tag} is ready. Open Settings → Updates to apply.",
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
        """One-shot: drop every app_routing entry whose app isn't currently
        making sound. Refreshes the panel immediately."""
        active_ids = {
            app_id for app_id in self.app_widgets
            if self.app_widgets[app_id]._active_indices
        }
        to_forget = [app_id for app_id in list(self.app_routing.keys()) if app_id not in active_ids]
        if not to_forget:
            QMessageBox.information(self.settings_dialog, "Forget offline apps",
                                    "No offline apps to forget.")
            return
        yn = QMessageBox.question(
            self.settings_dialog, "Forget offline apps",
            f"Drop saved routing for {len(to_forget)} offline app(s)?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        for app_id in to_forget:
            self.app_routing.pop(app_id, None)
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
        self._save_timer.start(500)

    def _virtual_channel_specs(self):
        specs = {}
        for display_name in self.virtual_channels:
            clean, safe = PipeWireEngine._sanitize_channel_name(display_name)
            specs[f"wavelinux_{safe}"] = clean
        return specs

    def _sync_runtime_persistent_state(self):
        monitor_hw = self.mon_out_combo.currentData() if hasattr(self, "mon_out_combo") else None
        stream_hw = self.str_out_combo.currentData() if hasattr(self, "str_out_combo") else None
        self.runtime.sync_persistent_state(
            selected_mic=self.selected_mic,
            submix_state=self.submix_state,
            active_effects=self.active_effects,
            effect_params=self.effect_params,
            app_routing=dict(self.app_routing),
            virtual_channels=self._virtual_channel_specs(),
            monitor_hw=monitor_hw,
            stream_hw=stream_hw,
        )

    def _display_name_for_app_id(self, app_id, fallback=None):
        if fallback:
            self.app_display_names[app_id] = fallback
            return fallback
        cached = self.app_display_names.get(app_id)
        if cached:
            return cached
        return PipeWireEngine.display_name_for_app_id(app_id)

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
        hidden_to_tray = self.tray is not None and not self.isVisible()
        settings_open  = getattr(self, 'settings_dialog', None) and self.settings_dialog.isVisible()
        if hidden_to_tray and not settings_open:
            return
        self._event_refresh_timer.start()

    def _on_event_proc_error(self, err):
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

        # Settings dialog — tabbed (Apps / Hidden / Advanced).
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
    _MAX_STRIP_W = 180
    _MIN_SLIDER_H = 80
    _MAX_SLIDER_H = 140

    def eventFilter(self, obj, event):
        if obj is self.inputs_scroll.viewport() and event.type() == QEvent.Type.Resize:
            self._rescale_strips()
        return super().eventFilter(obj, event)

    # Approximate fixed-height overhead in a strip (icon row, name label,
    # peak bar, link row, MON/STR labels, %, mute buttons, margins/spacing)
    # — everything that isn't the slider. Used to back-compute how much
    # vertical room is left for the sliders.
    _STRIP_VERT_OVERHEAD = 170

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
        t = (strip_w - self._MIN_STRIP_W) / (self._MAX_STRIP_W - self._MIN_STRIP_W)
        slider_h_w = int(self._MIN_SLIDER_H + t * (self._MAX_SLIDER_H - self._MIN_SLIDER_H))
        avail_h = self.inputs_scroll.viewport().height()
        slider_h_h = avail_h - self._STRIP_VERT_OVERHEAD
        slider_h = max(self._MIN_SLIDER_H,
                       min(self._MAX_SLIDER_H, slider_h_w, slider_h_h))
        for strip in strips:
            strip.apply_scale(strip_w, slider_h)

    def _refresh(self):
        """Update UI to match PipeWire state without destroying everything.
        Skip when minimised to tray unless the settings dialog is open
        (app routing list needs to stay live when the user has it open)."""
        hidden_to_tray = self.tray is not None and not self.isVisible()
        settings_open  = getattr(self, 'settings_dialog', None) and self.settings_dialog.isVisible()
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

    def _on_mix_out_change(self, mix_name, hw_sink_name):
        self.runtime.set_mix_hardware_route(mix_name, hw_sink_name)
        self.schedule_save()

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
        # Resolve a stale or unset selected_mic (unplugged device, fresh
        # launch) to the system default if available, else the first mic.
        if mics and (not self.selected_mic or self.selected_mic not in mic_names):
            if default_src is None:
                default_src = (
                    self.engine.get_default_source()
                    if hasattr(self.engine, 'get_default_source') else None
                )
            if default_src and default_src in mic_names:
                self.selected_mic = default_src
            else:
                self.selected_mic = mics[0].name
            self.runtime.set_selected_mic(self.selected_mic)
            self.schedule_save()

        mic_fp = tuple((m.name, m.description or '') for m in mics)
        if getattr(self, '_mic_combo_fp', None) != mic_fp:
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
            )
            strip._refresh_fx_indicator(active=getattr(node, "fx_running", False))
            node_health = (getattr(view, "health", {}) or {}).get(nname)
            if node_health:
                strip.setToolTip(
                    f"Runtime health: {node_health}. "
                    "Right-click and select Recover Channel."
                )
            else:
                strip.setToolTip("")

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
            display_name = self._display_name_for_app_id(
                app_id,
                runtime_app.app_name if runtime_app is not None else None,
            )
            preferred_sink = self.app_routing.get(app_id)
            live_sink = runtime_app.current_sink if runtime_app is not None else None
            current_sink = preferred_sink or live_sink
            current_volume = runtime_app.current_volume if runtime_app is not None else None
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
            current_data = combo.currentData()
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
                idx = combo.findData(current_hw)
                if idx < 0:
                    idx = combo.findData(current_data)
                if idx >= 0:
                    combo.setCurrentIndex(idx)
                combo.blockSignals(False)
            elif current_hw != current_data:
                idx = combo.findData(current_hw)
                if idx >= 0 and idx != combo.currentIndex():
                    combo.blockSignals(True)
                    combo.setCurrentIndex(idx)
                    combo.blockSignals(False)

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

    def load_config(self):
        if not os.path.exists(self.config_path):
            # First launch — set up the standard mixes, route Monitor to
            # the system default, and seed starter channels.
            self.runtime.ensure_output_mix_sync("Monitor")
            self.runtime.ensure_output_mix_sync("Stream")
            def_sink = self.engine.get_default_sink()
            if def_sink:
                self._on_mix_out_change("Monitor", def_sink)
            self._seed_default_channels()
            self.save_config()
            return

        try:
            with open(self.config_path, 'r') as f:
                conf = json.load(f)
                self.submix_state = self._migrate_submix_state(conf.get('submixes', {}))
                self.hidden_nodes = self._migrate_hidden_nodes(conf.get('hidden', []))
                self.app_routing = {
                    k: v for k, v in (conf.get('app_routing', {}) or {}).items()
                    if PipeWireEngine.is_persistent_app_id(k)
                }
                self.app_last_seen = {
                    k: int(v) for k, v in (conf.get('app_last_seen', {}) or {}).items()
                    if isinstance(k, str) and isinstance(v, (int, float))
                    and PipeWireEngine.is_persistent_app_id(k)
                }
                self.app_display_names = {
                    k: v for k, v in (conf.get('app_display_names', {}) or {}).items()
                    if isinstance(k, str) and isinstance(v, str) and PipeWireEngine.is_persistent_app_id(k)
                }
                self.app_prune_days = int(conf.get('app_prune_days', self.app_prune_days) or 14)
                # Persistent ✕ blocklist. Set for O(1) membership.
                self.forgotten_apps = {
                    name for name in (conf.get('forgotten_apps', []) or [])
                    if isinstance(name, str) and PipeWireEngine.is_persistent_app_id(name)
                }
                # Purge host-named ghosts from older configs whose engine
                # filter didn't normalise whitespace.
                for name in list(self.app_routing.keys()):
                    if PipeWireEngine.name_matches_host(name):
                        self.app_routing.pop(name, None)
                for name in list(self.app_last_seen.keys()):
                    if PipeWireEngine.name_matches_host(name):
                        self.app_last_seen.pop(name, None)
                        self.app_display_names.pop(name, None)
                for app_id in set(self.app_routing) | set(self.app_last_seen) | set(self.forgotten_apps):
                    self.app_display_names.setdefault(
                        app_id,
                        PipeWireEngine.display_name_for_app_id(app_id),
                    )
                self._prune_stale_apps()
                self.virtual_channels = conf.get('channels', [])
                self.channel_order = conf.get('channel_order', []) or []
                # Single-mic mode: which mic the user picked in the master
                # "Microphone Input" combo. None means "use system default";
                # the refresh path resolves None to `pactl get-default-source`
                # the first time it runs.
                self.selected_mic = conf.get('selected_mic') or None
                self.runtime.set_selected_mic(self.selected_mic)
                self.effect_params = conf.get('effect_params', {}) or {}
                self.active_effects = {
                    k: list(v) for k, v in (conf.get('active_effects', {}) or {}).items()
                    if isinstance(v, list)
                }

                # Saved FX intent is reconciled by the runtime controller
                # worker against the observed graph.

                # Migrate compressor keys from pre-rewrite snake_case
                # (`threshold_db` etc.) to sc4m's real LADSPA port names.
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

                # Create standard mixes (always needed)
                self.runtime.ensure_output_mix_sync("Monitor")
                self.runtime.ensure_output_mix_sync("Stream")

                # Legacy master-bus Clipguard → per-mic limiter migration.
                # `selected_mic` may not be resolved yet here, so we just
                # stash the intent and `_refresh` applies it once the
                # picker has landed.
                self._pending_clipguard_migration = bool(conf.get('clipguard'))
                if self._pending_clipguard_migration and self.selected_mic:
                    self._apply_pending_clipguard_migration()

                # Recreate virtual channels
                for name in self.virtual_channels:
                    self.runtime.ensure_virtual_channel_sync(name)
                    
                # Restore output mix hardware routing. Block the combo signals
                # so setCurrentIndex doesn't also trigger the change handler —
                # we call it explicitly exactly once.
                mon_hw = conf.get('monitor_hw') or self.engine.get_default_sink()
                str_hw = conf.get('stream_hw')
                for combo, mix_name, hw in (
                    (self.mon_out_combo, "Monitor", mon_hw),
                    (self.str_out_combo, "Stream", str_hw),
                ):
                    if not hw:
                        continue
                    combo.blockSignals(True)
                    idx = combo.findData(hw)
                    if idx >= 0:
                        combo.setCurrentIndex(idx)
                    combo.blockSignals(False)
                    self._on_mix_out_change(mix_name, hw)
                self._sync_runtime_persistent_state()

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
        conf = {
            'schema_version': 1,
            'monitor_hw': self.mon_out_combo.currentData(),
            'stream_hw': self.str_out_combo.currentData(),
            'channels': self.virtual_channels,
            'selected_mic': self.selected_mic,
            'submixes': self.submix_state,
            'hidden': list(self.hidden_nodes),
            'app_routing': {k: v for k, v in self.app_routing.items()
                            if PipeWireEngine.is_persistent_app_id(k)},
            'channel_order': self.channel_order,
            'effect_params': self.effect_params,
            'active_effects': self.active_effects,
            'app_last_seen': {k: v for k, v in self.app_last_seen.items()
                              if PipeWireEngine.is_persistent_app_id(k)},
            'app_display_names': {
                k: v for k, v in self.app_display_names.items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            'app_prune_days': self.app_prune_days,
            'forgotten_apps': sorted(
                name for name in self.forgotten_apps
                if PipeWireEngine.is_persistent_app_id(name)
            ),
        }
        try:
            tmp = self.config_path + '.tmp'
            with open(tmp, 'w') as f:
                json.dump(conf, f, indent=4)
            os.replace(tmp, self.config_path)
        except Exception as e:
            logging.error(f"Error saving config: {e}")



    def forget_app(self, app_id):
        """Drop all state for an app and add it to the persistent
        `forgotten_apps` blocklist. Recover via Settings → Advanced →
        'Restore forgotten apps' or by editing config.json."""
        # System Sounds is a permanent built-in entry — never let it
        # land in the blocklist, even if something calls forget on it.
        if app_id == PipeWireEngine.SYSTEM_SOUNDS_BUCKET:
            return
        self.app_routing.pop(app_id, None)
        self.app_last_seen.pop(app_id, None)
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
            name for name in list(self.app_routing.keys())
            if self.app_last_seen.get(name, 0) < cutoff
        ]
        for name in stale_routed:
            self.app_routing.pop(name, None)
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
            if name not in self.app_routing and name not in self.forgotten_apps:
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
        return os.path.expanduser("~/.config/autostart/wavelinux.desktop")

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
        main_path = os.path.abspath(__file__)
        contents = (
            "[Desktop Entry]\n"
            "Type=Application\n"
            "Name=WaveLinux\n"
            f"Exec=python3 {main_path}\n"
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
        self.runtime.full_audio_reset_sync()
        self.runtime.shutdown()
        logging.info("Audio reset complete. Exiting.")
        QApplication.instance().quit()

    def _cleanup_before_exit(self):
        self.runtime.cleanup_sync()


# ── Entry Point ────────────────────────────────────────────────────
def main():
    app = QApplication(sys.argv)
    app.setApplicationName("WaveLinux")
    app.setStyleSheet(STYLESHEET)

    # Try to use a nice font
    font = QFont("Inter", 10)
    app.setFont(font)

    # Single instance lock
    lock_path = os.path.join(os.path.expanduser("~"), ".wavelinux.lock")
    lock_file = QLockFile(lock_path)
    if not lock_file.tryLock(100):
        print("WaveLinux is already running.")
        sys.exit(0)

    window = WaveLinuxWindow()
    window.show()

    # Ensure cleanup on standard application quit
    app.aboutToQuit.connect(window._cleanup_before_exit)

    sys.exit(app.exec())


if __name__ == '__main__':
    main()
