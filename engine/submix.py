"""Submix loopback and sink-input helpers for the PipeWire engine."""

from __future__ import annotations

import logging
import time


def load_loopback_module(
    engine,
    source_name,
    sink_name,
    latency_msec=20,
    channels=None,
    channel_map=None,
    source_dont_move=False,
    sink_dont_move=False,
):
    cmd = [
        "pactl",
        "load-module",
        "module-loopback",
        f"source={source_name}",
        f"sink={sink_name}",
        f"latency_msec={int(latency_msec)}",
        "adjust_time=0",
    ]
    if channels is not None:
        cmd.append(f"channels={int(channels)}")
    if channel_map:
        cmd.append(f"channel_map={channel_map}")
    if source_dont_move:
        cmd.append("source_dont_move=true")
    if sink_dont_move:
        cmd.append("sink_dont_move=true")
    out = engine._run(cmd)
    if not out:
        return None
    stripped = out.strip().splitlines()[-1].strip()
    if not stripped.isdigit():
        return None
    return stripped


def create_submix_replacement(engine, source_name, mix_name, initial_state=None):
    mix = engine.output_mixes.get(mix_name)
    if not mix or not mix.sink_name:
        return None
    module_id = engine._find_loopback_for(source_name, mix.sink_name)
    if module_id is None:
        module_id = engine._load_loopback_module(source_name, mix.sink_name)
        if module_id is None:
            return None
        engine.invalidate_snapshot()
    module_id = str(module_id)
    if not engine._module_is_alive(module_id):
        return None
    state = dict(initial_state or {})
    if state:
        key = next(
            (
                existing_key
                for existing_key, existing_module in engine.submix_loopbacks.items()
                if str(existing_module) == str(module_id)
            ),
            None,
        )
        if key:
            engine.submix_state_cache[key] = {
                "vol": engine._clamp(state.get("vol", 1.0)),
                "mute": bool(state.get("mute", False)),
            }
            if engine._apply_loopback_state(module_id, state):
                engine._pending_submix_state_reapply.discard(key)
            else:
                engine._pending_submix_state_reapply.add(key)
    return module_id


def commit_submix_replacements(engine, replacements, *, new_source):
    """Swap submix bookkeeping to replacement loopbacks."""
    for key, binding in replacements.items():
        engine.submix_loopbacks[key] = binding["module_id"]
        engine.submix_sources[key] = new_source
        state = dict(binding.get("state", {}) or {})
        if state:
            engine.submix_state_cache[key] = {
                "vol": engine._clamp(state.get("vol", 1.0)),
                "mute": bool(state.get("mute", False)),
            }
            engine._pending_submix_state_reapply.add(key)
    for binding in replacements.values():
        old_module = binding.get("old_module_id")
        new_module = binding.get("module_id")
        if old_module is None or str(old_module) == str(new_module):
            continue
        engine._run(["pactl", "unload-module", str(old_module)])
    if replacements:
        engine.invalidate_snapshot()


def route_input_to_submix(
    engine,
    node_id,
    node_name,
    media_class,
    mix_name,
    snap=None,
    initial_state=None,
):
    """Route an input source or sink monitor into a submix."""
    key = f"{node_id}->{mix_name}"

    mix = engine.output_mixes.get(mix_name)
    if not mix:
        return False

    short = snap.short_modules_text if snap else None

    fx_source = engine.get_channel_fx_source(node_name, snap=snap)
    raw_source_id = str(node_name)
    if fx_source:
        source_id = fx_source
    elif media_class == "Audio/Sink":
        source_id = f"{node_name}.monitor"
    else:
        source_id = raw_source_id

    known = engine.submix_loopbacks.get(key)
    known_source = engine.submix_sources.get(key)
    known_alive = bool(known and engine._module_is_alive(known, short_text=short))
    if known_alive and known_source == source_id:
        return True

    existing = engine._find_loopback_for(source_id, mix.sink_name, snap=snap)
    target_module = str(existing) if existing else None
    if not target_module:
        target_module = engine._run(
            [
                "pactl",
                "load-module",
                "module-loopback",
                f"source={source_id}",
                f"sink={mix.sink_name}",
                "latency_msec=20",
                "adjust_time=0",
            ]
        )
        if not target_module:
            if known_alive:
                return True
            return False
        engine.invalidate_snapshot()

    engine.submix_loopbacks[key] = target_module
    engine.submix_sources[key] = source_id
    if fx_source and media_class != "Audio/Sink":
        stale_raw_module = engine._find_loopback_for(raw_source_id, mix.sink_name, snap=snap)
        if stale_raw_module is not None and str(stale_raw_module) != str(target_module):
            engine._run(["pactl", "unload-module", str(stale_raw_module)])
            engine.invalidate_snapshot()
    if target_module != str(known or ""):
        state = dict(initial_state or engine.submix_state_cache.get(key, {}) or {})
        if state:
            engine.submix_state_cache[key] = {
                "vol": engine._clamp(state.get("vol", 1.0)),
                "mute": bool(state.get("mute", False)),
            }
            engine._apply_loopback_state(target_module, state)
            engine._pending_submix_state_reapply.add(key)
        if known and str(known) != str(target_module):
            logging.warning(
                f"[FX-DEBUG] route_input_to_submix({key}): source changed "
                f"'{known_source}' -> '{source_id}', replacing module {known} with {target_module}"
            )
            engine._run(["pactl", "unload-module", str(known)])
            engine.invalidate_snapshot()
    return True


def apply_loopback_state(engine, module_id, state):
    """Push volume/mute onto a loopback sink-input once it becomes visible."""
    if not module_id or not state:
        return False
    sink_input = engine._sink_input_for_module(module_id)
    if sink_input is None:
        for _ in range(20):
            time.sleep(0.005)
            sink_input = engine._sink_input_for_module(module_id)
            if sink_input is not None:
                break
    if sink_input is None:
        return False
    volume = engine._clamp(state.get("vol", 1.0))
    mute = bool(state.get("mute", False))
    pct = max(0, min(int(round(volume * 100)), 100))
    engine._run(["pactl", "set-sink-input-volume", sink_input, f"{pct}%"])
    engine._run(["pactl", "set-sink-input-mute", sink_input, "1" if mute else "0"])
    return True


def build_loopback_index(modules_text):
    """Parse a pactl module dump once into {(source, sink): module_id}."""
    index = {}
    curr_id = None
    curr_name = ""
    curr_args = []

    def flush():
        if curr_id and curr_name == "module-loopback":
            src = next(
                (arg.split("=", 1)[1] for arg in curr_args if arg.startswith("source=")),
                None,
            )
            sink = next(
                (arg.split("=", 1)[1] for arg in curr_args if arg.startswith("sink=")),
                None,
            )
            if src and sink:
                index.setdefault((src, sink), curr_id)

    for line in modules_text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Module #"):
            flush()
            curr_id = stripped.split("#", 1)[1].strip()
            curr_name = ""
            curr_args = []
        elif stripped.startswith("Name:"):
            curr_name = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("Argument:"):
            curr_args = stripped.split("Argument:", 1)[1].strip().split()
    flush()
    return index


def find_loopback_for(engine, source_token, sink_token, snap=None):
    if snap is not None:
        if snap._loopback_index is None:
            snap._loopback_index = engine._build_loopback_index(snap.modules_text)
        return snap._loopback_index.get((source_token, sink_token))
    modules_text = engine._run(["pactl", "list", "modules"]) or ""
    return engine._build_loopback_index(modules_text).get((source_token, sink_token))


def remove_node_routing(engine, node_id):
    """Clean up all loopbacks associated with a removed node."""
    node_id = str(node_id)
    for key in list(engine.submix_loopbacks.keys()):
        if key.startswith(f"{node_id}->"):
            mod_id = engine.submix_loopbacks.pop(key)
            engine.submix_sources.pop(key, None)
            engine.submix_state_cache.pop(key, None)
            engine._pending_submix_state_reapply.discard(key)
            engine._run(["pactl", "unload-module", str(mod_id)])


def get_submix_sink_input(engine, node_id, mix_name, snap=None):
    module_id = engine.submix_loopbacks.get(f"{node_id}->{mix_name}")
    if module_id is None:
        return None
    module_id = str(module_id)

    text = snap.sink_inputs_text if snap else engine._run(["pactl", "list", "sink-inputs"])
    if not text:
        return None
    current_si = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Sink Input #"):
            current_si = stripped.split("#", 1)[1].strip()
        elif "module.id =" in stripped and f'"{module_id}"' in stripped:
            return current_si
        elif stripped.startswith("Owner Module:"):
            owner = stripped.split(":", 1)[1].strip()
            if owner == module_id:
                return current_si
    return None


def set_submix_volume(engine, node_id, mix_name, volume):
    key = f"{node_id}->{mix_name}"
    cache = engine.submix_state_cache.setdefault(key, {})
    cache["vol"] = engine._clamp(volume)
    sink_input = engine.get_submix_sink_input(node_id, mix_name)
    if not sink_input:
        logging.warning(f"Could not find sink-input for {node_id}->{mix_name}")
        engine._pending_submix_state_reapply.add(key)
        return False
    clamped = cache["vol"]
    pct = max(0, min(int(round(clamped * 100)), 100))
    engine._run(["pactl", "set-sink-input-volume", sink_input, f"{pct}%"])
    engine._pending_submix_state_reapply.discard(key)
    return True


def set_submix_mute(engine, node_id, mix_name, mute):
    key = f"{node_id}->{mix_name}"
    cache = engine.submix_state_cache.setdefault(key, {})
    cache["mute"] = bool(mute)
    sink_input = engine.get_submix_sink_input(node_id, mix_name)
    if not sink_input:
        logging.warning(f"Could not find sink-input to mute for {node_id}->{mix_name}")
        engine._pending_submix_state_reapply.add(key)
        return False
    engine._run(["pactl", "set-sink-input-mute", sink_input, "1" if cache["mute"] else "0"])
    engine._pending_submix_state_reapply.discard(key)
    return True


def reapply_submix_state_cache(engine):
    pending = set(getattr(engine, "_pending_submix_state_reapply", set()) or set())
    for key in pending:
        cache = engine.submix_state_cache.get(key)
        if not cache:
            engine._pending_submix_state_reapply.discard(key)
            continue
        mod_id = engine.submix_loopbacks.get(key)
        if mod_id is None:
            continue
        sink_input = engine._sink_input_for_module(mod_id)
        if sink_input is None:
            continue
        if "vol" in cache:
            pct = max(0, min(int(round(cache["vol"] * 100)), 100))
            engine._run(["pactl", "set-sink-input-volume", sink_input, f"{pct}%"])
        if "mute" in cache:
            engine._run(
                ["pactl", "set-sink-input-mute", sink_input, "1" if cache["mute"] else "0"]
            )
        engine._pending_submix_state_reapply.discard(key)


def sink_input_for_module(engine, module_id):
    if not module_id:
        return None
    text = engine._run(["pactl", "list", "sink-inputs"])
    if not text:
        return None
    module_id = str(module_id)
    current_si = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Sink Input #"):
            current_si = stripped.split("#", 1)[1].strip()
        elif stripped.startswith("Owner Module:") and stripped.split(":", 1)[1].strip() == module_id:
            return current_si
        elif "module.id =" in stripped and f'"{module_id}"' in stripped:
            return current_si
    return None


def wait_sink_input_for_module(engine, module_id, attempts=20, delay=0.05):
    if not module_id:
        return None
    for _ in range(max(1, int(attempts))):
        sink_input = engine._sink_input_for_module(module_id)
        if sink_input is not None:
            return sink_input
        time.sleep(max(0.0, float(delay)))
    return None


def wait_load_loopback(
    engine,
    source,
    sink,
    latency_msec=20,
    attempts=20,
    delay=0.1,
    channels=None,
    channel_map=None,
    source_dont_move=False,
    sink_dont_move=False,
):
    for _ in range(attempts):
        cmd = [
            "pactl",
            "load-module",
            "module-loopback",
            f"source={source}",
            f"sink={sink}",
            f"latency_msec={int(latency_msec)}",
            "adjust_time=0",
        ]
        if channels is not None:
            cmd.append(f"channels={int(channels)}")
        if channel_map:
            cmd.append(f"channel_map={channel_map}")
        if source_dont_move:
            cmd.append("source_dont_move=true")
        if sink_dont_move:
            cmd.append("sink_dont_move=true")
        out = engine._run(cmd)
        if out:
            stripped = out.strip().splitlines()[-1].strip()
            if stripped.isdigit():
                for _ in range(20):
                    sink_input = engine._sink_input_for_module(stripped)
                    if sink_input is None:
                        time.sleep(0.01)
                        continue
                    engine._run(["pactl", "set-sink-input-volume", sink_input, "100%"])
                    engine._run(["pactl", "set-sink-input-mute", sink_input, "0"])
                    break
                return stripped
        time.sleep(delay)
    return None
