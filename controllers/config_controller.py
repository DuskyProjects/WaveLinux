"""Config serialization, migration, import, and load helpers."""

from __future__ import annotations

import json
import logging
import os
import shutil
import time

from PyQt6.QtWidgets import QFileDialog, QMessageBox

from app_core import ConfigChanged
from pipewire_engine import PipeWireEngine


class ConfigController:
    def __init__(self, window):
        self.window = window

    def _attrs(self):
        return self.window.__dict__

    def _sync_window_state(self):
        sync = getattr(type(self.window), "_sync_window_state", None)
        if sync is not None:
            sync(self.window)

    def _publish_config_changed(self, config):
        event_bus = self._attrs().get("_event_bus")
        if event_bus is not None:
            event_bus.publish(ConfigChanged(config=config))
        manager = self._attrs().get("module_manager")
        if manager is not None:
            manager.on_config_changed(config)

    def schedule_save(self):
        timer = self._attrs().get("_save_timer")
        if timer is not None:
            timer.start(500)

    def flush_pending_ui_state(self):
        master_timer = self._attrs().get("_master_commit_timer")
        if master_timer is not None and master_timer.isActive():
            master_timer.stop()
            self.window._commit_master_vols()

        for strip in self._attrs().get("channel_widgets", {}).values():
            if hasattr(strip, "flush_pending_state"):
                strip.flush_pending_state()

        for row in self._attrs().get("app_widgets", {}).values():
            if hasattr(row, "flush_pending_state"):
                row.flush_pending_state()

    def virtual_channel_specs(self):
        specs = {}
        for display_name in self.window.virtual_channels:
            clean, safe = PipeWireEngine._sanitize_channel_name(display_name)
            specs[f"wavelinux_{safe}"] = clean
        return specs

    def sync_runtime_persistent_state(self, *, immediate=False):
        monitor_hw = self.window._desired_mix_hw.get("Monitor")
        stream_hw = self.window._desired_mix_hw.get("Stream")
        if monitor_hw is None and hasattr(self.window, "mon_out_combo"):
            monitor_hw = self.window.mon_out_combo.currentData()
        if stream_hw is None and hasattr(self.window, "str_out_combo"):
            stream_hw = self.window.str_out_combo.currentData()
        self.window._suppress_pactl_events_for(1.5)
        self.window.runtime.sync_persistent_state(
            selected_mic=self.window.selected_mic,
            submix_state=self.window.submix_state,
            active_effects=self.window.active_effects,
            effect_params=self.window.effect_params,
            app_routing=dict(self.window.app_routing),
            app_volumes=self.window._normalize_app_volume_prefs(
                getattr(self.window, "app_volumes", {}),
            ),
            virtual_channels=self.window._virtual_channel_specs(),
            monitor_hw=monitor_hw,
            stream_hw=stream_hw,
            monitor_mix_volume=self.window._current_mix_master_volume("Monitor"),
            stream_mix_volume=self.window._current_mix_master_volume("Stream"),
            apply_now=bool(immediate),
        )

    def serialize_config(self):
        scenes = self._attrs().get("scenes", {})
        desired_mix_hw = self._attrs().get("_desired_mix_hw", {}) or {}
        return {
            "schema_version": 1,
            "monitor_hw": desired_mix_hw.get("Monitor"),
            "stream_hw": desired_mix_hw.get("Stream"),
            "preferred_monitor_hw_id": self._attrs().get("_preferred_monitor_hw_id", ""),
            "preferred_monitor_hw_name": self._attrs().get("_preferred_monitor_hw_name", ""),
            "preferred_selected_mic_id": self._attrs().get("_preferred_selected_mic_id", ""),
            "preferred_selected_mic_name": self._attrs().get("_preferred_selected_mic_name", ""),
            "monitor_mix_volume": self.window._current_mix_master_volume("Monitor"),
            "stream_mix_volume": self.window._current_mix_master_volume("Stream"),
            "channels": list(self.window.virtual_channels),
            "scenes": self.window._normalize_scene_library(scenes),
            "onboarding_completed": bool(self._attrs().get("_onboarding_completed", True)),
            "quick_start_template": self._attrs().get("_selected_setup_template", ""),
            "selected_mic": self.window.selected_mic,
            "submixes": dict(self.window.submix_state),
            "hidden": sorted(self.window.hidden_nodes),
            "app_routing": {
                k: v for k, v in self.window.app_routing.items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            "app_volumes": {
                k: v for k, v in self.window._normalize_app_volume_prefs(
                    getattr(self.window, "app_volumes", {}),
                ).items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            "channel_order": list(self.window.channel_order),
            "effect_params": self.window.effect_params,
            "active_effects": self.window.active_effects,
            "app_last_seen": {
                k: v for k, v in self.window.app_last_seen.items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            "app_display_names": {
                k: v for k, v in self.window.app_display_names.items()
                if PipeWireEngine.is_persistent_app_id(k)
            },
            "app_identity_overrides": self.window._normalize_app_identity_overrides(
                self._attrs().get("app_identity_overrides", {}),
            ),
            "app_label_overrides": self.window._normalize_app_label_overrides(
                self._attrs().get("app_label_overrides", {}),
            ),
            "app_prune_days": self.window.app_prune_days,
            "forgotten_apps": sorted(
                name for name in self.window.forgotten_apps
                if PipeWireEngine.is_persistent_app_id(name)
            ),
        }

    @staticmethod
    def write_config_file(path, payload):
        tmp = path + ".tmp"
        with open(tmp, "w") as fh:
            json.dump(payload, fh, indent=4)
        os.replace(tmp, path)

    def apply_config_dict(self, conf, *, remove_missing_virtuals=False):
        if not isinstance(conf, dict):
            raise ValueError("WaveLinux config must be a JSON object.")
        runtime = getattr(self.window, "runtime", None)
        previous_virtuals = list(self._attrs().get("virtual_channels", []) or [])
        self.window.submix_state = self.window._migrate_submix_state(conf.get("submixes", {}))
        self.window.hidden_nodes = self.window._migrate_hidden_nodes(conf.get("hidden", []))
        self.window.app_routing = {
            k: v for k, v in (conf.get("app_routing", {}) or {}).items()
            if PipeWireEngine.is_persistent_app_id(k)
        }
        self.window.app_volumes = self.window._normalize_app_volume_prefs(conf.get("app_volumes", {}))
        self.window.app_last_seen = {
            k: int(v) for k, v in (conf.get("app_last_seen", {}) or {}).items()
            if isinstance(k, str) and isinstance(v, (int, float))
            and PipeWireEngine.is_persistent_app_id(k)
        }
        self.window.app_display_names = {
            k: v for k, v in (conf.get("app_display_names", {}) or {}).items()
            if isinstance(k, str) and isinstance(v, str) and PipeWireEngine.is_persistent_app_id(k)
        }
        self.window.app_identity_overrides = self.window._normalize_app_identity_overrides(
            conf.get("app_identity_overrides", {}),
        )
        self.window.app_label_overrides = self.window._normalize_app_label_overrides(
            conf.get("app_label_overrides", {}),
        )
        self.window.app_prune_days = int(conf.get("app_prune_days", self.window.app_prune_days) or 14)
        self.window.forgotten_apps = {
            name for name in (conf.get("forgotten_apps", []) or [])
            if isinstance(name, str) and PipeWireEngine.is_persistent_app_id(name)
        }
        for name in list(self.window.app_routing.keys()):
            if PipeWireEngine.name_matches_host(name):
                self.window.app_routing.pop(name, None)
        for name in list(self.window.app_volumes.keys()):
            if PipeWireEngine.name_matches_host(name):
                self.window.app_volumes.pop(name, None)
        for name in list(self.window.app_last_seen.keys()):
            if PipeWireEngine.name_matches_host(name):
                self.window.app_last_seen.pop(name, None)
                self.window.app_display_names.pop(name, None)
        for app_id in (
            set(self.window.app_routing)
            | set(self.window.app_volumes)
            | set(self.window.app_last_seen)
            | set(self.window.forgotten_apps)
            | set(self.window.app_identity_overrides.values())
            | set(self.window.app_label_overrides.keys())
        ):
            self.window.app_display_names.setdefault(
                app_id,
                PipeWireEngine.display_name_for_app_id(app_id),
            )
        for app_id, label in self.window.app_label_overrides.items():
            self.window.app_display_names[app_id] = label
        self.window._set_engine_identity_overrides()
        self.window._prune_stale_apps()
        self.window.virtual_channels = self.window._dedupe_names(conf.get("channels", []) or [])
        self.window.scenes = self.window._normalize_scene_library(conf.get("scenes", {}))
        self.window._onboarding_completed = bool(conf.get("onboarding_completed", True))
        self.window._selected_setup_template = str(conf.get("quick_start_template") or "")
        self.window.channel_order = self.window._dedupe_names(conf.get("channel_order", []) or [])
        self.window.selected_mic = None
        self.window._mic_selection_initialized = False
        self.window._preferred_monitor_hw_id = str(
            conf.get("preferred_monitor_hw_id")
            or self.window._stable_sink_id_for_name(conf.get("monitor_hw"))
            or ""
        ).strip()
        self.window._preferred_monitor_hw_name = str(
            conf.get("preferred_monitor_hw_name")
            or conf.get("monitor_hw")
            or ""
        ).strip()
        self.window._preferred_selected_mic_id = str(
            conf.get("preferred_selected_mic_id")
            or self.window._stable_source_id_for_name(conf.get("selected_mic"))
            or ""
        ).strip()
        self.window._preferred_selected_mic_name = str(
            conf.get("preferred_selected_mic_name")
            or conf.get("selected_mic")
            or ""
        ).strip()
        self.window._restorable_monitor_hw_id = ""
        self.window._restorable_monitor_hw_name = ""
        self.window._restorable_selected_mic_id = ""
        self.window._restorable_selected_mic_name = ""
        self.window._active_monitor_fallback = False
        self.window._active_mic_fallback = False
        self.window._set_mix_master_volume(
            "Monitor",
            conf.get("monitor_mix_volume", 1.0),
            persist=False,
            update_slider=True,
        )
        self.window._set_mix_master_volume(
            "Stream",
            conf.get("stream_mix_volume", 1.0),
            persist=False,
            update_slider=True,
        )
        self.window.effect_params = conf.get("effect_params", {}) or {}
        self.window.active_effects = {
            k: list(v) for k, v in (conf.get("active_effects", {}) or {}).items()
            if isinstance(v, list)
        }

        comp_key_remap = {
            "threshold_db": "Threshold level (dB)",
            "ratio": "Ratio (1:n)",
            "attack_ms": "Attack time (ms)",
            "release_ms": "Release time (ms)",
            "makeup_gain_db": "Makeup gain (dB)",
        }
        for node_params in self.window.effect_params.values():
            comp = node_params.get("compressor")
            if not isinstance(comp, dict):
                continue
            for old_key, new_key in comp_key_remap.items():
                if old_key in comp and new_key not in comp:
                    comp[new_key] = comp.pop(old_key)
                elif old_key in comp:
                    comp.pop(old_key, None)
        self.window._normalize_loaded_effect_state()

        if runtime is not None:
            runtime.ensure_output_mix_sync("Monitor", refresh=False)
            runtime.ensure_output_mix_sync("Stream", refresh=False)

        self.window._pending_clipguard_migration = bool(conf.get("clipguard"))
        if self.window._pending_clipguard_migration and self.window.selected_mic:
            self.window._apply_pending_clipguard_migration()

        startup_mic = self.window._resolve_startup_mic_target()
        if startup_mic:
            self.window._set_selected_mic_target(
                startup_mic,
                record_preference=True,
                persist=False,
                request_refresh=False,
                view=self._attrs().get("_runtime_view_state"),
            )

        mon_hw = self.window._resolve_startup_monitor_target()
        str_hw = conf.get("stream_hw")
        self.window._set_mix_output_target(
            "Monitor",
            mon_hw,
            persist=False,
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        self.window._record_preferred_monitor(mon_hw, view=self._attrs().get("_runtime_view_state"))
        self.window._set_mix_output_target(
            "Stream",
            str_hw,
            persist=False,
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        if remove_missing_virtuals and runtime is not None:
            for name in previous_virtuals:
                if name in self.window.virtual_channels:
                    continue
                _, safe = PipeWireEngine._sanitize_channel_name(name)
                runtime.remove_virtual_channel_sync(f"wavelinux_{safe}", refresh=False)
        if runtime is not None:
            for name in self.window.virtual_channels:
                runtime.ensure_virtual_channel_sync(name, refresh=False)
        self.window._sync_runtime_persistent_state(immediate=True)
        if runtime is not None:
            self.window._request_runtime_refresh("post-config-virtual-sync")
        config = self.window._serialize_config()
        self._publish_config_changed(config)
        self._sync_window_state()

    def load_config(self):
        if not os.path.exists(self.window.config_path):
            self.window._onboarding_completed = False
            self.window._selected_setup_template = ""
            self.window._show_first_run_setup = True
            self.window.runtime.ensure_output_mix_sync("Monitor", refresh=False)
            self.window.runtime.ensure_output_mix_sync("Stream", refresh=False)
            startup_mic = self.window._resolve_startup_mic_target()
            if startup_mic:
                self.window._set_selected_mic_target(
                    startup_mic,
                    record_preference=True,
                    persist=False,
                    request_refresh=False,
                    view=self._attrs().get("_runtime_view_state"),
                )
            def_sink = self.window._resolve_startup_monitor_target()
            if def_sink:
                self.window._set_mix_output_target(
                    "Monitor",
                    def_sink,
                    persist=False,
                    update_combo=True,
                    sync_runtime=True,
                    sync_runtime_refresh=False,
                )
                self.window._record_preferred_monitor(
                    def_sink,
                    view=self._attrs().get("_runtime_view_state"),
                )
            self.window._seed_default_channels()
            self.window._sync_runtime_persistent_state(immediate=True)
            self.window.save_config()
            self._sync_window_state()
            return

        try:
            with open(self.window.config_path, "r") as fh:
                conf = json.load(fh)
            self.window._apply_config_dict(conf, remove_missing_virtuals=True)
            self._sync_window_state()
        except Exception as exc:
            logging.error("Error loading config: %s", exc)

    @staticmethod
    def migrate_submix_state(raw):
        if not isinstance(raw, dict):
            return {}
        clean = {}
        legacy_suffixes = ("_Monitor", "_Stream")
        for key, val in raw.items():
            if not isinstance(key, str) or "_" not in key:
                continue
            if key.endswith(legacy_suffixes):
                prefix = key.rsplit("_", 1)[0]
                try:
                    int(prefix)
                    continue
                except ValueError:
                    pass
            clean[key] = val
        return clean

    @staticmethod
    def migrate_hidden_nodes(raw):
        if not isinstance(raw, (list, set, tuple)):
            return set()
        return {entry for entry in raw if isinstance(entry, str) and entry}

    def save_config(self):
        try:
            self.window._flush_pending_ui_state()
            config = self.window._serialize_config()
            self.window._write_config_file(self.window.config_path, config)
            self._publish_config_changed(config)
            self._sync_window_state()
        except Exception as exc:
            logging.error("Error saving config: %s", exc)

    def backup_current_config(self):
        if not os.path.exists(self.window.config_path):
            return ""
        stamp = time.strftime("%Y%m%d-%H%M%S")
        backup_path = f"{self.window.config_path}.{stamp}.bak"
        shutil.copy2(self.window.config_path, backup_path)
        return backup_path

    def export_full_config(self):
        default_name = os.path.join(
            os.path.dirname(self.window.config_path),
            f"wavelinux-export-{time.strftime('%Y%m%d-%H%M%S')}.json",
        )
        path, _ = QFileDialog.getSaveFileName(
            self._attrs().get("settings_dialog"),
            "Export WaveLinux Config",
            default_name,
            "JSON Files (*.json)",
        )
        if not path:
            return
        if not path.lower().endswith(".json"):
            path += ".json"
        try:
            self.window._write_config_file(path, self.window._serialize_config())
        except Exception as exc:
            QMessageBox.warning(
                self._attrs().get("settings_dialog"),
                "Config export failed",
                str(exc),
            )
            return
        status_lbl = self._attrs().get("status_lbl")
        if status_lbl is not None and hasattr(status_lbl, "setText"):
            status_lbl.setText("Config exported")
        QMessageBox.information(
            self._attrs().get("settings_dialog"),
            "Config exported",
            f"Saved WaveLinux config to:\n{path}",
        )

    def import_full_config(self):
        path, _ = QFileDialog.getOpenFileName(
            self._attrs().get("settings_dialog"),
            "Import WaveLinux Config",
            os.path.dirname(self.window.config_path),
            "JSON Files (*.json)",
        )
        if not path:
            return
        yn = QMessageBox.question(
            self._attrs().get("settings_dialog"),
            "Import WaveLinux Config",
            "Replace the current WaveLinux configuration with the selected file?\n\n"
            "This overwrites scenes, routing, FX, and saved app state.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if yn != QMessageBox.StandardButton.Yes:
            return
        try:
            with open(path, "r") as fh:
                payload = json.load(fh)
            backup_path = self.window._backup_current_config()
            self.window._apply_config_dict(payload, remove_missing_virtuals=True)
            self.window.save_config()
        except Exception as exc:
            QMessageBox.warning(
                self._attrs().get("settings_dialog"),
                "Config import failed",
                str(exc),
            )
            return
        self.window._refresh_scenes_tab()
        self.window._refresh_hidden_list()
        self.window._refresh_advanced_tab()
        self.window._refresh_system_tab()
        self.window._refresh_update_tab()
        self.window._refresh()
        status_lbl = self._attrs().get("status_lbl")
        if status_lbl is not None and hasattr(status_lbl, "setText"):
            status_lbl.setText("Config imported")
        msg = f"Imported WaveLinux config from:\n{path}"
        if backup_path:
            msg += f"\n\nBackup of the previous config:\n{backup_path}"
        QMessageBox.information(
            self._attrs().get("settings_dialog"),
            "Config imported",
            msg,
        )
