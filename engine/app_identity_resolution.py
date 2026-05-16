"""Candidate generation and resolution for app identity."""

from __future__ import annotations

import os


def window_identity_candidates(engine, current):
    candidates = []
    for key in engine._WINDOW_IDENTITY_KEYS:
        raw = current.get(key)
        if not raw:
            continue
        if key in {"application.id", "pipewire.access.portal.app_id", "xdg.portal.app_id"}:
            base_score = 116
        elif key == "window.app_id":
            base_score = 108
        else:
            base_score = 96
        for token in engine._split_identity_tokens(raw):
            lowered = token.lower()
            if "." in token:
                candidates.append(engine._candidate_from_raw("app", token, engine._canonicalize_app_id(token), base_score, key))
            mapped = engine._BINARY_DISPLAY_NAMES.get(lowered)
            if mapped:
                candidates.append(engine._candidate_from_raw("binary", token, mapped, base_score - 2, key))
            elif not engine._is_generic_name(token) and not engine.name_matches_host(token):
                candidates.append(engine._candidate_from_raw("wmclass", token, engine._sanitize_app_label(token), base_score - 10, key))
    return [candidate for candidate in candidates if candidate]


def binary_identity_candidates(engine, pid, current):
    candidates = []
    current_binary = (current.get("application.process.binary") or "").strip()
    if current_binary:
        mapped = engine._BINARY_DISPLAY_NAMES.get(current_binary.lower())
        if mapped:
            candidates.append(engine._candidate_from_raw("binary", current_binary, mapped, 88, "application.process.binary"))
        elif not engine._is_generic_name(current_binary):
            candidates.append(engine._candidate_from_raw("binary", current_binary, current_binary, 70, "application.process.binary"))

    index = engine._desktop_app_index() or {}
    wrapper_set = engine._EXEC_WRAPPERS | {"bwrap", "python", "python3", "flatpak", "snap", "snap-confine"}
    for depth, cur in enumerate(engine._pid_lineage(pid)):
        raw_candidates = [
            engine._proc_exe_basename(cur),
            engine._proc_comm(cur),
        ]
        cmdline = engine._read_proc_cmdline(cur)
        if cmdline:
            raw_candidates.append(os.path.basename(cmdline[0]))
        seen = set()
        for raw in raw_candidates:
            if not raw:
                continue
            lowered = raw.lower()
            if lowered in seen or lowered in wrapper_set:
                continue
            seen.add(lowered)
            mapped = engine._BINARY_DISPLAY_NAMES.get(lowered)
            score_penalty = depth * 2
            if lowered in engine._MULTIPROCESS_CHILD_BINARIES:
                score_penalty += 22
            if lowered in index:
                candidates.append(engine._candidate_from_raw("binary", lowered, index[lowered], 104 - score_penalty, f"desktop-index:{depth}"))
            if mapped:
                candidates.append(engine._candidate_from_raw("binary", lowered, mapped, 100 - score_penalty, f"binary-map:{depth}"))
            elif not engine._is_generic_name(raw):
                candidates.append(engine._candidate_from_raw("binary", lowered, raw, 74 - score_penalty, f"binary:{depth}"))
    return [candidate for candidate in candidates if candidate]


def cmdline_identity_candidates(engine, pid):
    candidates = []
    if not pid:
        return candidates
    index = engine._desktop_app_index() or {}
    wrapper_set = engine._EXEC_WRAPPERS | {"bwrap", "python", "python3", "flatpak", "snap", "snap-confine"}
    for depth, cur in enumerate(engine._pid_lineage(pid)):
        cmdline = engine._read_proc_cmdline(cur)
        if not cmdline:
            continue
        seen = set()
        score_penalty = depth * 2
        for token in cmdline:
            if not token:
                continue
            if token.startswith("--") and "=" in token:
                flag, raw_value = token.split("=", 1)
                value = raw_value.strip()
                if not value:
                    continue
                low_flag = flag.lower()
                if low_flag in {"--class", "--name"} and not engine._is_generic_name(value):
                    candidates.append(
                        engine._candidate_from_raw(
                            "wmclass" if low_flag == "--class" else "name",
                            value,
                            engine._sanitize_app_label(value),
                            106 - score_penalty,
                            f"cmdline:{low_flag}",
                        )
                    )
                    continue
                if low_flag in {"--app", "--app-id"} and ("." in value or value.lower() in engine._KNOWN_APP_IDS):
                    candidates.append(
                        engine._candidate_from_raw(
                            "app",
                            value,
                            engine._canonicalize_app_id(value),
                            112 - score_penalty,
                            f"cmdline:{low_flag}",
                        )
                    )
                    continue
            if token.startswith("-"):
                continue
            if "." in token and token.lower() in engine._KNOWN_APP_IDS:
                candidates.append(engine._candidate_from_raw("app", token, engine._canonicalize_app_id(token), 112 - score_penalty, "cmdline-app-id"))
            base = os.path.basename(token).strip().lower()
            if not base or base in seen or base in wrapper_set:
                continue
            seen.add(base)
            if base in index:
                candidates.append(engine._candidate_from_raw("binary", base, index[base], 108 - score_penalty, f"cmdline-index:{depth}"))
            mapped = engine._BINARY_DISPLAY_NAMES.get(base)
            if mapped:
                child_penalty = 18 if base in engine._MULTIPROCESS_CHILD_BINARIES else 0
                candidates.append(engine._candidate_from_raw("binary", base, mapped, 106 - score_penalty - child_penalty, f"cmdline-binary:{depth}"))
            elif not engine._is_generic_name(base):
                candidates.append(engine._candidate_from_raw("binary", base, base, 78 - score_penalty, f"cmdline:{depth}"))
    return [candidate for candidate in candidates if candidate]


def text_identity_candidates(engine, current):
    candidates = []
    for key in engine._TEXT_IDENTITY_KEYS:
        raw = (current.get(key) or "").strip()
        if not raw:
            continue
        if engine.name_matches_host(raw):
            continue
        if key == "application.name":
            mapped = engine._BINARY_DISPLAY_NAMES.get(raw.lower())
            if mapped:
                candidates.append(engine._candidate_from_raw("name", raw, mapped, 89 if not engine._is_generic_name(raw) else 68, key))
        if engine._is_generic_name(raw):
            continue
        if key == "application.display_name":
            score = 92
        elif key == "application.name":
            score = 89
        elif key.startswith("application."):
            score = 72
        else:
            score = 56
        candidates.append(engine._candidate_from_raw("name", raw, engine._sanitize_app_label(raw), score, key))
    title_score = 90 if engine._generic_title_context(current) else 72
    for key in engine._WINDOW_TITLE_KEYS:
        raw = (current.get(key) or "").strip()
        if not raw:
            continue
        label = engine._window_title_identity_label(raw)
        if not label:
            continue
        candidate = engine._stream_identity_candidate(current, label, title_score - (6 if key == "media.title" else 0), key)
        if candidate:
            candidates.append(candidate)
    return [candidate for candidate in candidates if candidate]


def stream_fallback_identity(engine, current):
    stream_id = str(current.get("node.id") or current.get("index") or current.get("pid") or "?")
    display = None
    for key in ("application.name", "node.description", "node.name", "media.name"):
        raw = (current.get(key) or "").strip()
        if raw and not engine.name_matches_host(raw):
            display = engine._sanitize_app_label(raw)
            break
    if not display:
        display = f"Audio Stream #{stream_id}"
    return {
        "app_id": engine._make_app_route_key("stream", stream_id),
        "app_name": display,
        "resolved_app_id": engine._make_app_route_key("stream", stream_id),
        "resolved_app_name": display,
        "source": "fallback",
        "override_applied": False,
    }


def candidate_source_preference(candidate):
    source = str((candidate or {}).get("source") or "")
    if source in {"gio-desktop", "flatpak", "snap-env", "flatpak-cgroup", "snap-cgroup"}:
        return 5
    if source.startswith(("cmdline:--app", "cmdline:--app-id")):
        return 5
    if source.startswith(("desktop-index:", "cmdline-index:", "cmdline-binary:", "appimage")):
        return 4
    if source == "exe-path" or source.startswith(("flatpak-cmdline", "gtk_application_id", "app_id", "xdg_current_desktop_app")):
        return 4
    if source.startswith(("application.id", "pipewire.access.portal.app_id", "xdg.portal.app_id", "window.app_id")):
        return 4
    if source.startswith(("window.x11.", "window.class", "wmclass")):
        return 3
    if source.startswith(("application.display_name", "application.name", "node.description", "node.nick")):
        return 2
    if source.startswith(("application.process.binary", "binary-map:", "binary:", "name")):
        return 1
    return 0


def prefer_wrapper_identity_candidate(engine, candidates, best):
    if not best:
        return best
    best_source_pref = engine._candidate_source_preference(best)
    best_is_generic = engine._is_generic_name(best.get("app_name"))
    alternatives = [
        candidate
        for candidate in candidates
        if candidate
        and not engine._is_generic_name(candidate.get("app_name"))
        and not engine.name_matches_host(candidate.get("app_name"))
        and engine._candidate_source_preference(candidate) >= 3
    ]
    if not alternatives:
        return best
    alternative = max(
        alternatives,
        key=lambda item: (
            engine._candidate_source_preference(item),
            item["score"],
            len(item["app_name"]),
        ),
    )
    alt_source_pref = engine._candidate_source_preference(alternative)
    if alt_source_pref > best_source_pref and alternative["score"] >= best["score"] - 18:
        return alternative
    if best_is_generic and alt_source_pref >= best_source_pref and alternative["score"] >= best["score"] - 18:
        return alternative
    return best


def prefer_specific_identity_candidate(engine, candidates, best):
    if not best or not engine._is_generic_name(best.get("app_name")):
        return best
    alternatives = [
        candidate
        for candidate in candidates
        if candidate
        and not engine._is_generic_name(candidate.get("app_name"))
        and not engine.name_matches_host(candidate.get("app_name"))
    ]
    if not alternatives:
        return best
    alternative = max(alternatives, key=lambda item: (item["score"], len(item["app_name"])))
    if alternative["score"] >= best["score"] - 14:
        return alternative
    return best


def is_lineage_fallback_identity_source(source):
    source = str(source or "")
    if source in {"binary", "exe-path", "application.process.binary"}:
        return True
    return source.startswith((
        "cmdline:",
        "cmdline-index:",
        "cmdline-binary:",
        "binary:",
        "binary-map:",
        "desktop-index:",
        "appimage",
    ))


def prefer_explicit_stream_identity_candidate(engine, current, candidates, best):
    if not best or not engine._is_lineage_fallback_identity_source(best.get("source")):
        return best
    explicit_candidates = []
    for key in ("application.display_name", "application.name"):
        raw = (current.get(key) or "").strip()
        if not raw or engine._is_generic_name(raw) or engine.name_matches_host(raw):
            continue
        label = engine._sanitize_app_label(raw)
        if not label:
            continue
        coherence = 0
        for match_key in ("media.name", "node.name", "node.description"):
            match_raw = (current.get(match_key) or "").strip()
            if match_raw and engine._sanitize_app_label(match_raw) == label:
                coherence += 1
        for candidate in candidates:
            if not candidate or candidate.get("source") != key:
                continue
            if candidate.get("app_name") != label:
                continue
            explicit_candidates.append((coherence, candidate))
    if not explicit_candidates:
        return best
    coherence, explicit = max(
        explicit_candidates,
        key=lambda item: (item[0], item[1]["score"], len(item[1]["app_name"])),
    )
    if coherence <= 0 and explicit.get("source") != "application.display_name":
        return best
    if explicit["score"] >= best["score"] - 12:
        return explicit
    return best


def apply_identity_override(engine, identity):
    if not isinstance(identity, dict):
        return identity
    resolved_app_id = str(identity.get("app_id") or "").strip()
    resolved_app_name = str(identity.get("app_name") or "").strip()
    source = str(identity.get("source") or "").strip()
    if not resolved_app_id:
        return identity
    target_app_id = getattr(engine, "_app_identity_overrides", {}).get(resolved_app_id, resolved_app_id)
    override_applied = target_app_id != resolved_app_id
    if override_applied:
        display_name = engine._override_display_name_for_app_id(target_app_id)
    else:
        display_name = engine._override_display_name_for_app_id(target_app_id, fallback=resolved_app_name)
    return {
        "app_id": target_app_id,
        "app_name": display_name or resolved_app_name or engine.display_name_for_app_id(target_app_id),
        "resolved_app_id": resolved_app_id,
        "resolved_app_name": resolved_app_name or engine.display_name_for_app_id(resolved_app_id),
        "source": source,
        "override_applied": override_applied,
    }


def resolve_app_identity(engine, current):
    if engine._is_system_sound_stream(current):
        return {
            "app_id": engine.SYSTEM_SOUNDS_BUCKET,
            "app_name": engine.SYSTEM_SOUNDS_BUCKET,
            "resolved_app_id": engine.SYSTEM_SOUNDS_BUCKET,
            "resolved_app_name": engine.SYSTEM_SOUNDS_BUCKET,
            "source": "system-sounds",
            "override_applied": False,
        }

    pid = current.get("pid") or current.get("application.process.id")
    candidates = []
    for candidate in (
        engine._gio_identity_candidate(pid),
        engine._sandbox_identity_candidate(pid),
        engine._path_identity_candidate(pid),
    ):
        if candidate:
            candidates.append(candidate)
    candidates.extend(engine._cmdline_identity_candidates(pid))
    candidates.extend(engine._window_identity_candidates(current))
    candidates.extend(engine._binary_identity_candidates(pid, current))
    candidates.extend(engine._text_identity_candidates(current))

    best_by_id = {}
    for candidate in candidates:
        if not candidate:
            continue
        key = candidate["app_id"]
        existing = best_by_id.get(key)
        rank = (candidate["score"], len(candidate["app_name"]))
        if existing is None or rank > (existing["score"], len(existing["app_name"])):
            best_by_id[key] = candidate

    if not best_by_id:
        return engine._stream_fallback_identity(current)

    best = max(best_by_id.values(), key=lambda item: (item["score"], len(item["app_name"])))
    best = engine._prefer_wrapper_identity_candidate(candidates, best)
    best = engine._prefer_explicit_stream_identity_candidate(current, candidates, best)
    best = engine._prefer_specific_identity_candidate(candidates, best)
    return engine._apply_identity_override(
        {
            "app_id": best["app_id"],
            "app_name": best["app_name"],
            "source": best["source"],
        }
    )


def process_sink_input(engine, current, entries, sink_id_to_name):
    sink_id = current.get("sink_id")
    current["sink"] = sink_id_to_name.get(sink_id, sink_id)

    node_name = (current.get("node.name") or "").lower()
    media_name = (current.get("media.name") or "").lower()
    if any(token in node_name for token in ("wavelinux_mix", "wavelinux_src", "wavelinux.fx", "rnnoise", "loopback")):
        return
    if "wavelinux_mix" in media_name:
        return

    identity = engine._resolve_app_identity(current)
    current["app_id"] = identity["app_id"]
    current["app_name"] = identity["app_name"] or "Unknown App"
    current["resolved_app_id"] = identity.get("resolved_app_id") or current["app_id"]
    current["resolved_app_name"] = identity.get("resolved_app_name") or current["app_name"]
    current["app_identity_source"] = identity.get("source", "")
    current["app_identity_override_applied"] = bool(identity.get("override_applied"))
    current["app_icon_candidates"] = engine._app_icon_candidates(
        current,
        app_id=current["app_id"],
        resolved_app_id=current["resolved_app_id"],
        app_name=current["app_name"],
        resolved_app_name=current["resolved_app_name"],
    )
    current["_is_system_sound"] = current["app_id"] == engine.SYSTEM_SOUNDS_BUCKET
    entries.append(current)
