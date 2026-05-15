"""Apps settings tab builder."""

from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QFrame, QLabel, QScrollArea, QVBoxLayout, QWidget


def build_apps_tab(window):
    apps_tab = QWidget()
    apps_layout = QVBoxLayout(apps_tab)
    apps_layout.setContentsMargins(8, 8, 8, 8)
    r_title = QLabel("APP ROUTING")
    r_title.setObjectName("sectionLabel")
    apps_layout.addWidget(r_title)
    window.routing_scroll = QScrollArea()
    window.routing_scroll.setWidgetResizable(True)
    window.routing_scroll.setFrameShape(QFrame.Shape.NoFrame)
    window.routing_scroll.setStyleSheet("background: transparent;")
    window.routing_container = QWidget()
    window.routing_layout = QVBoxLayout(window.routing_container)
    window.routing_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
    window.routing_layout.setContentsMargins(0, 0, 0, 0)
    window.routing_scroll.setWidget(window.routing_container)
    apps_layout.addWidget(window.routing_scroll, 1)
    return apps_tab
