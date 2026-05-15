"""App routing row widget."""

from __future__ import annotations

from PyQt6.QtCore import Qt, QTimer
from PyQt6.QtGui import QIcon
from PyQt6.QtWidgets import QComboBox, QHBoxLayout, QLabel, QMenu, QPushButton, QSlider, QWidget

from pipewire_engine import PipeWireEngine


class AppRoutingRow(QWidget):
    """A row showing an app name and a dropdown to choose which sink it goes to."""

    _GENERIC_APP_ICON_CANDIDATES = [
        "audio-x-generic",
        "multimedia-player",
        "applications-multimedia",
    ]
    _SYSTEM_ICON_CANDIDATES = [
        "preferences-system-sound",
        "audio-volume-high",
        "audio-card",
    ]

    def __init__(self, app_id, display_name, engine, sinks, main_win=None, parent=None):
        super().__init__(parent)
        self.engine = engine
        self.app_id = app_id
        self.app_name = display_name
        self.resolved_app_id = app_id
        self.resolved_app_name = display_name
        self.identity_source = ""
        self.override_applied = False
        self.manual_override_active = False
        self.reset_source_app_id = ""
        self._main_win = main_win
        self._active_indices = []
        self._last_active_state = None
        self._last_sink_selection = object()

        layout = QHBoxLayout(self)
        layout.setContentsMargins(0, 4, 0, 4)
        layout.setAlignment(Qt.AlignmentFlag.AlignVCenter)

        self.icon_lbl = QLabel("🎵")
        self.icon_lbl.setObjectName("channelIcon")
        self.icon_lbl.setFixedSize(28, 24)
        self.icon_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(self.icon_lbl)

        self.name_lbl = QLabel(display_name)
        self.name_lbl.setObjectName("appName")
        self.name_lbl.setMinimumWidth(150)
        self.name_lbl.setAlignment(Qt.AlignmentFlag.AlignVCenter | Qt.AlignmentFlag.AlignLeft)
        layout.addWidget(self.name_lbl)

        layout.addStretch()

        self.vol_slider = QSlider(Qt.Orientation.Horizontal)
        self.vol_slider.setRange(0, 100)
        self.vol_slider.setFixedWidth(100)
        self.vol_slider.setValue(100)
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

        self.manage_btn = QPushButton("⋯")
        self.manage_btn.setObjectName("forgetBtn")
        self.manage_btn.setFixedWidth(26)
        self.manage_btn.setToolTip("Pin, merge, or reset this app identity")
        self.manage_btn.clicked.connect(self._show_identity_menu)
        layout.addWidget(self.manage_btn)

        self.forget_btn = QPushButton("✕")
        self.forget_btn.setObjectName("forgetBtn")
        self.forget_btn.setFixedWidth(26)
        self.forget_btn.setToolTip("Forget this app so it stops showing up in the list")
        self.forget_btn.clicked.connect(self._on_forget)
        layout.addWidget(self.forget_btn)

        self._pending_app_vol = None
        self._app_commit_timer = QTimer(self)
        self._app_commit_timer.setSingleShot(True)
        self._app_commit_timer.setInterval(40)
        self._app_commit_timer.timeout.connect(self._commit_app_vol)

        self.update_state(display_name, [], sinks, None)

    def _set_icon_candidates(self, icon_candidates, *, is_system):
        ordered = []
        seen = set()
        for candidate in list(icon_candidates or []) + (
            self._SYSTEM_ICON_CANDIDATES if is_system else self._GENERIC_APP_ICON_CANDIDATES
        ):
            key = str(candidate or "").strip()
            if not key or key in seen:
                continue
            seen.add(key)
            ordered.append(key)
        for candidate in ordered:
            icon = QIcon.fromTheme(candidate)
            if icon.isNull():
                continue
            self.icon_lbl.clear()
            self.icon_lbl.setPixmap(icon.pixmap(20, 20))
            self.icon_lbl.setToolTip(candidate)
            return
        self.icon_lbl.clear()
        self.icon_lbl.setText("🔔" if is_system else "🎵")
        self.icon_lbl.setToolTip("")

    def _on_vol_change(self, value):
        self._pending_app_vol = value
        self._app_commit_timer.start()

    def _commit_app_vol(self):
        v = self._pending_app_vol
        self._pending_app_vol = None
        if v is None:
            return
        normalized = max(0.0, min(float(v) / 100.0, 1.0))
        win = self._main_win
        if win is not None:
            app_volumes = getattr(win, "app_volumes", None)
            if (
                app_volumes is not None
                and PipeWireEngine.is_persistent_app_id(self.app_id)
                and app_volumes.get(self.app_id) != normalized
            ):
                app_volumes[self.app_id] = normalized
                win._sync_runtime_persistent_state()
                win.schedule_save()
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            return
        for idx in self._active_indices:
            runtime.set_app_volume(idx, normalized)

    def flush_pending_state(self):
        timer = getattr(self, "_app_commit_timer", None)
        if timer is not None and timer.isActive():
            timer.stop()
            self._commit_app_vol()

    def _on_route_change(self, idx):
        win = self._main_win
        if win is not None and hasattr(win, "_module_enabled") and not win._module_enabled("app_routing"):
            return
        sink_name = self.combo.itemData(idx)
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            return
        runtime.set_app_route(self.app_id, sink_name)
        if win is not None:
            win.app_routing[self.app_id] = sink_name
            win.save_config()

    def _on_forget(self):
        win = self._main_win
        if win is not None:
            win.forget_app(self.app_id)

    def _identity_source_app_id(self):
        if PipeWireEngine.is_persistent_app_id(self.resolved_app_id):
            return self.resolved_app_id
        if PipeWireEngine.is_persistent_app_id(self.app_id):
            return self.app_id
        return ""

    def _show_identity_menu(self):
        win = self._main_win
        if win is None:
            return
        menu = QMenu(self)
        persistent_source = self._identity_source_app_id()
        is_system = self.app_id == PipeWireEngine.SYSTEM_SOUNDS_BUCKET
        can_pin_merge = bool(persistent_source) and not is_system
        can_reset = bool(self.manual_override_active) and not is_system

        if not can_pin_merge:
            info_act = menu.addAction(
                "WaveLinux needs a stable app signature before it can pin or merge this stream."
            )
            info_act.setEnabled(False)
            menu.addSeparator()

        pin_act = menu.addAction("Pin / Rename App…")
        pin_act.setEnabled(can_pin_merge)
        pin_act.triggered.connect(lambda checked=False: win._pin_app_identity(self))

        merge_act = menu.addAction("Merge Into Existing App…")
        merge_act.setEnabled(can_pin_merge)
        merge_act.triggered.connect(lambda checked=False: win._merge_app_identity(self))

        reset_act = menu.addAction("Reset to Auto Detection")
        reset_act.setEnabled(can_reset)
        reset_act.triggered.connect(lambda checked=False: win._reset_app_identity_override(self))

        menu.exec(self.manage_btn.mapToGlobal(self.manage_btn.rect().bottomLeft()))

    def update_state(
        self,
        display_name,
        active_indices,
        sinks,
        current_sink,
        current_volume=None,
        saved_volume=None,
        resolved_app_id=None,
        resolved_app_name=None,
        identity_source="",
        override_applied=False,
        manual_override_active=False,
        reset_source_app_id="",
        icon_candidates=None,
    ):
        self.app_name = display_name or self.app_name
        self._active_indices = active_indices
        self.resolved_app_id = str(resolved_app_id or self.app_id)
        self.resolved_app_name = str(resolved_app_name or self.app_name)
        self.identity_source = str(identity_source or "")
        self.override_applied = bool(override_applied)
        self.manual_override_active = bool(manual_override_active)
        self.reset_source_app_id = str(reset_source_app_id or "")
        is_active = len(active_indices) > 0
        is_system = self.app_id == PipeWireEngine.SYSTEM_SOUNDS_BUCKET

        derived_icon_candidates = list(icon_candidates or [])
        for app_token, app_label in (
            (self.app_id, self.app_name),
            (self.resolved_app_id, self.resolved_app_name),
        ):
            for candidate in PipeWireEngine.theme_icon_candidates_for_app_id(
                app_token,
                fallback_name=app_label,
            ):
                if candidate not in derived_icon_candidates:
                    derived_icon_candidates.append(candidate)
        self._set_icon_candidates(derived_icon_candidates, is_system=is_system)
        label_text = (
            f"{self.app_name} (Idle)" if (is_system and not is_active)
            else f"{self.app_name} (Offline)" if not is_active
            else self.app_name
        )
        active_state = (label_text, is_active, is_system)
        if active_state != self._last_active_state:
            self.name_lbl.setText(label_text)
            self.vol_slider.setEnabled(True)
            self.vol_lbl.setStyleSheet("" if is_active else "color: #666;")
            self.name_lbl.setStyleSheet("" if is_active else "color: #888;")
            self._last_active_state = active_state

        self.forget_btn.setVisible(not is_system)
        self.manage_btn.setVisible(not is_system)
        if not is_system:
            self.manage_btn.setEnabled(True)
            self.manage_btn.setToolTip("Pin, merge, or reset this app identity")
            self.forget_btn.setEnabled(True)
            self.forget_btn.setToolTip(
                "Permanently hide this app from the routing list. Drops its saved volume / destination too."
            )
        tooltip_lines = [
            f"Canonical app ID: {self.app_id}",
            f"Resolved app ID: {self.resolved_app_id or 'n/a'}",
            "Identity source: " + (self.identity_source or "n/a"),
            "Manual override active: " + ("yes" if self.manual_override_active else "no"),
        ]
        tooltip = "\n".join(tooltip_lines)
        self.name_lbl.setToolTip(tooltip)
        self.manage_btn.setToolTip("Pin, merge, or reset this app identity\n\n" + tooltip)

        vol = current_volume
        if vol is None:
            vol = saved_volume
        if vol is None and is_active:
            vol = self.engine.get_sink_input_volume(active_indices[0])
        if vol is not None and not self.vol_slider.isSliderDown():
            vol_pct = int(vol * 100)
            if self.vol_slider.value() != vol_pct:
                self.vol_slider.blockSignals(True)
                self.vol_slider.setValue(vol_pct)
                self.vol_slider.blockSignals(False)

        if not self.combo.view().isVisible():
            sink_fp = tuple(
                (s.get("name"), s.get("display_name")) if isinstance(s, dict)
                else (getattr(s, "name", None), getattr(s, "display_name", None))
                for s in sinks
            )
            if getattr(self, "_combo_sink_fp", None) != sink_fp:
                self._combo_sink_fp = sink_fp
                self.combo.blockSignals(True)
                curr_data = self.combo.currentData()
                self.combo.clear()
                self.combo.addItem("System Default", None)
                for s in sinks:
                    if isinstance(s, dict):
                        name = s["name"]
                        display_name = s.get("display_name")
                    else:
                        name = getattr(s, "name", None)
                        display_name = getattr(s, "display_name", None)
                    if name is None:
                        continue
                    if name.startswith("wavelinux_mix_") or name.startswith("wavelinux_src_"):
                        continue
                    if name.endswith(".monitor"):
                        continue
                    if name.startswith("wavelinux_"):
                        pretty = name.replace("wavelinux_", "").replace("_", " ").title()
                        display = display_name or pretty
                    else:
                        display = display_name or self.engine.display_name_for_sink(name)
                    self.combo.addItem(display, name)

                target_sink = current_sink or curr_data
                idx = self.combo.findData(target_sink)
                if idx >= 0:
                    self.combo.setCurrentIndex(idx)
                self.combo.blockSignals(False)
                self._last_sink_selection = target_sink
            elif current_sink != self._last_sink_selection:
                idx = self.combo.findData(current_sink)
                if idx >= 0 and idx != self.combo.currentIndex():
                    self.combo.blockSignals(True)
                    self.combo.setCurrentIndex(idx)
                    self.combo.blockSignals(False)
                self._last_sink_selection = current_sink
