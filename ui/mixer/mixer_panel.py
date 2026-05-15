"""Mixer panel builder and strip controller."""

from __future__ import annotations

from dataclasses import dataclass

from PyQt6.QtCore import QEvent, Qt
from PyQt6.QtWidgets import (
    QComboBox,
    QFrame,
    QHBoxLayout,
    QLabel,
    QScrollArea,
    QSlider,
    QVBoxLayout,
    QWidget,
)

from pipewire_engine import PipeWireEngine
from ui.mixer.channel_strip import ChannelStrip, MeterWorker


@dataclass
class MixerStripMetrics:
    strip_width: int
    slider_height: int
    strip_height: int
    outer_margin: int
    inner_spacing: int
    fader_spacing: int
    peak_height: int
    link_button_size: int
    mute_button_size: int
    mic_gain_height: int
    use_horizontal_scroll: bool
    use_vertical_scroll: bool = False


class MixerPanelController:
    _MIN_STRIP_W = ChannelStrip._MIN_W
    _MAX_STRIP_W = ChannelStrip._MAX_W
    _MIN_SLIDER_H = ChannelStrip._MIN_SLIDER_H
    _MAX_SLIDER_H = ChannelStrip._MAX_SLIDER_H
    _SLIDER_WIDTH_SCALE_CAP = ChannelStrip._WIDTH_SCALE_CAP
    _STRIP_CARD_GAP = 2
    _STRIP_ROW_MARGIN = 2
    _STRIP_MIN_OUTER_MARGIN = 1
    _STRIP_MAX_OUTER_MARGIN = 7
    _STRIP_MIN_INNER_SPACING = 1
    _STRIP_MAX_INNER_SPACING = 5
    _STRIP_MIN_FADER_SPACING = 4
    _STRIP_MAX_FADER_SPACING = 10
    _STRIP_MIN_PEAK_HEIGHT = 3
    _STRIP_MAX_PEAK_HEIGHT = 6
    _STRIP_MIN_LINK_SIZE = 18
    _STRIP_MAX_LINK_SIZE = 26
    _STRIP_MIN_MUTE_SIZE = 20
    _STRIP_MAX_MUTE_SIZE = 30
    _STRIP_MIN_MIC_GAIN_HEIGHT = 12
    _STRIP_MAX_MIC_GAIN_HEIGHT = 24
    _STRIP_NON_SLIDER_HEIGHT_BUDGET = 188
    _STRIP_MIN_TOTAL_HEIGHT = 188

    def __init__(self, window):
        self.window = window

    def build(self, parent_layout) -> None:
        body = QVBoxLayout()
        body.setContentsMargins(20, 6, 20, 0)
        body.setSpacing(0)

        input_lbl = QLabel("AUDIO SOURCES")
        input_lbl.setObjectName("sectionLabel")
        body.addWidget(input_lbl)

        self.window.inputs_scroll = QScrollArea()
        self.window.inputs_scroll.setObjectName("inputsScroll")
        self.window.inputs_scroll.setWidgetResizable(False)
        self.window.inputs_scroll.setFrameShape(QFrame.Shape.NoFrame)
        self.window.inputs_scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self.window.inputs_scroll.setVerticalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.window.inputs_scroll.viewport().setObjectName("inputsViewport")
        self.window.inputs_scroll.viewport().setAutoFillBackground(False)
        self.window.inputs_container = QWidget()
        self.window.inputs_container.setObjectName("inputsContainer")
        self.window.input_layout = QHBoxLayout(self.window.inputs_container)
        self.window.input_layout.setAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignTop)
        self.window.input_layout.setContentsMargins(2, 2, 2, 2)
        self.window.input_layout.setSpacing(2)
        self.window.inputs_scroll.setWidget(self.window.inputs_container)
        self.window.inputs_scroll.viewport().installEventFilter(self.window)
        body.addWidget(self.window.inputs_scroll, 1)
        parent_layout.addLayout(body, 1)

        bottom_widget = QWidget()
        bottom_outer = QVBoxLayout(bottom_widget)
        bottom_outer.setContentsMargins(0, 0, 0, 0)
        bottom_outer.setSpacing(0)

        bottom_container = QHBoxLayout()
        bottom_container.setContentsMargins(20, 4, 20, 4)
        bottom_container.setSpacing(20)

        out_frame = QFrame()
        out_frame.setObjectName("routingPanel")
        o_layout = QVBoxLayout(out_frame)
        o_layout.setContentsMargins(12, 8, 12, 8)
        o_title = QLabel("MASTER")
        o_title.setObjectName("sectionLabel")
        o_layout.addWidget(o_title)
        o_layout.addSpacing(4)

        mic_row = QHBoxLayout()
        mic_lbl = QLabel("🎤 Microphone Input")
        mic_lbl.setObjectName("masterMixLabel")
        self.window.mic_in_combo = QComboBox()
        self.window.mic_in_combo.setToolTip("Pick which physical mic the mixer uses")
        self.window.mic_in_combo.currentIndexChanged.connect(self.window._on_mic_input_change)
        mic_row.addWidget(mic_lbl)
        mic_row.addWidget(self.window.mic_in_combo, 1)
        o_layout.addLayout(mic_row)
        o_layout.addSpacing(4)

        mon_row = QHBoxLayout()
        mon_lbl = QLabel("🎧 Monitor Output")
        mon_lbl.setObjectName("masterMixLabel")
        self.window.mon_out_combo = QComboBox()
        self.window.mon_out_combo.setToolTip("Pick the physical output you listen on (headphones / speakers)")
        self.window.mon_out_combo.currentIndexChanged.connect(
            lambda idx: self.window._on_mix_out_change(
                "Monitor",
                self.window.mon_out_combo.itemData(idx),
            )
        )
        mon_row.addWidget(mon_lbl)
        mon_row.addWidget(self.window.mon_out_combo, 1)
        o_layout.addLayout(mon_row)

        self.window.mon_master_slider = QSlider(Qt.Orientation.Horizontal)
        self.window.mon_master_slider.setRange(0, 100)
        self.window.mon_master_slider.setFixedHeight(20)
        self.window.mon_master_slider.valueChanged.connect(
            lambda v: self.window._on_master_vol_change("Monitor", v)
        )
        o_layout.addWidget(self.window.mon_master_slider)
        o_layout.addSpacing(6)

        str_row = QHBoxLayout()
        str_lbl = QLabel("📡 Stream")
        str_lbl.setObjectName("masterMixLabel")
        str_row.addWidget(str_lbl)
        self.window.str_out_label = QLabel("OBS input: WaveLinux-Stream")
        self.window.str_out_label.setObjectName("streamHintLabel")
        str_row.addWidget(self.window.str_out_label, 1)
        self.window.str_out_combo = QComboBox()
        self.window.str_out_combo.hide()
        o_layout.addLayout(str_row)

        str_master_row = QHBoxLayout()
        self.window.str_master_slider = QSlider(Qt.Orientation.Horizontal)
        self.window.str_master_slider.setRange(0, 100)
        self.window.str_master_slider.setFixedHeight(20)
        self.window.str_master_slider.valueChanged.connect(
            lambda v: self.window._on_master_vol_change("Stream", v)
        )
        str_master_row.addWidget(self.window.str_master_slider, 1)
        o_layout.addLayout(str_master_row)

        bottom_container.addWidget(out_frame, 1)
        bottom_outer.addLayout(bottom_container)
        parent_layout.addWidget(bottom_widget, 0)

    def handle_event_filter(self, obj, event) -> None:
        viewport = getattr(getattr(self.window, "inputs_scroll", None), "viewport", lambda: None)()
        if obj is viewport and event.type() == QEvent.Type.Resize:
            self.rescale_strips()

    @staticmethod
    def _clamp_int(value, minimum, maximum):
        return max(int(minimum), min(int(maximum), int(round(value))))

    def compute_strip_metrics(self, strips=None) -> MixerStripMetrics:
        strips = list(strips or self.window.channel_widgets.values())
        count = len(strips)
        if count == 0:
            return MixerStripMetrics(
                strip_width=self._MIN_STRIP_W,
                slider_height=self._MIN_SLIDER_H,
                strip_height=self._STRIP_MIN_TOTAL_HEIGHT,
                outer_margin=self._STRIP_MIN_OUTER_MARGIN,
                inner_spacing=self._STRIP_MIN_INNER_SPACING,
                fader_spacing=self._STRIP_MIN_FADER_SPACING,
                peak_height=self._STRIP_MIN_PEAK_HEIGHT,
                link_button_size=self._STRIP_MIN_LINK_SIZE,
                mute_button_size=self._STRIP_MIN_MUTE_SIZE,
                mic_gain_height=self._STRIP_MIN_MIC_GAIN_HEIGHT,
                use_horizontal_scroll=False,
                use_vertical_scroll=False,
            )

        viewport = self.window.inputs_scroll.viewport()
        avail_w = max(0, int(viewport.width()))
        avail_h = max(0, int(viewport.height()))
        spacing = int(self._STRIP_CARD_GAP)
        horizontal_chrome = int(self._STRIP_ROW_MARGIN) * 2
        available_row_width = max(0, avail_w - horizontal_chrome - spacing * max(0, count - 1))
        ideal_width = available_row_width // count if count else self._MAX_STRIP_W
        use_horizontal_scroll = ideal_width < self._MIN_STRIP_W
        strip_width = self._MIN_STRIP_W if use_horizontal_scroll else self._clamp_int(
            ideal_width,
            self._MIN_STRIP_W,
            self._MAX_STRIP_W,
        )

        width_span = max(1, self._SLIDER_WIDTH_SCALE_CAP - self._MIN_STRIP_W)
        control_width_t = (
            min(strip_width, self._SLIDER_WIDTH_SCALE_CAP) - self._MIN_STRIP_W
        ) / width_span
        control_width_t = max(0.0, min(1.0, control_width_t))

        lerp = lambda lo, hi, t: lo + (hi - lo) * t
        available_card_height = max(
            self._STRIP_MIN_TOTAL_HEIGHT,
            avail_h - int(self._STRIP_ROW_MARGIN) * 2,
        )

        def build_metrics(slider_height):
            slider_span = max(1, self._MAX_SLIDER_H - self._MIN_SLIDER_H)
            height_t = (slider_height - self._MIN_SLIDER_H) / slider_span
            height_t = max(0.0, min(1.0, height_t))
            control_t = min(control_width_t, height_t)
            return MixerStripMetrics(
                strip_width=strip_width,
                slider_height=slider_height,
                strip_height=0,
                outer_margin=self._clamp_int(
                    lerp(self._STRIP_MIN_OUTER_MARGIN, self._STRIP_MAX_OUTER_MARGIN, control_t),
                    self._STRIP_MIN_OUTER_MARGIN,
                    self._STRIP_MAX_OUTER_MARGIN,
                ),
                inner_spacing=self._clamp_int(
                    lerp(self._STRIP_MIN_INNER_SPACING, self._STRIP_MAX_INNER_SPACING, control_t),
                    self._STRIP_MIN_INNER_SPACING,
                    self._STRIP_MAX_INNER_SPACING,
                ),
                fader_spacing=self._clamp_int(
                    lerp(self._STRIP_MIN_FADER_SPACING, self._STRIP_MAX_FADER_SPACING, control_t),
                    self._STRIP_MIN_FADER_SPACING,
                    self._STRIP_MAX_FADER_SPACING,
                ),
                peak_height=self._clamp_int(
                    lerp(self._STRIP_MIN_PEAK_HEIGHT, self._STRIP_MAX_PEAK_HEIGHT, control_t),
                    self._STRIP_MIN_PEAK_HEIGHT,
                    self._STRIP_MAX_PEAK_HEIGHT,
                ),
                link_button_size=self._clamp_int(
                    lerp(self._STRIP_MIN_LINK_SIZE, self._STRIP_MAX_LINK_SIZE, control_t),
                    self._STRIP_MIN_LINK_SIZE,
                    self._STRIP_MAX_LINK_SIZE,
                ),
                mute_button_size=self._clamp_int(
                    lerp(self._STRIP_MIN_MUTE_SIZE, self._STRIP_MAX_MUTE_SIZE, control_t),
                    self._STRIP_MIN_MUTE_SIZE,
                    self._STRIP_MAX_MUTE_SIZE,
                ),
                mic_gain_height=self._clamp_int(
                    lerp(self._STRIP_MIN_MIC_GAIN_HEIGHT, self._STRIP_MAX_MIC_GAIN_HEIGHT, control_t),
                    self._STRIP_MIN_MIC_GAIN_HEIGHT,
                    self._STRIP_MAX_MIC_GAIN_HEIGHT,
                ),
                use_horizontal_scroll=use_horizontal_scroll,
                use_vertical_scroll=False,
            )

        slider_budget = avail_h - self._STRIP_NON_SLIDER_HEIGHT_BUDGET
        slider_height = self._clamp_int(
            slider_budget,
            self._MIN_SLIDER_H,
            self._MAX_SLIDER_H,
        )
        metrics = build_metrics(slider_height)
        strip_height = self.measure_strip_heights(metrics, strips)
        while strip_height > available_card_height and slider_height > self._MIN_SLIDER_H:
            overflow = strip_height - available_card_height
            step = max(4, min(16, overflow))
            next_slider_height = max(self._MIN_SLIDER_H, slider_height - step)
            if next_slider_height == slider_height:
                break
            slider_height = next_slider_height
            metrics = build_metrics(slider_height)
            strip_height = self.measure_strip_heights(metrics, strips)
        metrics.strip_height = strip_height
        metrics.use_vertical_scroll = strip_height > available_card_height
        return metrics

    def measure_strip_heights(self, metrics, strips) -> int:
        strips = list(strips or [])
        if not strips:
            return metrics.strip_height
        return max(strip.measure_scaled_height(metrics) for strip in strips)

    def apply_strip_metrics(self, metrics, strips) -> None:
        horizontal_policy = (
            Qt.ScrollBarPolicy.ScrollBarAsNeeded
            if metrics.use_horizontal_scroll
            else Qt.ScrollBarPolicy.ScrollBarAlwaysOff
        )
        vertical_policy = (
            Qt.ScrollBarPolicy.ScrollBarAsNeeded
            if metrics.use_vertical_scroll
            else Qt.ScrollBarPolicy.ScrollBarAlwaysOff
        )
        self.window.inputs_scroll.setHorizontalScrollBarPolicy(horizontal_policy)
        self.window.inputs_scroll.setVerticalScrollBarPolicy(vertical_policy)
        self.window.input_layout.setSpacing(int(self._STRIP_CARD_GAP))
        self.window.input_layout.setContentsMargins(
            int(self._STRIP_ROW_MARGIN),
            int(self._STRIP_ROW_MARGIN),
            int(self._STRIP_ROW_MARGIN),
            int(self._STRIP_ROW_MARGIN),
        )
        for strip in strips:
            strip.apply_scale(metrics, target_height=metrics.strip_height)
        self.window.inputs_container.adjustSize()

    def rescale_strips(self) -> None:
        strips = list(self.window.channel_widgets.values())
        if not strips:
            return
        metrics = self.compute_strip_metrics(strips)
        if not metrics.strip_height:
            metrics.strip_height = self.measure_strip_heights(metrics, strips)
        self.apply_strip_metrics(metrics, strips)

    def stop_all_meters(self) -> None:
        for meter in list(self.window.meters.values()):
            meter.stop()
        self.window.meters.clear()

    def sync_mic_picker(self, mics, default_src=None) -> None:
        combo = self.window.mic_in_combo
        mic_names = {m.name for m in mics}
        if self.window.selected_mic and self.window.selected_mic in mic_names:
            self.window._mic_selection_initialized = True

        if mics and not getattr(self.window, "_mic_selection_initialized", False):
            if default_src is None:
                default_src = (
                    self.window.engine.get_default_source()
                    if hasattr(self.window.engine, "get_default_source") else None
                )
            if self.window.selected_mic in mic_names:
                self.window._mic_selection_initialized = True
            else:
                target_mic = default_src if default_src and default_src in mic_names else mics[0].name
                self.window._set_selected_mic_target(
                    target_mic,
                    record_preference=False,
                    persist=False,
                    request_refresh=False,
                )
                self.window.schedule_save()

        mic_fp = tuple((m.name, m.description or "") for m in mics)
        if self.window.__dict__.get("_mic_combo_fp") != mic_fp:
            self.window._mic_combo_fp = mic_fp
            combo.blockSignals(True)
            combo.clear()
            for mic in mics:
                label = (
                    getattr(mic, "label", None)
                    or PipeWireEngine.friendly_name(getattr(mic, "description", None))
                    or mic.name
                )
                combo.addItem(label, mic.name)
            if not mics:
                combo.addItem("(no microphone detected)", None)
            combo.blockSignals(False)

        idx = combo.findData(self.window.selected_mic)
        if idx >= 0 and combo.currentIndex() != idx:
            combo.blockSignals(True)
            combo.setCurrentIndex(idx)
            combo.blockSignals(False)

    def input_display_sort_key(self, node_name, *, is_mic=False):
        order = list(self.window.__dict__.get("channel_order", []) or [])
        order_index = {nm: i for i, nm in enumerate(order)}
        return (
            0 if is_mic else 1,
            order_index.get(node_name, len(order_index) + 1),
            str(node_name or "").lower(),
        )

    def sorted_input_nodes(self, mic_nodes, virtual_nodes):
        combined = list(mic_nodes or []) + list(virtual_nodes or [])
        return sorted(
            combined,
            key=lambda node: self.input_display_sort_key(
                getattr(node, "name", ""),
                is_mic=bool(getattr(node, "is_mic", False)),
            ),
        )

    def refresh_view(self, view) -> None:
        mics = list(getattr(view, "mic_inputs", []) or [])
        virtuals = list(getattr(view, "virtual_channels", []) or [])
        self.sync_mic_picker(mics, default_src=getattr(view, "default_source", None))
        if getattr(self.window, "_pending_clipguard_migration", False) and self.window.selected_mic:
            self.window._apply_pending_clipguard_migration()

        present_names = set(getattr(view, "present_node_names", set()) or set())
        if self.window._known_node_names:
            added = present_names - self.window._known_node_names
            removed = self.window._known_node_names - present_names
            if added:
                self.window._notify_hotplug(added, added=True)
            if removed:
                self.window._notify_hotplug(removed, added=False)
        self.window._known_node_names = present_names

        current_node_ids = set()
        visible_strip_ids = []
        input_layout_changed = False
        selected_mic_node = next((m for m in mics if m.name == self.window.selected_mic), None)
        shown_mics = [selected_mic_node] if selected_mic_node is not None else []
        sorted_nodes = self.sorted_input_nodes(shown_mics, virtuals)

        if self.window._module_enabled("mixer_ui"):
            for node in sorted_nodes:
                pw_id = str(node.node_id)
                current_node_ids.add(pw_id)
                node_name = node.name
                is_hidden = node_name in self.window.hidden_nodes
                if is_hidden and not self.window.show_hidden:
                    if pw_id in self.window.channel_widgets:
                        strip = self.window.channel_widgets[pw_id]
                        if not strip.isHidden():
                            strip.hide()
                            input_layout_changed = True
                    meter = self.window.meters.pop(pw_id, None)
                    if meter is not None:
                        meter.stop()
                    continue
                visible_strip_ids.append(pw_id)

                mon_key = f"{node_name}_Monitor"
                str_key = f"{node_name}_Stream"
                fresh_mon = mon_key not in self.window.submix_state
                fresh_str = str_key not in self.window.submix_state
                if fresh_mon:
                    self.window.submix_state[mon_key] = {
                        "vol": float(node.monitor_volume),
                        "mute": bool(node.monitor_mute),
                    }
                if fresh_str:
                    self.window.submix_state[str_key] = {
                        "vol": float(node.stream_volume),
                        "mute": bool(node.stream_mute),
                    }
                if fresh_mon or fresh_str:
                    self.window.schedule_save()

                if pw_id not in self.window.channel_widgets:
                    strip = ChannelStrip(
                        pw_id,
                        node_name,
                        node.label,
                        node.channel_type,
                        node.icon,
                        self.window.engine,
                    )
                    self.window.channel_widgets[pw_id] = strip
                    self.window.input_layout.addWidget(strip)
                    input_layout_changed = True

                strip = self.window.channel_widgets[pw_id]
                strip.node_id = pw_id
                if strip.isHidden():
                    strip.show()
                    input_layout_changed = True
                strip.update_from_node(
                    float(node.monitor_volume),
                    bool(node.monitor_mute),
                    float(node.stream_volume),
                    bool(node.stream_mute),
                    is_hidden,
                    source_vol=float(getattr(node, "source_volume", 1.0)),
                    source_mute=bool(getattr(node, "source_mute", False)),
                )
                strip._refresh_fx_indicator(active=getattr(node, "fx_running", False))
                issue = self.window.channel_runtime_issue(node_name)
                strip.set_runtime_issue(issue["degraded"], issue["tooltip"])
                strip.setToolTip(issue["tooltip"] if issue["degraded"] else "")

                if self.window._module_enabled("metering"):
                    meter = self.window.meters.get(pw_id)
                    meter_source = node.meter_source
                    if meter is None or meter.source_name != meter_source:
                        if meter is not None:
                            meter.stop()
                        meter = MeterWorker(meter_source, self.window)
                        meter.peak.connect(strip.on_peak)
                        meter.start()
                        self.window.meters[pw_id] = meter
                else:
                    meter = self.window.meters.pop(pw_id, None)
                    if meter is not None:
                        meter.stop()
            for stale in [pid for pid in self.window.channel_widgets if pid not in current_node_ids]:
                widget = self.window.channel_widgets.pop(stale)
                widget.setParent(None)
                widget.deleteLater()
                meter = self.window.meters.pop(stale, None)
                if meter is not None:
                    meter.stop()
                input_layout_changed = True
        else:
            self.stop_all_meters()
            for strip in list(self.window.channel_widgets.values()):
                strip.hide()

        visible_strip_sig = tuple(visible_strip_ids)
        if input_layout_changed or visible_strip_sig != self.window._visible_strip_ids:
            self.window._visible_strip_ids = visible_strip_sig
            self.rescale_strips()
            self.window.inputs_container.adjustSize()

        self.refresh_master_controls(view)

    def refresh_master_controls(self, view) -> None:
        combo = self.window.mon_out_combo
        if not combo.view().isVisible():
            current_hw = getattr(view.mixes.get("Monitor"), "hardware_sink", None)
            desired_hw = self.window._desired_mix_hw.get("Monitor")
            current_data = combo.currentData()
            sink_fp = tuple(
                (sink.name, sink.display_name)
                for sink in (getattr(view, "sinks", []) or [])
                if not sink.is_internal and not sink.name.startswith("wavelinux_")
            )
            if sink_fp != self.window._monitor_sink_fp:
                self.window._monitor_sink_fp = sink_fp
                combo.blockSignals(True)
                combo.clear()
                combo.addItem("None", None)
                for sink_name, display_name in sink_fp:
                    combo.addItem(display_name, sink_name)
                idx = combo.findData(desired_hw)
                if idx < 0:
                    idx = combo.findData(current_data)
                if idx < 0 and desired_hw is None:
                    idx = combo.findData(current_hw)
                if idx >= 0:
                    combo.setCurrentIndex(idx)
                combo.blockSignals(False)
            elif desired_hw != current_data:
                idx = combo.findData(desired_hw)
                if idx >= 0 and idx != combo.currentIndex():
                    combo.blockSignals(True)
                    combo.setCurrentIndex(idx)
                    combo.blockSignals(False)

        mon_mix = view.mixes.get("Monitor")
        if mon_mix and not self.window.mon_master_slider.isSliderDown():
            self.window._set_mix_master_volume(
                "Monitor",
                getattr(mon_mix, "master_volume", 1.0),
                persist=False,
                update_slider=False,
            )
            self.window.mon_master_slider.blockSignals(True)
            self.window.mon_master_slider.setValue(int(mon_mix.master_volume * 100))
            self.window.mon_master_slider.blockSignals(False)
        str_mix = view.mixes.get("Stream")
        if str_mix and not self.window.str_master_slider.isSliderDown():
            self.window._set_mix_master_volume(
                "Stream",
                getattr(str_mix, "master_volume", 1.0),
                persist=False,
                update_slider=False,
            )
            self.window.str_master_slider.blockSignals(True)
            self.window.str_master_slider.setValue(int(str_mix.master_volume * 100))
            self.window.str_master_slider.blockSignals(False)

    def relayout_channel_strips(self) -> None:
        widgets = list(self.window.channel_widgets.values())
        for widget in widgets:
            self.window.input_layout.removeWidget(widget)
        name_to_widget = {widget.node_name: widget for widget in widgets if widget.node_name}
        ordered_names = sorted(
            list(name_to_widget.keys()),
            key=lambda node_name: self.input_display_sort_key(
                node_name,
                is_mic=bool(getattr(name_to_widget.get(node_name), "is_mic", False)),
            ),
        )
        for node_name in ordered_names:
            widget = name_to_widget.pop(node_name, None)
            if widget is not None:
                self.window.input_layout.addWidget(widget)
        self.rescale_strips()
        self.window.inputs_container.adjustSize()
