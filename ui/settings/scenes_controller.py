"""Scenes settings tab controller and scene-state helpers."""

from __future__ import annotations

import re
import time

from PyQt6.QtWidgets import QInputDialog, QMessageBox

from pipewire_engine import PipeWireEngine


class ScenesTabController:
    def __init__(self, window):
        self.window = window

    @staticmethod
    def dedupe_names(values):
        seen = set()
        ordered = []
        for value in values or ():
            if not isinstance(value, str):
                continue
            clean = re.sub(r"\s+", " ", value).strip()
            if not clean or clean in seen:
                continue
            seen.add(clean)
            ordered.append(clean)
        return ordered

    @staticmethod
    def scene_owner_from_key(key):
        if not isinstance(key, str) or "_" not in key:
            return str(key or "")
        return key.rsplit("_", 1)[0]

    @staticmethod
    def normalize_mix_volume(value, default=1.0):
        try:
            numeric = float(value)
        except (TypeError, ValueError):
            numeric = float(default)
        return max(0.0, min(1.0, numeric))

    def current_mix_master_volume(self, mix_name, default=1.0):
        desired = self.window.__dict__.get("_desired_mix_volumes", {}).get(mix_name)
        if desired is not None:
            return self.normalize_mix_volume(desired, default)
        slider_name = "mon_master_slider" if mix_name == "Monitor" else "str_master_slider"
        slider = self.window.__dict__.get(slider_name)
        if slider is not None and hasattr(slider, "value"):
            return self.normalize_mix_volume(slider.value() / 100.0, default)
        return self.normalize_mix_volume(default, default)

    def set_mix_master_volume(self, mix_name, volume, *, persist=True, update_slider=False):
        normalized = self.normalize_mix_volume(volume)
        desired = self.window.__dict__.get("_desired_mix_volumes")
        if desired is None:
            self.window._desired_mix_volumes = {"Monitor": 1.0, "Stream": 1.0}
            desired = self.window._desired_mix_volumes
        desired[mix_name] = normalized
        if update_slider:
            slider_name = "mon_master_slider" if mix_name == "Monitor" else "str_master_slider"
            slider = self.window.__dict__.get(slider_name)
            if slider is not None and hasattr(slider, "setValue"):
                slider_value = int(round(normalized * 100))
                if not getattr(slider, "isSliderDown", lambda: False)():
                    slider.blockSignals(True)
                    slider.setValue(slider_value)
                    slider.blockSignals(False)
        if persist:
            self.window.schedule_save()

    def capture_scene_snapshot(self):
        submixes = {}
        for key, value in (self.window.submix_state or {}).items():
            if not isinstance(key, str):
                continue
            if isinstance(value, dict):
                submixes[key] = {
                    "vol": float(value.get("vol", 1.0)),
                    "mute": bool(value.get("mute", False)),
                }
            elif key.endswith("_linked"):
                submixes[key] = bool(value)
        return {
            "saved_at": int(time.time()),
            "selected_mic": self.window.selected_mic or None,
            "selected_mic_id": (
                self.window._stable_source_id_for_name(self.window.selected_mic)
                if self.window.selected_mic else ""
            ),
            "monitor_hw": self.window._desired_mix_hw.get("Monitor"),
            "monitor_hw_id": self.window._stable_sink_id_for_name(
                self.window._desired_mix_hw.get("Monitor")
            ),
            "stream_hw": self.window._desired_mix_hw.get("Stream"),
            "monitor_mix_volume": self.current_mix_master_volume("Monitor"),
            "stream_mix_volume": self.current_mix_master_volume("Stream"),
            "virtual_channels": list(self.dedupe_names(self.window.virtual_channels)),
            "channel_order": self.dedupe_names(self.window.channel_order),
            "submixes": submixes,
            "app_routing": {
                k: v for k, v in (self.window.app_routing or {}).items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            "app_volumes": {
                k: v for k, v in (
                    self.normalize_app_volume_prefs(
                        getattr(self.window, "app_volumes", {}),
                    ) or {}
                ).items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            "active_effects": {
                k: list(v) for k, v in (self.window.active_effects or {}).items()
                if isinstance(k, str) and isinstance(v, list)
            },
            "effect_params": {
                node_name: {
                    effect_id: dict(values)
                    for effect_id, values in (effects or {}).items()
                    if isinstance(effect_id, str) and isinstance(values, dict)
                }
                for node_name, effects in (self.window.effect_params or {}).items()
                if isinstance(node_name, str) and isinstance(effects, dict)
            },
        }

    @staticmethod
    def normalize_app_volume_prefs(raw):
        if not isinstance(raw, dict):
            return {}
        cleaned = {}
        for app_id, value in raw.items():
            if not isinstance(app_id, str) or not PipeWireEngine.is_persistent_app_id(app_id):
                continue
            try:
                cleaned[app_id] = max(0.0, min(float(value), 1.0))
            except (TypeError, ValueError):
                continue
        return cleaned

    @classmethod
    def normalize_scene_snapshot(cls, raw):
        if not isinstance(raw, dict):
            return None
        submixes = {}
        for key, value in (raw.get("submixes", {}) or {}).items():
            if not isinstance(key, str):
                continue
            if isinstance(value, dict):
                submixes[key] = {
                    "vol": float(value.get("vol", 1.0)),
                    "mute": bool(value.get("mute", False)),
                }
            elif key.endswith("_linked"):
                submixes[key] = bool(value)
        effect_params = {}
        for node_name, effects in (raw.get("effect_params", {}) or {}).items():
            if not isinstance(node_name, str) or not isinstance(effects, dict):
                continue
            clean_effects = {}
            for effect_id, values in effects.items():
                if isinstance(effect_id, str) and isinstance(values, dict):
                    clean_effects[effect_id] = {
                        str(param): float(val)
                        for param, val in values.items()
                        if isinstance(param, str) and isinstance(val, (int, float))
                    }
            effect_params[node_name] = clean_effects
        active_effects = {
            node_name: [str(effect_id) for effect_id in effects if isinstance(effect_id, str)]
            for node_name, effects in (raw.get("active_effects", {}) or {}).items()
            if isinstance(node_name, str) and isinstance(effects, list)
        }
        return {
            "saved_at": int(raw.get("saved_at") or time.time()),
            "selected_mic": raw.get("selected_mic") or None,
            "selected_mic_id": str(raw.get("selected_mic_id") or "").strip(),
            "monitor_hw": raw.get("monitor_hw"),
            "monitor_hw_id": str(raw.get("monitor_hw_id") or "").strip(),
            "stream_hw": raw.get("stream_hw"),
            "monitor_mix_volume": cls.normalize_mix_volume(
                raw.get("monitor_mix_volume", 1.0)
            ),
            "stream_mix_volume": cls.normalize_mix_volume(
                raw.get("stream_mix_volume", 1.0)
            ),
            "virtual_channels": cls.dedupe_names(raw.get("virtual_channels", []) or []),
            "channel_order": cls.dedupe_names(raw.get("channel_order", []) or []),
            "submixes": submixes,
            "app_routing": {
                k: v for k, v in (raw.get("app_routing", {}) or {}).items()
                if isinstance(k, str) and PipeWireEngine.is_persistent_app_id(k)
            },
            "app_volumes": cls.normalize_app_volume_prefs(raw.get("app_volumes", {})),
            "active_effects": active_effects,
            "effect_params": effect_params,
        }

    @classmethod
    def normalize_scene_library(cls, raw):
        if not isinstance(raw, dict):
            return {}
        scenes = {}
        for name, snapshot in raw.items():
            if not isinstance(name, str):
                continue
            clean_name = re.sub(r"\s+", " ", name).strip()
            if not clean_name:
                continue
            normalized = cls.normalize_scene_snapshot(snapshot)
            if normalized is not None:
                scenes[clean_name] = normalized
        return scenes

    def selected_scene_name(self):
        combo = getattr(self.window, "_scene_combo", None)
        if combo is None or getattr(combo, "count", lambda: 0)() <= 0:
            return ""
        current_data = getattr(combo, "currentData", lambda: "")()
        current_text = getattr(combo, "currentText", lambda: "")()
        return (current_data or current_text or "").strip()

    def scene_summary_text(self, snapshot):
        snapshot = self.normalize_scene_snapshot(snapshot)
        if not snapshot:
            return "No saved scenes yet."
        saved_at = int(snapshot.get("saved_at") or 0)
        stamp = (
            time.strftime("%Y-%m-%d %H:%M", time.localtime(saved_at))
            if saved_at else "unknown"
        )
        selected_mic = snapshot.get("selected_mic") or "system default mic"
        return (
            f"Saved {stamp} · mic: {selected_mic} · "
            f"channels: {len(snapshot.get('virtual_channels', []))} · "
            f"routes: {len(snapshot.get('app_routing', {}))} · "
            f"FX chains: {len(snapshot.get('active_effects', {}))}"
        )

    def on_scene_selection_change(self, _index):
        self.refresh_scenes_tab()

    def refresh_scenes_tab(self, selected_name=None):
        if not self.window._module_enabled("scenes"):
            return
        combo = getattr(self.window, "_scene_combo", None)
        summary_lbl = getattr(self.window, "_scene_summary_lbl", None)
        if combo is None or summary_lbl is None:
            return
        current_data = getattr(combo, "currentData", lambda: "")()
        current_text = getattr(combo, "currentText", lambda: "")()
        current = (selected_name or current_data or current_text or "").strip()
        names = list(self.window.scenes.keys())
        combo.blockSignals(True)
        combo.clear()
        for name in names:
            combo.addItem(name, name)
        if current:
            idx = combo.findData(current)
            if idx >= 0:
                combo.setCurrentIndex(idx)
        combo.blockSignals(False)
        selected = self.selected_scene_name()
        snapshot = self.window.scenes.get(selected)
        summary_lbl.setText(self.scene_summary_text(snapshot))
        has_scene = bool(snapshot)
        for attr in (
            "_apply_scene_btn",
            "_overwrite_scene_btn",
            "_rename_scene_btn",
            "_delete_scene_btn",
        ):
            btn = getattr(self.window, attr, None)
            if btn is not None:
                btn.setEnabled(has_scene)
        self.window._mark_settings_tab_refreshed("Scenes")

    def apply_scene_snapshot(self, snapshot, *, scene_name=""):
        snapshot = self.normalize_scene_snapshot(snapshot)
        if snapshot is None:
            return False
        window = self.window
        scene_virtuals = list(snapshot.get("virtual_channels", []))
        existing_virtuals = [
            name for name in window.virtual_channels if name not in scene_virtuals
        ]
        window.virtual_channels = scene_virtuals + existing_virtuals
        for name in scene_virtuals:
            window.runtime.ensure_virtual_channel_sync(name, refresh=False)

        selected_mic = (
            window._resolve_hardware_source_name(snapshot.get("selected_mic_id"))
            or window._resolve_hardware_source_name(snapshot.get("selected_mic"))
            or snapshot.get("selected_mic")
            or None
        )
        if selected_mic:
            window._set_selected_mic_target(
                selected_mic,
                record_preference=True,
                persist=False,
                request_refresh=False,
                view=window.__dict__.get("_runtime_view_state"),
            )
        else:
            window.selected_mic = None
            window._mic_selection_initialized = False

        scene_nodes = set(snapshot.get("channel_order", []) or [])
        scene_nodes.update(snapshot.get("active_effects", {}).keys())
        scene_nodes.update(snapshot.get("effect_params", {}).keys())
        scene_nodes.update(
            self.scene_owner_from_key(key)
            for key in (snapshot.get("submixes", {}) or {}).keys()
        )
        if selected_mic:
            scene_nodes.add(selected_mic)

        window.submix_state = {
            key: value
            for key, value in window.submix_state.items()
            if self.scene_owner_from_key(key) not in scene_nodes
        }
        window.submix_state.update(snapshot.get("submixes", {}))

        window.active_effects = {
            key: value
            for key, value in window.active_effects.items()
            if key not in scene_nodes
        }
        window.active_effects.update(snapshot.get("active_effects", {}))

        window.effect_params = {
            key: value
            for key, value in window.effect_params.items()
            if key not in scene_nodes
        }
        window.effect_params.update(snapshot.get("effect_params", {}))

        window.app_routing = dict(snapshot.get("app_routing", {}))
        window.app_volumes = self.normalize_app_volume_prefs(
            snapshot.get("app_volumes", {})
        )
        scene_order = list(snapshot.get("channel_order", []))
        window.channel_order = scene_order + [
            name for name in window.channel_order
            if name not in scene_order
        ]
        self.set_mix_master_volume(
            "Monitor",
            snapshot.get("monitor_mix_volume", 1.0),
            persist=False,
            update_slider=True,
        )
        self.set_mix_master_volume(
            "Stream",
            snapshot.get("stream_mix_volume", 1.0),
            persist=False,
            update_slider=True,
        )
        window._set_mix_output_target(
            "Monitor",
            (
                window._resolve_hardware_sink_name(snapshot.get("monitor_hw_id"))
                or window._resolve_hardware_sink_name(snapshot.get("monitor_hw"))
                or snapshot.get("monitor_hw")
            ),
            persist=False,
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        window._record_preferred_monitor(
            window.__dict__.get("_desired_mix_hw", {}).get("Monitor"),
            view=window.__dict__.get("_runtime_view_state"),
        )
        window._set_mix_output_target(
            "Stream",
            snapshot.get("stream_hw"),
            persist=False,
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        window._sync_runtime_persistent_state(immediate=True)
        window.schedule_save()
        if hasattr(window, "_refresh_hidden_list"):
            window._refresh_hidden_list()
        if hasattr(window, "_refresh_advanced_tab"):
            window._refresh_advanced_tab()
        if hasattr(window, "_refresh_scenes_tab"):
            window._refresh_scenes_tab(scene_name or self.selected_scene_name())
        if hasattr(window, "_refresh"):
            window._refresh()
        label = scene_name or "scene"
        window.status_lbl.setText(f"Applied {label}")
        return True

    def save_current_scene_as(self):
        current_name = self.selected_scene_name()
        text, ok = QInputDialog.getText(
            self.window.settings_dialog,
            "Save Scene",
            "Scene name:",
            text=current_name,
        )
        if not ok:
            return
        name = re.sub(r"\s+", " ", text).strip()
        if not name:
            return
        if name in self.window.scenes:
            yn = QMessageBox.question(
                self.window.settings_dialog,
                "Overwrite Scene",
                f"Replace the existing scene '{name}'?",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if yn != QMessageBox.StandardButton.Yes:
                return
        self.window.scenes[name] = self.capture_scene_snapshot()
        self.window.save_config()
        self.refresh_scenes_tab(name)
        self.window.status_lbl.setText(f"Saved scene: {name}")

    def overwrite_selected_scene(self):
        name = self.selected_scene_name()
        if not name:
            return
        self.window.scenes[name] = self.capture_scene_snapshot()
        self.window.save_config()
        self.refresh_scenes_tab(name)
        self.window.status_lbl.setText(f"Updated scene: {name}")

    def apply_selected_scene(self):
        name = self.selected_scene_name()
        if not name:
            return
        self.apply_scene_snapshot(self.window.scenes.get(name), scene_name=name)

    def rename_selected_scene(self):
        current = self.selected_scene_name()
        if not current:
            return
        text, ok = QInputDialog.getText(
            self.window.settings_dialog,
            "Rename Scene",
            "Scene name:",
            text=current,
        )
        if not ok:
            return
        new_name = re.sub(r"\s+", " ", text).strip()
        if not new_name or new_name == current:
            return
        if new_name in self.window.scenes:
            yn = QMessageBox.question(
                self.window.settings_dialog,
                "Replace Scene",
                f"Replace the existing scene '{new_name}'?",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if yn != QMessageBox.StandardButton.Yes:
                return
        snapshot = self.window.scenes.pop(current)
        self.window.scenes[new_name] = snapshot
        self.window.save_config()
        self.refresh_scenes_tab(new_name)
        self.window.status_lbl.setText(f"Renamed scene: {new_name}")

    def delete_selected_scene(self):
        name = self.selected_scene_name()
        if not name:
            return
        yn = QMessageBox.question(
            self.window.settings_dialog,
            "Delete Scene",
            f"Delete the scene '{name}'?",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        self.window.scenes.pop(name, None)
        self.window.save_config()
        self.refresh_scenes_tab()
        self.window.status_lbl.setText(f"Deleted scene: {name}")
