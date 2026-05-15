"""Channel FX selection dialog."""

from __future__ import annotations

import queue

from PyQt6.QtCore import Qt, QTimer
from PyQt6.QtWidgets import (
    QDialog,
    QFrame,
    QHBoxLayout,
    QLabel,
    QPushButton,
    QScrollArea,
    QSlider,
    QVBoxLayout,
    QWidget,
)

from wavelinux_theme import STYLESHEET


class FXSelectionDialog(QDialog):
    _TOGGLE_STYLE = (
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
    _TOGGLE_FAILED_STYLE = (
        "QPushButton[role=\"fxToggle\"] {"
        " background: #2a1a22; color: #ff6a8a;"
        " border: 1px solid #ff6a8a;"
        " border-radius: 6px; font-weight: bold; padding: 4px 0; }"
    )

    def __init__(self, node_id, node_name, capture_target, engine, runtime=None, parent=None):
        super().__init__(parent)
        self.node_id = str(node_id)
        self.node_name = node_name
        self.capture_target = capture_target
        self.engine = engine
        self.runtime = runtime
        self.setWindowTitle("Channel Effects")
        self.setMinimumWidth(400)
        self.setStyleSheet(STYLESHEET)

        self._param_sliders = {}
        self._param_frames = {}
        self._toggle_btns = {}

        win = self._main_window_static(parent)
        self._main_win = win
        self._saved_effects_list = list(win.active_effects.get(self.node_name, []) if win else [])
        self._saved_effects = set(self._saved_effects_list)
        self._effect_defs = list(self.engine.get_available_effects())
        saved_params = (win.effect_params.get(self.node_name, {}) if win else {}) or {}
        self._saved_effect_params = {
            effect_id: dict(values)
            for effect_id, values in saved_params.items()
            if isinstance(values, dict)
        }
        self._effect_available = {
            fx["id"]: self.engine.effect_available(fx["id"])
            for fx in self._effect_defs
        }
        self._effect_ui_meta = {
            fx["id"]: {
                "params": self.engine.get_effect_params(fx["id"]),
                "help": self.engine.get_effect_help(fx["id"]),
                "presets": self.engine.get_effect_presets(fx["id"]),
            }
            for fx in self._effect_defs
        }

        self._pending_fx_generation = 0
        self._runtime_inflight = False
        self._pending_close = False
        self._initial_effects_list = list(self._saved_effects_list)
        self._initial_effect_params = {
            effect_id: dict(values)
            for effect_id, values in self._saved_effect_params.items()
        }
        self._inflight_effects_list = []
        self._inflight_effect_params = {}
        if self.runtime is not None:
            self.runtime.fx_status_changed.connect(self._on_runtime_fx_status)

        self._param_timer = QTimer(self)
        self._param_timer.setSingleShot(True)
        self._param_timer.setInterval(120)
        self._param_timer.timeout.connect(self._commit_live_patch)

        root = QVBoxLayout(self)
        root.setSpacing(12)
        root.setContentsMargins(20, 20, 20, 20)

        title = QLabel("✨ Channel Effects")
        title.setStyleSheet("font-size: 18px; font-weight: bold; color: #fff;")
        root.addWidget(title)

        desc = QLabel("Processing is live per-channel. Parameters save automatically.")
        desc.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        root.addWidget(desc)

        self._status_lbl = QLabel("")
        self._status_lbl.setWordWrap(True)
        self._status_lbl.setVisible(False)
        self._status_lbl.setStyleSheet("color: #ffb26a; font-size: 11px;")
        root.addWidget(self._status_lbl)

        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.Shape.NoFrame)
        inner = QWidget()
        scroll_layout = QVBoxLayout(inner)
        scroll_layout.setSpacing(10)

        for fx in self._effect_defs:
            scroll_layout.addWidget(self._build_effect_card(fx))

        scroll_layout.addStretch()
        scroll.setWidget(inner)
        root.addWidget(scroll, 1)

        self.close_btn = QPushButton("Apply")
        self.close_btn.setObjectName("addBtn")
        self.close_btn.clicked.connect(self._on_done)
        root.addWidget(self.close_btn)

        self._refresh_toggle_status()

    @staticmethod
    def _main_window_static(parent):
        p = parent
        while p is not None and not hasattr(p, "effect_params"):
            p = p.parent()
        return p

    def _on_done(self):
        if self._param_timer.isActive():
            self._param_timer.stop()
            self._commit_live_patch()
        self._pending_close = False
        self.accept()

    def _build_effect_card(self, fx):
        fid = fx["id"]
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
        icon_lbl = QLabel(fx["icon"])
        icon_lbl.setStyleSheet("font-size: 20px;")
        header.addWidget(icon_lbl)

        text_col = QVBoxLayout()
        name_lbl = QLabel(fx["name"])
        name_lbl.setStyleSheet("color: #e0e0ee; font-weight: bold; font-size: 13px;")
        text_col.addWidget(name_lbl)
        info_lbl = QLabel(fx["desc"])
        info_lbl.setStyleSheet("color: #6b6b82; font-size: 10px;")
        text_col.addWidget(info_lbl)
        header.addLayout(text_col, 1)

        toggle_btn = QPushButton()
        toggle_btn.setCheckable(True)
        toggle_btn.setProperty("role", "fxToggle")
        toggle_btn.setStyleSheet(self._TOGGLE_STYLE)
        active = fid in self._saved_effects
        toggle_btn.setChecked(active)
        toggle_btn.setFixedWidth(60)

        available = self._effect_available.get(fid, False)
        if not available:
            toggle_btn.setText("N/A")
            toggle_btn.setEnabled(False)
            toggle_btn.setToolTip(
                "LADSPA plugin not installed.\n"
                "rnnoise needs librnnoise_ladspa; "
                "compressor/gate/limiter need swh-plugins. "
                "highpass uses PipeWire's built-in biquad."
            )
            info_lbl.setStyleSheet("color: #ff6a8a; font-size: 10px;")
            info_lbl.setText(fx["desc"] + " — plugin missing")
        else:
            toggle_btn.setText("ON" if active else "OFF")
            toggle_btn.clicked.connect(lambda checked, fid=fid: self._on_toggle(fid))

        header.addWidget(toggle_btn)
        self._toggle_btns[fid] = toggle_btn
        vlay.addLayout(header)

        meta = self._effect_ui_meta.get(fid, {})
        params = meta.get("params", [])
        help_text = meta.get("help", "")
        presets = meta.get("presets", [])

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
        return dict(self._saved_effect_params.get(effect_id, {}))

    def _main_window(self):
        return self._main_win or self._main_window_static(self.parent())

    def _collect_params(self, effect_id):
        out = {}
        for key, (slider, _lbl) in self._param_sliders.get(effect_id, {}).items():
            pmin = float(slider.property("pmin"))
            pmax = float(slider.property("pmax"))
            val = pmin + (pmax - pmin) * (slider.value() / 1000.0)
            out[key] = val
        return out

    def _effective_fx_request(self, wanted=None, params_map=None):
        wanted = list(wanted if wanted is not None else self._active_effect_ids())
        params_map = dict(params_map if params_map is not None else self._all_params_map())
        win = self._main_window()
        if win is not None and hasattr(win, "_normalize_effect_request_for_node"):
            return win._normalize_effect_request_for_node(self.node_name, wanted, params_map)
        return wanted, params_map

    def _active_effect_ids(self):
        wanted = []
        for fx in self._effect_defs:
            fid = fx["id"]
            btn = self._toggle_btns.get(fid)
            if btn is not None and btn.isChecked() and btn.isEnabled():
                wanted.append(fid)
        return wanted

    def _all_params_map(self):
        out = {}
        for fid in self._param_sliders.keys():
            out[fid] = self._collect_params(fid)
        return out

    def _has_pending_changes(self, wanted=None, params_map=None):
        wanted, params_map = self._effective_fx_request(wanted, params_map)
        normalized = {fid: dict(values) for fid, values in params_map.items() if values}
        return wanted != list(self._initial_effects_list) or normalized != self._initial_effect_params

    def _queue_live_patch(self):
        if self.runtime is None:
            return
        self._param_timer.start()

    def _commit_live_patch(self):
        if self.runtime is None:
            return
        wanted, params_map = self._effective_fx_request()
        if not self._has_pending_changes(wanted, params_map):
            return
        self._save_chain_state(wanted, params_map)
        self._start_fx_worker(wanted, params_map)

    def _start_fx_worker(self, wanted, params_map):
        self._inflight_effects_list = list(wanted or [])
        self._inflight_effect_params = {
            fid: dict(vals)
            for fid, vals in (params_map or {}).items()
            if vals
        }
        if wanted:
            self._pending_fx_generation = self.runtime.set_channel_fx(
                self.node_name, self.capture_target, wanted, params_map
            )
        else:
            self._pending_fx_generation = self.runtime.clear_channel_fx(self.node_name)
        self._runtime_inflight = True

    def _on_runtime_fx_status(self, status):
        if getattr(status, "node_name", None) != self.node_name:
            return
        if self._pending_fx_generation and status.generation:
            if status.generation < self._pending_fx_generation:
                return
        self._runtime_inflight = status.state in {"building", "cutover_pending", "clearing"}
        if status.state == "degraded":
            recovery_note = ""
            win = self._main_window()
            if hasattr(win, "fx_recovery_status_message"):
                recovery_note = (win.fx_recovery_status_message(self.node_name) or "").strip()
            status_text = (status.message or "").strip()
            if hasattr(win, "format_fx_status_message"):
                status_text = (win.format_fx_status_message(status) or "").strip()
            parts = [
                "WaveLinux could not apply one or more effects.",
                status_text,
            ]
            if recovery_note:
                parts.append(recovery_note)
            self._status_lbl.setText("\n\n".join(part for part in parts if part))
            self._status_lbl.setVisible(True)
        elif status.state in {"active", "idle"}:
            self._status_lbl.clear()
            self._status_lbl.setVisible(False)
        if status.state not in {"building", "cutover_pending", "clearing"}:
            self.close_btn.setText("Apply")
            self.close_btn.setEnabled(True)
            self._refresh_toggle_status()
            has_more_changes = False
            if status.state in {"active", "idle"}:
                self._initial_effects_list = list(self._inflight_effects_list)
                self._initial_effect_params = {
                    fid: dict(vals)
                    for fid, vals in self._inflight_effect_params.items()
                }
                has_more_changes = self._has_pending_changes()
                if has_more_changes:
                    self._queue_live_patch()
            if self._pending_close and status.state in {"active", "idle"} and not has_more_changes:
                self._pending_close = False
                self.accept()
            elif status.state == "degraded":
                self._pending_close = False

    def reject(self):
        if self._param_timer.isActive():
            self._param_timer.stop()
            self._commit_live_patch()
        self._pending_close = False
        super().reject()

    def closeEvent(self, event):
        if self.runtime is None:
            super().closeEvent(event)
            return
        try:
            self.runtime.fx_status_changed.disconnect(self._on_runtime_fx_status)
        except TypeError:
            pass
        super().closeEvent(event)

    def _refresh_toggle_status(self):
        if self.runtime is None:
            for btn in self._toggle_btns.values():
                if btn.isEnabled():
                    btn.setStyleSheet(self._TOGGLE_STYLE)
                    btn.setToolTip("")
            return
        status = self.runtime.fx_status_for(self.node_name)
        degraded = getattr(status, "state", "") == "degraded"
        message = (status.message or "").strip()
        for fid, btn in self._toggle_btns.items():
            if not btn.isEnabled():
                continue
            if degraded and btn.isChecked():
                btn.setStyleSheet(self._TOGGLE_FAILED_STYLE)
                btn.setToolTip(message or "FX chain degraded. Re-apply or recover the channel.")
            else:
                btn.setStyleSheet(self._TOGGLE_STYLE)
                btn.setToolTip("")

    def _save_chain_state(self, effects, params_map):
        win = self._main_window()
        if win is None:
            return
        if effects:
            win.active_effects[self.node_name] = list(effects)
        else:
            win.active_effects.pop(self.node_name, None)
        if params_map:
            stash = win.effect_params.setdefault(self.node_name, {})
            for fid, vals in params_map.items():
                if vals:
                    stash[fid] = dict(vals)
                else:
                    stash.pop(fid, None)
            if not stash:
                win.effect_params.pop(self.node_name, None)
        self._saved_effect_params = {fid: dict(vals) for fid, vals in params_map.items() if vals}
        if hasattr(win, "schedule_save"):
            win.schedule_save()
        elif hasattr(win, "save_config"):
            win.save_config()

    def _on_toggle(self, effect_id):
        btn = self._toggle_btns[effect_id]
        frame = self._param_frames.get(effect_id)
        btn.setText("ON" if btn.isChecked() else "OFF")
        if frame:
            frame.setVisible(btn.isChecked())
        self._queue_live_patch()

    def _apply_preset(self, effect_id, values):
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
        self._queue_live_patch()

    def _on_param_changed(self, effect_id, slider, value_lbl):
        pmin = float(slider.property("pmin"))
        pmax = float(slider.property("pmax"))
        suffix = slider.property("psuffix") or ""
        val = pmin + (pmax - pmin) * (slider.value() / 1000.0)
        value_lbl.setText(self._fmt_value(val, suffix))
        self._queue_live_patch()
