"""Bluetooth reconnect and playback-profile recovery helpers."""

from __future__ import annotations

import logging
import re
import subprocess

from PyQt6.QtCore import QTimer


class BluetoothController:
    def __init__(self, window):
        self.window = window

    def _attrs(self):
        return self.window.__dict__

    @staticmethod
    def preferred_playback_profile_name(profiles):
        available = [
            str(profile_name or "").strip()
            for profile_name in (profiles or [])
            if str(profile_name or "").strip()
        ]
        if "a2dp-sink" in available:
            return "a2dp-sink"
        for profile_name in available:
            if profile_name.startswith("a2dp-sink"):
                return profile_name
        return ""

    @staticmethod
    def bluetooth_mac_from_card_name(card_name):
        raw = str(card_name or "").strip()
        if raw.startswith("bluez_card."):
            raw = raw.split(".", 1)[1]
        match = re.search(r"([0-9A-Fa-f]{2}(?:[_:-][0-9A-Fa-f]{2}){5})", raw)
        if not match:
            return ""
        return match.group(1).replace("_", ":").replace("-", ":").upper()

    @staticmethod
    def run_bluetoothctl_commands(*commands, timeout=8):
        script = "\n".join(str(cmd) for cmd in commands if str(cmd or "").strip()) + "\nquit\n"
        if not script.strip():
            return None
        return subprocess.run(
            ["bluetoothctl"],
            input=script,
            capture_output=True,
            text=True,
            timeout=timeout,
        )

    def complete_bluetooth_reconnect(self, mac):
        mac = str(mac or "").strip().upper()
        pending = self._attrs().setdefault("_pending_bluetooth_reconnect_macs", set())
        try:
            if self._attrs().get("_shutting_down", False) or not mac:
                return
            self.window._run_bluetoothctl_commands(f"connect {mac}", timeout=10)
        except Exception as exc:
            logging.warning("Bluetooth reconnect failed for %s: %s", mac, exc)
        finally:
            pending.discard(mac)
            timer = self._attrs().get("_bluetooth_refresh_timer")
            if timer is not None and not self._attrs().get("_shutting_down", False):
                timer.start()

    def schedule_bluetooth_reconnect_mac(
        self,
        mac,
        *,
        disconnect_first=True,
        settle_delay_ms=900,
    ):
        mac = str(mac or "").strip().upper()
        if not mac or self._attrs().get("_shutting_down", False):
            return False
        pending = self._attrs().setdefault("_pending_bluetooth_reconnect_macs", set())
        if mac in pending:
            return False
        pending.add(mac)
        if disconnect_first:
            try:
                self.window._run_bluetoothctl_commands(f"disconnect {mac}", timeout=8)
            except Exception as exc:
                logging.warning("Bluetooth disconnect failed for %s: %s", mac, exc)
        delay_ms = max(0, int(settle_delay_ms or 0))
        QTimer.singleShot(delay_ms, lambda mac=mac: self.window._complete_bluetooth_reconnect(mac))
        logging.info(
            "Scheduled Bluetooth reconnect for %s (disconnect_first=%s, delay_ms=%s)",
            mac,
            disconnect_first,
            delay_ms,
        )
        return True

    def schedule_bluetooth_reconnect(
        self,
        card_name,
        *,
        disconnect_first=True,
        settle_delay_ms=900,
    ):
        return self.window._schedule_bluetooth_reconnect_mac(
            self.window._bluetooth_mac_from_card_name(card_name),
            disconnect_first=disconnect_first,
            settle_delay_ms=settle_delay_ms,
        )

    def known_bluetooth_target_macs(self):
        candidates = [
            self._attrs().get("_desired_mix_hw", {}).get("Monitor", ""),
            self._attrs().get("_preferred_monitor_hw_name", ""),
            self._attrs().get("_preferred_monitor_hw_id", ""),
            self._attrs().get("_restorable_monitor_hw_name", ""),
            self._attrs().get("_restorable_monitor_hw_id", ""),
        ]
        macs = []
        seen = set()
        for candidate in candidates:
            mac = self.window._bluetooth_mac_from_card_name(candidate)
            if not mac or mac in seen:
                continue
            seen.add(mac)
            macs.append(mac)
        return macs

    def selected_mic_uses_bluetooth_input(self):
        selected_mic = str(self._attrs().get("selected_mic", "") or "").strip().lower()
        if selected_mic.startswith("bluez_input."):
            return True
        view = self._attrs().get("_runtime_view_state")
        runtime_selected = str(getattr(view, "selected_mic", "") or "").strip().lower()
        return runtime_selected.startswith("bluez_input.")

    def schedule_known_bluetooth_monitor_reconnect(
        self,
        *,
        disconnect_first,
        settle_delay_ms,
    ):
        reconnect_scheduled = False
        for mac in self.window._known_bluetooth_target_macs():
            reconnect_scheduled = (
                self.window._schedule_bluetooth_reconnect_mac(
                    mac,
                    disconnect_first=disconnect_first,
                    settle_delay_ms=settle_delay_ms,
                )
                or reconnect_scheduled
            )
        return reconnect_scheduled

    def has_bluetooth_playback_cards(self):
        engine = self._attrs().get("engine")
        if engine is None or not hasattr(engine, "list_cards"):
            return False
        try:
            cards = list(engine.list_cards() or [])
        except Exception:
            return False
        return any(
            str((card or {}).get("name") or "").strip().startswith("bluez_card.")
            for card in cards
        )

    def reassert_bluetooth_playback_profile(self):
        if self._attrs().get("_shutting_down", False):
            return False, False
        engine = self._attrs().get("engine")
        if engine is None:
            return False, False
        try:
            if hasattr(engine, "lock_bluetooth_to_a2dp"):
                engine.lock_bluetooth_to_a2dp()
        except Exception as exc:
            logging.warning("Failed to re-lock Bluetooth autoswitch: %s", exc)
        if self.window._selected_mic_uses_bluetooth_input():
            return False, False
        if not hasattr(engine, "list_cards") or not hasattr(engine, "set_card_profile"):
            return False, False
        changed = False
        retry_needed = False
        try:
            cards = list(engine.list_cards() or [])
        except Exception as exc:
            logging.warning("Failed to inspect Bluetooth cards after server churn: %s", exc)
            return False, False
        bluetooth_cards = [
            card
            for card in cards
            if str((card or {}).get("name") or "").strip().startswith("bluez_card.")
        ]
        retries_left = int(self._attrs().get("_bluetooth_profile_reassert_retries", 0) or 0)
        if not bluetooth_cards and retries_left > 0:
            reconnect_scheduled = self.window._schedule_known_bluetooth_monitor_reconnect(
                disconnect_first=False,
                settle_delay_ms=250,
            )
            if reconnect_scheduled:
                return False, True
        for card in cards:
            card_name = str((card or {}).get("name") or "").strip()
            if not card_name.startswith("bluez_card."):
                continue
            active_profile = str((card or {}).get("active_profile") or "").strip()
            if (
                active_profile in {"", "off", "headset-head-unit", "headset-head-unit-cvsd"}
                and retries_left > 0
            ):
                self.window._schedule_bluetooth_reconnect(card_name)
                retry_needed = True
            target_profile = self.window._preferred_bluetooth_playback_profile_name(
                [
                    profile.get("name")
                    for profile in ((card or {}).get("profiles") or [])
                    if bool(profile.get("available"))
                ]
            )
            if active_profile.startswith("a2dp-sink"):
                continue
            if not target_profile:
                retry_needed = retry_needed or active_profile in {
                    "",
                    "off",
                    "headset-head-unit",
                    "headset-head-unit-cvsd",
                }
                if retry_needed:
                    self.window._schedule_bluetooth_reconnect(card_name)
                continue
            try:
                changed = bool(engine.set_card_profile(card_name, target_profile)) or changed
                retry_needed = True
            except Exception as exc:
                logging.warning(
                    "Failed to restore Bluetooth playback profile on %s: %s",
                    card_name,
                    exc,
                )
                retry_needed = True
        return changed, retry_needed

    def prime_bluetooth_playback_profile(self):
        if self._attrs().get("_shutting_down", False) or self.window._selected_mic_uses_bluetooth_input():
            return False
        if not self.window._has_bluetooth_playback_cards():
            return False
        self.window._bluetooth_profile_reassert_retries = max(
            int(self._attrs().get("_bluetooth_profile_reassert_retries", 0) or 0),
            4,
        )
        changed, retry_needed = self.window._reassert_bluetooth_playback_profile()
        if changed:
            self.window._request_runtime_refresh("startup-bt-profile")
        timer = self._attrs().get("_bluetooth_refresh_timer")
        if timer is not None and hasattr(timer, "start"):
            timer.start()
        return changed or retry_needed

    def handle_bluetooth_settle_refresh(self):
        if self._attrs().get("_shutting_down", False):
            return
        self.window._restart_event_subscriber_if_needed()
        _, retry_needed = self.window._reassert_bluetooth_playback_profile()
        self.window._request_runtime_refresh("bluetooth-settle")
        retries_left = int(self._attrs().get("_bluetooth_profile_reassert_retries", 0) or 0)
        if retry_needed and retries_left > 0:
            self.window._bluetooth_profile_reassert_retries = retries_left - 1
            timer = self._attrs().get("_bluetooth_refresh_timer")
            if timer is not None:
                timer.start(600)
        else:
            self.window._bluetooth_profile_reassert_retries = 0

    @staticmethod
    def should_schedule_bluetooth_settle_refresh_for_pactl_event(payload):
        structural_targets = {"sink", "source", "server", "card"}
        for line in payload.splitlines():
            text = line.strip().lower()
            if not text or "bluez" not in text:
                continue
            match = re.search(r"\bon\s+([a-z-]+)\b", text)
            if not match:
                continue
            if match.group(1) in structural_targets:
                return True
        return False
