"""Snapshot parsing and stable-device helpers for the PipeWire engine."""

from __future__ import annotations

import json
import logging
import re
import time

from .models import AudioNode, EngineSnapshot


def create_snapshot(engine, force=False):
    """Fetch and cache expensive pactl/pw-dump state for one refresh tick."""
    now = time.monotonic()
    if getattr(engine, "_pending_submix_state_reapply", None):
        engine._reapply_submix_state_cache()
        force = True
    cached = getattr(engine, "_snapshot_cache", None)
    cached_at = getattr(engine, "_snapshot_cache_at", 0.0)
    if cached is not None and not force and (now - cached_at) < engine._SNAPSHOT_TTL:
        return cached

    snap = EngineSnapshot(
        modules_text=engine._run(["pactl", "list", "modules"]) or "",
        short_modules_text=engine._run(["pactl", "list", "short", "modules"]) or "",
        sink_inputs_text=engine._run(["pactl", "list", "sink-inputs"]) or "",
        sinks_text=engine._run(["pactl", "list", "sinks"]) or "",
        sources_text=engine._run(["pactl", "list", "sources"]) or "",
        nodes=engine._parse_nodes(),
        sinks=engine._parse_short_sinks(),
    )
    engine._snapshot_cache = snap
    engine._snapshot_cache_at = now
    engine.reap_dead_processes()
    return snap


def invalidate_snapshot(engine):
    """Drop the cached snapshot so the next refresh re-fetches fresh state."""
    engine._snapshot_cache = None
    engine._snapshot_cache_at = 0.0


def parse_sink_descriptions(text):
    """Return {sink_name: description} from `pactl list sinks`."""
    out = {}
    curr_name = None
    curr_desc = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Sink #"):
            if curr_name is not None and curr_desc:
                out[curr_name] = curr_desc
            curr_name = None
            curr_desc = None
        elif stripped.startswith("Name:"):
            curr_name = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("Description:"):
            curr_desc = stripped.split(":", 1)[1].strip()
    if curr_name is not None and curr_desc:
        out[curr_name] = curr_desc
    return out


def parse_sinks_state(text):
    """Parse `pactl list sinks` into {sink_name: (volume, muted)}."""
    state = {}
    curr_name = None
    curr_vol = None
    curr_mute = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Sink #"):
            if curr_name is not None and curr_vol is not None:
                state[curr_name] = (curr_vol, curr_mute)
            curr_name = None
            curr_vol = None
            curr_mute = False
        elif stripped.startswith("Name:"):
            curr_name = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("Mute:"):
            curr_mute = stripped.split(":", 1)[1].strip().lower() == "yes"
        elif stripped.startswith("Volume:") and curr_vol is None:
            match = re.search(r"/\s*(\d+)%", stripped)
            if match:
                try:
                    curr_vol = int(match.group(1)) / 100.0
                except ValueError:
                    pass
    if curr_name is not None and curr_vol is not None:
        state[curr_name] = (curr_vol, curr_mute)
    return state


def parse_sources_state(text):
    """Parse `pactl list sources` into {source_name: (volume, muted)}."""
    state = {}
    curr_name = None
    curr_vol = None
    curr_mute = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Source #"):
            if curr_name is not None and curr_vol is not None:
                state[curr_name] = (curr_vol, curr_mute)
            curr_name = None
            curr_vol = None
            curr_mute = False
        elif stripped.startswith("Name:"):
            curr_name = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("Mute:"):
            curr_mute = stripped.split(":", 1)[1].strip().lower() == "yes"
        elif stripped.startswith("Volume:") and curr_vol is None:
            match = re.search(r"/\s*(\d+)%", stripped)
            if match:
                try:
                    curr_vol = int(match.group(1)) / 100.0
                except ValueError:
                    pass
    if curr_name is not None and curr_vol is not None:
        state[curr_name] = (curr_vol, curr_mute)
    return state


def parse_nodes(engine):
    raw = engine._run(["pw-dump"], timeout=4)
    if not raw:
        logging.warning("pw-dump returned no output; node graph empty for this tick")
        return []
    try:
        data = json.loads(raw)
    except json.JSONDecodeError:
        logging.warning("pw-dump output was not valid JSON; node graph empty for this tick")
        return []

    client_props_by_id = {}
    for obj in data:
        if obj.get("type") != "PipeWire:Interface:Client":
            continue
        props = obj.get("info", {}).get("props", {}) or {}
        client_id = obj.get("id")
        if client_id is None:
            continue
        client_props_by_id[str(client_id)] = dict(props)

    nodes = []
    for obj in data:
        if obj.get("type") != "PipeWire:Interface:Node":
            continue
        props = dict(obj.get("info", {}).get("props", {}) or {})
        client_id = props.get("client.id")
        client_props = client_props_by_id.get(str(client_id or ""))
        if client_props:
            for key, value in client_props.items():
                if value and not props.get(key):
                    props[key] = value
        media_class = props.get("media.class", "")
        if not media_class.startswith(("Audio/", "Stream/")):
            continue
        nodes.append(
            AudioNode(
                pw_id=obj["id"],
                name=props.get("node.name", ""),
                description=props.get("node.description", props.get("node.name", "Unknown")),
                media_class=media_class,
                app_name=props.get("application.name"),
                props=props,
            )
        )
    return nodes


def parse_short_sinks(engine):
    out = engine._run(["pactl", "list", "short", "sinks"])
    if not out:
        return []
    sinks = []
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 2:
            sinks.append({"index": parts[0], "name": parts[1]})
    return sinks


def get_all_nodes(engine, snap=None):
    if snap is not None and hasattr(snap, "nodes"):
        return snap.nodes
    return engine._parse_nodes()


def is_internal_node_name(name):
    raw = str(name or "").strip().lower()
    if raw.startswith(
        (
            "wavelinux_stress_fx_",
            "output.wavelinux_stress_fx_",
            "input.wavelinux_stress_fx_",
        )
    ):
        return False
    return raw.startswith(
        (
            "wavelinux_",
            "wavelinux.",
            "output.wavelinux_",
            "output.wavelinux.",
            "input.wavelinux_",
            "input.wavelinux.",
        )
    )


def get_hardware_outputs(engine, snap=None):
    return [
        node
        for node in engine.get_all_nodes(snap)
        if node.media_class == "Audio/Sink" and not engine._is_internal_node_name(node.name)
    ]


def normalize_stable_component(value):
    return re.sub(r"[^a-z0-9]+", "_", str(value or "").strip().lower()).strip("_")


def looks_like_stable_device_id(value):
    value = str(value or "").strip().lower()
    return value.startswith(("bt:", "usb:", "hw:", "name:"))


def normalized_bt_family(engine_cls, name, *, source=False):
    name = str(name or "").strip()
    if not name:
        return ""
    matcher = engine_cls._BT_SOURCE_FAMILY_RE if source else engine_cls._BT_SINK_FAMILY_RE
    match = matcher.match(name)
    if not match:
        return ""
    return engine_cls._normalize_stable_component(match.group(1))


def stable_device_id_from_props(engine_cls, prefix, name, props=None, *, source=False):
    _ = prefix
    name = str(name or "").strip()
    props = dict(props or {})
    bt_family = engine_cls._normalized_bt_family(name, source=source)
    if bt_family:
        return f"bt:{bt_family}"

    for key in (
        "device.string",
        "device.api",
        "device.description",
        "node.description",
        "device.name",
    ):
        bt_family = engine_cls._normalized_bt_family(props.get(key), source=source)
        if bt_family:
            return f"bt:{bt_family}"

    for key in (
        "device.serial",
        "device.bus-id",
        "device.bus_id",
        "device.product.id",
        "device.vendor.id",
    ):
        token = engine_cls._normalize_stable_component(props.get(key))
        if token:
            return f"usb:{token}"

    for key in (
        "device.bus-path",
        "device.bus_path",
        "device.path",
        "api.alsa.path",
        "object.path",
        "alsa.card",
        "api.alsa.card",
        "api.alsa.card.longname",
        "device.name",
    ):
        token = engine_cls._normalize_stable_component(props.get(key))
        if token:
            return f"hw:{token}"

    stem = re.sub(r"^(?:output|input)\.", "", name, flags=re.IGNORECASE)
    stem = re.sub(r"\.monitor$", "", stem, flags=re.IGNORECASE)
    token = engine_cls._normalize_stable_component(stem)
    if token:
        return f"name:{token}"
    return ""


def stable_sink_id(engine_cls, sink_name):
    sink_name = str(sink_name or "").strip()
    if not sink_name:
        return ""
    if engine_cls._looks_like_stable_device_id(sink_name):
        return sink_name.lower()
    return engine_cls._stable_device_id_from_props("sink", sink_name, {})


def node_by_name(engine, node_name, snap=None):
    node_name = str(node_name or "").strip()
    if not node_name:
        return None
    for node in engine.get_all_nodes(snap):
        if str(getattr(node, "name", "") or "").strip() == node_name:
            return node
    return None


def stable_source_id(engine, source_name_or_node, snap=None):
    if isinstance(source_name_or_node, AudioNode):
        node = source_name_or_node
        return engine._stable_device_id_from_props(
            "source",
            getattr(node, "name", ""),
            getattr(node, "props", {}) or {},
            source=True,
        )
    source_name = str(source_name_or_node or "").strip()
    if not source_name:
        return ""
    if engine._looks_like_stable_device_id(source_name):
        return source_name.lower()
    node = engine._node_by_name(source_name, snap=snap)
    props = getattr(node, "props", {}) or {}
    return engine._stable_device_id_from_props(
        "source",
        source_name,
        props,
        source=True,
    )


def stable_sink_inventory(engine, snap=None):
    inventory = []
    for sink in engine.get_all_sinks(snap=snap):
        sink_name = str(sink.get("name") or "").strip()
        if not sink_name or engine._is_internal_node_name(sink_name):
            continue
        node = engine._node_by_name(sink_name, snap=snap)
        stable_id = engine._stable_device_id_from_props(
            "sink",
            sink_name,
            getattr(node, "props", {}) or {},
        )
        inventory.append(
            {
                "name": sink_name,
                "display_name": engine.display_name_for_sink(sink_name, snap=snap),
                "stable_id": stable_id or engine.stable_sink_id(sink_name),
            }
        )
    return inventory


def stable_source_inventory(engine, snap=None):
    inventory = []
    for node in engine.get_hardware_inputs(snap=snap):
        source_name = str(getattr(node, "name", "") or "").strip()
        if not source_name:
            continue
        inventory.append(
            {
                "name": source_name,
                "display_name": engine.display_name_for_source(node, snap=snap),
                "stable_id": engine.stable_source_id(node, snap=snap),
            }
        )
    return inventory


def resolve_hardware_sink_name(engine, sink_name, snap=None):
    sink_name = str(sink_name or "").strip()
    if not sink_name:
        return None
    inventory = engine.stable_sink_inventory(snap=snap)
    names = [sink["name"] for sink in inventory if sink.get("name")]
    if sink_name in names:
        return sink_name
    wanted = (
        sink_name.lower()
        if engine._looks_like_stable_device_id(sink_name)
        else engine.stable_sink_id(sink_name)
    )
    if not wanted:
        return None
    for sink in inventory:
        if sink.get("stable_id") == wanted:
            return sink.get("name")
    return None


def resolve_hardware_source_name(engine, source_or_stable_id, snap=None):
    source_or_stable_id = str(source_or_stable_id or "").strip()
    if not source_or_stable_id:
        return None
    inventory = engine.stable_source_inventory(snap=snap)
    names = [source["name"] for source in inventory if source.get("name")]
    if source_or_stable_id in names:
        return source_or_stable_id
    wanted = (
        source_or_stable_id.lower()
        if engine._looks_like_stable_device_id(source_or_stable_id)
        else engine.stable_source_id(source_or_stable_id, snap=snap)
    )
    for source in inventory:
        if source.get("stable_id") == wanted:
            return source.get("name")
    return None


def get_hardware_inputs(engine, snap=None):
    nodes = []
    for node in engine.get_all_nodes(snap):
        if node.media_class != "Audio/Source":
            continue
        if "rnnoise" in node.name.lower():
            continue
        if engine._is_internal_node_name(node.name):
            continue
        node.volume, node.muted = engine.get_source_volume_by_name(node.name, snap=snap)
        nodes.append(node)
    return nodes


def display_name_for_source(engine, source_name_or_node, snap=None):
    if isinstance(source_name_or_node, AudioNode):
        node = source_name_or_node
        source_name = str(getattr(node, "name", "") or "").strip()
    else:
        source_name = str(source_name_or_node or "").strip()
        node = engine._node_by_name(source_name, snap=snap)
    if node is None:
        return engine.friendly_name(source_name) or source_name
    props = dict(getattr(node, "props", {}) or {})
    for candidate in (
        props.get("device.description"),
        props.get("node.nick"),
        props.get("device.nick"),
        props.get("device.product.name"),
        props.get("api.alsa.card.name"),
        props.get("alsa.card_name"),
        getattr(node, "description", ""),
        props.get("node.description"),
        props.get("device.profile.description"),
        source_name,
    ):
        cleaned = engine.friendly_name(candidate)
        if cleaned and cleaned != "Unknown":
            return cleaned
    return source_name


def get_virtual_sinks(engine, snap=None):
    return [
        node
        for node in engine.get_all_nodes(snap)
        if node.media_class == "Audio/Sink"
        and node.name in engine.virtual_sink_modules
        and not node.name.startswith("wavelinux_mix_")
        and not node.name.startswith("wavelinux_src_")
    ]


def get_app_streams(engine, snap=None):
    return [
        node
        for node in engine.get_all_nodes(snap)
        if node.media_class.startswith("Stream/Output/Audio")
    ]


def get_all_sinks(engine, snap=None):
    if snap is not None and hasattr(snap, "sinks"):
        return snap.sinks
    return engine._parse_short_sinks()


def get_sink_description(engine, sink_name, snap=None):
    if snap is None:
        text = engine._run(["pactl", "list", "sinks"]) or ""
        return engine._parse_sink_descriptions(text).get(sink_name)
    if snap._sink_descriptions is None:
        snap._sink_descriptions = engine._parse_sink_descriptions(snap.sinks_text)
    return snap._sink_descriptions.get(sink_name)


def display_name_for_sink(engine, sink_name, snap=None):
    desc = engine.get_sink_description(sink_name, snap=snap)
    if desc:
        cleaned = engine.friendly_name(desc)
        if cleaned and cleaned != "Unknown":
            return cleaned
    return engine.friendly_name(sink_name)
