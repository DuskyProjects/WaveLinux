#!/usr/bin/env python3
"""WaveLinux — Native PipeWire Audio Mixer for KDE/Linux"""

import json
import logging
import sys
import os
import re

from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QLabel, QSlider, QPushButton, QFrame, QScrollArea, QDialog,
    QLineEdit, QDialogButtonBox, QComboBox, QMessageBox, QSystemTrayIcon,
    QMenu, QInputDialog, QProgressBar, QSizePolicy
)
from PyQt6.QtCore import Qt, QTimer, QLockFile, QProcess, pyqtSignal, QObject
from PyQt6.QtGui import QFont, QIcon, QAction

from pipewire_engine import PipeWireEngine
from wavelinux_theme import STYLESHEET


import struct


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
        # Emit at most ~20 Hz so we don't spam the UI. parec is already
        # chunked by the PulseAudio bridge.
        self._sample_bytes = 48000 * 2 // 20   # 50 ms of s16 mono at 48 kHz
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

    def __init__(self, engine, parent=None):
        super().__init__(parent)
        self.engine = engine
        self.setWindowTitle("Sound Card Profiles")
        self.setMinimumWidth(520)
        self.setStyleSheet(STYLESHEET)
        self._combos = []   # (card_name, combo)

        layout = QVBoxLayout(self)
        layout.setSpacing(12)
        layout.setContentsMargins(20, 20, 20, 20)

        title = QLabel("🎛 Sound Card Profiles")
        title.setStyleSheet("font-size: 18px; font-weight: bold; color: #fff;")
        layout.addWidget(title)

        desc = QLabel("Pick the ALSA profile for each card — e.g. Analog Stereo "
                      "for headphones or Pro Audio for interfaces with many channels.")
        desc.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        desc.setWordWrap(True)
        layout.addWidget(desc)

        cards = self.engine.list_cards()
        if not cards:
            empty = QLabel("No cards reported by PipeWire.")
            empty.setStyleSheet("color: #6b6b82; padding: 24px;")
            layout.addWidget(empty)
        for card in cards:
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
            layout.addWidget(row)
            self._combos.append((card['name'], combo))

        btns = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Apply | QDialogButtonBox.StandardButton.Close
        )
        btns.button(QDialogButtonBox.StandardButton.Apply).clicked.connect(self._apply)
        btns.button(QDialogButtonBox.StandardButton.Close).clicked.connect(self.accept)
        layout.addWidget(btns)

    def _apply(self):
        for card_name, combo in self._combos:
            target = combo.currentData()
            if target:
                self.engine.set_card_profile(card_name, target)


# ── FX Selection Dialog ───────────────────────────────────────────
class FXSelectionDialog(QDialog):
    def __init__(self, node_id, node_name, engine, parent=None):
        super().__init__(parent)
        self.node_id = str(node_id)
        self.node_name = node_name
        self.engine = engine
        self.setWindowTitle("Channel Effects")
        self.setMinimumWidth(400)
        self.setStyleSheet(STYLESHEET)

        # Per-(effect_id) widgets we need to touch from _on_toggle / slider changes.
        self._param_sliders = {}   # effect_id -> {param_key: (slider, value_lbl)}
        self._param_frames  = {}   # effect_id -> QFrame holding the param rows
        self._toggle_btns   = {}   # effect_id -> QPushButton

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

        close_btn = QPushButton("Done")
        close_btn.setObjectName("addBtn")
        close_btn.clicked.connect(self.accept)
        root.addWidget(close_btn)

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
        active = self.engine.is_effect_active(self.node_id, fid)
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

        # Parameter panel (shown only when the effect is ON).
        params = self.engine.get_effect_params(fid)
        if params:
            param_frame = QFrame()
            param_frame.setStyleSheet("background: transparent;")
            pf_layout = QVBoxLayout(param_frame)
            pf_layout.setContentsMargins(40, 6, 4, 2)
            pf_layout.setSpacing(4)
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
        win = self.window().parent()
        # self.window() is the dialog itself; walk through parent() to main.
        p = self.parent()
        while p is not None and not hasattr(p, 'effect_params'):
            p = p.parent()
        if p is None:
            return {}
        ep = p.effect_params.get(self.node_name, {}).get(effect_id, {})
        return dict(ep)

    def _save_params(self, effect_id, params):
        p = self.parent()
        while p is not None and not hasattr(p, 'effect_params'):
            p = p.parent()
        if p is None:
            return
        p.effect_params.setdefault(self.node_name, {})[effect_id] = dict(params)
        if hasattr(p, 'schedule_save'):
            p.schedule_save()
        elif hasattr(p, 'save_config'):
            p.save_config()

    def _collect_params(self, effect_id):
        out = {}
        for key, (slider, _lbl) in self._param_sliders.get(effect_id, {}).items():
            pmin = float(slider.property("pmin"))
            pmax = float(slider.property("pmax"))
            val = pmin + (pmax - pmin) * (slider.value() / 1000.0)
            out[key] = val
        return out

    # ── Handlers ───────────────────────────────────────────────────

    def _on_toggle(self, effect_id):
        btn = self._toggle_btns[effect_id]
        frame = self._param_frames.get(effect_id)
        if btn.isChecked():
            btn.setText("ON")
            params = self._collect_params(effect_id)
            self.engine.apply_effect(self.node_id, effect_id, params=params or None)
            self._save_params(effect_id, params)
            if frame:
                frame.setVisible(True)
        else:
            btn.setText("OFF")
            self.engine.remove_effect(self.node_id, effect_id)
            if frame:
                frame.setVisible(False)

    def _on_param_changed(self, effect_id, slider, value_lbl):
        pmin = float(slider.property("pmin"))
        pmax = float(slider.property("pmax"))
        suffix = slider.property("psuffix") or ""
        val = pmin + (pmax - pmin) * (slider.value() / 1000.0)
        value_lbl.setText(self._fmt_value(val, suffix))

        params = self._collect_params(effect_id)
        self._save_params(effect_id, params)

        # If the effect is currently running, restart it with new params so
        # the change is audible immediately. Otherwise we just persist.
        if self.engine.is_effect_active(self.node_id, effect_id):
            self.engine.remove_effect(self.node_id, effect_id)
            self.engine.apply_effect(self.node_id, effect_id, params=params)


# ── Channel Strip Widget ───────────────────────────────────────────
class ChannelStrip(QFrame):
    """A single mixer channel: icon, name, vertical fader, mute, FX."""

    def __init__(self, node_id, node_name, name, ch_type, icon, engine, parent=None):
        super().__init__(parent)
        self.setObjectName("channelStrip")
        self.setMinimumWidth(160)
        self.setMaximumWidth(180)
        self.node_id = node_id          # PipeWire numeric id (ephemeral)
        self.node_name = node_name      # PipeWire node.name (stable across restarts)
        self.ch_name = name
        self.ch_type = ch_type
        self.engine = engine
        self.is_mic = ch_type.lower() == "microphone"
        self._muted = False
        self._mon_muted = False
        self._str_muted = False

        layout = QVBoxLayout(self)
        layout.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.setSpacing(2)

        # Top row: reorder ← / icon / reorder →
        top_row = QHBoxLayout()
        self.left_btn = QPushButton("◀")
        self.left_btn.setObjectName("reorderBtn")
        self.left_btn.setFixedSize(20, 20)
        self.left_btn.setToolTip("Move this channel left")
        self.left_btn.clicked.connect(lambda: self._request_move(-1))
        top_row.addWidget(self.left_btn)

        top_row.addStretch()
        icon_lbl = QLabel(icon)
        icon_lbl.setObjectName("channelIcon")
        top_row.addWidget(icon_lbl)
        top_row.addStretch()

        self.right_btn = QPushButton("▶")
        self.right_btn.setObjectName("reorderBtn")
        self.right_btn.setFixedSize(20, 20)
        self.right_btn.setToolTip("Move this channel right")
        self.right_btn.clicked.connect(lambda: self._request_move(1))
        top_row.addWidget(self.right_btn)
        layout.addLayout(top_row)

        # Enable right-click context menu
        self.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        self.customContextMenuRequested.connect(self._show_context_menu)

        # Name
        name_lbl = QLabel(name)
        name_lbl.setObjectName("channelName")
        name_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        name_lbl.setWordWrap(True)
        name_lbl.setMinimumHeight(24) # allow 2 lines
        layout.addWidget(name_lbl)

        # Type label
        type_lbl = QLabel(ch_type.upper())
        type_lbl.setObjectName("channelType")
        type_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(type_lbl)

        if self.is_mic:
            badge = QLabel("RNNoise")
            badge.setObjectName("rnBadge")
            badge.setAlignment(Qt.AlignmentFlag.AlignCenter)
            layout.addWidget(badge)

        layout.addSpacing(4)

        # Sliders layout
        sliders_layout = QVBoxLayout()
        sliders_layout.setSpacing(6)

        # Top controls for this channel: link
        link_row = QHBoxLayout()
        self.link_btn = QPushButton("🔗")
        self.link_btn.setObjectName("soloBtn")
        self.link_btn.setCheckable(True)
        self.link_btn.setFixedSize(24, 24)
        self.link_btn.setToolTip("Link Monitor and Stream faders")
        self.link_btn.clicked.connect(self._on_link_toggle)
        link_row.addStretch()
        link_row.addWidget(self.link_btn)
        link_row.addStretch()
        sliders_layout.addLayout(link_row)

        # Peak meter: horizontal progress bar driven by the MeterWorker.
        self.peak_bar = QProgressBar()
        self.peak_bar.setObjectName("peakBar")
        self.peak_bar.setRange(0, 1000)
        self.peak_bar.setTextVisible(False)
        self.peak_bar.setFixedHeight(6)
        self.peak_bar.setValue(0)
        sliders_layout.addWidget(self.peak_bar)

        # The two faders side-by-side
        faders_row = QHBoxLayout()
        faders_row.setSpacing(10)

        # Monitor Fader Column
        mon_col = QVBoxLayout()
        mon_col.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        mon_label = QLabel("MON")
        mon_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        mon_label.setStyleSheet("color: #00e5ff; font-size: 8px; font-weight: bold; letter-spacing: 1px;")
        mon_col.addWidget(mon_label)
        self.mon_vol_lbl = QLabel("100%")
        self.mon_vol_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.mon_vol_lbl.setObjectName("volumeLabel")
        mon_col.addWidget(self.mon_vol_lbl)

        self.mon_slider = QSlider(Qt.Orientation.Vertical)
        self.mon_slider.setRange(0, 150)
        self.mon_slider.setValue(100)
        self.mon_slider.setMinimumHeight(120)
        self.mon_slider.valueChanged.connect(self._on_mon_vol)
        mon_col.addWidget(self.mon_slider, 1)

        self.mon_mute = QPushButton("🎧")
        self.mon_mute.setObjectName("muteBtn")
        self.mon_mute.setFixedSize(28, 28)
        self.mon_mute.clicked.connect(self._on_mon_mute)
        mon_col.addWidget(self.mon_mute)
        faders_row.addLayout(mon_col)

        # Stream Fader Column
        str_col = QVBoxLayout()
        str_col.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        str_label = QLabel("STR")
        str_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        str_label.setStyleSheet("color: #7000ff; font-size: 8px; font-weight: bold; letter-spacing: 1px;")
        str_col.addWidget(str_label)
        self.str_vol_lbl = QLabel("100%")
        self.str_vol_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.str_vol_lbl.setObjectName("volumeLabel")
        str_col.addWidget(self.str_vol_lbl)

        self.str_slider = QSlider(Qt.Orientation.Vertical)
        self.str_slider.setRange(0, 150)
        self.str_slider.setValue(100)
        self.str_slider.setMinimumHeight(120)
        self.str_slider.valueChanged.connect(self._on_str_vol)
        str_col.addWidget(self.str_slider, 1)

        self.str_mute = QPushButton("📡")
        self.str_mute.setObjectName("muteBtn")
        self.str_mute.setFixedSize(28, 28)
        self.str_mute.clicked.connect(self._on_str_mute)
        str_col.addWidget(self.str_mute)
        faders_row.addLayout(str_col)

        sliders_layout.addLayout(faders_row, 1)

        layout.addLayout(sliders_layout, 1)

        layout.addSpacing(8)

        # FX / RNNoise button
        self.fx_btn = QPushButton("✨ Add FX")
        self.fx_btn.setObjectName("fxBtn")
        self.fx_btn.clicked.connect(self._on_fx_toggle)
        layout.addWidget(self.fx_btn)

    def _stash_submix(self, mix_name, vol, mute):
        win = self.window()
        if not hasattr(win, 'submix_state') or not self.node_name:
            return
        win.submix_state[f"{self.node_name}_{mix_name}"] = {'vol': vol, 'mute': mute}
        if hasattr(win, 'schedule_save'):
            win.schedule_save()

    def _on_link_toggle(self):
        if self.link_btn.isChecked():
            self.str_slider.setValue(self.mon_slider.value())
            self.save_link_state()

    def save_link_state(self):
        win = self.window()
        if hasattr(win, 'submix_state') and self.node_name:
            win.submix_state[f"{self.node_name}_linked"] = self.link_btn.isChecked()
            if hasattr(win, 'schedule_save'):
                win.schedule_save()

    def _on_mon_vol(self, value):
        self.mon_vol_lbl.setText(f"{value}%")
        if self.node_id:
            self.engine.set_submix_volume(self.node_id, "Monitor", value / 100.0)
            self._stash_submix("Monitor", value / 100.0, self._mon_muted)
        
        if self.link_btn.isChecked() and self.str_slider.value() != value:
            self.str_slider.setValue(value)

    def _on_str_vol(self, value):
        self.str_vol_lbl.setText(f"{value}%")
        if self.node_id:
            self.engine.set_submix_volume(self.node_id, "Stream", value / 100.0)
            self._stash_submix("Stream", value / 100.0, self._str_muted)
            
        if self.link_btn.isChecked() and self.mon_slider.value() != value:
            self.mon_slider.setValue(value)

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

    def _on_fx_toggle(self):
        dlg = FXSelectionDialog(self.node_id, self.node_name, self.engine, self)
        dlg.exec()
        # Visual update will happen in next refresh cycle or we can force it
        is_any_fx = False
        for fx in self.engine.get_available_effects():
            if self.engine.is_effect_active(str(self.node_id), fx['id']):
                is_any_fx = True
                break
        
        self.fx_btn.setProperty("active", "true" if is_any_fx else "false")
        self.fx_btn.style().unpolish(self.fx_btn)
        self.fx_btn.style().polish(self.fx_btn)

    def _show_context_menu(self, pos):
        """Right-click context menu for channel strip actions."""
        menu = QMenu(self)
        menu.setStyleSheet("""
            QMenu { background: #1a1a28; color: #e0e0ee; border: 1px solid rgba(255,255,255,0.1); border-radius: 6px; padding: 4px; }
            QMenu::item { padding: 6px 20px; }
            QMenu::item:selected { background: rgba(0,229,255,0.15); }
        """)
        hide_act = menu.addAction("👁 Hide Channel")
        hide_act.triggered.connect(self._request_hide)
        if self.ch_type.lower() == "virtual":
            rename_act = menu.addAction("✏️ Rename Channel")
            rename_act.triggered.connect(self._request_rename)
        menu.exec(self.mapToGlobal(pos))

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
        """Update UI from stored state."""
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
            is_linked = win.submix_state.get(f"{self.node_name}_linked", False)

        self.link_btn.blockSignals(True)
        self.link_btn.setChecked(is_linked)
        self.link_btn.blockSignals(False)

        self.mon_vol_lbl.setText(f"{int(mon_vol * 100)}%")
        self.str_vol_lbl.setText(f"{int(str_vol * 100)}%")
        
        self.mon_mute.setText("🔇" if mon_mute else "🎧")
        self.mon_mute.setProperty("muted", "true" if mon_mute else "false")
        self.mon_mute.style().unpolish(self.mon_mute)
        self.mon_mute.style().polish(self.mon_mute)
        
        self.str_mute.setText("🔇" if str_mute else "📡")
        self.str_mute.setProperty("muted", "true" if str_mute else "false")
        self.str_mute.style().unpolish(self.str_mute)
        self.str_mute.style().polish(self.str_mute)
        
        # Rewire the hide button for whichever state we're in. disconnect()
        # raises if nothing is connected, so swallow that narrowly.
        try:
            self.hide_btn.clicked.disconnect()
        except TypeError:
            pass
        if is_hidden:
            self.hide_btn.setText("👁 Unhide")
            self.hide_btn.clicked.connect(self._request_unhide)
        else:
            self.hide_btn.setText("👁")
            self.hide_btn.clicked.connect(self._request_hide)
        self.hide_btn.setStyleSheet("")


# ── App Routing Row ────────────────────────────────────────────────
class AppRoutingRow(QWidget):
    """A row showing an app name and a dropdown to choose which sink it goes to."""

    def __init__(self, app_name, engine, sinks, parent=None):
        super().__init__(parent)
        self.engine = engine
        self.app_name = app_name
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

        self.update_state([], sinks, None)

    def _on_vol_change(self, value):
        for idx in self._active_indices:
            self.engine.set_sink_input_volume(idx, value / 100.0)

    def _on_route_change(self, idx):
        sink_name = self.combo.itemData(idx)
        # Move all active instances
        for app_idx in self._active_indices:
            self.engine.move_app_to_sink(app_idx, sink_name)
        
        # Save preference
        win = self.window()
        if hasattr(win, 'app_routing'):
            win.app_routing[self.app_name] = sink_name
            win.save_config()

    def _on_forget(self):
        """Remove this app from persisted app_routing. Only meaningful for
        offline apps; running apps would just re-appear next refresh."""
        win = self.window()
        if hasattr(win, 'forget_app'):
            win.forget_app(self.app_name)

    def update_state(self, active_indices, sinks, current_sink):
        self._active_indices = active_indices
        is_active = len(active_indices) > 0

        # Dim if inactive
        if not is_active:
            self.name_lbl.setText(f"{self.app_name} (Offline)")
            self.vol_slider.setEnabled(False)
            self.vol_lbl.setStyleSheet("color: #666;")
            self.name_lbl.setStyleSheet("color: #888;")
        else:
            self.name_lbl.setText(self.app_name)
            self.vol_slider.setEnabled(True)
            self.vol_lbl.setStyleSheet("")
            self.name_lbl.setStyleSheet("")

            # Update volume from engine
            vol = self.engine.get_sink_input_volume(active_indices[0])
            self.vol_slider.blockSignals(True)
            self.vol_slider.setValue(int(vol * 100))
            self.vol_slider.blockSignals(False)

        # Forget is only useful for offline apps with a remembered routing;
        # for a running app, it would be hidden and come right back.
        win = self.window()
        saved = hasattr(win, 'app_routing') and self.app_name in win.app_routing
        self.forget_btn.setEnabled((not is_active) and saved)

        # Update combo box
        if not self.combo.view().isVisible():
            self.combo.blockSignals(True)
            curr_data = self.combo.currentData()
            self.combo.clear()
            self.combo.addItem("None (System Default)", None)
            for s in sinks:
                display = PipeWireEngine.friendly_name(s['name'])
                if 'wavelinux_' in s['name']:
                    display = s['name'].replace('wavelinux_', '').replace('_', ' ').title() + ' ⭐'
                self.combo.addItem(display, s['name'])
            
            idx = self.combo.findData(current_sink or curr_data)
            if idx >= 0:
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
        self.virtual_channels = []  # list of names
        # All user-facing state is keyed by PipeWire node.name (stable across
        # PipeWire restarts); pw_id is only used when talking to the engine.
        self.hidden_nodes = set()      # {node.name}
        self.show_hidden = False
        self.effect_params = {}        # node.name -> effect_id -> {param_key: value}
        self.channel_order = []        # [node.name, ...] — persistent UI order
        self.meters = {}               # pw_id -> MeterWorker
        self._known_node_names = set() # for hot-plug detection
        self.config_path = os.path.expanduser("~/.config/wavelinux/config.json")
        os.makedirs(os.path.dirname(self.config_path), exist_ok=True)
        
        # The 2-second timer is our fallback — the event subscriber below
        # drives most refreshes, and we want a backstop in case pactl
        # subscribe is unavailable or misses an event.
        self.refresh_timer = QTimer(self)
        self.refresh_timer.timeout.connect(self._refresh)

        # Coalesce rapid save requests (sliders fire valueChanged on every pixel).
        self._save_timer = QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.timeout.connect(self.save_config)

        # Coalesce rapid refresh requests (pactl subscribe can fire 5+ events
        # for a single operation); one refresh per 150 ms is plenty.
        self._event_refresh_timer = QTimer(self)
        self._event_refresh_timer.setSingleShot(True)
        self._event_refresh_timer.setInterval(150)
        self._event_refresh_timer.timeout.connect(self._refresh)

        self._setup_ui()
        self.load_config()
        self._refresh()
        self.refresh_timer.start(2000)
        self._start_event_subscriber()

    def _open_settings(self):
        self._refresh_hidden_list()
        self.settings_dialog.show()
        self.settings_dialog.raise_()

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

            unhide_btn = QPushButton("👁 Show")
            unhide_btn.setObjectName("addBtn")
            unhide_btn.setFixedHeight(28)
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

    def _start_event_subscriber(self):
        """Run `pactl subscribe` under a QProcess so an external mute/volume
        change (pavucontrol, media keys, another app) triggers a refresh
        within ~150 ms instead of waiting up to 2 s for the poll timer.
        The poll timer is kept as a backstop."""
        self._event_proc = QProcess(self)
        self._event_proc.setProcessChannelMode(QProcess.ProcessChannelMode.MergedChannels)
        self._event_proc.readyReadStandardOutput.connect(self._on_pactl_event)
        self._event_proc.errorOccurred.connect(self._on_event_proc_error)
        try:
            self._event_proc.start("pactl", ["subscribe"])
        except Exception as e:
            logging.warning(f"pactl subscribe unavailable: {e} — falling back to poll")

    def _on_pactl_event(self):
        """Any event at all just kicks the debounce; we don't filter here
        because the refresh body is already cheap under the snapshot cache."""
        try:
            _ = bytes(self._event_proc.readAllStandardOutput())
        except Exception:
            pass
        if self.tray is not None and not self.isVisible():
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
        body.setContentsMargins(20, 16, 20, 0)
        body.setSpacing(0)

        input_lbl = QLabel("AUDIO SOURCES")
        input_lbl.setObjectName("sectionLabel")
        body.addWidget(input_lbl)

        # Inputs Area
        self.inputs_container = QWidget()
        self.input_layout = QHBoxLayout(self.inputs_container)
        self.input_layout.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignTop)
        self.input_layout.setSpacing(10)
        
        body.addWidget(self.inputs_container, 1)

        root.addLayout(body, 1)

        # ── Outputs & App Routing ──
        bottom_widget = QWidget()
        bottom_widget.setMinimumHeight(200)
        bottom_outer = QVBoxLayout(bottom_widget)
        bottom_outer.setContentsMargins(0, 0, 0, 0)
        bottom_outer.setSpacing(0)

        bottom_container = QHBoxLayout()
        bottom_container.setContentsMargins(20, 8, 20, 0)
        bottom_container.setSpacing(20)
        
        # Outputs Assignment Panel
        out_frame = QFrame()
        out_frame.setObjectName("routingPanel")
        o_layout = QVBoxLayout(out_frame)
        o_layout.setContentsMargins(20, 20, 20, 20)
        o_title = QLabel("MASTER MIXES")
        o_title.setObjectName("sectionLabel")
        o_layout.addWidget(o_title)
        o_layout.addSpacing(10)
        
        # Monitor row
        mon_row = QHBoxLayout()
        mon_lbl = QLabel("Monitor:")
        mon_lbl.setStyleSheet("color: #e0e0ee; font-size: 11px; font-weight: bold;")
        self.mon_out_combo = QComboBox()
        self.mon_out_combo.currentIndexChanged.connect(lambda idx: self._on_mix_out_change("Monitor", self.mon_out_combo.itemData(idx)))
        mon_row.addWidget(mon_lbl)
        mon_row.addWidget(self.mon_out_combo, 1)
        o_layout.addLayout(mon_row)
        
        # Monitor Master Slider
        self.mon_master_slider = QSlider(Qt.Orientation.Horizontal)
        self.mon_master_slider.setRange(0, 100)
        self.mon_master_slider.setFixedHeight(20)
        self.mon_master_slider.valueChanged.connect(lambda v: self._on_master_vol_change("Monitor", v))
        o_layout.addWidget(self.mon_master_slider)
        o_layout.addSpacing(10)

        # Stream row — always routes to the WaveLinux virtual device (for OBS)
        str_row = QHBoxLayout()
        str_lbl = QLabel("Stream:")
        str_lbl.setStyleSheet("color: #e0e0ee; font-size: 11px; font-weight: bold;")
        str_row.addWidget(str_lbl)
        self.str_out_label = QLabel("WaveLinux Stream (Virtual — use in OBS)")
        self.str_out_label.setStyleSheet("color: #7000ff; font-size: 11px; font-weight: 600;")
        str_row.addWidget(self.str_out_label, 1)
        # Hidden combo for compatibility with save/load code
        self.str_out_combo = QComboBox()
        self.str_out_combo.hide()
        o_layout.addLayout(str_row)

        # Stream Master Slider + Clipguard (Wave Link-style auto-limiter)
        str_master_row = QHBoxLayout()
        self.str_master_slider = QSlider(Qt.Orientation.Horizontal)
        self.str_master_slider.setRange(0, 100)
        self.str_master_slider.setFixedHeight(20)
        self.str_master_slider.valueChanged.connect(lambda v: self._on_master_vol_change("Stream", v))
        str_master_row.addWidget(self.str_master_slider, 1)

        self.clipguard_btn = QPushButton("🛡 Clipguard")
        self.clipguard_btn.setObjectName("clipguardBtn")
        self.clipguard_btn.setCheckable(True)
        if self.engine.effect_available('limiter'):
            self.clipguard_btn.setToolTip("Enable a limiter on the Stream bus so your broadcast never clips")
            self.clipguard_btn.clicked.connect(self._on_clipguard_toggle)
        else:
            self.clipguard_btn.setEnabled(False)
            self.clipguard_btn.setToolTip(
                "Install swh-plugins (fast_lookahead_limiter_1913) to enable Clipguard."
            )
        str_master_row.addWidget(self.clipguard_btn)
        o_layout.addLayout(str_master_row)

        bottom_container.addWidget(out_frame, 1)

        # App Routing Dialog (Settings)
        self.settings_dialog = QDialog(self)
        self.settings_dialog.setWindowTitle("Settings - App Routing")
        self.settings_dialog.setMinimumSize(600, 400)
        self.settings_dialog.setStyleSheet(STYLESHEET)
        
        sd_layout = QVBoxLayout(self.settings_dialog)
        
        r_title = QLabel("APP ROUTING")
        r_title.setObjectName("sectionLabel")
        sd_layout.addWidget(r_title)
        sd_layout.addSpacing(10)

        self.routing_scroll = QScrollArea()
        self.routing_scroll.setWidgetResizable(True)
        self.routing_scroll.setFrameShape(QFrame.Shape.NoFrame)
        self.routing_scroll.setStyleSheet("background: transparent;")
        
        self.routing_container = QWidget()
        self.routing_layout = QVBoxLayout(self.routing_container)
        self.routing_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        self.routing_layout.setContentsMargins(0, 0, 0, 0)
        
        self.routing_scroll.setWidget(self.routing_container)
        sd_layout.addWidget(self.routing_scroll, 1)

        # Hidden Channels section
        sd_layout.addSpacing(16)
        hidden_title = QLabel("HIDDEN CHANNELS")
        hidden_title.setObjectName("sectionLabel")
        sd_layout.addWidget(hidden_title)
        sd_layout.addSpacing(6)

        self.hidden_list_container = QWidget()
        self.hidden_list_layout = QVBoxLayout(self.hidden_list_container)
        self.hidden_list_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        self.hidden_list_layout.setContentsMargins(0, 0, 0, 0)
        self.hidden_list_layout.setSpacing(4)
        sd_layout.addWidget(self.hidden_list_container)

        bottom_outer.addLayout(bottom_container)

        # ── Status Bar ──
        status = QFrame()
        status.setObjectName("statusBar")
        s_layout = QHBoxLayout(status)
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

    def _refresh(self):
        """Update UI to match PipeWire state without destroying everything.
        Cheap path: skip everything when the window is minimised to tray —
        the UI isn't visible, and we don't need to poll PipeWire 30×/min
        to keep a hidden window in sync."""
        if self.tray is not None and not self.isVisible():
            return

        try:
            snap = self.engine.create_snapshot()
            mics = self.engine.get_hardware_inputs(snap=snap)
            vsinks = self.engine.get_virtual_sinks(snap=snap)
            apps = self.engine.get_sink_inputs(snap=snap)
            all_sinks = self.engine.get_all_sinks(snap=snap)

            # pw-dump returning nothing is usually PipeWire mid-restart.
            # Show status, keep existing widget state, and let the next
            # tick reconcile rather than tearing the UI down.
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

            # Sort by the persistent channel order so reorder arrows stick.
            # Unseen nodes go to the end (keeps hot-plugged devices visible).
            order_index = {nm: i for i, nm in enumerate(self.channel_order)}
            sorted_nodes = sorted(
                (mics + vsinks),
                key=lambda n: order_index.get(n.name, len(order_index) + 1),
            )
            for node in sorted_nodes:
                pw_id = node.pw_id
                current_node_ids.add(pw_id)
                nname = node.name

                is_hidden = nname in self.hidden_nodes
                if is_hidden and not self.show_hidden:
                    if pw_id in self.channel_widgets:
                        self.channel_widgets[pw_id].hide()
                    continue

                # Create submix routes if they don't exist. Pass the snapshot
                # so we don't re-run `pactl list modules` four times per tick.
                self.engine.route_input_to_submix(pw_id, nname, node.media_class, "Monitor", snap=snap)
                self.engine.route_input_to_submix(pw_id, nname, node.media_class, "Stream", snap=snap)

                mon_state = dict(self.submix_state.get(f"{nname}_Monitor", {'vol': 1.0, 'mute': False}))
                str_state = dict(self.submix_state.get(f"{nname}_Stream", {'vol': 1.0, 'mute': False}))

                # State sync: on first tick after startup (or after a PipeWire
                # restart), push our saved config into PipeWire so mutes/volumes
                # survive across sessions. After that, let PipeWire's live state
                # overlay so external tools (pavucontrol, media keys) reflect.
                if not hasattr(self, '_synced_nodes'):
                    self._synced_nodes = set()

                sync_key = nname
                owner_mon = self.engine.submix_loopbacks.get(f"{pw_id}->Monitor")
                owner_str = self.engine.submix_loopbacks.get(f"{pw_id}->Stream")

                if sync_key not in self._synced_nodes:
                    # Sink-inputs may not exist yet; wait until both loopbacks appear.
                    if owner_mon is not None and owner_str is not None:
                        self.engine.set_submix_volume(pw_id, "Monitor", mon_state['vol'])
                        self.engine.set_submix_mute(pw_id, "Monitor", mon_state.get('mute', False))
                        self.engine.set_submix_volume(pw_id, "Stream", str_state['vol'])
                        self.engine.set_submix_mute(pw_id, "Stream", str_state.get('mute', False))
                        self._synced_nodes.add(sync_key)
                else:
                    # After initial push, overlay live PipeWire state into the UI
                    for mix_name, state in (("Monitor", mon_state), ("Stream", str_state)):
                        owner = self.engine.submix_loopbacks.get(f"{pw_id}->{mix_name}")
                        if owner is None:
                            continue
                        live = live_by_owner.get(str(owner))
                        if live is not None:
                            state['vol'], state['mute'] = live

                if pw_id not in self.channel_widgets:
                    # Create new widget
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

                    if ch_type == "Virtual":
                        rem_btn = QPushButton("❌ Remove")
                        rem_btn.setObjectName("removeBtn")
                        rem_btn.clicked.connect(lambda checked, sn=nname: self._remove_sink(sn))
                        strip.layout().addWidget(rem_btn)

                strip = self.channel_widgets[pw_id]
                # Keep the strip's pw_id fresh across PipeWire restarts — the
                # node.name stays stable but the numeric id can change.
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
                
                # Update FX button active state
                is_any_fx = False
                for fx in self.engine.get_available_effects():
                    if self.engine.is_effect_active(str(pw_id), fx['id']):
                        is_any_fx = True
                        break
                
                strip.fx_btn.setProperty("active", "true" if is_any_fx else "false")

            # Reap widgets (and meters) whose node has disappeared (unplugged
            # mic, removed sink, PipeWire restart).
            for stale in [pid for pid in self.channel_widgets if pid not in current_node_ids]:
                widget = self.channel_widgets.pop(stale)
                widget.setParent(None)
                widget.deleteLater()
                self.engine.remove_node_routing(stale)
                meter = self.meters.pop(stale, None)
                if meter is not None:
                    meter.stop()

            # 2. Update App Routing (Persistent & Grouped)
            # Map app_name -> list of active indices
            apps_by_name = {}
            for app in apps:
                app_name = app.get('app_name') or app.get('binary') or "Unknown App"
                if app_name not in apps_by_name:
                    apps_by_name[app_name] = []
                idx = app.get('index')
                if idx:
                    apps_by_name[app_name].append(idx)
                    # Apply persistent routing immediately if new instance
                    preferred_sink = self.app_routing.get(app_name)
                    if preferred_sink and app.get('sink') != preferred_sink:
                        self.engine.move_app_to_sink(idx, preferred_sink)

            # Show ALL known apps (including offline ones from config)
            all_display_apps = set(apps_by_name.keys()) | set(self.app_routing.keys())
            
            for app_name in all_display_apps:
                active_indices = apps_by_name.get(app_name, [])
                current_sink = None
                if active_indices:
                    # Find sink for first active instance
                    for a in apps:
                        if a.get('index') == active_indices[0]:
                            current_sink = a.get('sink')
                            break
                else:
                    current_sink = self.app_routing.get(app_name)

                if app_name not in self.app_widgets:
                    row = AppRoutingRow(app_name, self.engine, all_sinks)
                    self.app_widgets[app_name] = row
                    self.routing_layout.addWidget(row)
                
                self.app_widgets[app_name].update_state(active_indices, all_sinks, current_sink)

            # Cleanup widgets for apps we never want to see again (not in config and not active)
            for name in list(self.app_widgets.keys()):
                if name not in all_display_apps:
                    self.app_widgets[name].setParent(None)
                    self.app_widgets[name].deleteLater()
                    del self.app_widgets[name]

            # 3. Update Monitor Mix hardware dropdown (Stream is fixed to virtual)
            combo = self.mon_out_combo
            if not combo.view().isVisible():  # Don't update while user is looking at it
                curr_data = combo.currentData()
                combo.blockSignals(True)
                combo.clear()
                combo.addItem("None (Disconnected)", None)
                for s in all_sinks:
                    # Only show real hardware outputs — never WaveLinux
                    # internal sinks, null-sinks, or other virtual devices.
                    if s['name'].startswith('alsa_output.'):
                        combo.addItem(PipeWireEngine.friendly_name(s['name']), s['name'])
                
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
        # value is 0-100
        vol = value / 100.0
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

    def _on_clipguard_toggle(self):
        enable = self.clipguard_btn.isChecked()
        if enable:
            ok = self.engine.apply_clipguard("Stream", True)
            if not ok:
                self.clipguard_btn.setChecked(False)
                QMessageBox.warning(
                    self, "Clipguard",
                    "Could not start the Stream limiter — check "
                    "~/.config/wavelinux/fx-logs/limiter-mix_stream.log. "
                    "swh-plugins (fast_lookahead_limiter_1913) is required."
                )
        else:
            self.engine.apply_clipguard("Stream", False)
        self.schedule_save()



    def load_config(self):
        if not os.path.exists(self.config_path):
            # Create standard mixes
            self.engine.create_output_mix("Monitor")
            self.engine.create_output_mix("Stream")
            
            # Default Monitor to system default
            def_sink = self.engine.get_default_sink()
            if def_sink:
                self.engine.route_mix_to_hardware("Monitor", def_sink)
            return

        try:
            with open(self.config_path, 'r') as f:
                conf = json.load(f)
                self.submix_state = self._migrate_submix_state(conf.get('submixes', {}))
                self.hidden_nodes = self._migrate_hidden_nodes(conf.get('hidden', []))
                self.app_routing = conf.get('app_routing', {})
                self.virtual_channels = conf.get('channels', [])
                self.channel_order = conf.get('channel_order', []) or []
                self.effect_params = conf.get('effect_params', {}) or {}

                # Re-apply saved effects for each node as they come back
                # online. We can only act on user virtual sinks here because
                # mic pw_ids aren't known yet; _refresh will pick up the
                # effects for those on first tick via is_effect_active.
                for node_name, effects in self.effect_params.items():
                    # Don't auto-enable — parameters are just remembered.
                    # Enabling happens the next time the user toggles ON.
                    pass

                # Create standard mixes (always needed)
                self.engine.create_output_mix("Monitor")
                self.engine.create_output_mix("Stream")

                # Restore clipguard if it was on last run.
                if conf.get('clipguard'):
                    self.engine.apply_clipguard("Stream", True)
                    self.clipguard_btn.setChecked(True)

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
        """Drop legacy entries keyed by the ephemeral pw_id (e.g. '42_Monitor').
        New keys use PipeWire node.name (e.g. 'wavelinux_game_Monitor',
        'alsa_input.pci-..._Monitor'), which survives a PipeWire restart."""
        if not isinstance(raw, dict):
            return {}
        clean = {}
        for key, val in raw.items():
            if not isinstance(key, str) or '_' not in key:
                continue
            prefix = key.rsplit('_', 1)[0]
            try:
                int(prefix)
                # Legacy pw_id key — drop it rather than carry junk forward.
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
        try:
            clipguard = self.clipguard_btn.isChecked()
        except AttributeError:
            clipguard = False
        conf = {
            'monitor_hw': self.mon_out_combo.currentData(),
            'stream_hw': self.str_out_combo.currentData(),
            'channels': self.virtual_channels,
            'submixes': self.submix_state,
            'hidden': list(self.hidden_nodes),
            'app_routing': self.app_routing,
            'clipguard': clipguard,
            'channel_order': self.channel_order,
            'effect_params': self.effect_params,
        }
        try:
            tmp = self.config_path + '.tmp'
            with open(tmp, 'w') as f:
                json.dump(conf, f, indent=4)
            os.replace(tmp, self.config_path)
        except Exception as e:
            logging.error(f"Error saving config: {e}")



    def forget_app(self, app_name):
        """Drop persisted routing for an offline app so it stops cluttering
        the panel. Running apps are allowed to come back because they'll
        just reappear on the next refresh."""
        if app_name in self.app_routing:
            del self.app_routing[app_name]
        widget = self.app_widgets.pop(app_name, None)
        if widget is not None:
            widget.setParent(None)
            widget.deleteLater()
        self.save_config()
        self._refresh()

    # ── Channel reorder / rename ──────────────────────────────────

    def move_channel(self, node_name, delta):
        """Move a channel left (-1) or right (+1) in the persistent order."""
        # Seed the order with every known node so the visible strips and
        # the persisted list stay in lockstep.
        order = list(self.channel_order)
        if node_name not in order:
            order.append(node_name)
        # Make sure all currently-visible channels are in the order list;
        # anything previously unseen goes to the end.
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
        """Re-home the existing ChannelStrip widgets in persistent order."""
        # Detach everything from the HBox, then re-add in order.
        widgets = list(self.channel_widgets.values())
        for w in widgets:
            self.input_layout.removeWidget(w)
        name_to_widget = {w.node_name: w for w in widgets if w.node_name}
        for nm in self.channel_order:
            w = name_to_widget.pop(nm, None)
            if w is not None:
                self.input_layout.addWidget(w)
        # Anything left over (no order entry yet) goes to the end.
        for w in name_to_widget.values():
            self.input_layout.addWidget(w)

    def rename_channel(self, old_node_name):
        """Rename a user-created virtual channel in place. Hardware mic
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

        # Strip's pw_id is about to change — drop the widget so _refresh
        # re-creates it with the new label.
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
        for mix in ("Monitor", "Stream"):
            k_old = f"{old_name}_{mix}"
            if k_old in self.submix_state:
                self.submix_state[f"{new_name}_{mix}"] = self.submix_state.pop(k_old)
        k_gain_old = f"{old_name}_gain"
        if k_gain_old in self.submix_state:
            self.submix_state[f"{new_name}_gain"] = self.submix_state.pop(k_gain_old)
        if old_name in self.hidden_nodes:
            self.hidden_nodes.discard(old_name)
            self.hidden_nodes.add(new_name)
        if old_name in self.effect_params:
            self.effect_params[new_name] = self.effect_params.pop(old_name)
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
        if self.tray is not None and self.tray.isVisible():
            try:
                self.tray.showMessage(title, body, self.tray_icon_obj, 3000)
            except Exception:
                pass
        logging.info(f"{title}: {body}")

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
