"""Window mixin for UI shell behavior and controller facade methods."""

from __future__ import annotations

import re

from controllers import BluetoothController, ConfigController
from ui.dialogs.card_profile_dialog import CardProfileDialog
from ui.main_window import build_main_window
from ui.mixer import MixerPanelController, MixerStripMetrics
from ui.routing import AppRoutingPanelController


class WindowShellMixin:
    def _any_slider_dragging(self):
        for slider in (
            getattr(self, "mon_master_slider", None),
            getattr(self, "str_master_slider", None),
        ):
            if slider is not None and slider.isSliderDown():
                return True
        for strip in self.channel_widgets.values():
            for slider in (
                getattr(strip, "mon_slider", None),
                getattr(strip, "str_slider", None),
            ):
                if slider is not None and slider.isSliderDown():
                    return True
        for row in self.app_widgets.values():
            slider = getattr(row, "vol_slider", None)
            if slider is not None and slider.isSliderDown():
                return True
        return False

    def recover_channel(self, node_name):
        self._recovery_controller().recover_channel(node_name)

    def _start_event_subscriber(self):
        self._audio_event_controller().start_event_subscriber()

    def _on_pactl_event(self):
        self._audio_event_controller().on_pactl_event()

    def _on_event_proc_error(self, err):
        self._audio_event_controller().on_event_proc_error(err)

    def _on_event_proc_finished(self, exit_code, exit_status):
        self._audio_event_controller().on_event_proc_finished(exit_code, exit_status)

    def _schedule_audio_server_recovery(self):
        self._audio_event_controller().schedule_audio_server_recovery()

    def _restart_event_subscriber_if_needed(self):
        self._audio_event_controller().restart_event_subscriber_if_needed()

    @staticmethod
    def _preferred_bluetooth_playback_profile_name(profiles):
        return BluetoothController.preferred_playback_profile_name(profiles)

    @staticmethod
    def _bluetooth_mac_from_card_name(card_name):
        return BluetoothController.bluetooth_mac_from_card_name(card_name)

    @staticmethod
    def _run_bluetoothctl_commands(*commands, timeout=8):
        return BluetoothController.run_bluetoothctl_commands(*commands, timeout=timeout)

    def _complete_bluetooth_reconnect(self, mac):
        self._bluetooth_controller().complete_bluetooth_reconnect(mac)

    def _schedule_bluetooth_reconnect_mac(
        self,
        mac,
        *,
        disconnect_first=True,
        settle_delay_ms=900,
    ):
        return self._bluetooth_controller().schedule_bluetooth_reconnect_mac(
            mac,
            disconnect_first=disconnect_first,
            settle_delay_ms=settle_delay_ms,
        )

    def _schedule_bluetooth_reconnect(
        self,
        card_name,
        *,
        disconnect_first=True,
        settle_delay_ms=900,
    ):
        return self._bluetooth_controller().schedule_bluetooth_reconnect(
            card_name,
            disconnect_first=disconnect_first,
            settle_delay_ms=settle_delay_ms,
        )

    def _known_bluetooth_target_macs(self):
        return self._bluetooth_controller().known_bluetooth_target_macs()

    def _selected_mic_uses_bluetooth_input(self):
        return self._bluetooth_controller().selected_mic_uses_bluetooth_input()

    def _schedule_known_bluetooth_monitor_reconnect(
        self,
        *,
        disconnect_first,
        settle_delay_ms,
    ):
        return self._bluetooth_controller().schedule_known_bluetooth_monitor_reconnect(
            disconnect_first=disconnect_first,
            settle_delay_ms=settle_delay_ms,
        )

    def _has_bluetooth_playback_cards(self):
        return self._bluetooth_controller().has_bluetooth_playback_cards()

    def _reassert_bluetooth_playback_profile(self):
        return self._bluetooth_controller().reassert_bluetooth_playback_profile()

    def _prime_bluetooth_playback_profile(self):
        return self._bluetooth_controller().prime_bluetooth_playback_profile()

    def _handle_bluetooth_settle_refresh(self):
        self._bluetooth_controller().handle_bluetooth_settle_refresh()

    @staticmethod
    def _should_refresh_for_pactl_event(payload):
        refresh_targets = {"sink", "source", "sink-input", "server", "card", "client"}
        ignored_targets = {"source-output", "module"}
        saw_any = False
        for line in payload.splitlines():
            text = line.strip().lower()
            if not text:
                continue
            match = re.search(r"\bon\s+([a-z-]+)\b", text)
            if not match:
                continue
            saw_any = True
            target = match.group(1)
            if target in ignored_targets:
                continue
            if target in refresh_targets:
                return True
        return not saw_any

    @staticmethod
    def _should_schedule_settle_refresh_for_pactl_event(payload):
        settle_targets = {"sink", "source", "server", "card"}
        for line in payload.splitlines():
            text = line.strip().lower()
            if not text:
                continue
            match = re.search(r"\bon\s+([a-z-]+)\b", text)
            if not match:
                continue
            target = match.group(1)
            if target in settle_targets:
                return True
        return False

    @staticmethod
    def _should_schedule_bluetooth_settle_refresh_for_pactl_event(payload):
        return BluetoothController.should_schedule_bluetooth_settle_refresh_for_pactl_event(payload)

    def _setup_ui(self):
        build_main_window(self)

    def _mixer_panel_controller(self):
        controller = self.__dict__.get("_mixer_panel")
        if controller is None:
            controller = MixerPanelController(self)
            self._mixer_panel = controller
        return controller

    def _app_routing_panel_controller(self):
        controller = self.__dict__.get("_app_routing_panel")
        if controller is None:
            controller = AppRoutingPanelController(self)
            self._app_routing_panel = controller
        return controller

    def _clear_layout(self, layout):
        while layout.count():
            item = layout.takeAt(0)
            widget = item.widget()
            if widget:
                widget.setParent(None)
                widget.deleteLater()
            elif item.layout():
                self._clear_layout(item.layout())

    _MIN_STRIP_W = MixerPanelController._MIN_STRIP_W
    _MAX_STRIP_W = MixerPanelController._MAX_STRIP_W
    _MIN_SLIDER_H = MixerPanelController._MIN_SLIDER_H
    _MAX_SLIDER_H = MixerPanelController._MAX_SLIDER_H
    _SLIDER_WIDTH_SCALE_CAP = MixerPanelController._SLIDER_WIDTH_SCALE_CAP
    _STRIP_CARD_GAP = MixerPanelController._STRIP_CARD_GAP
    _STRIP_ROW_MARGIN = MixerPanelController._STRIP_ROW_MARGIN
    _STRIP_MIN_OUTER_MARGIN = MixerPanelController._STRIP_MIN_OUTER_MARGIN
    _STRIP_MAX_OUTER_MARGIN = MixerPanelController._STRIP_MAX_OUTER_MARGIN
    _STRIP_MIN_INNER_SPACING = MixerPanelController._STRIP_MIN_INNER_SPACING
    _STRIP_MAX_INNER_SPACING = MixerPanelController._STRIP_MAX_INNER_SPACING
    _STRIP_MIN_FADER_SPACING = MixerPanelController._STRIP_MIN_FADER_SPACING
    _STRIP_MAX_FADER_SPACING = MixerPanelController._STRIP_MAX_FADER_SPACING
    _STRIP_MIN_PEAK_HEIGHT = MixerPanelController._STRIP_MIN_PEAK_HEIGHT
    _STRIP_MAX_PEAK_HEIGHT = MixerPanelController._STRIP_MAX_PEAK_HEIGHT
    _STRIP_MIN_LINK_SIZE = MixerPanelController._STRIP_MIN_LINK_SIZE
    _STRIP_MAX_LINK_SIZE = MixerPanelController._STRIP_MAX_LINK_SIZE
    _STRIP_MIN_MUTE_SIZE = MixerPanelController._STRIP_MIN_MUTE_SIZE
    _STRIP_MAX_MUTE_SIZE = MixerPanelController._STRIP_MAX_MUTE_SIZE
    _STRIP_MIN_MIC_GAIN_HEIGHT = MixerPanelController._STRIP_MIN_MIC_GAIN_HEIGHT
    _STRIP_MAX_MIC_GAIN_HEIGHT = MixerPanelController._STRIP_MAX_MIC_GAIN_HEIGHT

    def eventFilter(self, obj, event):
        self._mixer_panel_controller().handle_event_filter(obj, event)
        return super().eventFilter(obj, event)

    _STRIP_NON_SLIDER_HEIGHT_BUDGET = MixerPanelController._STRIP_NON_SLIDER_HEIGHT_BUDGET
    _STRIP_MIN_TOTAL_HEIGHT = MixerPanelController._STRIP_MIN_TOTAL_HEIGHT

    def _compute_mixer_strip_metrics(self, strips=None) -> MixerStripMetrics:
        return self._mixer_panel_controller().compute_strip_metrics(strips)

    def _measure_strip_heights(self, metrics, strips) -> int:
        return self._mixer_panel_controller().measure_strip_heights(metrics, strips)

    def _apply_mixer_strip_metrics(self, metrics, strips) -> None:
        self._mixer_panel_controller().apply_strip_metrics(metrics, strips)

    def resizeEvent(self, event):
        out = super().resizeEvent(event)
        if hasattr(self, "inputs_scroll"):
            self._rescale_strips()
        return out

    def _rescale_strips(self):
        self._mixer_panel_controller().rescale_strips()

    def _refresh(self):
        hidden_to_tray = self.tray is not None and not self.isVisible()
        settings_open = self._settings_dialog_visible()
        if hidden_to_tray and not settings_open:
            return
        if self._any_slider_dragging():
            self._event_refresh_timer.start()
            return
        self.runtime.refresh_now("ui-refresh")
        if self._runtime_view_state is None:
            self.status_lbl.setText("PipeWire syncing...")

    def _on_master_vol_change(self, mix_name, value):
        self._channel_controller().on_master_vol_change(mix_name, value)

    def _commit_master_vols(self):
        self._channel_controller().commit_master_vols()

    def _set_mix_output_target(
        self,
        mix_name,
        hw_sink_name,
        *,
        persist=True,
        update_combo=False,
        sync_runtime=False,
        sync_runtime_refresh=True,
    ):
        self._channel_controller().set_mix_output_target(
            mix_name,
            hw_sink_name,
            persist=persist,
            update_combo=update_combo,
            sync_runtime=sync_runtime,
            sync_runtime_refresh=sync_runtime_refresh,
        )

    def _on_mix_out_change(self, mix_name, hw_sink_name):
        self._channel_controller().on_mix_out_change(mix_name, hw_sink_name)

    def _schedule_monitor_route_followups(self, hw_sink_name):
        self._device_policy_controller().schedule_monitor_route_followups(hw_sink_name)

    def _reassert_persistent_state_after_monitor_switch(self, reason):
        self._device_policy_controller().reassert_persistent_state_after_monitor_switch(reason)

    def _apply_pending_clipguard_migration(self):
        self._channel_controller().apply_pending_clipguard_migration()

    def _sync_mic_picker(self, mics, default_src=None):
        self._mixer_panel_controller().sync_mic_picker(mics, default_src=default_src)

    def _input_display_sort_key(self, node_name, *, is_mic=False):
        return self._mixer_panel_controller().input_display_sort_key(node_name, is_mic=is_mic)

    def _sorted_input_nodes(self, mic_nodes, virtual_nodes):
        return self._mixer_panel_controller().sorted_input_nodes(mic_nodes, virtual_nodes)

    def _refresh_runtime_view(self):
        view = self._runtime_view_state
        if view is None:
            self.status_lbl.setText("PipeWire syncing...")
            return
        self._mixer_panel_controller().refresh_view(view)
        self._app_routing_panel_controller().refresh_view(view)
        if not getattr(view, "health", {}):
            self.status_lbl.setText(
                f"PipeWire connected · {getattr(view, 'node_count', 0)} nodes · "
                f"{getattr(view, 'app_count', 0)} apps"
            )

    def _on_mic_input_change(self, idx):
        self._channel_controller().on_mic_input_change(idx)

    def _on_add_channel(self):
        self._channel_controller().on_add_channel()

    def _remove_sink(self, sink_name):
        self._channel_controller().remove_sink(sink_name)

    _DEFAULT_CHANNELS = ("Music", "Game", "Browser", "Voice Chat", "System")

    def _seed_default_channels(self):
        self._channel_controller().seed_default_channels()

    def _serialize_config(self):
        return self._config_controller().serialize_config()

    @staticmethod
    def _write_config_file(path, payload):
        ConfigController.write_config_file(path, payload)

    def _apply_config_dict(self, conf, *, remove_missing_virtuals=False):
        self._config_controller().apply_config_dict(
            conf,
            remove_missing_virtuals=remove_missing_virtuals,
        )

    def load_config(self):
        self._config_controller().load_config()

    @staticmethod
    def _migrate_submix_state(raw):
        return ConfigController.migrate_submix_state(raw)

    @staticmethod
    def _migrate_hidden_nodes(raw):
        return ConfigController.migrate_hidden_nodes(raw)

    def save_config(self):
        self._config_controller().save_config()

    def _backup_current_config(self):
        return self._config_controller().backup_current_config()

    def _export_full_config(self):
        self._config_controller().export_full_config()

    def _import_full_config(self):
        self._config_controller().import_full_config()

    def forget_app(self, app_id):
        self._app_identity_controller().forget_app(app_id)

    def _prune_stale_apps(self):
        self._app_identity_controller().prune_stale_apps()

    def move_channel(self, node_name, delta):
        self._channel_controller().move_channel(node_name, delta)

    def _relayout_channel_strips(self):
        self._mixer_panel_controller().relayout_channel_strips()

    def rename_channel(self, old_node_name):
        self._channel_controller().rename_channel(old_node_name)

    def _rekey_state(self, old_name, new_name):
        self._channel_controller().rekey_state(old_name, new_name)

    @property
    def autostart_path(self):
        return self._lifecycle_controller().autostart_path

    def is_autostart_enabled(self):
        return self._lifecycle_controller().is_autostart_enabled()

    def set_autostart(self, enable):
        self._lifecycle_controller().set_autostart(enable)

    def _open_card_profiles(self):
        dialog = CardProfileDialog(self.engine, self.runtime, self)
        dialog.exec()

    def _show_notification(self, title, body):
        self._lifecycle_controller().show_notification(title, body)

    def _notify_hotplug(self, node_names, *, added):
        self._lifecycle_controller().notify_hotplug(node_names, added=added)

    def hide_node(self, node_name):
        self._channel_controller().hide_node(node_name)

    def unhide_node(self, node_name):
        self._channel_controller().unhide_node(node_name)

    def _setup_tray(self):
        self._lifecycle_controller().setup_tray()

    def _on_tray_activated(self, reason):
        self._lifecycle_controller().on_tray_activated(reason)

    def hideEvent(self, event):
        super().hideEvent(event)
        self._lifecycle_controller().on_hide_event()

    def showEvent(self, event):
        super().showEvent(event)
        self._lifecycle_controller().on_show_event()

    def _request_quit_app(self):
        self._lifecycle_controller().request_quit_app()

    def closeEvent(self, event):
        self._lifecycle_controller().close_event(event)

    def _close_open_dialogs_for_quit(self):
        self._lifecycle_controller().close_open_dialogs_for_quit()

    def _stop_all_meters(self):
        self._mixer_panel_controller().stop_all_meters()

    def _quit_app(self):
        self._lifecycle_controller().quit_app()

    def _cleanup_before_exit(self):
        self._lifecycle_controller().cleanup_before_exit()
