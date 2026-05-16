"""Small runtime helper functions for the PipeWire engine."""

from __future__ import annotations


def sink_visible(engine, sink_name):
    if not sink_name:
        return False
    out = engine._run(["pactl", "list", "short", "sinks"])
    if not out:
        return False
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 2 and parts[1].strip() == sink_name:
            return True
    return False


def preferred_hardware_source_fallback(engine, snap=None):
    def resolve_visible_source(candidate):
        try:
            return engine.resolve_source_name(candidate, snap=snap)
        except TypeError:
            return engine.resolve_source_name(candidate)

    current_default = str(engine.get_default_source() or "").strip()
    resolved_default = (
        engine.resolve_hardware_source_name(current_default, snap=snap)
        or resolve_visible_source(current_default)
        or str(current_default or "").strip()
    )
    if resolved_default and not engine._is_internal_node_name(resolved_default):
        return resolved_default
    for info in getattr(engine, "channel_fx", {}).values():
        for candidate in (info.get("prev_default"), info.get("capture_target")):
            resolved = (
                engine.resolve_hardware_source_name(candidate, snap=snap)
                or resolve_visible_source(candidate)
                or str(candidate or "").strip()
            )
            if resolved and not engine._is_internal_node_name(resolved):
                return resolved
    for node in engine.get_hardware_inputs(snap=snap):
        name = str(getattr(node, "name", "") or "").strip()
        if name:
            return name
    return None


def rename_virtual_sink(engine, old_sink_name, new_display_name):
    if not old_sink_name.startswith("wavelinux_"):
        return None
    _display_clean, safe_tail = engine._sanitize_channel_name(new_display_name)
    new_sink_name = f"wavelinux_{safe_tail}"
    if new_sink_name == old_sink_name:
        return old_sink_name
    engine.remove_virtual_sink(old_sink_name)
    if engine.create_virtual_sink(new_display_name) is None:
        return None
    return new_sink_name


def find_module_by_arg(engine, pattern, modules_text=None):
    text = modules_text if modules_text is not None else engine._run(["pactl", "list", "modules"])
    if not text:
        return None
    curr_id = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("Module #"):
            curr_id = stripped.split("#", 1)[1].strip()
            continue
        if not curr_id or "Argument:" not in stripped:
            continue
        args = stripped.split("Argument:", 1)[1].strip().split()
        if pattern in args:
            return curr_id
    return None


def module_is_alive(engine, module_id, short_text=None):
    if module_id is None:
        return False
    text = short_text if short_text is not None else engine._run(["pactl", "list", "short", "modules"])
    if not text:
        return False
    mid = str(module_id)
    for line in text.splitlines():
        parts = line.split("\t", 1)
        if parts and parts[0].strip() == mid:
            return True
    return False
