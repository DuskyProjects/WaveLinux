"""Advanced settings tab builder."""

from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import QCheckBox, QHBoxLayout, QLabel, QPushButton, QSpinBox, QVBoxLayout, QWidget


def build_advanced_tab(window):
    tab = QWidget()
    layout = QVBoxLayout(tab)
    layout.setContentsMargins(8, 8, 8, 8)
    layout.setSpacing(12)

    def _heading(text):
        lbl = QLabel(text)
        lbl.setObjectName("sectionLabel")
        layout.addWidget(lbl)

    _heading("APP CLEANUP")
    prune_row = QHBoxLayout()
    prune_row.addWidget(QLabel("Forget offline apps after (days):"))
    window.prune_spin = QSpinBox()
    window.prune_spin.setRange(1, 365)
    window.prune_spin.setValue(window.app_prune_days)
    window.prune_spin.valueChanged.connect(window._on_prune_days_change)
    prune_row.addWidget(window.prune_spin)
    prune_row.addStretch()
    layout.addLayout(prune_row)

    forget_all_btn = QPushButton("Forget all offline apps now")
    forget_all_btn.setObjectName("removeBtn")
    forget_all_btn.clicked.connect(window._forget_all_offline)
    layout.addWidget(forget_all_btn, alignment=Qt.AlignmentFlag.AlignLeft)

    window.restore_forgotten_btn = QPushButton("Restore forgotten apps")
    window.restore_forgotten_btn.setObjectName("showHiddenBtn")
    window.restore_forgotten_btn.setToolTip(
        "Clear the per-app ✕ blocklist so apps you've previously forgotten can show up in the routing tab again."
    )
    window.restore_forgotten_btn.clicked.connect(window._restore_forgotten_apps)
    layout.addWidget(window.restore_forgotten_btn, alignment=Qt.AlignmentFlag.AlignLeft)

    _heading("STARTUP & TRAY")
    window.autostart_check = QCheckBox("Start WaveLinux at login")
    window.autostart_check.setChecked(window.is_autostart_enabled())
    window.autostart_check.toggled.connect(window.set_autostart)
    layout.addWidget(window.autostart_check)

    quick_start_btn = QPushButton("Quick Start Setup…")
    quick_start_btn.setObjectName("showHiddenBtn")
    quick_start_btn.clicked.connect(window._open_quick_start_setup)
    layout.addWidget(quick_start_btn, alignment=Qt.AlignmentFlag.AlignLeft)

    profiles_btn = QPushButton("Sound Card Profiles…")
    profiles_btn.setObjectName("showHiddenBtn")
    profiles_btn.clicked.connect(window._open_card_profiles)
    layout.addWidget(profiles_btn, alignment=Qt.AlignmentFlag.AlignLeft)

    _heading("CONFIG")
    config_btn_row = QHBoxLayout()
    import_config_btn = QPushButton("Import Full Config…")
    import_config_btn.setObjectName("showHiddenBtn")
    import_config_btn.setToolTip(
        "Replace the current WaveLinux configuration with a saved JSON export."
    )
    import_config_btn.clicked.connect(window._import_full_config)
    config_btn_row.addWidget(import_config_btn)

    export_config_btn = QPushButton("Export Full Config…")
    export_config_btn.setObjectName("showHiddenBtn")
    export_config_btn.setToolTip(
        "Save the current WaveLinux configuration, scenes, routing, and FX state to JSON."
    )
    export_config_btn.clicked.connect(window._export_full_config)
    config_btn_row.addWidget(export_config_btn)
    config_btn_row.addStretch()
    layout.addLayout(config_btn_row)

    _heading("DIAGNOSTICS")
    probed = len(window.engine.ladspa_plugins)
    ladspa_lbl = QLabel(
        f"LADSPA plugins detected: {probed}\n"
        f"Paths searched: $LADSPA_PATH + standard host LADSPA directories.\n"
        f"AppImage bundled LADSPA is disabled by default; opt in with "
        f"WAVELINUX_ENABLE_BUNDLED_LADSPA=1."
    )
    ladspa_lbl.setStyleSheet("color: #8b8b9e; font-size: 11px;")
    ladspa_lbl.setWordWrap(True)
    layout.addWidget(ladspa_lbl)

    export_diag_btn = QPushButton("Export Runtime Diagnostics")
    export_diag_btn.setObjectName("showHiddenBtn")
    export_diag_btn.clicked.connect(window._export_runtime_diagnostics)
    layout.addWidget(export_diag_btn, alignment=Qt.AlignmentFlag.AlignLeft)

    window.recover_degraded_btn = QPushButton("Recover degraded channels")
    window.recover_degraded_btn.setObjectName("showHiddenBtn")
    window.recover_degraded_btn.setToolTip(
        "Request runtime recovery for each channel currently marked degraded."
    )
    window.recover_degraded_btn.clicked.connect(window._recover_all_degraded_channels)
    layout.addWidget(window.recover_degraded_btn, alignment=Qt.AlignmentFlag.AlignLeft)

    emergency_btn = QPushButton("Emergency Reset (unload all WaveLinux modules)")
    emergency_btn.setObjectName("removeBtn")
    emergency_btn.clicked.connect(window._on_emergency_reset)
    layout.addWidget(emergency_btn, alignment=Qt.AlignmentFlag.AlignLeft)

    layout.addStretch(1)
    return tab
