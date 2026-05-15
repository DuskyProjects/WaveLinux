"""Mixer strip and meter worker widgets."""

from __future__ import annotations

import queue
import shutil
import struct
import subprocess
import threading

from PyQt6.QtCore import QObject, QProcess, QTimer, Qt, pyqtSignal
from PyQt6.QtWidgets import (
    QFrame,
    QHBoxLayout,
    QLabel,
    QMenu,
    QMessageBox,
    QProgressBar,
    QPushButton,
    QSizePolicy,
    QSlider,
    QVBoxLayout,
)

from pipewire_engine import PipeWireEngine
from ui.dialogs.fx_dialog import FXSelectionDialog


class MeterWorker(QObject):
    """Emit normalized channel peaks from `parec`."""

    peak = pyqtSignal(float)

    def __init__(self, source_name, parent=None):
        super().__init__(parent)
        self.source_name = source_name
        self._proc = None
        self._sample_rate = 24000
        self._frame_hz = 30
        self._capture_latency_ms = 20
        self._sample_bytes = self._sample_rate * 2 // self._frame_hz
        self._max_buffer_bytes = self._sample_bytes * 2
        self._decode_queue = queue.Queue(maxsize=2)
        self._decode_stop = threading.Event()
        self._decode_thread = None

    @staticmethod
    def _frame_peak(frame, last_peak):
        count = len(frame) // 2
        if count <= 0:
            return last_peak
        samples = struct.unpack(f"<{count}h", frame)
        peak_int = max((abs(s) for s in samples), default=0)
        normalized = peak_int / 32768.0
        if normalized >= last_peak:
            return normalized
        return max(normalized, last_peak * 0.6)

    def start(self):
        if self._proc is not None:
            return
        self._decode_stop.clear()
        self._drain_decode_queue()
        if self._decode_thread is None or not self._decode_thread.is_alive():
            self._decode_thread = threading.Thread(
                target=self._decode_loop,
                daemon=True,
                name=f"meter:{self.source_name}",
            )
            self._decode_thread.start()
        self._proc = QProcess(self)
        self._proc.setProcessChannelMode(QProcess.ProcessChannelMode.SeparateChannels)
        self._proc.readyReadStandardOutput.connect(self._on_bytes)
        args = [
            f"--device={self.source_name}",
            f"--rate={self._sample_rate}",
            "--format=s16le",
            "--channels=1",
            "--raw",
            f"--latency-msec={self._capture_latency_ms}",
        ]
        self._proc.start("parec", args)

    def stop(self):
        if self._proc is None:
            return
        try:
            self._proc.kill()
            self._proc.waitForFinished(200)
        except Exception:
            pass
        self._proc = None
        self._decode_stop.set()
        try:
            self._decode_queue.put_nowait(None)
        except queue.Full:
            try:
                self._decode_queue.get_nowait()
            except queue.Empty:
                pass
            try:
                self._decode_queue.put_nowait(None)
            except queue.Full:
                pass
        thread = self._decode_thread
        if thread is not None and thread.is_alive():
            thread.join(timeout=0.3)
        self._decode_thread = None
        self._drain_decode_queue()

    def _on_bytes(self):
        if self._proc is None:
            return
        chunk = bytes(self._proc.readAllStandardOutput())
        if not chunk:
            return
        try:
            self._decode_queue.put_nowait(chunk)
        except queue.Full:
            try:
                self._decode_queue.get_nowait()
            except queue.Empty:
                pass
            try:
                self._decode_queue.put_nowait(chunk)
            except queue.Full:
                pass

    def _decode_loop(self):
        buf = bytearray()
        last_peak = 0.0
        while not self._decode_stop.is_set():
            try:
                chunk = self._decode_queue.get(timeout=0.2)
            except queue.Empty:
                continue
            if chunk is None:
                break
            buf.extend(chunk)
            saw_stop = False
            while True:
                try:
                    chunk = self._decode_queue.get_nowait()
                except queue.Empty:
                    break
                if chunk is None:
                    saw_stop = True
                    break
                buf.extend(chunk)
            if len(buf) > self._max_buffer_bytes:
                del buf[:-self._max_buffer_bytes]
            frame = self._take_latest_frame(buf, self._sample_bytes)
            if frame is None:
                if saw_stop:
                    break
                continue
            last_peak = self._frame_peak(frame, last_peak)
            self.peak.emit(min(last_peak, 1.0))
            if saw_stop:
                break

    @staticmethod
    def _take_latest_frame(buf, frame_size):
        if frame_size <= 0 or len(buf) < frame_size:
            return None
        full_bytes = len(buf) - (len(buf) % frame_size)
        start = full_bytes - frame_size
        frame = bytes(buf[start:full_bytes])
        del buf[:full_bytes]
        return frame

    def _drain_decode_queue(self):
        while True:
            try:
                self._decode_queue.get_nowait()
            except queue.Empty:
                return


class ChannelStrip(QFrame):
    """A single mixer channel: icon, name, vertical fader, mute, FX."""

    _MIN_W = 120
    _MAX_W = 420
    _MIN_SLIDER_H = 40
    _MAX_SLIDER_H = 240
    _VERT_SLIDER_END_PAD = 8
    _STRIP_HEIGHT_PAD = 4
    _WIDTH_SCALE_CAP = 220
    _MAX_WIDGET_HEIGHT = 16777215

    def __init__(self, node_id, node_name, name, ch_type, icon, engine, parent=None):
        super().__init__(parent)
        self.setObjectName("channelStrip")
        self.setFixedWidth(self._MAX_W)
        self.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
        self.node_id = node_id
        self.node_name = node_name
        self.ch_name = name
        self.ch_type = ch_type
        self.engine = engine
        self.is_mic = ch_type.lower() == "microphone"
        self._muted = False
        self._mon_muted = False
        self._str_muted = False
        self._main_win = None
        self._last_rendered_state = None
        self._last_fx_indicator_active = None
        self._last_runtime_issue_active = None
        self._last_peak_value = 0

        self._mon_commit_timer = QTimer(self)
        self._mon_commit_timer.setSingleShot(True)
        self._mon_commit_timer.setInterval(40)
        self._mon_commit_timer.timeout.connect(self._commit_mon_vol)
        self._str_commit_timer = QTimer(self)
        self._str_commit_timer.setSingleShot(True)
        self._str_commit_timer.setInterval(40)
        self._str_commit_timer.timeout.connect(self._commit_str_vol)
        self._src_commit_timer = QTimer(self)
        self._src_commit_timer.setSingleShot(True)
        self._src_commit_timer.setInterval(40)
        self._src_commit_timer.timeout.connect(self._commit_src_vol)
        self._pending_mon_vol = None
        self._pending_str_vol = None
        self._pending_src_vol = None
        self._src_muted = False

        layout = QVBoxLayout(self)
        self._root_layout = layout
        layout.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        layout.setContentsMargins(8, 8, 8, 8)
        layout.setSpacing(4)

        head_row = QHBoxLayout()
        head_row.setContentsMargins(0, 0, 0, 0)
        self.health_indicator = QLabel("!")
        self.health_indicator.setObjectName("healthIndicator")
        self.health_indicator.setToolTip("Runtime issue detected — right-click for recovery tools")
        self.health_indicator.setVisible(False)
        head_row.addWidget(self.health_indicator)
        head_row.addStretch()
        icon_lbl = QLabel(icon)
        icon_lbl.setObjectName("channelIcon")
        head_row.addWidget(icon_lbl)
        head_row.addStretch()
        self.fx_indicator = QLabel("✨")
        self.fx_indicator.setObjectName("fxIndicator")
        self.fx_indicator.setToolTip("Effect active — right-click → Effects to edit")
        self.fx_indicator.setVisible(False)
        head_row.addWidget(self.fx_indicator)
        layout.addLayout(head_row)

        self.setContextMenuPolicy(Qt.ContextMenuPolicy.CustomContextMenu)
        self.customContextMenuRequested.connect(self._show_context_menu)

        self.name_lbl = QLabel(name)
        self.name_lbl.setObjectName("channelName")
        self.name_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.name_lbl.setWordWrap(True)
        self.name_lbl.setMinimumHeight(24)
        layout.addWidget(self.name_lbl)

        layout.addSpacing(4)

        self.peak_bar = QProgressBar()
        self.peak_bar.setObjectName("peakBar")
        self.peak_bar.setRange(0, 1000)
        self.peak_bar.setTextVisible(False)
        self.peak_bar.setFixedHeight(5)
        self.peak_bar.setValue(0)
        layout.addWidget(self.peak_bar)

        self.src_slider = None
        self.src_vol_lbl = None
        self._src_box_layout = None
        self._src_head_layout = None
        if self.is_mic:
            src_box = QVBoxLayout()
            src_box.setContentsMargins(0, 2, 0, 2)
            src_box.setSpacing(2)
            src_head = QHBoxLayout()
            src_head.setContentsMargins(0, 0, 0, 0)
            src_label = QLabel("MIC")
            src_label.setObjectName("mixTagMic")
            src_head.addWidget(src_label)
            src_head.addStretch()
            self.src_vol_lbl = QLabel("100%")
            self.src_vol_lbl.setAlignment(Qt.AlignmentFlag.AlignRight)
            self.src_vol_lbl.setObjectName("volumeLabel")
            src_head.addWidget(self.src_vol_lbl)
            src_box.addLayout(src_head)
            self.src_slider = QSlider(Qt.Orientation.Horizontal)
            self.src_slider.setRange(0, 100)
            self.src_slider.setValue(100)
            self.src_slider.setToolTip("Hardware mic gain")
            self.src_slider.valueChanged.connect(self._on_src_vol)
            src_box.addWidget(self.src_slider)
            layout.addLayout(src_box)
            self._src_box_layout = src_box
            self._src_head_layout = src_head

        link_row = QHBoxLayout()
        link_row.addStretch()
        self.link_btn = QPushButton("🔗")
        self.link_btn.setObjectName("linkBtn")
        self.link_btn.setCheckable(True)
        self.link_btn.setFixedSize(24, 24)
        self.link_btn.setToolTip("Link the Monitor and Stream faders")
        self.link_btn.clicked.connect(self._on_link_toggle)
        link_row.addWidget(self.link_btn)
        link_row.addStretch()
        layout.addLayout(link_row)

        faders_row = QHBoxLayout()
        faders_row.setSpacing(10)
        self._faders_row = faders_row

        mon_col = QVBoxLayout()
        mon_col.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        mon_label = QLabel("MON")
        mon_label.setObjectName("mixTagMon")
        mon_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        mon_col.addWidget(mon_label)
        self.mon_vol_lbl = QLabel("100%")
        self.mon_vol_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.mon_vol_lbl.setObjectName("volumeLabel")
        mon_col.addWidget(self.mon_vol_lbl)
        self.mon_slider = QSlider(Qt.Orientation.Vertical)
        self.mon_slider.setRange(0, 100)
        self.mon_slider.setValue(100)
        self.mon_slider.setMinimumHeight(140)
        self.mon_slider.valueChanged.connect(self._on_mon_vol)
        mon_col.addWidget(self.mon_slider, 1, Qt.AlignmentFlag.AlignHCenter)
        self.mon_mute = QPushButton("🎧")
        self.mon_mute.setObjectName("muteBtn")
        self.mon_mute.setFixedSize(28, 28)
        self.mon_mute.setToolTip("Mute in Monitor mix")
        self.mon_mute.clicked.connect(self._on_mon_mute)
        mon_col.addWidget(self.mon_mute, 0, Qt.AlignmentFlag.AlignHCenter)
        faders_row.addLayout(mon_col)

        str_col = QVBoxLayout()
        str_col.setAlignment(Qt.AlignmentFlag.AlignHCenter)
        str_label = QLabel("STR")
        str_label.setObjectName("mixTagStr")
        str_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        str_col.addWidget(str_label)
        self.str_vol_lbl = QLabel("100%")
        self.str_vol_lbl.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.str_vol_lbl.setObjectName("volumeLabel")
        str_col.addWidget(self.str_vol_lbl)
        self.str_slider = QSlider(Qt.Orientation.Vertical)
        self.str_slider.setRange(0, 100)
        self.str_slider.setValue(100)
        self.str_slider.setMinimumHeight(140)
        self.str_slider.valueChanged.connect(self._on_str_vol)
        str_col.addWidget(self.str_slider, 1, Qt.AlignmentFlag.AlignHCenter)
        self.str_mute = QPushButton("📡")
        self.str_mute.setObjectName("muteBtn")
        self.str_mute.setFixedSize(28, 28)
        self.str_mute.setToolTip("Mute in Stream mix")
        self.str_mute.clicked.connect(self._on_str_mute)
        str_col.addWidget(self.str_mute, 0, Qt.AlignmentFlag.AlignHCenter)
        faders_row.addLayout(str_col)

        layout.addLayout(faders_row, 1)

        self.fx_btn = QPushButton()
        self.fx_btn.setVisible(False)

    def _apply_scale_metrics(self, metrics):
        self.setFixedWidth(int(metrics.strip_width))
        margin = int(metrics.outer_margin)
        self._root_layout.setContentsMargins(margin, margin, margin, margin)
        self._root_layout.setSpacing(int(metrics.inner_spacing))
        self._faders_row.setSpacing(int(metrics.fader_spacing))
        slider_widget_h = int(metrics.slider_height) + self._VERT_SLIDER_END_PAD
        self.mon_slider.setFixedHeight(slider_widget_h)
        self.str_slider.setFixedHeight(slider_widget_h)
        self.name_lbl.setMinimumHeight(20)
        self.peak_bar.setFixedHeight(int(metrics.peak_height))
        self.link_btn.setFixedSize(int(metrics.link_button_size), int(metrics.link_button_size))
        self.mon_mute.setFixedSize(int(metrics.mute_button_size), int(metrics.mute_button_size))
        self.str_mute.setFixedSize(int(metrics.mute_button_size), int(metrics.mute_button_size))
        if self._src_box_layout is not None:
            vertical_pad = max(1, metrics.inner_spacing - 1)
            self._src_box_layout.setContentsMargins(0, vertical_pad, 0, vertical_pad)
            self._src_box_layout.setSpacing(max(1, metrics.inner_spacing - 1))
        if self._src_head_layout is not None:
            self._src_head_layout.setContentsMargins(0, 0, 0, 0)
            self._src_head_layout.setSpacing(max(2, metrics.inner_spacing))
        if self.src_slider is not None:
            self.src_slider.setFixedHeight(int(metrics.mic_gain_height))

    def measure_scaled_height(self, metrics) -> int:
        self._apply_scale_metrics(metrics)
        self.setMinimumHeight(0)
        self.setMaximumHeight(self._MAX_WIDGET_HEIGHT)
        self._root_layout.activate()
        self.layout().activate()
        return self.sizeHint().height() + self._STRIP_HEIGHT_PAD

    def apply_scale(self, metrics, target_height: int | None = None):
        self._apply_scale_metrics(metrics)
        if target_height is None:
            target_height = self.measure_scaled_height(metrics)
        self.setFixedHeight(int(target_height))

    def _stash_submix(self, mix_name, vol, mute):
        win = self._main_window()
        if not hasattr(win, "submix_state") or not self.node_name:
            return
        win.submix_state[f"{self.node_name}_{mix_name}"] = {"vol": vol, "mute": mute}
        if hasattr(win, "schedule_save"):
            win.schedule_save()

    def _on_link_toggle(self):
        linked = self.link_btn.isChecked()
        if linked:
            self.str_slider.setValue(self.mon_slider.value())
        self._save_link_state(linked)

    def _save_link_state(self, linked):
        win = self._main_window()
        if hasattr(win, "submix_state") and self.node_name:
            win.submix_state[f"{self.node_name}_linked"] = bool(linked)
            if hasattr(win, "schedule_save"):
                win.schedule_save()

    def _on_mon_vol(self, value):
        self.mon_vol_lbl.setText(f"{value}%")
        self._pending_mon_vol = value
        self._mon_commit_timer.start()
        if self.link_btn.isChecked() and self.str_slider.value() != value:
            self.str_slider.setValue(value)

    def _on_src_vol(self, value):
        if self.src_vol_lbl is not None:
            self.src_vol_lbl.setText(f"{value}%")
        self._pending_src_vol = value
        self._src_commit_timer.start()

    def _on_str_vol(self, value):
        self.str_vol_lbl.setText(f"{value}%")
        self._pending_str_vol = value
        self._str_commit_timer.start()
        if self.link_btn.isChecked() and self.mon_slider.value() != value:
            self.mon_slider.setValue(value)

    def _commit_mon_vol(self):
        v = self._pending_mon_vol
        self._pending_mon_vol = None
        if v is None or not self.node_id:
            return
        win = self._main_window()
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            return
        runtime.set_submix_state(
            self.node_id, "Monitor", v / 100.0, self._mon_muted, node_name=self.node_name
        )
        self._stash_submix("Monitor", v / 100.0, self._mon_muted)

    def _commit_str_vol(self):
        v = self._pending_str_vol
        self._pending_str_vol = None
        if v is None or not self.node_id:
            return
        win = self._main_window()
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            return
        runtime.set_submix_state(
            self.node_id, "Stream", v / 100.0, self._str_muted, node_name=self.node_name
        )
        self._stash_submix("Stream", v / 100.0, self._str_muted)

    def _commit_src_vol(self):
        v = self._pending_src_vol
        self._pending_src_vol = None
        if v is None or not self.node_name:
            return
        win = self._main_window()
        runtime = getattr(win, "runtime", None)
        if runtime is None or not hasattr(runtime, "set_source_volume"):
            return
        runtime.set_source_volume(self.node_name, v / 100.0)

    def flush_pending_state(self):
        for timer, commit in (
            (getattr(self, "_mon_commit_timer", None), self._commit_mon_vol),
            (getattr(self, "_str_commit_timer", None), self._commit_str_vol),
            (getattr(self, "_src_commit_timer", None), self._commit_src_vol),
        ):
            if timer is not None and timer.isActive():
                timer.stop()
                commit()

    def _apply_mute_style(self, btn, muted):
        icon = "🎧" if btn == self.mon_mute else "📡"
        target_text = "🔇" if muted else icon
        target_state = "true" if muted else "false"
        if btn.text() == target_text and btn.property("muted") == target_state:
            return
        btn.setText(target_text)
        btn.setProperty("muted", target_state)
        btn.style().unpolish(btn)
        btn.style().polish(btn)

    def _on_mon_mute(self):
        self._mon_muted = not self._mon_muted
        if self.node_id:
            win = self._main_window()
            runtime = getattr(win, "runtime", None)
            if runtime is None:
                return
            runtime.set_submix_state(
                self.node_id,
                "Monitor",
                self.mon_slider.value() / 100.0,
                self._mon_muted,
                node_name=self.node_name,
            )
            self._stash_submix("Monitor", self.mon_slider.value() / 100.0, self._mon_muted)
        self._apply_mute_style(self.mon_mute, self._mon_muted)

    def _on_str_mute(self):
        self._str_muted = not self._str_muted
        if self.node_id:
            win = self._main_window()
            runtime = getattr(win, "runtime", None)
            if runtime is None:
                return
            runtime.set_submix_state(
                self.node_id,
                "Stream",
                self.str_slider.value() / 100.0,
                self._str_muted,
                node_name=self.node_name,
            )
            self._stash_submix("Stream", self.str_slider.value() / 100.0, self._str_muted)
        self._apply_mute_style(self.str_mute, self._str_muted)

    def fx_capture_target(self):
        if self.is_mic:
            return self.node_name
        return f"{self.node_name}.monitor"

    def _main_window(self):
        win = self._main_win
        if win is None:
            win = self.window()
            self._main_win = win
        return win

    def _on_fx_toggle(self):
        win = self._main_window()
        if hasattr(win, "_module_enabled") and not win._module_enabled("effects"):
            QMessageBox.information(
                self,
                "Effects disabled",
                "The effects module is currently disabled for diagnostics.",
            )
            return
        runtime = getattr(win, "runtime", None)
        if runtime is None:
            QMessageBox.warning(
                self,
                "Audio runtime unavailable",
                "WaveLinux's audio runtime is not available, so channel effects cannot be edited right now.",
            )
            return
        dlg = FXSelectionDialog(self.node_id, self.node_name, self.fx_capture_target(), self.engine, runtime, self)
        dlg.exec()
        self._refresh_fx_indicator()

    def _refresh_fx_indicator(self, active=None):
        if not self.node_name:
            self.fx_indicator.setVisible(False)
            self._last_fx_indicator_active = False
            return
        if active is None:
            win = self._main_window()
            active = bool(win.active_effects.get(self.node_name)) if hasattr(win, "active_effects") else False
        visible = bool(active)
        if visible == self._last_fx_indicator_active:
            return
        self.fx_indicator.setVisible(visible)
        self._last_fx_indicator_active = visible

    def set_runtime_issue(self, active, message=""):
        active = bool(active)
        if active != self._last_runtime_issue_active:
            self.health_indicator.setVisible(active)
            self.setProperty("degraded", "true" if active else "false")
            self.style().unpolish(self)
            self.style().polish(self)
            self._last_runtime_issue_active = active
        tip = message or "Runtime issue detected — right-click for recovery tools."
        self.health_indicator.setToolTip(tip if active else "")

    def _show_context_menu(self, pos):
        win = self._main_window()
        menu = QMenu(self)
        menu.setStyleSheet(
            "QMenu { background: #1a1a28; color: #e0e0ee;"
            " border: 1px solid rgba(255,255,255,0.1); border-radius: 6px; padding: 4px; }"
            "QMenu::item { padding: 6px 20px; }"
            "QMenu::item:selected { background: rgba(0,229,255,0.15); }"
            "QMenu::separator { background: rgba(255,255,255,0.08); height: 1px; margin: 4px 6px; }"
        )

        fx_act = menu.addAction("✨ Effects…")
        fx_act.setEnabled(not (hasattr(win, "_module_enabled") and not win._module_enabled("effects")))
        fx_act.triggered.connect(self._on_fx_toggle)

        if self.node_name:
            issue = win.channel_runtime_issue(self.node_name) if hasattr(win, "channel_runtime_issue") else {"degraded": False}
            if issue.get("degraded"):
                status_act = menu.addAction(issue.get("summary") or "Runtime issue detected.")
                status_act.setEnabled(False)
                retry_act = menu.addAction("Retry FX Now")
                retry_act.triggered.connect(self._request_recover)
                diag_act = menu.addAction("Open Diagnostics")
                diag_act.triggered.connect(self._open_diagnostics)
                menu.addSeparator()

        if shutil.which("carla"):
            vst_act = menu.addAction("🎹 Open VST plugin (Carla)…")
            vst_act.triggered.connect(self._launch_carla)

        menu.addSeparator()

        move_left = menu.addAction("◀ Move Left")
        move_left.triggered.connect(lambda: self._request_move(-1))
        move_right = menu.addAction("▶ Move Right")
        move_right.triggered.connect(lambda: self._request_move(1))

        menu.addSeparator()

        if self.ch_type.lower() == "virtual":
            rename_act = menu.addAction("✏️ Rename…")
            rename_act.triggered.connect(self._request_rename)
            remove_act = menu.addAction("❌ Remove Channel")
            remove_act.triggered.connect(self._request_remove)

        hide_act = menu.addAction("👁 Hide")
        hide_act.triggered.connect(self._request_hide)

        menu.exec(self.mapToGlobal(pos))

    def _request_remove(self):
        win = self._main_window()
        if hasattr(win, "_remove_sink") and self.node_name:
            win._remove_sink(self.node_name)

    def _launch_carla(self):
        try:
            subprocess.Popen(["carla"])
        except FileNotFoundError:
            QMessageBox.information(
                self,
                "Carla not found",
                "Install Carla from your distro or upstream package source to host VST3 / LV2 plugins. "
                "WaveLinux bridges to Carla rather than hosting those plugin formats directly.",
            )

    def _request_hide(self):
        win = self._main_window()
        if hasattr(win, "hide_node") and self.node_name:
            win.hide_node(self.node_name)

    def _request_move(self, delta):
        win = self._main_window()
        if hasattr(win, "move_channel") and self.node_name:
            win.move_channel(self.node_name, delta)

    def _request_recover(self):
        win = self._main_window()
        if hasattr(win, "recover_channel") and self.node_name:
            win.recover_channel(self.node_name)

    def _open_diagnostics(self):
        win = self._main_window()
        if hasattr(win, "open_channel_diagnostics") and self.node_name:
            win.open_channel_diagnostics(self.node_name)

    def _request_rename(self):
        win = self._main_window()
        if hasattr(win, "rename_channel") and self.node_name:
            win.rename_channel(self.node_name)

    def on_peak(self, peak_01):
        value = int(max(0.0, min(peak_01, 1.0)) * 1000)
        if value == self._last_peak_value:
            return
        self._last_peak_value = value
        self.peak_bar.setValue(value)

    def update_from_node(self, mon_vol, mon_mute, str_vol, str_mute, is_hidden, source_vol=1.0, source_mute=False):
        self._mon_muted = mon_mute
        self._str_muted = str_mute
        self._src_muted = bool(source_mute)
        mon_pct = int(mon_vol * 100)
        str_pct = int(str_vol * 100)
        src_pct = int(source_vol * 100)
        win = self._main_window()
        is_linked = False
        if hasattr(win, "submix_state") and self.node_name:
            is_linked = bool(win.submix_state.get(f"{self.node_name}_linked", False))
        state = (
            mon_pct,
            bool(mon_mute),
            str_pct,
            bool(str_mute),
            bool(is_linked),
            src_pct if self.is_mic else None,
            bool(source_mute) if self.is_mic else None,
        )
        if state == self._last_rendered_state:
            return

        if self.mon_slider.value() != mon_pct:
            self.mon_slider.blockSignals(True)
            self.mon_slider.setValue(mon_pct)
            self.mon_slider.blockSignals(False)
        if self.str_slider.value() != str_pct:
            self.str_slider.blockSignals(True)
            self.str_slider.setValue(str_pct)
            self.str_slider.blockSignals(False)
        if self.src_slider is not None and self.src_slider.value() != src_pct:
            self.src_slider.blockSignals(True)
            self.src_slider.setValue(src_pct)
            self.src_slider.blockSignals(False)

        if self.link_btn.isChecked() != is_linked:
            self.link_btn.blockSignals(True)
            self.link_btn.setChecked(is_linked)
            self.link_btn.blockSignals(False)

        mon_text = f"{mon_pct}%"
        if self.mon_vol_lbl.text() != mon_text:
            self.mon_vol_lbl.setText(mon_text)
        str_text = f"{str_pct}%"
        if self.str_vol_lbl.text() != str_text:
            self.str_vol_lbl.setText(str_text)
        if self.src_vol_lbl is not None:
            src_text = f"{src_pct}%"
            if self.src_vol_lbl.text() != src_text:
                self.src_vol_lbl.setText(src_text)
        if self.src_slider is not None:
            tip = "Hardware mic gain"
            if source_mute:
                tip += " (currently muted at the source)"
            if self.src_slider.toolTip() != tip:
                self.src_slider.setToolTip(tip)

        self._apply_mute_style(self.mon_mute, mon_mute)
        self._apply_mute_style(self.str_mute, str_mute)
        self._last_rendered_state = state
