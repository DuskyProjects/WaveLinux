"""Card/profile and Bluetooth autoswitch helpers for the PipeWire engine."""

from __future__ import annotations

import logging
import subprocess


def lock_bluetooth_to_a2dp(engine):
    """Disable WirePlumber's A2DP<->HSP autoswitch for this session."""
    try:
        result = subprocess.run(
            ["wpctl", "settings", "bluetooth.autoswitch-to-headset-profile", "false"],
            capture_output=True,
            text=True,
            timeout=3,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
        logging.warning(f"Could not lock bluetooth profile (wpctl unavailable): {exc}")
        return False
    if result.returncode != 0:
        logging.warning(
            f"wpctl rejected bluetooth.autoswitch override "
            f"(rc={result.returncode}): {result.stderr.strip()}"
        )
        return False
    engine._bt_autoswitch_overridden = True
    logging.info("Locked BT profile to A2DP for this session")
    return True


def unlock_bluetooth_autoswitch(engine):
    """Restore the BT autoswitch default."""
    if not engine._bt_autoswitch_overridden:
        return False
    try:
        result = subprocess.run(
            ["wpctl", "settings", "bluetooth.autoswitch-to-headset-profile", "true"],
            capture_output=True,
            text=True,
            timeout=3,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False
    if result.returncode == 0:
        engine._bt_autoswitch_overridden = False
        return True
    return False


def list_cards(engine):
    """Return card and profile information from `pactl list cards`."""
    out = engine._run(["pactl", "list", "cards"])
    if not out:
        return []
    cards = []
    current = None
    section = None
    for raw in out.splitlines():
        line = raw.rstrip()
        stripped = line.strip()
        if stripped.startswith("Card #"):
            if current is not None:
                cards.append(current)
            current = {
                "name": "",
                "description": "",
                "active_profile": "",
                "profiles": [],
            }
            section = None
            continue
        if current is None:
            continue
        if stripped.startswith("Name:"):
            current["name"] = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("Active Profile:"):
            current["active_profile"] = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("device.description ="):
            current["description"] = stripped.split("=", 1)[1].strip().strip('"')
        elif stripped.startswith("Profiles:"):
            section = "profiles"
        elif section == "profiles" and line.startswith("\t\t"):
            entry = stripped
            if ":" in entry:
                profile_name, rest = entry.split(":", 1)
                available = "available: yes" in rest or "available: unknown" in rest
                description = rest.strip()
                left_paren = description.rfind("(")
                if left_paren >= 0:
                    description = description[:left_paren].strip()
                current["profiles"].append(
                    {
                        "name": profile_name.strip(),
                        "description": description or profile_name.strip(),
                        "available": available,
                    }
                )
        elif stripped.startswith(("Ports:", "Sinks:", "Sources:", "Properties:")):
            section = None
    if current is not None:
        cards.append(current)
    return cards


def set_card_profile(engine, card_name, profile_name):
    return engine._run(["pactl", "set-card-profile", card_name, profile_name]) is not None
