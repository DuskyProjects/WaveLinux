"""Bluetooth control helpers for WaveLinux stress runs."""

from __future__ import annotations

import subprocess
import time

from tools.stress.system_snapshot import run_text


def bluetoothctl_script(*commands, timeout=20):
    script = "\n".join(str(cmd) for cmd in commands) + "\nquit\n"
    return subprocess.run(
        ["bluetoothctl"],
        input=script,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def disconnect_device(mac):
    return bluetoothctl_script(f"disconnect {mac}")


def connect_device(mac):
    return bluetoothctl_script(f"connect {mac}")


def reconnect_device(mac, *, settle_s=3.0):
    disconnect_device(mac)
    time.sleep(1.0)
    connect_device(mac)
    time.sleep(settle_s)


def card_block(card_name):
    text = run_text(["pactl", "list", "cards"], timeout=8).stdout
    marker = f"Name: {card_name}"
    lines = []
    capture = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Card #"):
            if capture:
                break
            capture = False
        if stripped == marker:
            capture = True
        if capture:
            lines.append(line)
    return "\n".join(lines)


def available_profiles(card_name):
    block = card_block(card_name)
    profiles = []
    in_profiles = False
    for line in block.splitlines():
        stripped = line.strip()
        if stripped == "Profiles:":
            in_profiles = True
            continue
        if in_profiles and stripped.startswith("Active Profile:"):
            break
        if not in_profiles or not stripped or stripped.startswith("Ports:"):
            continue
        if ":" in stripped:
            profiles.append(stripped.split(":", 1)[0].strip())
    return profiles


def active_profile(card_name):
    block = card_block(card_name)
    for line in block.splitlines():
        stripped = line.strip()
        if stripped.startswith("Active Profile:"):
            return stripped.split(":", 1)[1].strip()
    return ""


def autoswitch_to_headset_enabled():
    try:
        text = run_text(["wpctl", "settings"], timeout=8).stdout
    except Exception:
        return None
    in_target = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("- Id: "):
            in_target = stripped == "- Id: bluetooth.autoswitch-to-headset-profile"
            continue
        if not in_target:
            continue
        if stripped.startswith("Value:"):
            value = stripped.split(":", 1)[1].strip().lower()
            if value == "true":
                return True
            if value == "false":
                return False
            break
    return None


def set_autoswitch_to_headset(enabled):
    return run_text(
        [
            "wpctl",
            "settings",
            "bluetooth.autoswitch-to-headset-profile",
            "true" if enabled else "false",
        ],
        timeout=8,
    )


def set_profile(card_name, profile_name):
    return run_text(
        ["pactl", "set-card-profile", card_name, profile_name],
        timeout=10,
    )


def ensure_preferred_profile(card_name, preferred_profile, *, settle_s=2.0):
    profiles = available_profiles(card_name)
    if preferred_profile not in profiles:
        return {
            "ok": False,
            "available_profiles": profiles,
            "active_profile": active_profile(card_name),
        }
    result = set_profile(card_name, preferred_profile)
    time.sleep(settle_s)
    return {
        "ok": result.returncode == 0,
        "available_profiles": profiles,
        "active_profile": active_profile(card_name),
        "stderr": result.stderr.strip(),
    }
