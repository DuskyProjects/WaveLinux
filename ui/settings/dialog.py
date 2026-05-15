"""Settings dialog builder and assembly helpers."""

from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QDialog, QTabWidget, QVBoxLayout, QLabel, QPushButton, QHBoxLayout, QWidget

from wavelinux_theme import STYLESHEET

from .advanced_tab import build_advanced_tab
from .apps_tab import build_apps_tab
from .health_tab import build_health_tab
from .scenes_tab import build_scenes_tab
from .updates_tab import build_updates_tab


def _build_hidden_tab(window):
    hidden_tab = QWidget()
    hidden_layout = QVBoxLayout(hidden_tab)
    hidden_layout.setContentsMargins(8, 8, 8, 8)
    hidden_title = QLabel("HIDDEN CHANNELS")
    hidden_title.setObjectName("sectionLabel")
    hidden_layout.addWidget(hidden_title)
    window.hidden_list_container = QWidget()
    window.hidden_list_layout = QVBoxLayout(window.hidden_list_container)
    window.hidden_list_layout.setAlignment(Qt.AlignmentFlag.AlignTop)
    window.hidden_list_layout.setContentsMargins(0, 0, 0, 0)
    window.hidden_list_layout.setSpacing(4)
    hidden_layout.addWidget(window.hidden_list_container, 1)
    return hidden_tab


def build_settings_dialog(window):
    dialog = QDialog(window)
    dialog.setWindowTitle("WaveLinux Settings")
    dialog.setMinimumSize(640, 480)
    dialog.setStyleSheet(STYLESHEET)
    sd_layout = QVBoxLayout(dialog)
    sd_layout.setContentsMargins(16, 16, 16, 16)
    tabs = QTabWidget(dialog)
    window._settings_tabs = tabs
    sd_layout.addWidget(tabs)

    tabs.addTab(build_apps_tab(window), "Apps")
    tabs.addTab(_build_hidden_tab(window), "Hidden")
    window._scenes_tab_widget = build_scenes_tab(window)
    tabs.addTab(window._scenes_tab_widget, "Scenes")
    window._system_tab_widget = build_health_tab(window)
    tabs.addTab(window._system_tab_widget, "Health")
    tabs.addTab(build_advanced_tab(window), "Advanced")
    window._updates_tab_widget = build_updates_tab(window)
    tabs.addTab(window._updates_tab_widget, "Updates")
    window._settings_tab_names = tuple(tabs.tabText(i) for i in range(tabs.count()))
    tabs.currentChanged.connect(window._on_settings_tab_changed)

    window.settings_dialog = dialog
    return dialog
