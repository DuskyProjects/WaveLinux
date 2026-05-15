"""Updates settings tab builder."""

from __future__ import annotations

from PyQt6.QtWidgets import QHBoxLayout, QLabel, QProgressBar, QPushButton, QVBoxLayout, QWidget


def build_updates_tab(window):
    tab = QWidget()
    layout = QVBoxLayout(tab)
    layout.setContentsMargins(16, 16, 16, 16)
    layout.setSpacing(12)

    ver_lbl = QLabel(f"Current running version: <b>{getattr(window, '_app_version', '')}</b>")
    ver_lbl.setStyleSheet("color: #e0e0ee; font-size: 13px;")
    layout.addWidget(ver_lbl)

    window._update_status_lbl = QLabel("Click 'Check for Updates' to see if a newer version is available.")
    window._update_status_lbl.setWordWrap(True)
    window._update_status_lbl.setStyleSheet("color: #8b8b9e; font-size: 12px;")
    layout.addWidget(window._update_status_lbl)

    window._update_policy_lbl = QLabel()
    window._update_policy_lbl.setWordWrap(True)
    window._update_policy_lbl.setStyleSheet("color: #5a5a72; font-size: 11px;")
    layout.addWidget(window._update_policy_lbl)

    btn_row = QHBoxLayout()
    window._check_update_btn = QPushButton("Check for Updates")
    window._check_update_btn.setObjectName("showHiddenBtn")
    window._check_update_btn.clicked.connect(window._check_for_updates)
    btn_row.addWidget(window._check_update_btn)

    window._open_release_btn = QPushButton("Open Releases Page")
    window._open_release_btn.setObjectName("addBtn")
    window._open_release_btn.clicked.connect(window._open_release_page)
    btn_row.addWidget(window._open_release_btn)

    window._download_update_btn = QPushButton("Download && Install Latest AppImage")
    window._download_update_btn.setObjectName("showHiddenBtn")
    window._download_update_btn.clicked.connect(window._download_and_install_update)
    btn_row.addWidget(window._download_update_btn)

    window._install_runtime_btn = QPushButton()
    window._install_runtime_btn.setObjectName("showHiddenBtn")
    window._install_runtime_btn.clicked.connect(window._install_current_runtime_launcher)
    btn_row.addWidget(window._install_runtime_btn)

    window._rollback_update_btn = QPushButton("Restore Previous AppImage")
    window._rollback_update_btn.setObjectName("showHiddenBtn")
    window._rollback_update_btn.clicked.connect(window._restore_previous_appimage)
    btn_row.addWidget(window._rollback_update_btn)

    btn_row.addStretch()
    layout.addLayout(btn_row)

    window._install_state_lbl = QLabel()
    window._install_state_lbl.setWordWrap(True)
    window._install_state_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
    layout.addWidget(window._install_state_lbl)

    window._install_warning_lbl = QLabel()
    window._install_warning_lbl.setWordWrap(True)
    window._install_warning_lbl.setStyleSheet("color: #d28b26; font-size: 11px;")
    layout.addWidget(window._install_warning_lbl)

    window._update_progress = QProgressBar()
    window._update_progress.setVisible(False)
    window._update_progress.setTextVisible(True)
    window._update_progress.setRange(0, 100)
    window._update_progress.setValue(0)
    layout.addWidget(window._update_progress)

    window._update_note_lbl = QLabel()
    window._update_note_lbl.setWordWrap(True)
    window._update_note_lbl.setStyleSheet("color: #5a5a72; font-size: 11px;")
    layout.addWidget(window._update_note_lbl)

    layout.addStretch(1)
    return tab
