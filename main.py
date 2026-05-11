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

from pipewire_engine import PipeWireEngine
from wavelinux_theme import STYLESHEET

import struct

APP_VERSION = "1.1.0"
_GITHUB_OWNER = "excalprimeacct-gif"
_GITHUB_REPO  = "WaveLinux"
_UPDATE_FILES = ["main.py", "pipewire_engine.py", "wavelinux_theme.py"]
_RUNTIME_DEPS = ["pactl", "pw-dump", "wpctl", "parec", "pipewire"]


# ── In-app updater ───────────────────────────────────────────────
def _parse_version(v):
    """Return a comparable tuple from a semver string like '1.2.3' or 'v1.2.3'."""
    v = v.lstrip('v').strip()
    try:
        return tuple(int(x) for x in v.split('.'))
    except ValueError:
        return (0,)


class UpdateChecker:
    """Runs version checks and downloads on a daemon thread.

    Results are put into a SimpleQueue and retrieved by the Qt main
    thread via poll(). This avoids QTimer.singleShot from non-Qt threads
    which is unreliable in PyQt6.

    Queue items are tuples: ('result', tag) | ('error', msg) | ('progress', filename, done, total)
    """

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

    # ── internals ──────────────────────────────────────────────────

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


# ── Per-channel peak meter (VU) ───────────────────────────────────
class MeterWorker(QObject):
    """Spawns `parec` against a source and emits a normalized peak
    (0.0..1.0) every ~50 ms. One worker per visible channel strip.
    The process dies cleanly on .stop()."""

    peak = pyqtSignal(float)

    def __init__(self, source_name, parent=None):
        super().__init__(parent)
        self.source_name = source_name
        self._proc = None
        self._buf = bytearray()
        # ~40 Hz updates (25 ms window). Smooth enough for a proper VU feel
        # without spamming the UI; parec's own chunking keeps this cheap.
        self._sample_bytes = 48000 * 2 // 40
        self._last_peak = 0.0

    def start(self):
        if self._proc is not None:
            return
        self._proc = QProcess(self)
        self._proc.setProcessChannelMode(QProcess.ProcessChannelMode.SeparateChannels)
        self._proc.readyReadStandardOutput.connect(self._on_bytes)
        args = [
            f"--device={self.source_name}",
            "--rate=48000",
            "--format=s16le",
            "--channels=1",
            "--raw",
            "--latency-msec=25",
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

    def __init__(self, engine, parent=None):
        super().__init__(parent)
        self.engine = engine
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
        except Exception:
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
                threading.Thread(
                    target=self.engine.set_card_profile,
                    args=(card_name, target),
                    daemon=True,
                ).start()


# ── FX Selection Dialog ───────────────────────────────────────────
class FXSelectionDialog(QDialog):
    def __init__(self, node_id, node_name, capture_target, engine, parent=None):
        super().__init__(parent)
        self.node_id = str(node_id)
        self.node_name = node_name
        # The PipeWire source name the FX chain's first stage should pull
        # from. For mics that's the mic's own node.name; for virtual sinks
        # it's `<sink>.monitor`. Determined by the channel strip and passed
        # in so the engine doesn't need to guess about media class.
        self.capture_target = capture_target
        self.engine = engine
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
        self._saved_effects_list = list(
            win.active_effects.get(self.node_name, []) if win else []
        )
        self._saved_effects = set(self._saved_effects_list)


        # Background worker for spawning filter chains without blocking UI
        self._fx_queue = queue.SimpleQueue()
        self._fx_poll = QTimer(self)
        self._fx_poll.setInterval(30)
        self._fx_poll.timeout.connect(self._poll_fx_queue)
        self._fx_poll.start()
        self._fx_thread = None
        self._fx_next_job = None

        # Re-spawn from saved state if the chain isn't running, so the
        # dialog reflects what's actually audible.
        if self._saved_effects_list and not self.engine.is_channel_fx_running(self.node_name):
            params = (win.effect_params.get(self.node_name, {})
                      if win else {})
            self._start_fx_worker(list(self._saved_effects_list), params)

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

        effects = self.engine.get_available_effects()
        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.Shape.NoFrame)
        inner = QWidget()
        scroll_layout = QVBoxLayout(inner)
        scroll_layout.setSpacing(10)

        for fx in effects:
            scroll_layout.addWidget(self._build_effect_card(fx))

        scroll_layout.addStretch()
        scroll.setWidget(inner)
        root.addWidget(scroll, 1)

        close_btn = QPushButton("Apply")
        close_btn.setObjectName("addBtn")
        close_btn.clicked.connect(self._on_done)
        root.addWidget(close_btn)

        # Mark any failed-to-start toggles with red border + log tooltip.
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
        # Closing the dialog tears down `_fx_poll`, so any 'done' the
        # bg thread posts after this point is dropped. Kick the main
        # window's reroute timer ourselves — its 150ms debounce
        # combined with the engine's `is_fx_rebuilding()` gate means
        # the next refresh fires once the chain is up, even without
        # `_poll_fx_queue` ever firing again.
        win = self._main_window()
        if win is not None and hasattr(win, '_request_reroute'):
            win._request_reroute(self.node_name)
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
        # Brand-colour the toggle when ON.
        toggle_btn.setStyleSheet(
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
        # Saved intent wins over live state — if the chain isn't running,
        # the toggle still shows the user's last ON/OFF choice.
        active = (
            fid in self._saved_effects
            or self.engine.is_channel_effect_active(self.node_name, fid)
        )
        toggle_btn.setChecked(active)
        toggle_btn.setFixedWidth(60)

        available = self.engine.effect_available(fid)
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
        params = self.engine.get_effect_params(fid)
        help_text = self.engine.get_effect_help(fid)
        presets = self.engine.get_effect_presets(fid)

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
        """Look up the parent window's saved effect params for this node."""
        p = self.parent()
        while p is not None and not hasattr(p, 'effect_params'):
            p = p.parent()
        if p is None:
            return {}
        ep = p.effect_params.get(self.node_name, {}).get(effect_id, {})
        return dict(ep)

    def _main_window(self):
        """Walk up to the WaveLinuxWindow (only place that has effect_params)."""
        p = self.parent()
        while p is not None and not hasattr(p, 'effect_params'):
            p = p.parent()
        return p

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
        for fx in self.engine.get_available_effects():
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
        wanted = self._active_effect_ids()
        params_map = self._all_params_map()
        
        # Persist on the main-window state objects so a restart re-applies.
        self._save_chain_state(wanted, params_map)
        
        self._start_fx_worker(wanted, params_map)

    def _start_fx_worker(self, wanted, params_map):
        if self._fx_thread and self._fx_thread.is_alive():
            self._fx_next_job = (wanted, params_map)
            return
            
        self._fx_thread = threading.Thread(
            target=self._fx_bg_worker, 
            args=(wanted, params_map),
            daemon=True
        )
        self._fx_thread.start()

    def _fx_bg_worker(self, wanted, params_map):
        try:
            self.engine.set_channel_fx(
                self.node_name, self.capture_target, wanted, params_map
            )
            self._fx_queue.put(('done', None))
        except Exception as e:
            logging.exception(f"FX rebuild failed for {self.node_name}: {e}")
            self._fx_queue.put(('error', str(e)))
        
    def _poll_fx_queue(self):
        try:
            kind, message = self._fx_queue.get_nowait()
        except Exception:
            return

        if kind == 'error':
            QMessageBox.warning(
                self,
                "Effects rebuild failed",
                "WaveLinux could not apply one or more effects.\n\n"
                f"Error: {message}\n\n"
                "Check ~/.config/wavelinux/fx-logs for stage logs."
            )

        # Force a routing pass so the loopbacks pick up the new source.
        win = self._main_window()
        if win is not None and hasattr(win, '_request_reroute'):
            win._request_reroute(self.node_name)
        # Failed stages get a red border + log-path tooltip.
        self._refresh_toggle_status()

        if self._fx_next_job:
            w, p = self._fx_next_job
            self._fx_next_job = None
            self._start_fx_worker(w, p)

    def _refresh_toggle_status(self):
        """Annotate each toggle with the live chain state (running /
        failed / inactive). Failed stages get a red border + tooltip
        with the log path."""
        if not hasattr(self, 'engine'):
            return
        status = self.engine.fx_chain_status(self.node_name)
        for fid, btn in self._toggle_btns.items():
            if not btn.isEnabled():
                continue  # N/A toggles already have their own treatment.
            info = status.get(fid, {'state': 'inactive', 'log': None})
            state = info.get('state')
            log_path = info.get('log')
            if state == 'failed':
                btn.setStyleSheet(
                    "QPushButton[role=\"fxToggle\"] {"
                    " background: #2a1a22; color: #ff6a8a;"
                    " border: 1px solid #ff6a8a;"
                    " border-radius: 6px; font-weight: bold; padding: 4px 0; }"
                )
                tip = "FX stage failed to start."
                if log_path:
                    tip += f"\nSee {log_path}"
                btn.setToolTip(tip)
            else:
                btn.setStyleSheet(
                    "QPushButton[role=\"fxToggle\"] {"
                    " background: #1a1a28; color: #8b8b9e;"
                    " border: 1px solid rgba(255,255,255,0.12);"
                    " border-radius: 6px; font-weight: bold; padding: 4px 0; }"
                    "QPushButton[role=\"fxToggle\"]:hover {"
                    " border-color: rgba(0,229,255,0.4); }"
                    "QPushButton[role=\"fxToggle\"]:checked {"
                    " background: #00e5ff; color: #0d0d14;"
                    " border-color: #00e5ff; }"
                )
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
        win = self.window()
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
        win = self.window()
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
        self.engine.set_submix_volume(self.node_id, "Monitor", v / 100.0)
        self._stash_submix("Monitor", v / 100.0, self._mon_muted)

    def _commit_str_vol(self):
        v = self._pending_str_vol
        self._pending_str_vol = None
        if v is None or not self.node_id:
            return
        self.engine.set_submix_volume(self.node_id, "Stream", v / 100.0)
        self._stash_submix("Stream", v / 100.0, self._str_muted)

    def _apply_mute_style(self, btn, muted):
        if btn == self.mon_mute:
            icon = "🎧"
        else:
            icon = "📡"
        btn.setText("🔇" if muted else icon)
        btn.setProperty("muted", "true" if muted else "false")
        btn.style().unpolish(btn)
        btn.style().polish(btn)

    def _on_mon_mute(self):
        self._mon_muted = not self._mon_muted
        if self.node_id:
            self.engine.set_submix_mute(self.node_id, "Monitor", self._mon_muted)
            self._stash_submix("Monitor", self.mon_slider.value() / 100.0, self._mon_muted)
        self._apply_mute_style(self.mon_mute, self._mon_muted)

    def _on_str_mute(self):
        self._str_muted = not self._str_muted
        if self.node_id:
            self.engine.set_submix_mute(self.node_id, "Stream", self._str_muted)
            self._stash_submix("Stream", self.str_slider.value() / 100.0, self._str_muted)
        self._apply_mute_style(self.str_mute, self._str_muted)

    def fx_capture_target(self):
        """Source the FX chain's first stage pulls from. Mics → mic
        node.name; virtual sinks → `<sink>.monitor`."""
        if self.is_mic:
            return self.node_name
        return f"{self.node_name}.monitor"

    def _on_fx_toggle(self):
        """Open the effects dialog and refresh the ✨ indicator."""
        dlg = FXSelectionDialog(
            self.node_id, self.node_name, self.fx_capture_target(),
            self.engine, self,
        )
        dlg.exec()
        self._refresh_fx_indicator()

    def _refresh_fx_indicator(self):
        if not self.node_name:
            self.fx_indicator.setVisible(False)
            return
        self.fx_indicator.setVisible(self.engine.is_channel_fx_running(self.node_name))

    def _show_context_menu(self, pos):
        """Right-click menu for channel-strip actions."""
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
        win = self.window()
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
        win = self.window()
        if hasattr(win, 'hide_node') and self.node_name:
            win.hide_node(self.node_name)

    def _request_unhide(self):
        """Request parent window to unhide this channel."""
        win = self.window()
        if hasattr(win, 'unhide_node') and self.node_name:
            win.unhide_node(self.node_name)

    def _request_move(self, delta):
        win = self.window()
        if hasattr(win, 'move_channel') and self.node_name:
            win.move_channel(self.node_name, delta)

    def _request_rename(self):
        win = self.window()
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

        self.mon_slider.blockSignals(True)
        self.str_slider.blockSignals(True)
        self.mon_slider.setValue(int(mon_vol * 100))
        self.str_slider.setValue(int(str_vol * 100))
        self.mon_slider.blockSignals(False)
        self.str_slider.blockSignals(False)

        win = self.window()
        is_linked = False
        if hasattr(win, 'submix_state') and self.node_name:
            is_linked = bool(win.submix_state.get(f"{self.node_name}_linked", False))

        self.link_btn.blockSignals(True)
        self.link_btn.setChecked(is_linked)
        self.link_btn.blockSignals(False)

        self.mon_vol_lbl.setText(f"{int(mon_vol * 100)}%")
        self.str_vol_lbl.setText(f"{int(str_vol * 100)}%")

        self._apply_mute_style(self.mon_mute, mon_mute)
        self._apply_mute_style(self.str_mute, str_mute)


# ── App Routing Row ────────────────────────────────────────────────
class AppRoutingRow(QWidget):
    """A row showing an app name and a dropdown to choose which sink it goes to."""

    def __init__(self, app_name, engine, sinks, main_win=None, parent=None):
        super().__init__(parent)
        self.engine = engine
        self.app_name = app_name
        self._main_win = main_win
        self._active_indices = [] # Current sink-input indices for this app

        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 4, 0, 4)
        layout.setAlignment(Qt.AlignmentFlag.AlignVCenter)

        self.icon_lbl = QLabel("🎵")
        self.icon_lbl.setObjectName("channelIcon")
        self.icon_lbl.setFixedWidth(28)
        self.icon_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(self.icon_lbl)
        
        self.name_lbl = QLabel(app_name)
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

        self.update_state([], sinks, None)

    def _on_vol_change(self, value):
        self._pending_app_vol = value
        self._app_commit_timer.start()

    def _commit_app_vol(self):
        v = self._pending_app_vol
        self._pending_app_vol = None
        if v is None:
            return
        for idx in self._active_indices:
            self.engine.set_sink_input_volume(idx, v / 100.0)

    def _on_route_change(self, idx):
        sink_name = self.combo.itemData(idx)
        for app_idx in self._active_indices:
            self.engine.move_app_to_sink(app_idx, sink_name)
        win = self._main_win
        if win is not None:
            win.app_routing[self.app_name] = sink_name
            win.save_config()

    def _on_forget(self):
        """Permanently remove this app from the routing list."""
        win = self._main_win
        if win is not None:
            win.forget_app(self.app_name)

    def update_state(self, active_indices, sinks, current_sink):
        self._active_indices = active_indices
        is_active = len(active_indices) > 0
        is_system = (self.app_name == "System Sounds")

        if is_system:
            self.icon_lbl.setText("🔔")
        if not is_active:
            self.name_lbl.setText(f"{self.app_name} (Idle)" if is_system
                                  else f"{self.app_name} (Offline)")
            self.vol_slider.setEnabled(False)
            self.vol_lbl.setStyleSheet("color: #666;")
            self.name_lbl.setStyleSheet("color: #888;")
        else:
            self.name_lbl.setText(self.app_name)
            self.vol_slider.setEnabled(True)
            self.vol_lbl.setStyleSheet("")
            self.name_lbl.setStyleSheet("")

            vol = self.engine.get_sink_input_volume(active_indices[0])
            self.vol_slider.blockSignals(True)
            self.vol_slider.setValue(int(vol * 100))
            self.vol_slider.blockSignals(False)

        # System Sounds is a permanent fixture — hide its ✕ button.
        if is_system:
            self.forget_btn.setVisible(False)
        else:
            self.forget_btn.setVisible(True)
            self.forget_btn.setEnabled(True)
            self.forget_btn.setToolTip(
                "Permanently hide this app from the routing list. "
                "Drops its saved volume / destination too."
            )

        # Combo: hardware sinks + user-created WaveLinux channels (starred).
        # Internal mix/source nodes stay hidden. Rebuild only when the
        # sink list actually changes (cached via a fingerprint).
        if not self.combo.view().isVisible():
            sink_fp = tuple(s['name'] for s in sinks)
            if getattr(self, '_combo_sink_fp', None) != sink_fp:
                self._combo_sink_fp = sink_fp
                self.combo.blockSignals(True)
                curr_data = self.combo.currentData()
                self.combo.clear()
                self.combo.addItem("System Default", None)
                for s in sinks:
                    name = s['name']
                    if name.startswith('wavelinux_mix_') or name.startswith('wavelinux_src_'):
                        continue
                    if name.endswith('.monitor'):
                        continue
                    if name.startswith('wavelinux_'):
                        pretty = name.replace('wavelinux_', '').replace('_', ' ').title()
                        display = f"{pretty} ⭐"
                    else:
                        display = self.engine.display_name_for_sink(name)
                    self.combo.addItem(display, name)

                idx = self.combo.findData(current_sink or curr_data)
                if idx >= 0:
                    self.combo.setCurrentIndex(idx)
                self.combo.blockSignals(False)
            elif current_sink is not None:
                # Sink list unchanged but the current selection may have
                # — sync the combobox cheaply.
                idx = self.combo.findData(current_sink)
                if idx >= 0 and idx != self.combo.currentIndex():
                    self.combo.blockSignals(True)
                    self.combo.setCurrentIndex(idx)
                    self.combo.blockSignals(False)


# ── Main Window ────────────────────────────────────────────────────
class WaveLinuxWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("WaveLinux")
        
        self.resize(1200, 720)
        
        # Set app icon (using new perfectly centered icon.png)
        icon_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icon.png")
        if os.path.exists(icon_path):
            app_icon = QIcon(icon_path)
            self.setWindowIcon(app_icon)
            QApplication.instance().setWindowIcon(app_icon)
            self.tray_icon_obj = app_icon
        else:
            self.tray_icon_obj = QIcon.fromTheme("audio-card")
        
        self.engine = PipeWireEngine()
        # ── State ──
        self.channel_widgets = {}   # node_id -> ChannelStrip
        self.app_widgets = {}       # app_index -> AppRoutingRow
        self.submix_state = {}      # "node_id_MixName" -> {'vol': 1.0, 'mute': False}
        self.app_routing = {}       # app_name -> sink_name (persistent)
        self.app_last_seen = {}     # app_name -> epoch seconds (for stale prune)
        self.app_prune_days = 14    # forget routing entries not seen in this many days
        # ✕'d apps. Consulted in `_refresh` BEFORE the row is built so
        # the ✕ button sticks across re-syntheses.
        self.forgotten_apps = set()  # {app_name}
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
        self._effects_applied = set()  # nodes whose chain we've reconciled this session
        # Submix loopback module ids per channel. Used to detect rebuilds
        # (module id changes) and re-push saved volume/mute state.
        self._synced_submix_owners = {}  # node.name -> {"Monitor": id, "Stream": id}
        self.channel_order = []        # [node.name, ...] — persistent UI order
        self.meters = {}               # pw_id -> MeterWorker
        self._known_node_names = set() # for hot-plug detection
        # Sink-input indices we've already auto-routed to a saved sink.
        # Tracks per-stream so we don't re-fight a manual move via
        # pavucontrol; cleared when the index disappears.
        self._auto_routed_indices = set()
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
        self.engine.cleanup()
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
        active_names = {name for name in self.app_widgets
                        if self.app_widgets[name]._active_indices}
        to_forget = [n for n in list(self.app_routing.keys()) if n not in active_names]
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
        for name in to_forget:
            self.app_routing.pop(name, None)
            self.app_last_seen.pop(name, None)
            self.forgotten_apps.add(name)
            widget = self.app_widgets.pop(name, None)
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
            self.engine.full_audio_reset()
            self._effects_applied.clear()
            self.load_config()
            self._refresh()

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

    def _request_reroute(self, node_name):
        """Kick the debounced refresh so submix loopbacks pick up a
        rebuilt FX chain immediately. Saved volume/mute re-syncs
        automatically via `_synced_submix_owners`."""
        self._event_refresh_timer.start()

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
        """Any subscribe event kicks the debounce. Refresh body is
        cheap under the snapshot cache — no need to filter here."""
        try:
            _ = bytes(self._event_proc.readAllStandardOutput())
        except Exception:
            pass
        hidden_to_tray = self.tray is not None and not self.isVisible()
        settings_open  = getattr(self, 'settings_dialog', None) and self.settings_dialog.isVisible()
        if hidden_to_tray and not settings_open:
            return
        self._event_refresh_timer.start()

    def _on_event_proc_error(self, err):
        logging.warning(f"pactl subscribe error: {err}")

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

        # Defer refresh while a background FX rebuild is in flight.
        # Otherwise we'd observe channel_fx, submix_sources, and pactl
        # routing in a half-mutated state and unload the working submix
        # loopback — exactly the "monitor stops outputting after FX
        # apply" symptom.
        if self.engine.is_fx_rebuilding():
            self._event_refresh_timer.start()
            return

        try:
            snap = self.engine.create_snapshot()
            mics = self.engine.get_hardware_inputs(snap=snap)
            vsinks = self.engine.get_virtual_sinks(snap=snap)
            apps = self.engine.get_sink_inputs(snap=snap)
            all_sinks = self.engine.get_all_sinks(snap=snap)

            # Empty pw-dump usually means PipeWire is mid-restart — keep
            # widget state and let the next tick reconcile.
            if not snap.nodes and not all_sinks:
                self.status_lbl.setText("PipeWire error — is pipewire running?")
                return

            live_by_owner = self.engine.snapshot_sink_inputs_by_owner(snap=snap)

            # Hot-plug detection: which user-visible devices are new / gone
            # since the last tick? Only the mic/virtual-sink set matters — we
            # ignore transient app streams.
            present_names = {n.name for n in (mics + vsinks)}
            if self._known_node_names:
                added = present_names - self._known_node_names
                removed = self._known_node_names - present_names
                if added:
                    self._notify_hotplug(added, added=True)
                if removed:
                    self._notify_hotplug(removed, added=False)
            self._known_node_names = present_names

            # 1. Update Input Channels (Mics & Virtual Sinks)
            current_node_ids = set()

            # Single-mic mode: only the picked mic gets a strip; the
            # others stay listed in the master combo for switching.
            self._sync_mic_picker(mics)
            # Apply any pending clipguard→limiter migration once
            # selected_mic has resolved.
            if getattr(self, '_pending_clipguard_migration', False) \
                    and self.selected_mic:
                self._apply_pending_clipguard_migration()
            selected_mic_node = next(
                (m for m in mics if m.name == self.selected_mic), None
            )
            shown_inputs = []
            if selected_mic_node is not None:
                shown_inputs.append(selected_mic_node)
            shown_inputs.extend(vsinks)

            # Sort by the persistent channel order so reorder arrows stick.
            # Unseen nodes go to the end (keeps hot-plugged devices visible).
            order_index = {nm: i for i, nm in enumerate(self.channel_order)}
            sorted_nodes = sorted(
                shown_inputs,
                key=lambda n: order_index.get(n.name, len(order_index) + 1),
            )

            # The reaper at the end of this loop drops widgets whose
            # pw_id isn't in `current_node_ids` — that's how unselected
            # mics get hidden.
            for node in sorted_nodes:
                pw_id = node.pw_id
                current_node_ids.add(pw_id)
                nname = node.name

                is_hidden = nname in self.hidden_nodes
                if is_hidden and not self.show_hidden:
                    if pw_id in self.channel_widgets:
                        self.channel_widgets[pw_id].hide()
                    # Stop the parec subprocess driving the meter on a
                    # hidden strip — otherwise it keeps reading from the
                    # source forever (one parec per hidden mic) and the
                    # reaper at the end of this loop won't catch it
                    # because pw_id is still in current_node_ids.
                    meter = self.meters.pop(pw_id, None)
                    if meter is not None:
                        meter.stop()
                    continue

                # First-time defaults. Mics start MUTED in Monitor so a
                # fresh install doesn't immediately scream the user's
                # voice into their headphones (feedback risk on a
                # multi-monitor setup). Stream stays unmuted because
                # that's where the recording goes. Computed BEFORE the
                # routing calls so we can hand the saved state to the
                # engine — a fresh module-loopback's pulse-bridge default
                # is unmuted, and without an immediate state push the
                # mic leaks into Monitor for the gap before sync runs.
                mon_key = f"{nname}_Monitor"
                str_key = f"{nname}_Stream"
                fresh_mon = mon_key not in self.submix_state
                fresh_str = str_key not in self.submix_state
                is_mic_node = node in mics
                mon_default = {'vol': 1.0, 'mute': True} if is_mic_node else {'vol': 1.0, 'mute': False}
                str_default = {'vol': 1.0, 'mute': False}
                mon_state = dict(self.submix_state.get(mon_key, mon_default))
                str_state = dict(self.submix_state.get(str_key, str_default))
                if fresh_mon:
                    self.submix_state[mon_key] = dict(mon_state)
                if fresh_str:
                    self.submix_state[str_key] = dict(str_state)
                if fresh_mon or fresh_str:
                    self.schedule_save()

                # Create submix routes if they don't exist. Pass the snapshot
                # so we don't re-run `pactl list modules` four times per tick.
                self.engine.route_input_to_submix(
                    pw_id, nname, node.media_class, "Monitor",
                    snap=snap, initial_state=mon_state,
                )
                self.engine.route_input_to_submix(
                    pw_id, nname, node.media_class, "Stream",
                    snap=snap, initial_state=str_state,
                )

                # Push saved volume/mute through to PipeWire whenever
                # the underlying loopback module id changes (first tick,
                # FX toggle rebuild). Without this, enabling effects on
                # a muted mic would silently un-mute it.
                owner_mon = self.engine.submix_loopbacks.get(f"{pw_id}->Monitor")
                owner_str = self.engine.submix_loopbacks.get(f"{pw_id}->Stream")
                last = self._synced_submix_owners.get(nname, {})
                needs_sync = (
                    owner_mon is not None and owner_str is not None
                    and (last.get("Monitor") != owner_mon
                         or last.get("Stream") != owner_str)
                )

                if needs_sync:
                    # set_submix_volume / set_submix_mute return False if
                    # the sink-input isn't catalogued yet. Stamp the
                    # owner only when every push lands so a transient
                    # miss retries on the next tick.
                    pushed = (
                        self.engine.set_submix_volume(pw_id, "Monitor", mon_state['vol'])
                        and self.engine.set_submix_mute(pw_id, "Monitor", mon_state.get('mute', False))
                        and self.engine.set_submix_volume(pw_id, "Stream", str_state['vol'])
                        and self.engine.set_submix_mute(pw_id, "Stream", str_state.get('mute', False))
                    )
                    if pushed:
                        self._synced_submix_owners[nname] = {
                            "Monitor": owner_mon, "Stream": owner_str,
                        }
                else:
                    # Overlay live state into the UI so external mutes
                    # (pavucontrol, media keys) are visible. Persist
                    # only for real mics — virtual sinks own their own
                    # state, persisting transient PipeWire state on them
                    # caused the "audio randomly stops after BT profile
                    # change" bug.
                    is_real_mic = node in mics
                    overlay_changed = False
                    for mix_name, state, mix_key in (
                        ("Monitor", mon_state, mon_key),
                        ("Stream",  str_state, str_key),
                    ):
                        owner = self.engine.submix_loopbacks.get(f"{pw_id}->{mix_name}")
                        if owner is None:
                            continue
                        live = live_by_owner.get(str(owner))
                        if live is None:
                            continue
                        live_vol, live_mute = live
                        # Always reflect live state in the UI so external
                        # changes are visible.
                        state['vol'], state['mute'] = live_vol, live_mute
                        # Only persist live state for real mics — virtual
                        # sinks own their own state.
                        if not is_real_mic:
                            continue
                        saved = self.submix_state.get(mix_key) or {}
                        if (saved.get('vol') != live_vol
                                or bool(saved.get('mute', False)) != bool(live_mute)):
                            self.submix_state[mix_key] = {'vol': live_vol, 'mute': live_mute}
                            overlay_changed = True
                    if overlay_changed:
                        self.schedule_save()

                if pw_id not in self.channel_widgets:
                    if node in mics:
                        label = PipeWireEngine.friendly_name(node.description)
                        ch_type = "Microphone"
                        icon = "🎤"
                    else:
                        safe_name = nname.replace('wavelinux_', '')
                        label = safe_name.replace('_', ' ').title()
                        ch_type = "Virtual"
                        icon = "🎵"

                    strip = ChannelStrip(pw_id, nname, label, ch_type, icon, self.engine)
                    self.channel_widgets[pw_id] = strip
                    self.input_layout.addWidget(strip)

                strip = self.channel_widgets[pw_id]
                # Refresh pw_id — node.name is stable across PW restarts
                # but the numeric id is not.
                strip.node_id = pw_id
                strip.show()
                gain = self.submix_state.get(f"{nname}_gain", 1.0)
                if isinstance(gain, dict):  # legacy pw_id-keyed dict
                    gain = 1.0
                strip.update_from_node(
                    mon_state['vol'], mon_state['mute'],
                    str_state['vol'], str_state['mute'],
                    is_hidden
                )

                # VU meter: feed from the mic itself, or the virtual sink's
                # monitor source. Spawn lazily, and re-spawn if the node_name
                # changed (e.g. channel rename).
                meter = self.meters.get(pw_id)
                if node in mics:
                    meter_source = nname
                else:
                    meter_source = f"{nname}.monitor"
                if meter is None or meter.source_name != meter_source:
                    if meter is not None:
                        meter.stop()
                    meter = MeterWorker(meter_source, self)
                    meter.peak.connect(strip.on_peak)
                    meter.start()
                    self.meters[pw_id] = meter
                
                # First time this node shows up in the session — re-spawn
                # any saved FX chain. The next routing pass threads the
                # loopbacks through the chain's output.
                if nname and nname not in self._effects_applied:
                    self._effects_applied.add(nname)
                    saved_chain = list(self.active_effects.get(nname, []))
                    if saved_chain and not self.engine.is_channel_fx_running(nname):
                        capture_target = nname if (node in mics) else f"{nname}.monitor"
                        params_map = dict(self.effect_params.get(nname, {}) or {})
                        self.engine.set_channel_fx(
                            nname, capture_target, saved_chain, params_map,
                        )

                # Surface "an effect is on" as a tiny ✨ next to the icon
                # instead of repainting a big FX button.
                strip._refresh_fx_indicator()

            # Reap widgets (and meters) whose node has disappeared (unplugged
            # mic, removed sink, PipeWire restart).
            for stale in [pid for pid in self.channel_widgets if pid not in current_node_ids]:
                widget = self.channel_widgets.pop(stale)
                stale_nname = getattr(widget, 'node_name', None)
                widget.setParent(None)
                widget.deleteLater()
                self.engine.remove_node_routing(stale)
                # The mic is gone — kill its FX chain stage processes
                # so they don't sit idle. Re-applied via _effects_applied
                # if the node comes back.
                if stale_nname:
                    self.engine.clear_channel_fx(stale_nname)
                    self._effects_applied.discard(stale_nname)
                meter = self.meters.pop(stale, None)
                if meter is not None:
                    meter.stop()

            # Recompute strip sizes then let the container measure itself.
            self._rescale_strips()
            self.inputs_container.adjustSize()

            # 2. Update App Routing (Persistent & Grouped)
            # Map app_name -> list of active indices
            apps_by_name = {}
            now = int(time.time())

            # Auto-route enforcement: move new sink-inputs to their saved
            # sink exactly once per index. Tracking the index in
            # `_auto_routed_indices` keeps us from fighting manual moves
            # made via pavucontrol after the initial placement.
            valid_sink_names = {s['name'] for s in all_sinks}
            current_indices = set()

            for app in apps:
                app_name = app.get('app_name') or app.get('binary') or "Unknown App"
                if app_name not in apps_by_name:
                    apps_by_name[app_name] = []
                idx = app.get('index')
                self.app_last_seen[app_name] = now
                if idx is not None:
                    apps_by_name[app_name].append(idx)
                    current_indices.add(idx)

                    preferred_sink = self.app_routing.get(app_name)
                    if (preferred_sink
                            and preferred_sink in valid_sink_names
                            and idx not in self._auto_routed_indices):
                        if app.get('sink') != preferred_sink:
                            self.engine.move_app_to_sink(idx, preferred_sink)
                        self._auto_routed_indices.add(idx)

            # Drop tracking for indices that no longer exist so a
            # restarted app gets re-routed on its next first sighting.
            self._auto_routed_indices &= current_indices

            # Show every app we still 'remember' — currently making sound,
            # has a saved routing, OR was seen within the prune window. The
            # last category is what makes Discord stay in the panel after you
            # close it: without it, an app that was never explicitly routed
            # vanishes the moment its last sink-input goes away.
            cutoff = int(time.time()) - max(1, self.app_prune_days) * 24 * 3600
            recently_seen = {
                name for name, ts in self.app_last_seen.items()
                if ts >= cutoff
            }
            all_display_apps = (
                set(apps_by_name.keys())
                | set(self.app_routing.keys())
                | recently_seen
                | {PipeWireEngine.SYSTEM_SOUNDS_BUCKET}
            )
            # Hard-forget blocklist: rows the user explicitly clicked ✕ on
            # never come back, even if PipeWire keeps surfacing the stream.
            # This is the missing half of the ✕ button — without it the row
            # gets re-synthesised from `apps_by_name` on the next tick and
            # the click looks like a no-op.
            all_display_apps -= self.forgotten_apps
            # Defence-in-depth host filter: even if a row name slipped past
            # the engine-level host check (a property we don't sniff yet,
            # an old saved app_routing entry that bypasses the migration on
            # this load, etc.), drop anything that normalises to the host
            # before any UI work happens. Migration in load_config purges
            # those entries from app_routing and app_last_seen; here we
            # also cull them from the display set.
            all_display_apps = {
                name for name in all_display_apps
                if not PipeWireEngine.name_matches_host(name)
            }

            # Iterate in a stable order so newly-created rows always land in
            # the same position in `routing_layout`. A bare `for x in set(...)`
            # produces hash-randomised order and visibly shuffles the list
            # every tick a row appears or is recreated.
            sys_bucket = PipeWireEngine.SYSTEM_SOUNDS_BUCKET
            ordered_display_apps = sorted(
                all_display_apps,
                key=lambda n: (0 if n == sys_bucket else 1, n.lower()),
            )
            for app_name in ordered_display_apps:
                active_indices = apps_by_name.get(app_name, [])
                # Dropdown reflects user intent. If a saved routing
                # exists, show it even when the live sink-input is still
                # on the system default for one tick — otherwise the
                # dropdown briefly resets every time the app launches.
                preferred_sink = self.app_routing.get(app_name)
                live_sink = None
                if active_indices:
                    for a in apps:
                        if a.get('index') == active_indices[0]:
                            live_sink = a.get('sink')
                            break
                current_sink = preferred_sink or live_sink

                if app_name not in self.app_widgets:
                    row = AppRoutingRow(app_name, self.engine, all_sinks, main_win=self)
                    self.app_widgets[app_name] = row
                    self.routing_layout.addWidget(row)

                self.app_widgets[app_name].update_state(active_indices, all_sinks, current_sink)

            # Cleanup widgets for apps we never want to see again (not in config and not active)
            for name in list(self.app_widgets.keys()):
                if name not in all_display_apps:
                    self.app_widgets[name].setParent(None)
                    self.app_widgets[name].deleteLater()
                    del self.app_widgets[name]

            # Tell the scroll area's inner widget to recalculate its height
            # so newly-added rows are immediately visible.
            self.routing_container.updateGeometry()
            self.routing_container.adjustSize()

            # 3. Monitor output dropdown (Stream is fixed to virtual).
            # Use the Description field so BT sinks show real model names
            # like 'Sony WH-1000XM4' instead of 'Bd 10 1'.
            combo = self.mon_out_combo
            if not combo.view().isVisible():  # Don't update while user is looking at it
                curr_data = combo.currentData()
                combo.blockSignals(True)
                combo.clear()
                combo.addItem("None", None)
                for s in all_sinks:
                    name = s['name']
                    if name.startswith('wavelinux_'):
                        continue
                    if name.endswith('.monitor'):
                        continue
                    combo.addItem(self.engine.display_name_for_sink(name, snap=snap), name)
                idx = combo.findData(curr_data)
                if idx >= 0:
                    combo.setCurrentIndex(idx)
                combo.blockSignals(False)

            # 4. Update Master Mix Sliders (mix sinks are addressed by name).
            # Both reads come out of the shared snapshot — no extra pactl calls.
            mon_mix = self.engine.output_mixes.get("Monitor")
            if mon_mix and not self.mon_master_slider.isSliderDown():
                v, _ = self.engine.get_sink_volume_by_name(mon_mix.sink_name, snap=snap)
                self.mon_master_slider.blockSignals(True)
                self.mon_master_slider.setValue(int(v * 100))
                self.mon_master_slider.blockSignals(False)

            str_mix = self.engine.output_mixes.get("Stream")
            if str_mix and not self.str_master_slider.isSliderDown():
                v, _ = self.engine.get_sink_volume_by_name(str_mix.sink_name, snap=snap)
                self.str_master_slider.blockSignals(True)
                self.str_master_slider.setValue(int(v * 100))
                self.str_master_slider.blockSignals(False)

            # Update status bar info
            self.status_lbl.setText(f"PipeWire connected · {len(mics+vsinks)} nodes · {len(apps)} apps")
        except Exception as e:
            logging.error(f"UI Refresh error: {e}")
            self.status_lbl.setText(f"Refresh error: {str(e)[:30]}...")



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
            mix = self.engine.output_mixes.get(mix_name)
            if mix:
                self.engine.set_sink_volume_by_name(mix.sink_name, vol)

    def _on_mix_out_change(self, mix_name, hw_sink_name):
        if hw_sink_name:
            self.engine.route_mix_to_hardware(mix_name, hw_sink_name)
        else:
            # 'None (Disconnected)' — actually unload the loopback so the bus
            # stops sending to the previous hardware output.
            self.engine.unroute_mix_from_hardware(mix_name)

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
            self.schedule_save()
        self._pending_clipguard_migration = False

    def _sync_mic_picker(self, mics):
        """Refresh the master mic combo. Cheap on each tick — only
        rebuilds items when the mic list changes. Falls back to
        `pactl get-default-source` when nothing's saved yet."""
        combo = self.mic_in_combo
        mic_names = {m.name for m in mics}
        # Resolve a stale or unset selected_mic (unplugged device, fresh
        # launch) to the system default if available, else the first mic.
        if mics and (not self.selected_mic or self.selected_mic not in mic_names):
            default_src = (
                self.engine.get_default_source()
                if hasattr(self.engine, 'get_default_source') else None
            )
            if default_src and default_src in mic_names:
                self.selected_mic = default_src
            else:
                self.selected_mic = mics[0].name
            self.schedule_save()

        mic_fp = tuple((m.name, m.description or '') for m in mics)
        if getattr(self, '_mic_combo_fp', None) != mic_fp:
            self._mic_combo_fp = mic_fp
            combo.blockSignals(True)
            combo.clear()
            for m in mics:
                label = PipeWireEngine.friendly_name(m.description) or m.name
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

    def _on_mic_input_change(self, idx):
        """Persist a mic-picker change and refresh immediately so the
        strip swaps to the new mic on the same tick."""
        new_mic = self.mic_in_combo.itemData(idx)
        if new_mic == self.selected_mic:
            return
        # Drop the new mic from `_synced_submix_owners` so saved state
        # gets re-pushed as soon as the loopbacks come up. Without this
        # the strip briefly inherits the previous mic's live state.
        if new_mic:
            self._synced_submix_owners.pop(new_mic, None)
        self.selected_mic = new_mic
        self.schedule_save()
        self._refresh()

    def _on_add_channel(self):
        text, ok = QInputDialog.getText(self, "Add Virtual Channel", "Channel Name:")
        if not (ok and text):
            return
        clean = re.sub(r'\s+', ' ', text).strip()
        if not clean:
            return
        self.engine.create_virtual_sink(clean)
        if clean not in self.virtual_channels:
            self.virtual_channels.append(clean)
            self.save_config()
        self._refresh()

    def _remove_sink(self, sink_name):
        self.engine.remove_virtual_sink(sink_name)
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
            if self.engine.create_virtual_sink(name) is not None:
                if name not in self.virtual_channels:
                    self.virtual_channels.append(name)

    def load_config(self):
        if not os.path.exists(self.config_path):
            # First launch — set up the standard mixes, route Monitor to
            # the system default, and seed starter channels.
            self.engine.create_output_mix("Monitor")
            self.engine.create_output_mix("Stream")
            def_sink = self.engine.get_default_sink()
            if def_sink:
                self.engine.route_mix_to_hardware("Monitor", def_sink)
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
                    if not k.startswith('Media Stream #')
                }
                self.app_last_seen = {
                    k: int(v) for k, v in (conf.get('app_last_seen', {}) or {}).items()
                    if isinstance(k, str) and isinstance(v, (int, float))
                    and not k.startswith('Media Stream #')
                }
                self.app_prune_days = int(conf.get('app_prune_days', self.app_prune_days) or 14)
                # Persistent ✕ blocklist. Set for O(1) membership.
                self.forgotten_apps = {
                    name for name in (conf.get('forgotten_apps', []) or [])
                    if isinstance(name, str) and name
                }
                # Purge host-named ghosts from older configs whose engine
                # filter didn't normalise whitespace.
                for name in list(self.app_routing.keys()):
                    if PipeWireEngine.name_matches_host(name):
                        self.app_routing.pop(name, None)
                for name in list(self.app_last_seen.keys()):
                    if PipeWireEngine.name_matches_host(name):
                        self.app_last_seen.pop(name, None)
                self._prune_stale_apps()
                self.virtual_channels = conf.get('channels', [])
                self.channel_order = conf.get('channel_order', []) or []
                # Single-mic mode: which mic the user picked in the master
                # "Microphone Input" combo. None means "use system default";
                # the refresh path resolves None to `pactl get-default-source`
                # the first time it runs.
                self.selected_mic = conf.get('selected_mic') or None
                self.effect_params = conf.get('effect_params', {}) or {}
                self.active_effects = {
                    k: list(v) for k, v in (conf.get('active_effects', {}) or {}).items()
                    if isinstance(v, list)
                }

                # The chain spawn itself happens in _refresh once each
                # node's pw_id is known (`_effects_applied` gate).

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
                self.engine.create_output_mix("Monitor")
                self.engine.create_output_mix("Stream")

                # Legacy master-bus Clipguard → per-mic limiter migration.
                # `selected_mic` may not be resolved yet here, so we just
                # stash the intent and `_refresh` applies it once the
                # picker has landed.
                self._pending_clipguard_migration = bool(conf.get('clipguard'))
                if self._pending_clipguard_migration and self.selected_mic:
                    self._apply_pending_clipguard_migration()

                # Recreate virtual channels
                for name in self.virtual_channels:
                    self.engine.create_virtual_sink(name)
                    
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
            'monitor_hw': self.mon_out_combo.currentData(),
            'stream_hw': self.str_out_combo.currentData(),
            'channels': self.virtual_channels,
            'selected_mic': self.selected_mic,
            'submixes': self.submix_state,
            'hidden': list(self.hidden_nodes),
            'app_routing': {k: v for k, v in self.app_routing.items()
                            if not k.startswith('Media Stream #')},
            'channel_order': self.channel_order,
            'effect_params': self.effect_params,
            'active_effects': self.active_effects,
            'app_last_seen': {k: v for k, v in self.app_last_seen.items()
                              if not k.startswith('Media Stream #')},
            'app_prune_days': self.app_prune_days,
            'forgotten_apps': sorted(self.forgotten_apps),
        }
        try:
            tmp = self.config_path + '.tmp'
            with open(tmp, 'w') as f:
                json.dump(conf, f, indent=4)
            os.replace(tmp, self.config_path)
        except Exception as e:
            logging.error(f"Error saving config: {e}")



    def forget_app(self, app_name):
        """Drop all state for an app and add it to the persistent
        `forgotten_apps` blocklist. Recover via Settings → Advanced →
        'Restore forgotten apps' or by editing config.json."""
        # System Sounds is a permanent built-in entry — never let it
        # land in the blocklist, even if something calls forget on it.
        if app_name == PipeWireEngine.SYSTEM_SOUNDS_BUCKET:
            return
        self.app_routing.pop(app_name, None)
        self.app_last_seen.pop(app_name, None)
        self.forgotten_apps.add(app_name)
        widget = self.app_widgets.pop(app_name, None)
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
        # Apps without a saved routing also get reaped past the cutoff.
        stale_seen = [
            name for name, ts in list(self.app_last_seen.items())
            if ts < cutoff
        ]
        for name in stale_seen:
            self.app_last_seen.pop(name, None)
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
        new_sink = self.engine.rename_virtual_sink(old_node_name, cleaned)
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
        self._effects_applied.discard(old_name)
        # Force a state-resync for the renamed channel — the loopback
        # owners refer to a sink-input ID under the new node.name and
        # the previous mapping no longer applies.
        self._synced_submix_owners.pop(old_name, None)
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
        dlg = CardProfileDialog(self.engine, self)
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

    def closeEvent(self, event):
        """Minimize to tray when one is available; otherwise actually quit."""
        if self.tray is not None and self.tray.isVisible():
            event.ignore()
            self.hide()
            return
        self._quit_app()
        event.accept()

    def _quit_app(self):
        """Cleanly save state, unload all modules, and exit."""
        logging.info("Shutting down WaveLinux...")
        self.refresh_timer.stop()
        self._save_timer.stop()
        self._event_refresh_timer.stop()
        # Stop every parec meter subprocess.
        for meter in list(self.meters.values()):
            meter.stop()
        self.meters.clear()
        # Flush any pending slider writes before we tear down the engine.
        self.save_config()
        proc = getattr(self, "_event_proc", None)
        if proc is not None and proc.state() != QProcess.ProcessState.NotRunning:
            proc.kill()
            proc.waitForFinished(500)
        self.engine.full_audio_reset()
        logging.info("Audio reset complete. Exiting.")
        QApplication.instance().quit()


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
    app.aboutToQuit.connect(window.engine.cleanup)

    sys.exit(app.exec())


if __name__ == '__main__':
    main()
