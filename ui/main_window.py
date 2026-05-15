"""Main window shell builder."""

from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QLabel,
    QHBoxLayout,
    QPushButton,
    QFrame,
    QScrollArea,
    QVBoxLayout,
    QWidget,
)

from ui.mixer.mixer_panel import MixerPanelController
from ui.routing.app_routing_panel import AppRoutingPanelController
from ui.settings import build_settings_dialog


def build_main_window(window) -> None:
    window._setup_tray()
    central_scroll = QScrollArea()
    central_scroll.setWidgetResizable(True)
    central_scroll.setObjectName("centralScroll")
    window.setCentralWidget(central_scroll)

    central = QWidget()
    central.setObjectName("central")
    central_scroll.setWidget(central)

    root = QVBoxLayout(central)
    root.setContentsMargins(0, 0, 0, 0)
    root.setSpacing(0)

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

    window.settings_btn = QPushButton("⚙️ Settings")
    window.settings_btn.setObjectName("showHiddenBtn")
    window.settings_btn.setToolTip("Open App Routing settings")
    window.settings_btn.clicked.connect(window._open_settings)
    h_layout.addWidget(window.settings_btn)

    window.add_btn = QPushButton("+ Add Channel")
    window.add_btn.setObjectName("addBtn")
    window.add_btn.clicked.connect(window._on_add_channel)
    h_layout.addWidget(window.add_btn)

    root.addWidget(header)

    window._mixer_panel = MixerPanelController(window)
    window._mixer_panel.build(root)
    window._app_routing_panel = AppRoutingPanelController(window)

    build_settings_dialog(window)

    status = QFrame()
    status.setObjectName("statusBar")
    s_layout = QHBoxLayout(status)
    s_layout.setContentsMargins(0, 0, 0, 0)
    dot = QLabel("●")
    dot.setObjectName("statusDot")
    s_layout.addWidget(dot)
    window.status_lbl = QLabel("PipeWire connected")
    window.status_lbl.setObjectName("statusLabel")
    s_layout.addWidget(window.status_lbl)
    s_layout.addStretch()
    root.addWidget(status)
