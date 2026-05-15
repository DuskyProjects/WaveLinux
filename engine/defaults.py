"""Default sink/source control helpers."""

from __future__ import annotations

import time


def get_default_sink(engine):
    return engine._run(["pactl", "get-default-sink"])


def get_default_source(engine):
    return engine._run(["pactl", "get-default-source"])


def set_default_sink(engine, sink_name):
    if not sink_name:
        return False
    resolved = engine.resolve_hardware_sink_name(sink_name) or sink_name
    return engine._run(["pactl", "set-default-sink", resolved]) is not None


def source_name_aliases(source_name):
    source_name = str(source_name or "").strip()
    if not source_name:
        return []
    aliases = [source_name]
    if source_name.startswith("output."):
        aliases.append(source_name[len("output."):])
    else:
        aliases.append(f"output.{source_name}")
    seen = []
    for alias in aliases:
        if alias and alias not in seen:
            seen.append(alias)
    return seen


def set_default_source(engine, source_name, *, attempts=20, delay=0.05):
    if not source_name:
        return False
    resolved = (
        engine._wait_source_visible(source_name, attempts=attempts, delay=delay)
        or engine.resolve_source_name(source_name)
        or source_name
    )
    candidates = source_name_aliases(resolved)
    for _ in range(attempts):
        for candidate in candidates:
            if engine._run(["pactl", "set-default-source", candidate]) is None:
                continue
            current = engine.get_default_source()
            if not current or engine._source_names_match(current, candidate):
                return True
        time.sleep(delay)
        refreshed = engine.resolve_source_name(source_name)
        if refreshed:
            candidates = source_name_aliases(refreshed)
    return False
