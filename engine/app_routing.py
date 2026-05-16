"""Sink-input discovery helpers for the PipeWire engine."""

from __future__ import annotations

import re


def parse_pactl_si_map(text):
    """Parse `pactl list sink-inputs` text into lookup dicts."""
    by_node_id = {}
    by_index = {}
    current = {}
    current_index = None

    def flush():
        if current_index is None:
            return
        entry = dict(current)
        entry["_index"] = current_index
        by_index[current_index] = entry
        for ref in (
            current.get("node.id"),
            current.get("object.serial"),
            current_index,
        ):
            if ref is not None and str(ref).strip():
                by_node_id[str(ref).strip()] = entry

    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Sink Input #"):
            flush()
            current_index = stripped.split("#", 1)[1].strip()
            current = {}
        elif stripped.startswith("Sink:") and current_index is not None:
            current["_sink_id"] = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("Volume:") and current_index is not None:
            match = re.search(r"/\s*(\d+)%", stripped)
            if match:
                try:
                    current["volume"] = int(match.group(1)) / 100.0
                except ValueError:
                    pass
        elif "=" in stripped and current_index is not None:
            parts = stripped.split("=", 1)
            if len(parts) == 2:
                key = parts[0].strip()
                val = parts[1].strip().strip('"')
                current[key] = val
                if key in ("pipewire.sec.pid", "application.process.id"):
                    current["pid"] = val
                elif key == "application.process.binary":
                    current["binary"] = val
    flush()
    return by_node_id, by_index


def get_sink_inputs(engine, snap=None):
    sinks = engine.get_all_sinks(snap=snap)
    sink_id_to_name = {sink["index"]: sink["name"] for sink in sinks}

    si_text = snap.sink_inputs_text if snap else (engine._run(["pactl", "list", "sink-inputs"]) or "")
    by_node_id, _ = engine._parse_pactl_si_map(si_text)

    entries = []
    seen_pactl_refs = set()

    stream_nodes = engine.get_app_streams(snap=snap)
    for node in stream_nodes:
        pactl = {}
        for ref in (
            node.props.get("node.id"),
            node.props.get("object.id"),
            node.props.get("object.serial"),
            node.pw_id,
        ):
            if ref is None:
                continue
            pactl = by_node_id.get(str(ref), {})
            if pactl:
                seen_pactl_refs.add(str(ref))
                break

        current = dict(node.props)
        for key, value in pactl.items():
            if key in ("_index", "_sink_id"):
                continue
            if value and not current.get(key):
                current[key] = value

        current.setdefault("node.name", node.name)
        current.setdefault("node.description", node.description)
        if node.app_name:
            current.setdefault("application.name", node.app_name)
        current.setdefault("node.id", str(node.pw_id))

        if "pid" not in current:
            current["pid"] = (
                current.get("pipewire.sec.pid")
                or current.get("application.process.id")
            )

        current["index"] = pactl.get("_index")
        sink_id = pactl.get("_sink_id")
        current["sink_id"] = sink_id
        current["sink"] = sink_id_to_name.get(sink_id, sink_id) if sink_id else None

        engine._process_sink_input(current, entries, sink_id_to_name)

    for node_id_str, pactl in by_node_id.items():
        if node_id_str in seen_pactl_refs:
            continue
        current = {
            key: value
            for key, value in pactl.items()
            if key not in ("_index", "_sink_id")
        }
        current["index"] = pactl["_index"]
        sink_id = pactl.get("_sink_id")
        current["sink_id"] = sink_id
        current["sink"] = sink_id_to_name.get(sink_id, sink_id) if sink_id else None
        if "pid" not in current:
            current["pid"] = (
                current.get("pipewire.sec.pid")
                or current.get("application.process.id")
            )
        engine._process_sink_input(current, entries, sink_id_to_name)

    return entries
