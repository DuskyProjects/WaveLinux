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
    QMenu, QInputDialog
)
from PyQt6.QtCore import Qt, QTimer, QLockFile
from PyQt6.QtGui import QFont, QIcon, QAction

from pipewire_engine import PipeWireEngine
from wavelinux_theme import STYLESHEET


# ── FX Selection Dialog ───────────────────────────────────────────
class FXSelectionDialog(QDialog):
    def __init__(self, node_id, engine, parent=None):
        super().__init__(parent)
        self.node_id = str(node_id)
        self.engine = engine
        self.setWindowTitle("Channel Effects")
        self.setMinimumWidth(350)
        self.setStyleSheet(STYLESHEET)
        
        layout = QVBoxLayout(self)
        layout.setSpacing(15)
        layout.setContentsMargins(20, 20, 20, 20)
        
        title = QLabel("✨ Channel Effects")
        title.setStyleSheet("font-size: 18px; font-weight: bold; color: #fff;")
        layout.addWidget(title)
        
        desc = QLabel("Add professional processing to your audio stream.")
        desc.setStyleSheet("color: #8b8b9e; font-size: 12px;")
        layout.addWidget(desc)
        
        effects = self.engine.get_available_effects()
        for fx in effects:
            fx_frame = QFrame()
            fx_frame.setObjectName("fxItemFrame")
            fx_frame.setStyleSheet("""
                QFrame#fxItemFrame {
                    background: rgba(255,255,255,0.03);
                    border: 1px solid rgba(255,255,255,0.06);
                    border-radius: 10px;
                    padding: 10px;
                }
            """)
            fx_layout = QHBoxLayout(fx_frame)
            
            icon_lbl = QLabel(fx['icon'])
            icon_lbl.setStyleSheet("font-size: 20px;")
            fx_layout.addWidget(icon_lbl)
            
            text_col = QVBoxLayout()
            name_lbl = QLabel(fx['name'])
            name_lbl.setStyleSheet("color: #e0e0ee; font-weight: bold; font-size: 13px;")
            text_col.addWidget(name_lbl)
            
            info_lbl = QLabel(fx['desc'])
            info_lbl.setStyleSheet("color: #6b6b82; font-size: 10px;")
            text_col.addWidget(info_lbl)
            fx_layout.addLayout(text_col, 1)
            
            toggle_btn = QPushButton()
            toggle_btn.setCheckable(True)
            active = self.engine.is_effect_active(self.node_id, fx['id'])
            toggle_btn.setChecked(active)
            toggle_btn.setText("ON" if active else "OFF")
            toggle_btn.setFixedWidth(60)
            
            # Use a closure to capture fx['id']
            toggle_btn.clicked.connect(lambda checked, fid=fx['id'], btn=toggle_btn: self._on_toggle(fid, btn))
            
            fx_layout.addWidget(toggle_btn)
            layout.addWidget(fx_frame)
            
        layout.addStretch()
        
        close_btn = QPushButton("Done")
        close_btn.setObjectName("addBtn") # use primary button style
        close_btn.clicked.connect(self.accept)
        layout.addWidget(close_btn)

    def _on_toggle(self, effect_id, btn):
        if btn.isChecked():
            btn.setText("ON")
            self.engine.apply_effect(self.node_id, effect_id)
        else:
            btn.setText("OFF")
            self.engine.remove_effect(self.node_id, effect_id)


# ── Channel Strip Widget ───────────────────────────────────────────
class ChannelStrip(QFrame):
    """A single mixer channel: icon, name, vertical fader, mute, FX."""

    def __init__(self, node_id, name, ch_type, icon, engine, is_mic=False, parent=None):
        super().__init__(parent)
        self.setObjectName("channelStrip")
        self.setMinimumWidth(160)
        self.setMaximumWidth(180)
        self.setMaximumHeight(320)
        self.node_id = node_id
        self.ch_name = name
        self.ch_type = ch_type
        self.engine = engine
        self.is_mic = is_mic
        self._muted = False
        self._mon_muted = False
        self._str_muted = False

        layout = QVBoxLayout(self)
        layout.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.setSpacing(2)

        # Top row: icon + hide button
        top_row = QHBoxLayout()
        top_row.addStretch()
        icon_lbl = QLabel(icon)
        icon_lbl.setObjectName("channelIcon")
        top_row.addWidget(icon_lbl)
        self.hide_btn = QPushButton("👁")
        self.hide_btn.setObjectName("hideBtn")
        self.hide_btn.setToolTip("Hide this channel")
        self.hide_btn.clicked.connect(lambda: self._request_hide())
        top_row.addWidget(self.hide_btn)
        top_row.addStretch()
        layout.addLayout(top_row)

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

        if is_mic:
            badge = QLabel("RNNoise")
            badge.setObjectName("rnBadge")
            badge.setAlignment(Qt.AlignmentFlag.AlignCenter)
            layout.addWidget(badge)

        layout.addSpacing(4)

        # Sliders layout
        sliders_layout = QVBoxLayout()
        sliders_layout.setSpacing(4)

        # Monitor
        mon_lbl = QLabel("MONITOR")
        mon_lbl.setObjectName("mixLabel")
        sliders_layout.addWidget(mon_lbl)
        
        mon_row = QHBoxLayout()
        self.mon_mute = QPushButton("🔊")
        self.mon_mute.setObjectName("muteBtn")
        self.mon_mute.setFixedSize(24, 24)
        self.mon_mute.clicked.connect(self._on_mon_mute)
        mon_row.addWidget(self.mon_mute)

        self.mon_slider = QSlider(Qt.Orientation.Horizontal)
        self.mon_slider.setRange(0, 150)
        self.mon_slider.setValue(100)
        self.mon_slider.valueChanged.connect(self._on_mon_vol)
        mon_row.addWidget(self.mon_slider)
        
        self.mon_vol_lbl = QLabel("100%")
        self.mon_vol_lbl.setFixedWidth(30)
        self.mon_vol_lbl.setObjectName("volumeLabel")
        mon_row.addWidget(self.mon_vol_lbl)
        sliders_layout.addLayout(mon_row)

        # Stream
        str_lbl = QLabel("STREAM")
        str_lbl.setObjectName("mixLabel")
        sliders_layout.addWidget(str_lbl)

        str_row = QHBoxLayout()
        self.str_mute = QPushButton("🔊")
        self.str_mute.setObjectName("muteBtn")
        self.str_mute.setFixedSize(24, 24)
        self.str_mute.clicked.connect(self._on_str_mute)
        str_row.addWidget(self.str_mute)

        self.str_slider = QSlider(Qt.Orientation.Horizontal)
        self.str_slider.setRange(0, 150)
        self.str_slider.setValue(100)
        self.str_slider.valueChanged.connect(self._on_str_vol)
        str_row.addWidget(self.str_slider)

        self.str_vol_lbl = QLabel("100%")
        self.str_vol_lbl.setFixedWidth(30)
        self.str_vol_lbl.setObjectName("volumeLabel")
        str_row.addWidget(self.str_vol_lbl)
        sliders_layout.addLayout(str_row)

        layout.addLayout(sliders_layout, 1)

        layout.addSpacing(8)

        # FX / RNNoise button
        self.fx_btn = QPushButton("✨ Add FX")
        self.fx_btn.setObjectName("fxBtn")
        self.fx_btn.clicked.connect(self._on_fx_toggle)
        layout.addWidget(self.fx_btn)

    def _on_mon_vol(self, value):
        self.mon_vol_lbl.setText(f"{value}%")
        if self.node_id:
            self.engine.set_submix_volume(self.node_id, "Monitor", value / 100.0)
            win = self.window()
            if hasattr(win, 'submix_state'):
                win.submix_state[f"{self.node_id}_Monitor"] = {'vol': value / 100.0, 'mute': self._mon_muted}
                win.save_config()

    def _on_str_vol(self, value):
        self.str_vol_lbl.setText(f"{value}%")
        if self.node_id:
            self.engine.set_submix_volume(self.node_id, "Stream", value / 100.0)
            win = self.window()
            if hasattr(win, 'submix_state'):
                win.submix_state[f"{self.node_id}_Stream"] = {'vol': value / 100.0, 'mute': self._str_muted}
                win.save_config()

    def _on_mon_mute(self):
        self._mon_muted = not getattr(self, '_mon_muted', False)
        if self.node_id:
            self.engine.set_submix_mute(self.node_id, "Monitor", self._mon_muted)
            win = self.window()
            if hasattr(win, 'submix_state'):
                win.submix_state[f"{self.node_id}_Monitor"] = {'vol': self.mon_slider.value() / 100.0, 'mute': self._mon_muted}
                win.save_config()
                
        self.mon_mute.setText("🔇" if self._mon_muted else "🔊")
        self.mon_mute.setProperty("muted", "true" if self._mon_muted else "false")
        self.mon_mute.style().unpolish(self.mon_mute)
        self.mon_mute.style().polish(self.mon_mute)

    def _on_str_mute(self):
        self._str_muted = not getattr(self, '_str_muted', False)
        if self.node_id:
            self.engine.set_submix_mute(self.node_id, "Stream", self._str_muted)
            win = self.window()
            if hasattr(win, 'submix_state'):
                win.submix_state[f"{self.node_id}_Stream"] = {'vol': self.str_slider.value() / 100.0, 'mute': self._str_muted}
                win.save_config()
                
        self.str_mute.setText("🔇" if self._str_muted else "🔊")
        self.str_mute.setProperty("muted", "true" if self._str_muted else "false")
        self.str_mute.style().unpolish(self.str_mute)
        self.str_mute.style().polish(self.str_mute)

    def _on_fx_toggle(self):
        dlg = FXSelectionDialog(self.node_id, self.engine, self)
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

    def _request_hide(self):
        """Request parent window to hide this channel."""
        win = self.window()
        if hasattr(win, 'hide_node'):
            win.hide_node(self.node_id)
            
    def _request_unhide(self):
        """Request parent window to unhide this channel."""
        win = self.window()
        if hasattr(win, 'unhide_node'):
            win.unhide_node(self.node_id)

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
        
        self.mon_vol_lbl.setText(f"{int(mon_vol * 100)}%")
        self.str_vol_lbl.setText(f"{int(str_vol * 100)}%")
        
        self.mon_mute.setText("🔇" if mon_mute else "🔊")
        self.mon_mute.setProperty("muted", "true" if mon_mute else "false")
        self.mon_mute.style().unpolish(self.mon_mute)
        self.mon_mute.style().polish(self.mon_mute)
        
        self.str_mute.setText("🔇" if str_mute else "🔊")
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
        self.setMinimumSize(900, 650)
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
        self.hidden_nodes = set()
        self.show_hidden = False
        self.config_path = os.path.expanduser("~/.config/wavelinux/config.json")
        os.makedirs(os.path.dirname(self.config_path), exist_ok=True)
        
        self.refresh_timer = QTimer(self)
        self.refresh_timer.timeout.connect(self._refresh)

        self._setup_ui()
        self.load_config()
        self._refresh()
        self.refresh_timer.start(2000)

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

        self.show_hidden_btn = QPushButton("👁  Show Hidden")
        self.show_hidden_btn.setObjectName("showHiddenBtn")
        self.show_hidden_btn.setCheckable(True)
        self.show_hidden_btn.clicked.connect(self._toggle_show_hidden)
        h_layout.addWidget(self.show_hidden_btn)

        self.reset_btn = QPushButton("⚠️ Reset Audio")
        self.reset_btn.setObjectName("resetBtn")
        self.reset_btn.clicked.connect(self._on_emergency_reset)
        h_layout.addWidget(self.reset_btn)

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

        # Inputs Area (No internal scroll)
        self.inputs_container = QWidget()
        self.input_layout = QHBoxLayout(self.inputs_container)
        self.input_layout.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignTop)
        self.input_layout.setSpacing(10)
        body.addWidget(self.inputs_container, 1)

        root.addLayout(body, 1)

        # ── Outputs & App Routing ──
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

        # Stream row
        str_row = QHBoxLayout()
        str_lbl = QLabel("Stream:")
        str_lbl.setStyleSheet("color: #e0e0ee; font-size: 11px; font-weight: bold;")
        self.str_out_combo = QComboBox()
        self.str_out_combo.currentIndexChanged.connect(lambda idx: self._on_mix_out_change("Stream", self.str_out_combo.itemData(idx)))
        str_row.addWidget(str_lbl)
        str_row.addWidget(self.str_out_combo, 1)
        o_layout.addLayout(str_row)

        # Stream Master Slider
        self.str_master_slider = QSlider(Qt.Orientation.Horizontal)
        self.str_master_slider.setRange(0, 100)
        self.str_master_slider.setFixedHeight(20)
        self.str_master_slider.valueChanged.connect(lambda v: self._on_master_vol_change("Stream", v))
        o_layout.addWidget(self.str_master_slider)

        o_layout.addStretch()
        bottom_container.addWidget(out_frame, 1)

        # App Routing Panel
        routing_frame = QFrame()
        routing_frame.setObjectName("routingPanel")
        r_layout = QVBoxLayout(routing_frame)
        r_layout.setContentsMargins(20, 20, 20, 20)
        r_title = QLabel("APP ROUTING")
        r_title.setObjectName("sectionLabel")
        r_layout.addWidget(r_title)
        r_layout.addSpacing(10)

        self.routing_scroll = QScrollArea()
        self.routing_scroll.setWidgetResizable(True)
        self.routing_scroll.setFrameShape(QFrame.Shape.NoFrame)
        self.routing_scroll.setStyleSheet("background: transparent;")
        
        self.routing_container = QWidget()
        self.routing_layout = QVBoxLayout(self.routing_container)
        self.routing_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
        self.routing_layout.setContentsMargins(0, 0, 0, 0)
        
        self.routing_scroll.setWidget(self.routing_container)
        r_layout.addWidget(self.routing_scroll)

        bottom_container.addWidget(routing_frame, 2)
        root.addLayout(bottom_container, 2)

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
        root.addWidget(status)

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
        """Update UI to match PipeWire state without destroying everything."""
        try:
            mics = self.engine.get_hardware_inputs()
            vsinks = self.engine.get_virtual_sinks()
            apps = self.engine.get_sink_inputs()
            all_sinks = self.engine.get_all_sinks()

            if mics is None or vsinks is None:
                self.status_lbl.setText("PipeWire error — is pipewire running?")
                return

            # 1. Update Input Channels (Mics & Virtual Sinks)
            current_node_ids = set()
            
            for node in (mics + vsinks):
                pw_id = node.pw_id
                current_node_ids.add(pw_id)
                
                is_hidden = pw_id in self.hidden_nodes
                if is_hidden and not self.show_hidden:
                    if pw_id in self.channel_widgets:
                        self.channel_widgets[pw_id].hide()
                    continue

                # Create submix routes if they don't exist
                self.engine.route_input_to_submix(pw_id, node.name, node.media_class, "Monitor")
                self.engine.route_input_to_submix(pw_id, node.name, node.media_class, "Stream")
                
                mon_state = self.submix_state.get(f"{pw_id}_Monitor", {'vol': 1.0, 'mute': False})
                str_state = self.submix_state.get(f"{pw_id}_Stream", {'vol': 1.0, 'mute': False})

                if pw_id not in self.channel_widgets:
                    # Create new widget
                    if node in mics:
                        name = PipeWireEngine.friendly_name(node.description)
                        ch_type = "Microphone"
                        icon = "🎤"
                    else:
                        safe_name = node.name.replace('wavelinux_', '')
                        name = safe_name.replace('_', ' ').title()
                        ch_type = "Virtual"
                        icon = "🎵"
                    
                    strip = ChannelStrip(pw_id, name, ch_type, icon, self.engine)
                    self.channel_widgets[pw_id] = strip
                    self.input_layout.addWidget(strip)
                    
                    if ch_type == "Virtual":
                        rem_btn = QPushButton("❌ Remove")
                        rem_btn.setObjectName("removeBtn")
                        rem_btn.clicked.connect(lambda checked, sn=node.name: self._remove_sink(sn))
                        strip.layout().addWidget(rem_btn)

                strip = self.channel_widgets[pw_id]
                strip.show()
                strip.update_from_node(mon_state['vol'], mon_state['mute'], str_state['vol'], str_state['mute'], is_hidden)
                
                # Update FX button active state
                is_any_fx = False
                for fx in self.engine.get_available_effects():
                    if self.engine.is_effect_active(str(pw_id), fx['id']):
                        is_any_fx = True
                        break
                
                strip.fx_btn.setProperty("active", "true" if is_any_fx else "false")
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

            # 3. Update Mix Selection dropdowns
            for combo, mix_name in [(self.mon_out_combo, "Monitor"), (self.str_out_combo, "Stream")]:
                if not combo.view().isVisible(): # Don't update while user is looking at it
                    curr_data = combo.currentData()
                    combo.blockSignals(True)
                    combo.clear()
                    combo.addItem("None (Disconnected)", None)
                    for s in all_sinks:
                        if 'wavelinux_' not in s['name'] and 'rnnoise' not in s['name'].lower():
                            combo.addItem(PipeWireEngine.friendly_name(s['name']), s['name'])
                    
                    idx = combo.findData(curr_data)
                    if idx >= 0:
                        combo.setCurrentIndex(idx)
                    combo.blockSignals(False)

            # 4. Update Master Mix Sliders (mix sinks are addressed by name, not wpctl ID).
            mon_mix = self.engine.output_mixes.get("Monitor")
            if mon_mix and not self.mon_master_slider.isSliderDown():
                v, _ = self.engine.get_sink_volume_by_name(mon_mix.sink_name)
                self.mon_master_slider.blockSignals(True)
                self.mon_master_slider.setValue(int(v * 100))
                self.mon_master_slider.blockSignals(False)

            str_mix = self.engine.output_mixes.get("Stream")
            if str_mix and not self.str_master_slider.isSliderDown():
                v, _ = self.engine.get_sink_volume_by_name(str_mix.sink_name)
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

    def _on_emergency_reset(self):
        reply = QMessageBox.warning(
            self, "Emergency Reset",
            "This will unload ALL WaveLinux audio modules and restart the engine. "
            "Use this if your audio is broken or silent. Proceed?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No
        )
        if reply == QMessageBox.StandardButton.Yes:
            self.engine.full_audio_reset()
            self.load_config()
            self._refresh()

    def load_config(self):
        if not os.path.exists(self.config_path):
            # Default virtual channels if first run
            for name in ["Game", "Music", "Browser", "SFX"]:
                self.engine.create_virtual_sink(name)
                self.virtual_channels.append(name)
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
                self.submix_state = conf.get('submixes', {})
                self.hidden_nodes = set(conf.get('hidden', []))
                self.app_routing = conf.get('app_routing', {})
                self.virtual_channels = conf.get('channels', [])
                
                # Create standard mixes (always needed)
                self.engine.create_output_mix("Monitor")
                self.engine.create_output_mix("Stream")

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
            print(f"Error loading config: {e}")

    def save_config(self):
        conf = {
            'monitor_hw': self.mon_out_combo.currentData(),
            'stream_hw': self.str_out_combo.currentData(),
            'channels': self.virtual_channels,
            'submixes': self.submix_state,
            'hidden': list(self.hidden_nodes),
            'app_routing': self.app_routing
        }
        try:
            with open(self.config_path, 'w') as f:
                json.dump(conf, f, indent=4)
        except Exception as e:
            print(f"Error saving config: {e}")

    def hide_node(self, node_id):
        """Hide a channel strip by node ID."""
        self.hidden_nodes.add(node_id)
        self._refresh()
        
    def unhide_node(self, node_id):
        """Unhide a channel strip by node ID."""
        if node_id in self.hidden_nodes:
            self.hidden_nodes.remove(node_id)
        self._refresh()

    def _toggle_show_hidden(self):
        self.show_hidden = self.show_hidden_btn.isChecked()
        self._refresh()

    def _setup_tray(self):
        self.tray = QSystemTrayIcon(self)
        self.tray.setIcon(self.tray_icon_obj)
        
        menu = QMenu()
        show_act = QAction("Show WaveLinux", self)
        show_act.triggered.connect(self.showNormal)
        menu.addAction(show_act)
        
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
        """Minimize to tray instead of closing."""
        if self.tray.isVisible():
            event.ignore()
            self.hide()
        else:
            self._quit_app()

    def _quit_app(self):
        """Cleanly unload all modules and exit."""
        logging.info("Shutting down WaveLinux...")
        self.refresh_timer.stop()
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
