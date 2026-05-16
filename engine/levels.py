"""Volume, mute, and level helpers for the PipeWire engine."""

from __future__ import annotations

import re

MAX_VOLUME = 1.0
_PERCENT_RE = re.compile(r"/\s*(\d+)%")


def clamp(engine, volume):
    try:
        return max(0.0, min(float(volume), getattr(engine, "MAX_VOLUME", MAX_VOLUME)))
    except (TypeError, ValueError):
        return 1.0


def get_volume(engine, node_id):
    out = engine._run(["wpctl", "get-volume", str(node_id)])
    if out:
        muted = "[MUTED]" in out
        try:
            vol = float(out.split(":")[1].strip().split()[0])
            return vol, muted
        except (IndexError, ValueError):
            pass
    return 1.0, False


def set_volume(engine, node_id, volume):
    engine._run(["wpctl", "set-volume", str(node_id), f"{volume:.2f}"])


def set_mute(engine, node_id, mute):
    engine._run(["wpctl", "set-mute", str(node_id), "1" if mute else "0"])


def toggle_mute(engine, node_id):
    engine._run(["wpctl", "set-mute", str(node_id), "toggle"])


def set_sink_volume_by_name(engine, sink_name, volume):
    pct = max(0, min(int(round(clamp(engine, volume) * 100)), 100))
    engine._run(["pactl", "set-sink-volume", sink_name, f"{pct}%"])


def set_source_volume_by_name(engine, source_name, volume):
    resolved = engine.resolve_source_name(source_name) or source_name
    pct = max(0, min(int(round(clamp(engine, volume) * 100)), 100))
    engine._run(["pactl", "set-source-volume", resolved, f"{pct}%"])


def get_source_volume_by_name(engine, source_name, snap=None):
    wanted = engine._source_name_aliases(source_name)
    if not wanted:
        return 1.0, False
    if snap is not None:
        if snap._source_state_by_name is None:
            snap._source_state_by_name = engine._parse_sources_state(snap.sources_text)
        for candidate in wanted:
            hit = snap._source_state_by_name.get(candidate)
            if hit is not None:
                return clamp(engine, hit[0]), bool(hit[1])
        return 1.0, False

    out = engine._run(["pactl", "list", "sources"])
    if not out:
        return 1.0, False
    state = engine._parse_sources_state(out)
    for candidate in wanted:
        hit = state.get(candidate)
        if hit is not None:
            return clamp(engine, hit[0]), bool(hit[1])
    return 1.0, False


def get_sink_volume_by_name(engine, sink_name, snap=None):
    if snap is not None:
        if snap._sink_state_by_name is None:
            snap._sink_state_by_name = engine._parse_sinks_state(snap.sinks_text)
        hit = snap._sink_state_by_name.get(sink_name)
        if hit is not None:
            return hit
        return 1.0, False

    out = engine._run(["pactl", "get-sink-volume", sink_name])
    if not out:
        return 1.0, False
    muted = False
    mute_out = engine._run(["pactl", "get-sink-mute", sink_name])
    if mute_out and "yes" in mute_out.lower():
        muted = True
    match = _PERCENT_RE.search(out)
    if match:
        try:
            return int(match.group(1)) / 100.0, muted
        except ValueError:
            pass
    return 1.0, muted


def set_sink_mute_by_name(engine, sink_name, mute):
    engine._run(["pactl", "set-sink-mute", sink_name, "1" if mute else "0"])


def set_input_gain(engine, node_id, volume):
    engine._run(["wpctl", "set-volume", str(node_id), f"{clamp(engine, volume):.2f}"])


def set_sink_input_volume(engine, sink_input_index, volume):
    pct = max(0, min(int(round(clamp(engine, volume) * 100)), 100))
    engine._run(["pactl", "set-sink-input-volume", str(sink_input_index), f"{pct}%"])


def get_sink_input_volume(engine, sink_input_index):
    out = engine._run(["pactl", "list", "sink-inputs"])
    if not out:
        return 1.0
    target = f"Sink Input #{sink_input_index}"
    seen = False
    for line in out.splitlines():
        stripped = line.strip()
        if stripped.startswith("Sink Input #"):
            seen = stripped == target
        elif seen and stripped.startswith("Volume:"):
            match = _PERCENT_RE.search(stripped)
            if match:
                try:
                    return int(match.group(1)) / 100.0
                except ValueError:
                    pass
            return 1.0
    return 1.0


def snapshot_sink_inputs_by_owner(engine, snap=None):
    text = snap.sink_inputs_text if snap else engine._run(["pactl", "list", "sink-inputs"])
    if not text:
        return {}
    by_owner = {}
    curr_owner = None
    curr_vol = None
    curr_mute = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Sink Input #"):
            if curr_owner is not None and curr_vol is not None:
                by_owner[curr_owner] = (curr_vol, curr_mute)
            curr_owner = None
            curr_vol = None
            curr_mute = False
        elif stripped.startswith("Owner Module:"):
            curr_owner = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("Mute:"):
            curr_mute = stripped.split(":", 1)[1].strip().lower() == "yes"
        elif stripped.startswith("Volume:") and curr_vol is None:
            match = _PERCENT_RE.search(stripped)
            if match:
                try:
                    curr_vol = int(match.group(1)) / 100.0
                except ValueError:
                    pass
    if curr_owner is not None and curr_vol is not None:
        by_owner[curr_owner] = (curr_vol, curr_mute)
    return by_owner
