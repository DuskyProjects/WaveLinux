"""Window mixin for device-selection helpers and quick-start setup."""

from __future__ import annotations

import re

from PyQt6.QtWidgets import QComboBox, QDialog, QDialogButtonBox, QLabel, QVBoxLayout

from pipewire_engine import PipeWireEngine
from wavelinux_theme import STYLESHEET


QUICK_START_TEMPLATES = {
    "laptop_mic": {
        "title": "Laptop Mic",
        "description": "Lean setup for a built-in microphone with voice cleanup and a compact starter layout.",
        "channels": ["Music", "Browser", "Voice Chat"],
        "mic_effects": ["rnnoise", "limiter"],
    },
    "usb_interface": {
        "title": "USB Interface",
        "description": "Balanced setup for a USB mic or interface with a simple routing layout and light protection.",
        "channels": ["Music", "Game", "Voice Chat"],
        "mic_effects": ["limiter"],
    },
    "streaming_obs": {
        "title": "Streaming / OBS",
        "description": "Streaming-oriented layout with separate content channels and a fuller default voice chain.",
        "channels": ["Game", "Music", "Browser", "Voice Chat", "Alerts"],
        "mic_effects": ["rnnoise", "compressor", "limiter"],
    },
}


class WindowDeviceMixin:
    def _preferred_setup_mic(self):
        if self.selected_mic:
            return self.selected_mic
        view = self.__dict__.get("_runtime_view_state")
        default_source = getattr(view, "default_source", None) if view is not None else None
        if default_source:
            resolved = self._resolve_hardware_source_name(default_source)
            if resolved:
                return resolved
        mics = list(getattr(view, "mic_inputs", []) or []) if view is not None else []
        if mics:
            return getattr(mics[0], "name", "") or None
        return self._resolve_startup_mic_target()

    @staticmethod
    def _sink_stable_id_from_row(row):
        return str(getattr(row, "stable_id", "") or "").strip()

    @staticmethod
    def _source_stable_id_from_row(row):
        return str(getattr(row, "stable_id", "") or "").strip()

    def _hardware_sink_rows(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        return [
            sink for sink in (getattr(view, "sinks", []) or [])
            if not getattr(sink, "is_internal", False)
            and not str(getattr(sink, "name", "") or "").startswith("wavelinux_")
        ]

    def _hardware_source_rows(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        return list(getattr(view, "mic_inputs", []) or [])

    def _sink_row_for_name(self, sink_name, view=None):
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            return None
        for sink in self._hardware_sink_rows(view):
            if str(getattr(sink, "name", "") or "").strip() == sink_name:
                return sink
        return None

    def _source_row_for_name(self, source_name, view=None):
        source_name = str(source_name or "").strip()
        if not source_name:
            return None
        for source in self._hardware_source_rows(view):
            if str(getattr(source, "name", "") or "").strip() == source_name:
                return source
        return None

    def _sink_row_for_stable_id(self, stable_id, view=None):
        stable_id = str(stable_id or "").strip().lower()
        if not stable_id:
            return None
        for sink in self._hardware_sink_rows(view):
            if self._sink_stable_id_from_row(sink).lower() == stable_id:
                return sink
        return None

    def _source_row_for_stable_id(self, stable_id, view=None):
        stable_id = str(stable_id or "").strip().lower()
        if not stable_id:
            return None
        for source in self._hardware_source_rows(view):
            if self._source_stable_id_from_row(source).lower() == stable_id:
                return source
        return None

    def _display_name_for_sink_name(self, sink_name, view=None):
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            return ""
        row = self._sink_row_for_name(sink_name, view=view)
        if row is not None:
            return str(getattr(row, "display_name", "") or sink_name)
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "display_name_for_sink"):
            return engine.display_name_for_sink(sink_name)
        return sink_name

    def _stable_sink_id_for_name(self, sink_name):
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "stable_sink_id"):
            return engine.stable_sink_id(sink_name)
        return PipeWireEngine.stable_sink_id(sink_name)

    def _stable_source_id_for_name(self, source_name):
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "stable_source_id"):
            return engine.stable_source_id(source_name)
        return PipeWireEngine._stable_device_id_from_props(
            "source",
            source_name,
            {},
            source=True,
        )

    def _resolve_hardware_sink_name(self, sink_name):
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "resolve_hardware_sink_name"):
            return engine.resolve_hardware_sink_name(sink_name)
        return str(sink_name or "").strip() or None

    def _resolve_hardware_source_name(self, source_name):
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "resolve_hardware_source_name"):
            return engine.resolve_hardware_source_name(source_name)
        return str(source_name or "").strip() or None

    def _display_name_for_source_name(self, source_name, view=None):
        source_name = str(source_name or "").strip()
        if not source_name:
            return ""
        row = self._source_row_for_name(source_name, view=view)
        if row is not None:
            return str(getattr(row, "label", "") or getattr(row, "description", "") or source_name)
        engine = self.__dict__.get("engine")
        if engine is not None and hasattr(engine, "display_name_for_source"):
            return engine.display_name_for_source(source_name) or source_name
        return PipeWireEngine.friendly_name(source_name) or source_name

    def _visible_default_sink(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        default_sink = getattr(view, "default_sink", None) if view is not None else None
        engine = self.__dict__.get("engine")
        if not default_sink and engine is not None and hasattr(engine, "get_default_sink"):
            default_sink = engine.get_default_sink()
        if not default_sink:
            return None
        resolved = self._resolve_hardware_sink_name(default_sink)
        if resolved and (view is None or self._sink_row_for_name(resolved, view=view)):
            return resolved
        return None

    def _visible_default_source(self, view=None):
        view = view or self.__dict__.get("_runtime_view_state")
        default_source = getattr(view, "default_source", None) if view is not None else None
        engine = self.__dict__.get("engine")
        if not default_source and engine is not None and hasattr(engine, "get_default_source"):
            default_source = engine.get_default_source()
        if not default_source:
            return None
        resolved = self._resolve_hardware_source_name(default_source)
        if resolved and (view is None or self._source_row_for_name(resolved, view=view)):
            return resolved
        return None

    def _resolve_startup_monitor_target(self, view=None):
        return self._device_policy_controller().resolve_startup_monitor_target(view=view)

    def _resolve_startup_mic_target(self, view=None):
        return self._device_policy_controller().resolve_startup_mic_target(view=view)

    def _record_preferred_monitor(self, sink_name, *, view=None):
        self._device_policy_controller().record_preferred_monitor(sink_name, view=view)

    def _record_preferred_mic(self, source_name, *, view=None):
        self._device_policy_controller().record_preferred_mic(source_name, view=view)

    def _resolve_monitor_fallback(self, view=None):
        rows = self._hardware_sink_rows(view=view)
        if not rows:
            return None
        default_sink = self._visible_default_sink(view=view)
        if default_sink:
            return default_sink
        last_good_monitor_hw_id = self.__dict__.get("_last_good_monitor_hw_id", "")
        if last_good_monitor_hw_id:
            row = self._sink_row_for_stable_id(last_good_monitor_hw_id, view=view)
            if row is not None:
                return getattr(row, "name", None)
        return getattr(rows[0], "name", None)

    def _resolve_mic_fallback(self, view=None):
        rows = self._hardware_source_rows(view=view)
        if not rows:
            return None
        default_source = self._visible_default_source(view=view)
        if default_source:
            return default_source
        last_good_selected_mic_id = self.__dict__.get("_last_good_selected_mic_id", "")
        if last_good_selected_mic_id:
            row = self._source_row_for_stable_id(last_good_selected_mic_id, view=view)
            if row is not None:
                return getattr(row, "name", None)
        return getattr(rows[0], "name", None)

    def _normalize_effect_request_for_node(self, node_name, active_effects, params_map):
        _ = node_name
        wanted = [str(effect_id) for effect_id in list(active_effects or []) if effect_id]
        normalized = {
            str(effect_id): dict(values or {})
            for effect_id, values in dict(params_map or {}).items()
        }
        return wanted, normalized

    def _normalize_loaded_effect_state(self):
        normalized_effects = {}
        normalized_params = {}
        node_names = set((self.active_effects or {}).keys()) | set((self.effect_params or {}).keys())
        for node_name in node_names:
            wanted, params_map = self._normalize_effect_request_for_node(
                node_name,
                (self.active_effects or {}).get(node_name, []),
                (self.effect_params or {}).get(node_name, {}),
            )
            if wanted:
                normalized_effects[node_name] = wanted
            if params_map:
                normalized_params[node_name] = params_map
        self.active_effects = normalized_effects
        self.effect_params = normalized_params

    def _set_selected_mic_target(
        self,
        mic_name,
        *,
        record_preference=False,
        persist=True,
        request_refresh=True,
        view=None,
    ):
        self._device_policy_controller().set_selected_mic_target(
            mic_name,
            record_preference=record_preference,
            persist=persist,
            request_refresh=request_refresh,
            view=view,
        )

    def _restore_preferred_monitor(self):
        self._device_policy_controller().restore_preferred_monitor()

    def _restore_preferred_mic(self):
        self._device_policy_controller().restore_preferred_mic()

    def _reconcile_device_policy(self, view=None):
        return self._device_policy_controller().reconcile_device_policy(view=view)

    def _device_health_issues(self, view=None):
        return self._device_policy_controller().device_health_issues(view=view)

    def _apply_quick_start_template(self, template_id):
        template = QUICK_START_TEMPLATES.get(template_id)
        if template is None:
            return False
        template_channels = list(template.get("channels", []) or [])
        self.virtual_channels = self._dedupe_names(template_channels + list(self.virtual_channels))
        template_order = []
        for display_name in template_channels:
            _, safe = PipeWireEngine._sanitize_channel_name(display_name)
            template_order.append(f"wavelinux_{safe}")
        self.channel_order = self._dedupe_names(template_order + list(self.channel_order))
        for display_name in template_channels:
            self.runtime.ensure_virtual_channel_sync(display_name, refresh=False)

        selected_mic = self._preferred_setup_mic()
        if selected_mic:
            self._set_selected_mic_target(
                selected_mic,
                record_preference=True,
                persist=False,
                request_refresh=False,
                view=self.__dict__.get("_runtime_view_state"),
            )
            self.active_effects[selected_mic] = list(template.get("mic_effects", []) or [])

        default_sink = self._resolve_startup_monitor_target()
        if default_sink:
            self._set_mix_output_target(
                "Monitor",
                default_sink,
                persist=False,
                update_combo=True,
                sync_runtime=True,
                sync_runtime_refresh=False,
            )
            self._record_preferred_monitor(default_sink, view=self.__dict__.get("_runtime_view_state"))
        self._selected_setup_template = template_id
        self._onboarding_completed = True
        self._show_first_run_setup = False
        self._sync_runtime_persistent_state(immediate=True)
        self.schedule_save()
        self._refresh()
        self.status_lbl.setText(f"Applied quick start: {template['title']}")
        return True

    def _open_quick_start_setup(self):
        first_run = bool(self._show_first_run_setup and not self._onboarding_completed)
        dialog = QDialog(self)
        dialog.setWindowTitle("Quick Start Setup")
        dialog.setMinimumWidth(480)
        dialog.setStyleSheet(STYLESHEET)
        layout = QVBoxLayout(dialog)
        layout.setContentsMargins(16, 16, 16, 16)
        layout.setSpacing(12)

        title = QLabel("QUICK START")
        title.setObjectName("sectionLabel")
        layout.addWidget(title)

        intro = QLabel(
            "Pick a starter template. You can run this again later from Settings to reshape channels and default mic FX."
        )
        intro.setWordWrap(True)
        intro.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        layout.addWidget(intro)

        combo = QComboBox()
        for template_id, meta in QUICK_START_TEMPLATES.items():
            combo.addItem(meta["title"], template_id)
        if self._selected_setup_template:
            index = combo.findData(self._selected_setup_template)
            if index >= 0:
                combo.setCurrentIndex(index)
        layout.addWidget(combo)

        desc_label = QLabel()
        desc_label.setWordWrap(True)
        desc_label.setStyleSheet("color: #8b8b9e; font-size: 11px;")
        layout.addWidget(desc_label)

        def _sync_desc():
            template = QUICK_START_TEMPLATES.get(combo.currentData(), {})
            channels = ", ".join(template.get("channels", []) or []) or "None"
            mic_fx = ", ".join(template.get("mic_effects", []) or []) or "None"
            desc_label.setText(
                f"{template.get('description', '').strip()}\n\n"
                f"Starter channels: {channels}\n"
                f"Mic FX: {mic_fx}"
            )

        combo.currentIndexChanged.connect(_sync_desc)
        _sync_desc()

        buttons = QDialogButtonBox()
        apply_btn = buttons.addButton("Apply Template", QDialogButtonBox.ButtonRole.AcceptRole)
        skip_text = "Skip for now" if first_run else "Cancel"
        skip_btn = buttons.addButton(skip_text, QDialogButtonBox.ButtonRole.RejectRole)
        buttons.accepted.connect(dialog.accept)
        buttons.rejected.connect(dialog.reject)
        layout.addWidget(buttons)

        apply_btn.setDefault(True)
        skip_btn.setAutoDefault(False)

        if dialog.exec() != QDialog.DialogCode.Accepted:
            if first_run:
                self._onboarding_completed = True
                self._show_first_run_setup = False
                self.save_config()
                self.status_lbl.setText("Quick start skipped")
            return
        if self._apply_quick_start_template(combo.currentData()):
            self.save_config()
