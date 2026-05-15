"""Scenes settings tab builder."""

from __future__ import annotations

from PyQt6.QtWidgets import QComboBox, QHBoxLayout, QLabel, QPushButton, QVBoxLayout, QWidget


def build_scenes_tab(window):
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
    window._scene_combo = QComboBox()
    window._scene_combo.currentIndexChanged.connect(window._on_scene_selection_change)
    pick_row.addWidget(window._scene_combo, 1)
    layout.addLayout(pick_row)

    window._scene_summary_lbl = QLabel("No saved scenes yet.")
    window._scene_summary_lbl.setWordWrap(True)
    window._scene_summary_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
    layout.addWidget(window._scene_summary_lbl)

    btn_row = QHBoxLayout()
    window._apply_scene_btn = QPushButton("Apply Scene")
    window._apply_scene_btn.setObjectName("addBtn")
    window._apply_scene_btn.clicked.connect(window._apply_selected_scene)
    btn_row.addWidget(window._apply_scene_btn)

    window._save_scene_btn = QPushButton("Save Current As…")
    window._save_scene_btn.setObjectName("showHiddenBtn")
    window._save_scene_btn.clicked.connect(window._save_current_scene_as)
    btn_row.addWidget(window._save_scene_btn)

    window._overwrite_scene_btn = QPushButton("Update Selected")
    window._overwrite_scene_btn.setObjectName("showHiddenBtn")
    window._overwrite_scene_btn.clicked.connect(window._overwrite_selected_scene)
    btn_row.addWidget(window._overwrite_scene_btn)

    btn_row.addStretch()
    layout.addLayout(btn_row)

    edit_row = QHBoxLayout()
    window._rename_scene_btn = QPushButton("Rename")
    window._rename_scene_btn.setObjectName("showHiddenBtn")
    window._rename_scene_btn.clicked.connect(window._rename_selected_scene)
    edit_row.addWidget(window._rename_scene_btn)

    window._delete_scene_btn = QPushButton("Delete")
    window._delete_scene_btn.setObjectName("removeBtn")
    window._delete_scene_btn.clicked.connect(window._delete_selected_scene)
    edit_row.addWidget(window._delete_scene_btn)

    edit_row.addStretch()
    layout.addLayout(edit_row)
    layout.addStretch(1)
    return tab
