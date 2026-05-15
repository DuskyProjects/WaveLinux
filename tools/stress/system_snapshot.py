"""System snapshot helpers for WaveLinux stress runs."""

from __future__ import annotations

import json
import os
import pathlib
import shlex
import subprocess
import time


WAVELINUX_PROCESS_PATTERNS = (
    "WaveLinux",
    "WaveLinux.AppImage",
    "/usr/lib/wavelinux/WaveLinux",
    "python.*main.py",
)


def run_text(cmd, *, timeout=10, check=False, env=None, shell=False):
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
        shell=shell,
    )
    if check and result.returncode != 0:
        raise RuntimeError(
            f"command failed ({result.returncode}): "
            f"{cmd if isinstance(cmd, str) else shlex.join(cmd)}\n"
            f"{result.stderr.strip()}"
        )
    return result


def read_text(path):
    try:
        return pathlib.Path(path).read_text(encoding="utf-8")
    except OSError:
        return ""


def parse_short_table(text, headers):
    rows = []
    for line in (text or "").splitlines():
        parts = line.split("\t")
        if len(parts) < len(headers):
            continue
        row = {}
        for index, header in enumerate(headers):
            row[header] = parts[index]
        row["raw"] = line
        rows.append(row)
    return rows


def active_wavelinux_processes():
    pattern = "|".join(WAVELINUX_PROCESS_PATTERNS)
    result = run_text(["bash", "-lc", f"pgrep -af '{pattern}' || true"], timeout=5)
    lines = []
    for line in result.stdout.splitlines():
        clean = line.strip()
        if not clean:
            continue
        if "pgrep -af" in clean:
            continue
        if "run_stress_suite.py" in clean or "/tools/stress/" in clean:
            continue
        lines.append(clean)
    return lines


def list_wavelinux_named_objects():
    sinks = run_text(["pactl", "list", "short", "sinks"], timeout=5).stdout
    sources = run_text(["pactl", "list", "short", "sources"], timeout=5).stdout
    return {
        "sinks": [line for line in sinks.splitlines() if "wavelinux" in line.lower()],
        "sources": [line for line in sources.splitlines() if "wavelinux" in line.lower()],
    }


def capture_system_snapshot(*, profile=None):
    defaults = {
        "sink": run_text(["pactl", "get-default-sink"], timeout=5).stdout.strip(),
        "source": run_text(["pactl", "get-default-source"], timeout=5).stdout.strip(),
    }
    sinks_text = run_text(["pactl", "list", "short", "sinks"], timeout=5).stdout
    sources_text = run_text(["pactl", "list", "short", "sources"], timeout=5).stdout
    cards_text = run_text(["pactl", "list", "cards"], timeout=8).stdout
    sink_inputs_text = run_text(["pactl", "list", "sink-inputs"], timeout=8).stdout
    source_outputs_text = run_text(["pactl", "list", "source-outputs"], timeout=8).stdout
    wpctl_text = run_text(["wpctl", "status"], timeout=8).stdout
    log_path = os.path.expanduser("~/.config/wavelinux/wavelinux.log")
    log_tail = run_text(
        ["bash", "-lc", f"tail -n 200 {shlex.quote(log_path)} 2>/dev/null || true"],
        timeout=5,
    ).stdout
    process_lines = active_wavelinux_processes()
    modules = list_wavelinux_named_objects()
    bluetooth_profile = ""
    if profile:
        card_name = str((profile.get("bluetooth") or {}).get("card_name") or "").strip()
        if card_name:
            marker = f"Name: {card_name}"
            lines = []
            capture = False
            for line in cards_text.splitlines():
                stripped = line.strip()
                if stripped.startswith("Card #"):
                    if capture:
                        break
                    capture = False
                if stripped == marker:
                    capture = True
                if capture:
                    lines.append(line)
                    if stripped.startswith("Active Profile:"):
                        bluetooth_profile = stripped.split(":", 1)[1].strip()
            card_block = "\n".join(lines)
        else:
            card_block = ""
    else:
        card_block = ""
    bluetooth_autoswitch_enabled = None
    in_autoswitch = False
    settings_text = run_text(["wpctl", "settings"], timeout=8).stdout
    for line in settings_text.splitlines():
        stripped = line.strip()
        if stripped.startswith("- Id: "):
            in_autoswitch = stripped == "- Id: bluetooth.autoswitch-to-headset-profile"
            continue
        if not in_autoswitch:
            continue
        if stripped.startswith("Value:"):
            value = stripped.split(":", 1)[1].strip().lower()
            if value == "true":
                bluetooth_autoswitch_enabled = True
            elif value == "false":
                bluetooth_autoswitch_enabled = False
            break
    return {
        "captured_at": time.time(),
        "defaults": defaults,
        "sinks": parse_short_table(
            sinks_text,
            ("index", "name", "driver", "spec", "state"),
        ),
        "sources": parse_short_table(
            sources_text,
            ("index", "name", "driver", "spec", "state"),
        ),
        "cards_short": parse_short_table(
            run_text(["pactl", "list", "short", "cards"], timeout=5).stdout,
            ("index", "name", "driver"),
        ),
        "bluetooth_active_profile": bluetooth_profile,
        "bluetooth_autoswitch_enabled": bluetooth_autoswitch_enabled,
        "bluetooth_card_block": card_block,
        "wave_named_objects": modules,
        "wave_processes": process_lines,
        "wpctl_status": wpctl_text,
        "wpctl_settings_text": settings_text,
        "pactl_sinks_text": sinks_text,
        "pactl_sources_text": sources_text,
        "pactl_cards_text": cards_text,
        "pactl_sink_inputs_text": sink_inputs_text,
        "pactl_source_outputs_text": source_outputs_text,
        "wavelinux_log_tail": log_tail,
    }


def ensure_dir(path):
    pathlib.Path(path).mkdir(parents=True, exist_ok=True)


def write_json(path, payload):
    ensure_dir(pathlib.Path(path).parent)
    pathlib.Path(path).write_text(
        json.dumps(payload, indent=2, sort_keys=True),
        encoding="utf-8",
    )


def write_text(path, payload):
    ensure_dir(pathlib.Path(path).parent)
    pathlib.Path(path).write_text(str(payload), encoding="utf-8")


def write_snapshot_artifacts(run_dir, stem, snapshot):
    write_json(os.path.join(run_dir, f"{stem}.json"), snapshot)
    write_text(os.path.join(run_dir, f"{stem}.wpctl-status.txt"), snapshot.get("wpctl_status", ""))
    write_text(os.path.join(run_dir, f"{stem}.wpctl-settings.txt"), snapshot.get("wpctl_settings_text", ""))
    write_text(os.path.join(run_dir, f"{stem}.pactl-sinks.txt"), snapshot.get("pactl_sinks_text", ""))
    write_text(os.path.join(run_dir, f"{stem}.pactl-sources.txt"), snapshot.get("pactl_sources_text", ""))
    write_text(os.path.join(run_dir, f"{stem}.pactl-sink-inputs.txt"), snapshot.get("pactl_sink_inputs_text", ""))
    write_text(os.path.join(run_dir, f"{stem}.pactl-source-outputs.txt"), snapshot.get("pactl_source_outputs_text", ""))
    write_text(os.path.join(run_dir, f"{stem}.wavelinux-log-tail.txt"), snapshot.get("wavelinux_log_tail", ""))
