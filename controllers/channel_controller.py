"""Channel, mix-output, and hidden-node mutation helpers."""

from __future__ import annotations

import logging
import re

from PyQt6.QtCore import QTimer
from PyQt6.QtWidgets import QHBoxLayout, QLabel, QMessageBox, QPushButton, QInputDialog, QSizePolicy, QWidget

from pipewire_engine import PipeWireEngine


class ChannelController:
    def __init__(self, window):
        self.window = window

    def _attrs(self):
        return self.window.__dict__

    def _sync_window_state(self):
        sync = getattr(type(self.window), "_sync_window_state", None)
        if sync is not None:
            sync(self.window)

    def refresh_hidden_list(self):
        self.window._clear_layout(self.window.hidden_list_layout)
        if not self.window.hidden_nodes:
            empty = QLabel("No hidden channels")
            empty.setStyleSheet("color: #5a5a72; font-size: 11px; font-style: italic;")
            self.window.hidden_list_layout.addWidget(empty)
            self.window._mark_settings_tab_refreshed("Hidden")
            return
        for node_name in sorted(self.window.hidden_nodes):
            row = QHBoxLayout()
            friendly = str(node_name).replace("wavelinux_", "").replace("_", " ").title()
            lbl = QLabel(friendly)
            lbl.setStyleSheet("color: #e0e0ee; font-size: 12px;")
            row.addWidget(lbl, 1)

            unhide_btn = QPushButton("👁  Show")
            unhide_btn.setObjectName("addBtn")
            unhide_btn.setMinimumWidth(96)
            unhide_btn.setMinimumHeight(30)
            unhide_btn.setSizePolicy(QSizePolicy.Policy.Fixed, QSizePolicy.Policy.Fixed)
            unhide_btn.clicked.connect(
                lambda checked=False, nn=node_name: self.window._unhide_from_settings(nn)
            )
            row.addWidget(unhide_btn)

            row_widget = QWidget()
            row_widget.setLayout(row)
            self.window.hidden_list_layout.addWidget(row_widget)
        self.window._mark_settings_tab_refreshed("Hidden")

    def unhide_from_settings(self, node_name):
        self.window.unhide_node(node_name)
        self.window._refresh_hidden_list()

    def on_master_vol_change(self, mix_name, value):
        if "_pending_master_vol" not in self._attrs():
            self.window._pending_master_vol = {}
            timer = QTimer(self.window)
            timer.setSingleShot(True)
            timer.setInterval(40)
            timer.timeout.connect(self.window._commit_master_vols)
            self.window._master_commit_timer = timer
        normalized = self.window._normalize_mix_volume(value / 100.0)
        self.window._pending_master_vol[mix_name] = normalized
        self.window._set_mix_master_volume(
            mix_name,
            normalized,
            persist=True,
            update_slider=False,
        )
        self.window._master_commit_timer.start()
        self._sync_window_state()

    def commit_master_vols(self):
        if "_pending_master_vol" not in self._attrs():
            return
        pending = self.window._pending_master_vol
        self.window._pending_master_vol = {}
        runtime = self._attrs().get("runtime")
        for mix_name, vol in pending.items():
            if runtime is not None:
                runtime.set_mix_volume(mix_name, vol)
        self._sync_window_state()

    def set_mix_output_target(
        self,
        mix_name,
        hw_sink_name,
        *,
        persist=True,
        update_combo=False,
        sync_runtime=False,
        sync_runtime_refresh=True,
    ):
        self.window._desired_mix_hw[mix_name] = hw_sink_name
        runtime = self._attrs().get("runtime")
        if runtime is not None:
            self.window._suppress_pactl_events_for(1.0)
            if sync_runtime and hasattr(runtime, "set_mix_hardware_route_sync"):
                runtime.set_mix_hardware_route_sync(
                    mix_name,
                    hw_sink_name,
                    refresh=bool(sync_runtime_refresh),
                )
            else:
                runtime.set_mix_hardware_route(mix_name, hw_sink_name)
        if update_combo:
            combo_name = "mon_out_combo" if mix_name == "Monitor" else "str_out_combo"
            combo = getattr(self.window, combo_name, None)
            if combo is not None and hasattr(combo, "blockSignals"):
                combo.blockSignals(True)
                try:
                    idx = combo.findData(hw_sink_name) if hasattr(combo, "findData") else -1
                    if idx >= 0 and hasattr(combo, "setCurrentIndex"):
                        combo.setCurrentIndex(idx)
                finally:
                    combo.blockSignals(False)
        if persist:
            self.window.schedule_save()
        self._sync_window_state()

    def on_mix_out_change(self, mix_name, hw_sink_name):
        self.window._set_mix_output_target(
            mix_name,
            hw_sink_name,
            persist=True,
            update_combo=False,
            sync_runtime=(mix_name == "Monitor"),
            sync_runtime_refresh=(mix_name != "Monitor"),
        )
        if mix_name == "Monitor" and hw_sink_name:
            self.window._record_preferred_monitor(
                hw_sink_name,
                view=self._attrs().get("_runtime_view_state"),
            )
            self.window._schedule_monitor_route_followups(hw_sink_name)
        self._sync_window_state()

    def apply_pending_clipguard_migration(self):
        mic = self._attrs().get("selected_mic")
        if not mic:
            return
        chain = list(self.window.active_effects.get(mic, []))
        if "limiter" not in chain:
            chain.append("limiter")
            self.window.active_effects[mic] = chain
            logging.info(
                "Migrated master-bus clipguard=true -> per-mic limiter on %s",
                mic,
            )
            self.window._sync_runtime_persistent_state(immediate=True)
            self.window.schedule_save()
        self.window._pending_clipguard_migration = False
        self._sync_window_state()

    def on_mic_input_change(self, idx):
        new_mic = self.window.mic_in_combo.itemData(idx)
        if new_mic == self._attrs().get("selected_mic"):
            return
        self.window._set_selected_mic_target(
            new_mic,
            record_preference=True,
            persist=True,
            request_refresh=True,
            view=self._attrs().get("_runtime_view_state"),
        )

    def on_add_channel(self):
        text, ok = QInputDialog.getText(self.window, "Add Virtual Channel", "Channel Name:")
        if not (ok and text):
            return
        clean = re.sub(r"\s+", " ", text).strip()
        if not clean:
            return
        runtime = self._attrs().get("runtime")
        if runtime is not None:
            runtime.ensure_virtual_channel_sync(clean)
        if clean not in self.window.virtual_channels:
            self.window.virtual_channels.append(clean)
            self.window.save_config()
        self.window._refresh()
        self._sync_window_state()

    def remove_sink(self, sink_name):
        runtime = self._attrs().get("runtime")
        if runtime is not None:
            runtime.remove_virtual_channel_sync(sink_name)
        for display in list(self.window.virtual_channels):
            _, safe = PipeWireEngine._sanitize_channel_name(display)
            if f"wavelinux_{safe}" == sink_name or display == sink_name:
                self.window.virtual_channels.remove(display)
                break
        self.window.save_config()
        self.window._refresh()
        self._sync_window_state()

    def seed_default_channels(self):
        for name in self.window._DEFAULT_CHANNELS:
            created = self.window.runtime.ensure_virtual_channel_sync(name)
            if created is not None and name not in self.window.virtual_channels:
                self.window.virtual_channels.append(name)
        self._sync_window_state()

    def move_channel(self, node_name, delta):
        order = list(self.window.channel_order)
        if node_name not in order:
            order.append(node_name)
        visible_names = [s.node_name for s in self._attrs().get("channel_widgets", {}).values() if s.node_name]
        for nm in visible_names:
            if nm not in order:
                order.append(nm)

        idx = order.index(node_name)
        new_idx = max(0, min(len(order) - 1, idx + delta))
        if new_idx == idx:
            return
        order.pop(idx)
        order.insert(new_idx, node_name)
        self.window.channel_order = order
        self.window.save_config()
        self.window._relayout_channel_strips()
        self._sync_window_state()

    def rename_channel(self, old_node_name):
        if not str(old_node_name).startswith("wavelinux_"):
            QMessageBox.information(self.window, "Rename", "Only virtual channels can be renamed.")
            return
        old_display = str(old_node_name).replace("wavelinux_", "").replace("_", " ").title()
        new_name, ok = QInputDialog.getText(
            self.window,
            "Rename Channel",
            "New name:",
            text=old_display,
        )
        if not (ok and new_name):
            return
        cleaned = re.sub(r"\s+", " ", new_name).strip()
        if not cleaned:
            return
        runtime = self._attrs().get("runtime")
        new_sink = runtime.rename_virtual_channel_sync(old_node_name, cleaned) if runtime is not None else None
        if not new_sink:
            QMessageBox.warning(self.window, "Rename", "Could not rename the channel.")
            return

        self.window._rekey_state(old_node_name, new_sink)
        for i, name in enumerate(self.window.virtual_channels):
            _, safe = PipeWireEngine._sanitize_channel_name(name)
            if f"wavelinux_{safe}" == old_node_name:
                self.window.virtual_channels[i] = cleaned
                break
        else:
            self.window.virtual_channels.append(cleaned)

        for pw_id, widget in list(self._attrs().get("channel_widgets", {}).items()):
            if widget.node_name == old_node_name:
                self.window.input_layout.removeWidget(widget)
                widget.setParent(None)
                widget.deleteLater()
                del self.window.channel_widgets[pw_id]
                meter = self.window.meters.pop(pw_id, None)
                if meter is not None:
                    meter.stop()
        self.window.save_config()
        self.window._refresh()
        self._sync_window_state()

    def rekey_state(self, old_name, new_name):
        for suffix in ("Monitor", "Stream", "gain", "linked"):
            old_key = f"{old_name}_{suffix}"
            if old_key in self.window.submix_state:
                self.window.submix_state[f"{new_name}_{suffix}"] = self.window.submix_state.pop(old_key)
        if old_name in self.window.hidden_nodes:
            self.window.hidden_nodes.discard(old_name)
            self.window.hidden_nodes.add(new_name)
        if old_name in self.window.effect_params:
            self.window.effect_params[new_name] = self.window.effect_params.pop(old_name)
        if old_name in self.window.active_effects:
            self.window.active_effects[new_name] = self.window.active_effects.pop(old_name)
        if old_name in self.window.channel_order:
            i = self.window.channel_order.index(old_name)
            self.window.channel_order[i] = new_name
        self._sync_window_state()

    def hide_node(self, node_name):
        self.window.hidden_nodes.add(node_name)
        self.window.schedule_save()
        self.window._refresh()
        self._sync_window_state()

    def unhide_node(self, node_name):
        self.window.hidden_nodes.discard(node_name)
        self.window.schedule_save()
        self.window._refresh()
        self._sync_window_state()
