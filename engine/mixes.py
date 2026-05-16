"""Virtual sink and output mix lifecycle helpers for the PipeWire engine."""

from __future__ import annotations

import logging
import time

from .models import OutputMix


def create_virtual_sink(engine, display_name, custom_name=None):
    """Create a virtual null-sink and return the sink name on success."""
    display_clean, safe_tail = engine._sanitize_channel_name(display_name)
    safe_name = custom_name or f"wavelinux_{safe_tail}"
    description = engine._branding_label(display_clean)

    existing = engine._find_module_by_arg(f"sink_name={safe_name}")
    if existing:
        logging.info(f"Using existing sink {safe_name} (ID: {existing})")
        if not safe_name.startswith("wavelinux_mix_"):
            engine.virtual_sink_modules[safe_name] = existing
        return safe_name

    cmd = [
        "pactl",
        "load-module",
        "module-null-sink",
        f"sink_name={safe_name}",
        (
            f"sink_properties="
            f"device.description={description} "
            f"node.description={description} "
            f"node.nick={description} "
            f"media.name={description} "
            f"application.name={description} "
            f"media.class=Audio/Sink"
        ),
    ]
    out = engine._run(cmd)
    if out:
        engine._run(["pactl", "set-sink-mute", safe_name, "0"])
        engine._run(["pactl", "set-sink-volume", safe_name, "100%"])
        if not safe_name.startswith("wavelinux_mix_"):
            engine.virtual_sink_modules[safe_name] = out
        return safe_name
    return None


def remove_virtual_sink(engine, sink_name):
    """Unload a user-created virtual sink and any dependent loopbacks."""
    module_id = engine.virtual_sink_modules.pop(sink_name, None)
    if module_id is None:
        module_id = engine._find_module_by_arg(f"sink_name={sink_name}")
    if module_id is None:
        return False

    full = engine._run(["pactl", "list", "modules"]) or ""
    curr_id = None
    to_drop = []
    for line in full.splitlines():
        line = line.strip()
        if line.startswith("Module #"):
            curr_id = line.split("#", 1)[1].strip()
        elif "Argument:" in line and f"sink={sink_name}" in line and curr_id:
            to_drop.append(curr_id)
    for module_id_to_drop in to_drop:
        engine._run(["pactl", "unload-module", module_id_to_drop])

    monitor_token = f"{sink_name}.monitor"
    for key in list(engine.submix_sources.keys()):
        if engine.submix_sources.get(key) != monitor_token:
            continue
        mod = engine.submix_loopbacks.pop(key, None)
        engine.submix_sources.pop(key, None)
        engine.submix_state_cache.pop(key, None)
        if mod is not None:
            engine._run(["pactl", "unload-module", str(mod)])

    engine._run(["pactl", "unload-module", str(module_id)])
    return True


def create_output_mix(engine, name):
    """Create a mix bus with a null-sink and dedicated virtual source."""
    _, safe_name = engine._sanitize_channel_name(name)
    sink_name = f"wavelinux_mix_{safe_name}"
    requested_source_name = f"wavelinux_src_{safe_name}"
    description = engine._branding_label(name)

    if engine.create_virtual_sink(name, custom_name=sink_name) is None:
        return None
    sink_module_id = engine.virtual_sink_modules.get(sink_name) or engine._find_module_by_arg(
        f"sink_name={sink_name}"
    )

    src_module_id = engine._find_module_by_arg(f"source_name={requested_source_name}")
    if not src_module_id:
        src_module_id = engine._run(
            [
                "pactl",
                "load-module",
                "module-virtual-source",
                f"source_name={requested_source_name}",
                f"master={sink_name}.monitor",
                (
                    f"source_properties="
                    f"device.description={description} "
                    f"node.description={description} "
                    f"node.nick={description} "
                    f"media.name={description} "
                    f"application.name={description} "
                    f"media.class=Audio/Source "
                    f"device.class=sound"
                ),
            ]
        )
    source_name = (
        engine._wait_source_visible(requested_source_name, attempts=20, delay=0.05)
        or engine.resolve_source_name(requested_source_name)
        or requested_source_name
    )

    mix = OutputMix(name, sink_module_id=sink_module_id, sink_name=sink_name)
    mix.source_name = source_name
    mix.source_module_id = src_module_id
    engine.output_mixes[name] = mix
    return mix


def remove_output_mix(engine, mix_name):
    mix = engine.output_mixes.get(mix_name)
    if not mix:
        return False
    for module_id in (getattr(mix, "source_module_id", None), mix.sink_module_id):
        if module_id:
            engine._run(["pactl", "unload-module", str(module_id)])
    for key in list(engine.loopback_modules.keys()):
        if key.startswith(mix_name + "->"):
            engine._run(["pactl", "unload-module", str(engine.loopback_modules[key])])
            del engine.loopback_modules[key]
    for submix_key in list(engine.submix_loopbacks.keys()):
        if submix_key.endswith(f"->{mix_name}"):
            mod = engine.submix_loopbacks.pop(submix_key, None)
            engine.submix_sources.pop(submix_key, None)
            if mod is not None:
                engine._run(["pactl", "unload-module", str(mod)])
    del engine.output_mixes[mix_name]
    return True


def route_mix_to_hardware(engine, mix_name, hw_sink_name):
    """Route a mix bus to a hardware output via module-loopback."""
    mix = engine.output_mixes.get(mix_name)
    if not mix:
        return False
    requested_sink = str(hw_sink_name or "").strip()
    resolved_sink = engine.resolve_hardware_sink_name(requested_sink)
    allow_fallback = mix_name == "Monitor"
    if not resolved_sink and allow_fallback:
        resolved_sink = engine._preferred_hardware_sink_fallback(snap=None)
        if resolved_sink and requested_sink:
            logging.warning(
                "route_mix_to_hardware: falling back from missing sink %s to %s for %s",
                requested_sink,
                resolved_sink,
                mix_name,
            )
    current_key = f"{mix_name}->{resolved_sink or requested_sink}"
    short = engine._run(["pactl", "list", "short", "modules"]) or ""
    existing_mod = engine.loopback_modules.get(current_key)
    if (
        existing_mod
        and engine._module_is_alive(existing_mod, short_text=short)
        and engine._wait_sink_input_for_module(existing_mod, attempts=2, delay=0.01)
    ):
        mix.hardware_output = resolved_sink or requested_sink
        return True

    out = None
    adopted = None
    candidate_sink = resolved_sink
    if candidate_sink:
        adopted = engine._find_loopback_for(f"{mix.sink_name}.monitor", candidate_sink)
        if adopted and engine._wait_sink_input_for_module(adopted, attempts=2, delay=0.01):
            out = str(adopted)
    for _ in range(12):
        if out:
            break
        candidate_sink = engine.resolve_hardware_sink_name(requested_sink)
        if not candidate_sink and allow_fallback:
            candidate_sink = engine._preferred_hardware_sink_fallback(snap=None)
        if candidate_sink:
            adopted = engine._find_loopback_for(f"{mix.sink_name}.monitor", candidate_sink)
            if adopted and engine._wait_sink_input_for_module(adopted, attempts=2, delay=0.01):
                out = str(adopted)
                break
            out = engine._run(
                [
                    "pactl",
                    "load-module",
                    "module-loopback",
                    f"source={mix.sink_name}.monitor",
                    f"sink={candidate_sink}",
                    "latency_msec=20",
                    "adjust_time=0",
                ]
            )
            if out:
                break
        time.sleep(0.1)

    if not out:
        logging.warning(
            f"route_mix_to_hardware: could not load loopback "
            f"{mix.sink_name}.monitor -> {requested_sink or '<none>'}"
        )
        return False

    candidate_mod = str(out)
    if not engine._wait_sink_input_for_module(candidate_mod):
        if candidate_mod != str(existing_mod or ""):
            engine._run(["pactl", "unload-module", candidate_mod])
        logging.warning(
            f"route_mix_to_hardware: loopback module {candidate_mod} "
            f"never produced a sink-input for {mix_name}"
        )
        return False

    for key in list(engine.loopback_modules.keys()):
        if not key.startswith(mix_name + "->"):
            continue
        if str(engine.loopback_modules[key]) == candidate_mod:
            continue
        engine._run(["pactl", "unload-module", str(engine.loopback_modules[key])])
        del engine.loopback_modules[key]

    resolved_sink = candidate_sink or resolved_sink or requested_sink
    current_key = f"{mix_name}->{resolved_sink}"
    engine.loopback_modules[current_key] = candidate_mod
    mix.hardware_output = resolved_sink
    sink_input = engine._sink_input_for_module(candidate_mod)
    if sink_input is not None:
        engine._run(["pactl", "set-sink-input-volume", sink_input, "100%"])
        engine._run(["pactl", "set-sink-input-mute", sink_input, "0"])
    engine.invalidate_snapshot()
    return True


def unroute_mix_from_hardware(engine, mix_name):
    changed = False
    for key in list(engine.loopback_modules.keys()):
        if key.startswith(mix_name + "->"):
            engine._run(["pactl", "unload-module", str(engine.loopback_modules[key])])
            del engine.loopback_modules[key]
            changed = True
    mix = engine.output_mixes.get(mix_name)
    if mix:
        mix.hardware_output = None
    return changed


def get_live_mix_hardware_route(engine, mix_name, snap=None):
    mix = engine.output_mixes.get(mix_name)
    if not mix or not getattr(mix, "hardware_output", None):
        return None
    resolved_sink = engine.resolve_hardware_sink_name(mix.hardware_output, snap=snap)
    if not resolved_sink:
        return None
    current_key = f"{mix_name}->{resolved_sink}"
    short = snap.short_modules_text if snap is not None else None
    existing_mod = engine.loopback_modules.get(current_key)
    if (
        existing_mod
        and engine._module_is_alive(existing_mod, short_text=short)
        and engine._wait_sink_input_for_module(existing_mod, attempts=2, delay=0.01)
    ):
        return resolved_sink
    adopted = engine._find_loopback_for(f"{mix.sink_name}.monitor", resolved_sink, snap=snap)
    if (
        adopted
        and engine._module_is_alive(adopted, short_text=short)
        and engine._wait_sink_input_for_module(adopted, attempts=2, delay=0.01)
    ):
        engine.loopback_modules[current_key] = str(adopted)
        return resolved_sink
    return None


def preferred_hardware_sink_fallback(engine, snap=None):
    current_default = str(engine.get_default_sink() or "").strip()
    resolved_default = engine.resolve_hardware_sink_name(current_default, snap=snap)
    if resolved_default and not engine._is_internal_node_name(resolved_default):
        return resolved_default
    for mix_name in ("Monitor", "Stream"):
        mix = engine.output_mixes.get(mix_name)
        candidate = getattr(mix, "hardware_output", None)
        resolved = engine.resolve_hardware_sink_name(candidate, snap=snap)
        if resolved and not engine._is_internal_node_name(resolved):
            return resolved
    for node in engine.get_hardware_outputs(snap=snap):
        name = str(getattr(node, "name", "") or "").strip()
        if name:
            return name
    return None


def move_app_streams_off_managed_sinks(engine, fallback_sink, snap=None):
    fallback_sink = engine.resolve_hardware_sink_name(fallback_sink, snap=snap) or fallback_sink
    fallback_sink = str(fallback_sink or "").strip()
    if not fallback_sink or engine._is_internal_node_name(fallback_sink):
        return []
    moved = []
    for app in engine.get_sink_inputs(snap=snap):
        sink_name = str(app.get("sink") or "").strip()
        sink_input_index = str(app.get("index") or "").strip()
        if not sink_input_index or not sink_name.startswith("wavelinux_"):
            continue
        engine.move_app_to_sink(sink_input_index, fallback_sink)
        moved.append((sink_input_index, sink_name, fallback_sink))
    return moved
