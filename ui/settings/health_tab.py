"""Health settings tab builder."""

from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QFrame, QHBoxLayout, QLabel, QPushButton, QScrollArea, QVBoxLayout, QWidget


def build_health_tab(window):
    tab = QWidget()
    layout = QVBoxLayout(tab)
    layout.setContentsMargins(16, 16, 16, 16)
    layout.setSpacing(12)

    title = QLabel("HEALTH")
    title.setObjectName("sectionLabel")
    layout.addWidget(title)

    window._system_summary_lbl = QLabel()
    window._system_summary_lbl.setWordWrap(True)
    window._system_summary_lbl.setStyleSheet("color: #e0e0ee; font-size: 13px; font-weight: bold;")
    layout.addWidget(window._system_summary_lbl)

    window._system_runtime_lbl = QLabel()
    window._system_runtime_lbl.setWordWrap(True)
    window._system_runtime_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
    layout.addWidget(window._system_runtime_lbl)

    btn_row = QHBoxLayout()
    window._rerun_system_check_btn = QPushButton("Re-run System Check")
    window._rerun_system_check_btn.setObjectName("showHiddenBtn")
    window._rerun_system_check_btn.clicked.connect(window._rerun_system_check)
    btn_row.addWidget(window._rerun_system_check_btn)

    window._repair_launcher_btn = QPushButton("Repair Desktop Launchers")
    window._repair_launcher_btn.setObjectName("showHiddenBtn")
    window._repair_launcher_btn.clicked.connect(window._repair_installed_launchers)
    btn_row.addWidget(window._repair_launcher_btn)

    window._health_recover_btn = QPushButton("Recover degraded channels")
    window._health_recover_btn.setObjectName("showHiddenBtn")
    window._health_recover_btn.clicked.connect(window._recover_all_degraded_channels)
    btn_row.addWidget(window._health_recover_btn)

    window._health_diag_btn = QPushButton("Open diagnostics folder")
    window._health_diag_btn.setObjectName("showHiddenBtn")
    window._health_diag_btn.clicked.connect(window._open_diagnostics_folder)
    btn_row.addWidget(window._health_diag_btn)

    window._health_restart_btn = QPushButton("Restart WaveLinux")
    window._health_restart_btn.setObjectName("showHiddenBtn")
    window._health_restart_btn.clicked.connect(window._restart_app)
    btn_row.addWidget(window._health_restart_btn)

    btn_row.addStretch()
    layout.addLayout(btn_row)

    window._health_cards_scroll = QScrollArea()
    window._health_cards_scroll.setWidgetResizable(True)
    window._health_cards_scroll.setFrameShape(QFrame.Shape.NoFrame)
    window._health_cards_scroll.setStyleSheet("background: transparent;")
    window._health_cards_container = QWidget()
    window._health_cards_layout = QVBoxLayout(window._health_cards_container)
    window._health_cards_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
    window._health_cards_layout.setContentsMargins(0, 0, 0, 0)
    window._health_cards_layout.setSpacing(10)
    window._health_cards_scroll.setWidget(window._health_cards_container)
    layout.addWidget(window._health_cards_scroll, 1)

    return tab
