"""Source-output routing helpers for the PipeWire engine."""

from __future__ import annotations

import logging
import time


def _source_name_matches(engine, left, right):
    matcher = getattr(engine, "_source_names_match", None)
    if callable(matcher):
        return bool(matcher(left, right))
    return str(left or "").strip() == str(right or "").strip()


def _resolved_source_candidates(engine, source_name, *, attempts=20, delay=0.05):
    resolved = (
        engine._wait_source_visible(source_name, attempts=attempts, delay=delay)
        or engine.resolve_source_name(source_name)
        or source_name
    )
    candidates = []
    for seed in (resolved, source_name):
        for candidate in engine._source_name_aliases(seed):
            candidate = str(candidate or "").strip()
            if candidate and candidate not in candidates:
                candidates.append(candidate)
    return candidates


def resolve_source_name(engine, source_name, snap=None):
    """Resolve a persisted source token to a currently visible source name."""
    wanted = engine._source_name_aliases(source_name)
    if not wanted:
        return None
    visible = set()
    if snap is not None:
        visible.update(
            node.name
            for node in getattr(snap, "nodes", [])
            if getattr(node, "media_class", "") == "Audio/Source"
        )
    visible.update(engine._source_id_to_name().values())
    for candidate in wanted:
        if candidate in visible:
            return candidate
    return None


def source_id_to_name(engine):
    """Build {source_id: source_name} from `pactl list short sources`."""
    out = engine._run(["pactl", "list", "short", "sources"])
    mapping = {}
    if not out:
        return mapping
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) >= 2 and parts[0].strip().isdigit():
            mapping[parts[0].strip()] = parts[1].strip()
    return mapping


def list_source_outputs_on(engine, source_name, exclude_modules=None):
    """Return source-output ids currently capturing from `source_name`."""
    short = engine._run(["pactl", "list", "short", "source-outputs"])
    full = engine._run(["pactl", "list", "source-outputs"])
    if not short or not full:
        return []

    id_to_name = engine._source_id_to_name()
    current_by_so = {}
    for line in short.splitlines():
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        so_id = parts[0].strip()
        src_id = parts[1].strip()
        current_by_so[so_id] = id_to_name.get(src_id)

    excluded = {str(module_id) for module_id in (exclude_modules or ()) if module_id is not None}
    wanted_names = set(engine._source_name_aliases(source_name))
    resolved_name = engine.resolve_source_name(source_name)
    if resolved_name:
        wanted_names.add(resolved_name)
    ids = []
    current_so = None
    current_owner = None
    current_target = None

    def flush():
        if not current_so or current_owner in excluded:
            return
        live_source = current_by_so.get(current_so)
        if live_source in wanted_names or current_target in wanted_names:
            ids.append(current_so)

    for line in full.splitlines():
        stripped = line.strip()
        if stripped.startswith("Source Output #"):
            flush()
            current_so = stripped.split("#", 1)[1].strip()
            current_owner = None
            current_target = None
            continue
        if current_so is None:
            continue
        if stripped.startswith("Owner Module:"):
            current_owner = stripped.split(":", 1)[1].strip()
            continue
        if "target.object =" in stripped:
            current_target = stripped.split("=", 1)[1].strip().strip('"')
            continue

    flush()
    return ids


def move_source_outputs(engine, from_source, to_source, exclude_modules=None):
    """Move every source-output currently capturing from `from_source`."""
    if not from_source or not to_source or from_source == to_source:
        return
    candidates = _resolved_source_candidates(engine, to_source)
    if not candidates:
        logging.warning(
            f"Destination source {to_source} never became visible; "
            f"skipping move-source-output from {from_source}"
        )
        return
    for source_output_id in engine._list_source_outputs_on(
        from_source,
        exclude_modules=exclude_modules,
    ):
        engine._move_source_output_with_retry(
            source_output_id,
            from_source,
            candidates[0],
        )


def source_output_locations(engine):
    """Return {source_output_id: source_name} from short pactl state."""
    short = engine._run(["pactl", "list", "short", "source-outputs"])
    if not short:
        return {}
    id_to_name = engine._source_id_to_name()
    locations = {}
    for line in short.splitlines():
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        so_id = parts[0].strip()
        src_id = parts[1].strip()
        if not so_id:
            continue
        locations[so_id] = id_to_name.get(src_id)
    return locations


def snapshot_external_source_outputs(engine, source_name, exclude_modules=None):
    """Capture the current external source-outputs on `source_name`."""
    return list(
        engine._list_source_outputs_on(
            source_name,
            exclude_modules=exclude_modules,
        )
    )


def move_known_source_outputs(
    engine,
    source_output_ids,
    from_source,
    to_source,
    attempts=20,
    delay=0.05,
):
    """Move a known set of source-output ids and verify they rebind."""
    if not source_output_ids:
        return True
    if not from_source or not to_source or from_source == to_source:
        return True
    candidates = _resolved_source_candidates(
        engine,
        to_source,
        attempts=attempts,
        delay=delay,
    )
    if not candidates:
        logging.warning(
            f"Destination source {to_source} never became visible; "
            f"skipping targeted move-source-output from {from_source}"
        )
        return False
    wanted = {str(so_id).strip() for so_id in source_output_ids if str(so_id).strip()}
    for so_id in wanted:
        moved = False
        for _ in range(max(1, int(attempts))):
            for candidate in candidates:
                engine._run(["pactl", "move-source-output", so_id, candidate])
                current = engine._source_output_locations().get(so_id)
                if current is None or _source_name_matches(engine, current, candidate):
                    moved = True
                    break
            if moved:
                break
            time.sleep(max(0.0, float(delay)))
            refreshed = _resolved_source_candidates(
                engine,
                to_source,
                attempts=1,
                delay=delay,
            )
            if refreshed:
                candidates = refreshed
        if not moved:
            return False
    return True


def wait_source_visible(engine, source_name, attempts=20, delay=0.05):
    if not source_name:
        return False
    for _ in range(max(1, int(attempts))):
        resolved = engine.resolve_source_name(source_name)
        if resolved:
            return resolved
        time.sleep(max(0.0, float(delay)))
    return False


def move_source_output_with_retry(
    engine,
    source_output_id,
    from_source,
    to_source,
    attempts=20,
    delay=0.05,
):
    source_output_id = str(source_output_id).strip()
    if not source_output_id:
        return False
    candidates = _resolved_source_candidates(
        engine,
        to_source,
        attempts=attempts,
        delay=delay,
    )
    if not candidates:
        return False
    for _ in range(max(1, int(attempts))):
        for candidate in candidates:
            engine._run(["pactl", "move-source-output", source_output_id, candidate])
            current = engine._source_output_locations().get(source_output_id)
            if current is None or _source_name_matches(engine, current, candidate):
                return True
        time.sleep(max(0.0, float(delay)))
        refreshed = _resolved_source_candidates(
            engine,
            to_source,
            attempts=1,
            delay=delay,
        )
        if refreshed:
            candidates = refreshed
    logging.warning(
        f"Source output {source_output_id} stayed on {from_source} "
        f"after move attempt to {to_source}"
    )
    return False


def snapshot_submix_bindings(engine, source_name):
    """Capture current submix loopbacks reading from `source_name`."""
    bindings = {}
    for key, current_source in list(engine.submix_sources.items()):
        if current_source != source_name:
            continue
        _, _, mix_name = key.partition("->")
        bindings[key] = {
            "mix_name": mix_name,
            "module_id": engine.submix_loopbacks.get(key),
            "state": dict(engine.submix_state_cache.get(key, {}) or {}),
        }
    return bindings
