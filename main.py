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

from PyQt6.QtWidgets import (
    QApplication, QMainWindow, QWidget, QVBoxLayout, QHBoxLayout,
    QLabel, QSlider, QPushButton, QFrame, QScrollArea, QDialog,
    QLineEdit, QDialogButtonBox, QComboBox, QMessageBox, QSystemTrayIcon,
    QMenu, QInputDialog, QProgressBar, QSizePolicy, QTabWidget,
    QSpinBox, QCheckBox
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

        # The user's *intent*: which effects they last had ON. We surface
        # this in the dialog (tick the toggles) regardless of whether the
        # filter-chain is actually running right now, because the chain
        # might have died (PipeWire restart, stage crash) without the user
        # touching anything. This is what makes "turn ON, click Done,
        # reopen" show ON instead of OFF.
        win = self._main_window_static(parent)
        self._saved_effects = set(
            (win.active_effects.get(self.node_name, []) if win else [])
        )

        # If we have saved chain state but it isn't currently running on
        # the engine, try to spawn it now so what the dialog displays
        # matches what's audible.
        if self._saved_effects and not self.engine.is_channel_fx_running(self.node_name):
            params = (win.effect_params.get(self.node_name, {})
                      if win else {})
            self.engine.set_channel_fx(
                self.node_name, self.capture_target,
                list(self._saved_effects), params,
            )

        # Slider drags fire valueChanged on every pixel; tearing down and
        # respawning the whole chain at that rate would be unusable. Instead
        # debounce param-driven rebuilds to ~150 ms, which feels live but
        # only spawns once the user pauses.
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

        # Surface "this stage failed to start" state once the dialog
        # has finished constructing every toggle. Done after the layout
        # so toggles that were ticked by saved-intent but aren't actually
        # running get the red border + log-path tooltip immediately.
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
        # Brand-colour the toggle when ON so the dialog reads at a glance.
        # `:checked` is the standard Qt pseudo-state for QPushButton
        # toggles, paired with `setCheckable(True)` above.
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
        # Saved intent wins over live state: if the user previously turned
        # this ON and the chain isn't running (because a stage crashed
        # / PipeWire restarted / the spawn failed quietly), the toggle
        # still shows ON so the user can see their saved settings and
        # we can attempt a respawn.
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
        win = self.window().parent()
        # self.window() is the dialog itself; walk through parent() to main.
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

    # ── Chain rebuild (one path, three triggers) ───────────────────
    #
    # Toggling an effect, picking a preset, or dragging a slider all end
    # up calling `_rebuild_chain` so the channel's filter-chain is exactly
    # the set of "ON" effects with their current parameter values. That
    # also triggers a refresh on the main window so the submix loopbacks
    # get re-routed through (or around) the new bus immediately.

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
        """Snapshot every effect's current slider values, even effects that
        aren't ON right now. Persisting all of them means flipping an effect
        back ON later resumes from the user's last-tweaked values rather
        than the canned defaults."""
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
        self.engine.set_channel_fx(
            self.node_name, self.capture_target, wanted, params_map,
        )
        # Persist on the main-window state objects so a restart re-applies.
        self._save_chain_state(wanted, params_map)
        # Force a routing pass so the loopbacks pick up the new source.
        win = self._main_window()
        if win is not None and hasattr(win, '_request_reroute'):
            win._request_reroute(self.node_name)
        # Update each toggle's diagnostic state — failed stages get a
        # red border + tooltip pointing at the per-stage log so the user
        # can tell apart "feature off" from "feature broken".
        self._refresh_toggle_status()

    def _refresh_toggle_status(self):
        """Annotate each toggle with the current chain state. A 'failed'
        stage means we tried to spawn it but it died — usually a missing
        plugin or a malformed config — and the tooltip points at the log.
        Running stages get the brand-blue checked style; inactive ones
        get the default. Done as a styleSheet override so it composes
        with the base toggle stylesheet."""
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
        """Mirror the new chain into the main window's persistence buckets:
        active_effects keeps the ordered list, effect_params keeps every
        slider position so it survives a session restart."""
        win = self._main_window()
        if win is None:
            return
        if effects:
            win.active_effects[self.node_name] = list(effects)
        else:
            win.active_effects.pop(self.node_name, None)
        # Always save params even for OFF effects so toggling back ON
        # resumes from the user's last tweak.
        if params_map:
            stash = win.effect_params.setdefault(self.node_name, {})
            for fid, vals in params_map.items():
                stash[fid] = dict(vals)
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
        self._rebuild_chain()

    def _apply_preset(self, effect_id, values):
        """A preset slams every slider in the effect panel to a known good
        starting point, then rebuilds the chain so the change is audible."""
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
        self._rebuild_chain()

    def _on_param_changed(self, effect_id, slider, value_lbl):
        pmin = float(slider.property("pmin"))
        pmax = float(slider.property("pmax"))
        suffix = slider.property("psuffix") or ""
        val = pmin + (pmax - pmin) * (slider.value() / 1000.0)
        value_lbl.setText(self._fmt_value(val, suffix))
        # Debounce: a slider drag fires valueChanged for every pixel, and
        # set_channel_fx tears down + respawns the whole chain. Without
        # debouncing the chain would be in a restart loop the entire time
        # the user is dragging. 150 ms feels live but only commits the
        # change once the slider settles. The "Done" button flushes any
        # pending rebuild on close.
        self._param_timer.start()


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
        layout.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        layout.setContentsMargins(8, 8, 8, 8)
        layout.setSpacing(4)

        # Icon + (optional) FX indicator. Wave Link keeps the header almost
        # empty — everything extra lives behind the right-click menu.
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

        # Peak meter sits *between* the name and the faders so it's clearly
        # "this channel's level", not associated with a specific mix.
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
        self.link_btn.setToolTip("Link the Headphones and Stream faders")
        self.link_btn.clicked.connect(self._on_link_toggle)
        link_row.addWidget(self.link_btn)
        link_row.addStretch()
        layout.addLayout(link_row)

        # Two-column fader layout (Headphones + Stream).
        faders_row = QHBoxLayout()
        faders_row.setSpacing(10)

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
        self.mon_mute.setToolTip("Mute in Headphones mix")
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

        # fx_btn kept as a hidden widget so existing code paths that touch
        # `strip.fx_btn.setProperty(...)` can keep functioning. The real FX
        # entry point is the right-click menu.
        self.fx_btn = QPushButton()
        self.fx_btn.setVisible(False)

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
            # Snap Stream to Monitor so they start at the same place.
            self.str_slider.setValue(self.mon_slider.value())
        # Persist regardless of direction — the previous version only saved
        # when linking ON, so unlinking was silently lost.
        self._save_link_state(linked)

    def _save_link_state(self, linked):
        win = self.window()
        if hasattr(win, 'submix_state') and self.node_name:
            win.submix_state[f"{self.node_name}_linked"] = bool(linked)
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

    def fx_capture_target(self):
        """The PipeWire source name the FX chain's first stage should
        pull audio from. For mics that's the mic itself; for virtual
        sinks (Audio/Sink), it's the sink's monitor node."""
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
        """Right-click context menu for channel-strip actions.
        Lives here instead of on visible buttons so the strip stays as
        clean as Wave Link's."""
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

        # Optional: VST / LV2 hosting via Carla. Shown only if `carla` is
        # on $PATH; otherwise we'd be advertising broken functionality.
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
        """Spawn Carla so the user can host a VST/VST3/LV2 plugin and
        route it into this channel. We don't supervise the Carla process;
        the user wires its input/output in Carla itself. This keeps us
        out of the business of hosting proprietary plugin formats."""
        try:
            subprocess.Popen(["carla"])
        except FileNotFoundError:
            QMessageBox.information(
                self, "Carla not found",
                "Install Carla (e.g. `sudo pacman -S carla`) to host VST3 / "
                "LV2 plugins. WaveLinux doesn't host VST3 directly — it "
                "bridges to Carla, which does."
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
        """Update UI from stored state. `is_hidden` is informational; the
        strip is hidden/shown by the parent window, not by this widget."""
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

        # Forget is always available. For an offline row it deletes the
        # row outright; for an online app it nukes the saved routing /
        # last-seen entries so the next refresh re-discovers the app
        # under whatever (potentially better) name resolution gives us.
        # That second case is the escape hatch when an app is stuck under
        # an old, generic identification like "audio-src" because the
        # name was first cached before .desktop discovery improved.
        self.forget_btn.setEnabled(True)
        if is_active:
            self.forget_btn.setToolTip(
                "Forget this app and everything we've saved about it. "
                "If it's still playing audio it will reappear under its "
                "freshly-resolved name."
            )
        else:
            self.forget_btn.setToolTip(
                "Forget this app so it stops showing up in the list."
            )

        # Update combo box. Users can send an app to:
        #   • any hardware output (ALSA / Bluetooth / JACK)
        #   • any user-created WaveLinux channel (starred)
        # Internal mix/source devices stay hidden.
        if not self.combo.view().isVisible():
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
        self.virtual_channels = []  # list of names
        # All user-facing state is keyed by PipeWire node.name (stable across
        # PipeWire restarts); pw_id is only used when talking to the engine.
        self.hidden_nodes = set()      # {node.name}
        self.show_hidden = False
        self.effect_params = {}        # node.name -> effect_id -> {param_key: value}
        self.active_effects = {}       # node.name -> [effect_id, ...] — restored each run
        self._effects_applied = set()  # node.name keys we've already reconciled this session
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
        self._refresh_advanced_tab()
        self.settings_dialog.show()
        self.settings_dialog.raise_()

    def _build_advanced_tab(self):
        """Fine-grained knobs that most users never need. Each setting is
        applied immediately and persisted to config.json."""
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

    def _request_reroute(self, node_name):
        """The FX dialog calls this after rebuilding a channel's chain.
        Drop the channel out of the synced-set so the next refresh tick
        re-pushes saved volume/mute, and trigger the debounced refresh so
        submix loopbacks pick up the new (or removed) FX virtual-source
        without waiting up to two seconds for the poll timer."""
        if hasattr(self, '_synced_nodes'):
            self._synced_nodes.discard(node_name)
        self._event_refresh_timer.start()

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

        # Inputs area — wrapped in a horizontal scroll so the strips stay
        # usable below 1200 px window width. The inner widget keeps its
        # natural sizeHint (strip count × ~160 px) instead of being
        # squashed by the scroll area, which is why `setWidgetResizable`
        # is False here.
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
        body.addWidget(self.inputs_scroll, 1)

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
        o_title = QLabel("MASTER")
        o_title.setObjectName("sectionLabel")
        o_layout.addWidget(o_title)
        o_layout.addSpacing(10)

        # Headphones row (what you hear).
        mon_row = QHBoxLayout()
        mon_lbl = QLabel("🎧 Headphones")
        mon_lbl.setObjectName("masterMixLabel")
        self.mon_out_combo = QComboBox()
        self.mon_out_combo.setToolTip("Pick the physical output you listen on")
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
        o_layout.addSpacing(10)

        # Stream row (what OBS / your audience hears). Always routes to the
        # dedicated virtual device so there's one stable thing to pick in
        # OBS — matching Wave Link's behaviour.
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

        self.clipguard_btn = QPushButton("🛡")
        self.clipguard_btn.setObjectName("clipguardBtn")
        self.clipguard_btn.setCheckable(True)
        self.clipguard_btn.setFixedWidth(36)
        if self.engine.effect_available('limiter'):
            self.clipguard_btn.setToolTip("Clipguard — limits the Stream mix so your broadcast never clips")
            self.clipguard_btn.clicked.connect(self._on_clipguard_toggle)
        else:
            self.clipguard_btn.setEnabled(False)
            self.clipguard_btn.setToolTip(
                "Install swh-plugins (fast_lookahead_limiter_1913) to enable Clipguard."
            )
        str_master_row.addWidget(self.clipguard_btn)
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

                # First time we've ever seen this node: pick safe defaults.
                # Mics default to MUTED in Monitor so a fresh install does NOT
                # immediately scream the user's voice back at them through their
                # headphones / speakers (which on a 4-monitor setup turns into a
                # screaming feedback loop). Stream stays unmuted because that
                # mix exists for a recording target (OBS) where 'no audio'
                # would be more confusing than 'audio'.
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
                    # After initial push, overlay live PipeWire state into the
                    # UI *and* persist it. Without writing back, an external
                    # mute (pavucontrol, media keys, our own button) showed in
                    # the UI but was never saved, so a restart re-pushed the
                    # stale un-muted state — which is exactly what made fresh
                    # installs blast the mic back at the user.
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
                        state['vol'], state['mute'] = live_vol, live_mute
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
                
                # Re-apply any saved effects the first time this node shows
                # up in the session. `active_effects` is keyed by node.name
                # (stable across PipeWire restarts); filter-chain processes
                # live in-memory on the engine, so we re-spawn the chain
                # via set_channel_fx. The next routing pass below will
                # then thread the loopbacks through the chain's output.
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
                # The mic is gone, so its FX chain is now processing nothing
                # — kill the stage processes too so they don't sit idle.
                # The dropped node will be re-discovered (and its chain
                # re-applied via _effects_applied) if it comes back.
                if stale_nname:
                    self.engine.clear_channel_fx(stale_nname)
                    self._effects_applied.discard(stale_nname)
                meter = self.meters.pop(stale, None)
                if meter is not None:
                    meter.stop()

            # Let the strips container compute its natural width so the
            # enclosing horizontal scroll area can size / scroll correctly.
            self.inputs_container.adjustSize()

            # 2. Update App Routing (Persistent & Grouped)
            # Map app_name -> list of active indices
            apps_by_name = {}
            now = int(time.time())
            for app in apps:
                app_name = app.get('app_name') or app.get('binary') or "Unknown App"
                if app_name not in apps_by_name:
                    apps_by_name[app_name] = []
                idx = app.get('index')
                if idx:
                    apps_by_name[app_name].append(idx)
                    # Touch the last-seen stamp so this app's saved routing
                    # survives another prune cycle.
                    self.app_last_seen[app_name] = now
                    # Apply persistent routing immediately if new instance
                    preferred_sink = self.app_routing.get(app_name)
                    if preferred_sink and app.get('sink') != preferred_sink:
                        self.engine.move_app_to_sink(idx, preferred_sink)

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
            )
            
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

            # 3. Update Monitor output dropdown (Stream is fixed to virtual).
            # Use the PipeWire Description field so Bluetooth sinks show real
            # model names ('Sony WH-1000XM4') instead of garbled 'Bd 10 1'
            # from the ALSA node.name.
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
                    "Could not start the Stream limiter. Check "
                    "~/.config/wavelinux/fx-logs/limiter-mix_stream.log "
                    "for the spawn error. WaveLinux ships a builtin "
                    "clamp-based fallback for systems without swh-plugins, "
                    "so this typically only happens if PipeWire itself "
                    "failed to launch the filter-chain."
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
                self.app_routing = conf.get('app_routing', {}) or {}
                self.app_last_seen = {
                    k: int(v) for k, v in (conf.get('app_last_seen', {}) or {}).items()
                    if isinstance(k, str) and isinstance(v, (int, float))
                }
                self.app_prune_days = int(conf.get('app_prune_days', self.app_prune_days) or 14)
                self._prune_stale_apps()
                self.virtual_channels = conf.get('channels', [])
                self.channel_order = conf.get('channel_order', []) or []
                self.effect_params = conf.get('effect_params', {}) or {}
                self.active_effects = {
                    k: list(v) for k, v in (conf.get('active_effects', {}) or {}).items()
                    if isinstance(v, list)
                }

                # Saved effect parameters are loaded into self.effect_params
                # / self.active_effects above. The actual chain spawn happens
                # in _refresh once each node's pw_id is known, via
                # set_channel_fx — see the "_effects_applied" gate there.

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
            'active_effects': self.active_effects,
            'app_last_seen': self.app_last_seen,
            'app_prune_days': self.app_prune_days,
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
        self.app_last_seen.pop(app_name, None)
        widget = self.app_widgets.pop(app_name, None)
        if widget is not None:
            widget.setParent(None)
            widget.deleteLater()
        self.save_config()
        self._refresh()

    def _prune_stale_apps(self):
        """On load, drop saved app_routing entries we haven't seen in
        `app_prune_days`. Apps get their last_seen stamp refreshed every
        tick they're active, so quietly-running notification-only apps
        (Discord background, Slack, etc.) keep their slot. The 'offline'
        forever-clutter problem shows up when you install an app, route
        it once, and never open it again — that's what this reaps."""
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
        # Apps whose last_seen is older than the cutoff but which never had a
        # saved routing also get reaped — without this, the panel would grow
        # forever with apps the user opened once two years ago.
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
        if old_name in self.active_effects:
            self.active_effects[new_name] = self.active_effects.pop(old_name)
        self._effects_applied.discard(old_name)
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
