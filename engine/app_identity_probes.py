"""Process, desktop, and sandbox probing helpers for app identity."""

from __future__ import annotations

import os
import re
import shlex
import time


def read_proc_cmdline(pid):
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as handle:
            raw = handle.read()
    except OSError:
        return []
    return [part.decode("utf-8", "replace") for part in raw.split(b"\x00") if part]


def read_proc_env(pid):
    try:
        with open(f"/proc/{pid}/environ", "rb") as handle:
            raw = handle.read()
    except OSError:
        return {}
    env = {}
    for entry in raw.split(b"\x00"):
        if b"=" in entry:
            key, value = entry.split(b"=", 1)
            try:
                env[key.decode("utf-8", "replace")] = value.decode("utf-8", "replace")
            except Exception:
                continue
    return env


def read_proc_cgroup(pid):
    try:
        with open(f"/proc/{pid}/cgroup", "r") as handle:
            return handle.read()
    except OSError:
        return ""


def identify_sandboxed_app(engine, pid):
    if not pid:
        return None
    env = engine._read_proc_env(pid)

    flatpak_id = env.get("FLATPAK_ID")
    if not flatpak_id:
        try:
            with open(f"/proc/{pid}/root/.flatpak-info", "r") as handle:
                for line in handle:
                    line = line.strip()
                    if line.startswith("name=") or line.startswith("application="):
                        flatpak_id = line.split("=", 1)[1].strip()
                        break
        except OSError:
            pass
    if flatpak_id:
        return engine._canonicalize_app_id(flatpak_id)

    snap_name = env.get("SNAP_INSTANCE_NAME") or env.get("SNAP_NAME")
    if snap_name:
        return snap_name.replace("-", " ").replace("_", " ").title()

    cgroup = engine._read_proc_cgroup(pid)
    match = re.search(r"app-flatpak-([A-Za-z0-9_.+-]+?)-\d+\.scope", cgroup)
    if match:
        return engine._canonicalize_app_id(match.group(1))
    match = re.search(r"snap\.([A-Za-z0-9_-]+)", cgroup)
    if match:
        return match.group(1).replace("-", " ").replace("_", " ").title()
    match = re.search(r"app-([A-Za-z0-9_.+-]+?)\.slice", cgroup)
    if match:
        return engine._canonicalize_app_id(match.group(1))

    for env_key in ("GTK_APPLICATION_ID", "APP_ID", "XDG_CURRENT_DESKTOP_APP"):
        value = env.get(env_key)
        if value:
            return engine._canonicalize_app_id(value)

    cmdline = engine._read_proc_cmdline(pid)
    if cmdline:
        first = cmdline[0]
        match = re.search(r"/tmp/\.mount_([^/]+)", first)
        if match:
            stripped = re.sub(r"[A-Za-z0-9]{4,8}$", "", match.group(1)).rstrip("_-.")
            if stripped:
                return stripped.replace("_", " ").replace("-", " ").title()

    return None


def desktop_app_index(engine_cls):
    now = time.time()
    cache = getattr(engine_cls, "_desktop_cache", None)
    cache_at = getattr(engine_cls, "_desktop_cache_at", 0)
    if cache is not None and (now - cache_at) < 60:
        return cache

    roots = [
        "/usr/share/applications",
        "/usr/local/share/applications",
        os.path.expanduser("~/.local/share/applications"),
        "/var/lib/flatpak/exports/share/applications",
        os.path.expanduser("~/.local/share/flatpak/exports/share/applications"),
    ]
    index = {}
    for root in roots:
        try:
            entries = os.listdir(root)
        except OSError:
            continue
        for entry in entries:
            if not entry.endswith(".desktop"):
                continue
            path = os.path.join(root, entry)
            name, exec_line, no_display = engine_cls._parse_desktop_file(path)
            if no_display or not name or not exec_line:
                continue
            bin_name = engine_cls._resolve_exec_binary(exec_line)
            if bin_name and bin_name.lower() not in index:
                index[bin_name.lower()] = name
    engine_cls._desktop_cache = index
    engine_cls._desktop_cache_at = now
    return index


def parse_desktop_file(path):
    name = None
    exec_line = None
    no_display = False
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            in_main = False
            for line in handle:
                stripped = line.strip()
                if stripped.startswith("[") and stripped.endswith("]"):
                    in_main = stripped == "[Desktop Entry]"
                    continue
                if not in_main:
                    continue
                if stripped.startswith("Name=") and name is None:
                    name = stripped.split("=", 1)[1].strip() or None
                elif stripped.startswith("Exec=") and exec_line is None:
                    exec_line = stripped.split("=", 1)[1].strip() or None
                elif stripped.startswith("NoDisplay="):
                    no_display = stripped.split("=", 1)[1].strip().lower() == "true"
    except OSError:
        return None, None, False
    return name, exec_line, no_display


def resolve_exec_binary(engine_cls, exec_line):
    cleaned = re.sub(r"%[a-zA-Z]", "", exec_line).strip()
    if not cleaned:
        return None
    try:
        tokens = shlex.split(cleaned, posix=True)
    except ValueError:
        tokens = cleaned.split()
    index = 0
    while index < len(tokens):
        token = tokens[index]
        base = os.path.basename(token).lower()
        if base in engine_cls._EXEC_WRAPPERS:
            index += 1
            while index < len(tokens) and (
                tokens[index].startswith("-") or "=" in tokens[index]
            ):
                index += 1
            continue
        return os.path.basename(token)
    return None


def infer_name_from_exe(engine, pid, current_name=None):
    if not pid:
        return None
    try:
        exe = os.readlink(f"/proc/{pid}/exe")
    except OSError:
        exe = ""
    cmdline = engine._read_proc_cmdline(pid)
    haystacks = [exe]
    if cmdline:
        haystacks.extend(cmdline)
    for haystack in haystacks:
        if not haystack:
            continue
        for pattern in engine._PATH_TITLE_PATTERNS:
            match = pattern.search(haystack)
            if match:
                title = match.group(1).strip()
                if not title or title.startswith("."):
                    continue
                if current_name and title.lower() == current_name.lower():
                    continue
                return title
    return None


def identify_via_desktop(engine, pid):
    if not pid:
        return None
    index = engine._desktop_app_index() or {}
    candidates = []
    try:
        exe = os.readlink(f"/proc/{pid}/exe")
        if exe:
            candidates.append(os.path.basename(exe).lower())
    except OSError:
        pass
    try:
        with open(f"/proc/{pid}/comm", "r") as handle:
            comm = handle.read().strip().lower()
            if comm:
                candidates.append(comm)
    except OSError:
        pass
    cmdline = engine._read_proc_cmdline(pid)
    if cmdline:
        candidates.append(os.path.basename(cmdline[0]).lower())
    for candidate in candidates:
        if candidate in engine._BINARY_DISPLAY_NAMES:
            return engine._BINARY_DISPLAY_NAMES[candidate]
        if candidate in index:
            return index[candidate]
    return None


def proc_exe_basename(pid):
    if not pid:
        return ""
    try:
        exe = os.readlink(f"/proc/{pid}/exe")
    except OSError:
        return ""
    return os.path.basename(exe) if exe else ""


def proc_comm(pid):
    if not pid:
        return ""
    try:
        with open(f"/proc/{pid}/comm", "r") as handle:
            return handle.read().strip()
    except OSError:
        return ""


def parent_pid(pid):
    try:
        with open(f"/proc/{pid}/status", "r") as handle:
            for line in handle:
                if line.startswith("PPid:"):
                    return line.split()[1]
    except OSError:
        return None
    return None


def pid_lineage(engine, pid, limit=10):
    if not pid:
        return []
    out = []
    seen = set()
    cur = str(pid)
    for _ in range(limit):
        if not cur or cur in seen or cur == "0":
            break
        seen.add(cur)
        out.append(cur)
        cur = engine._parent_pid(cur)
    return out


def split_identity_tokens(raw):
    if raw is None:
        return []
    text = str(raw).strip()
    if not text:
        return []
    parts = [text]
    parts.extend(part.strip() for part in re.split(r"[;,]", text) if part.strip())
    out = []
    seen = set()
    for part in parts:
        lowered = part.lower()
        if lowered in seen:
            continue
        seen.add(lowered)
        out.append(part)
    return out


def identity_candidate(app_id, display_name, score, source):
    if not app_id or not display_name:
        return None
    return {
        "app_id": app_id,
        "app_name": display_name,
        "score": int(score),
        "source": source,
    }


def candidate_from_raw(engine_cls, prefix, raw_value, display_name, score, source):
    app_id = engine_cls._make_app_route_key(prefix, raw_value)
    label = display_name or engine_cls.display_name_for_app_id(app_id)
    return engine_cls._identity_candidate(app_id, label, score, source)


def stream_identity_candidate(engine, current, display_name, score, source):
    stream_id = (
        current.get("node.id")
        or current.get("index")
        or current.get("pid")
        or current.get("application.process.id")
    )
    if not stream_id:
        return None
    return engine._identity_candidate(
        engine._make_app_route_key("stream", stream_id),
        display_name,
        score,
        source,
    )


def window_title_identity_label(engine, raw):
    title = str(raw).strip()
    if not title:
        return None
    lowered = title.lower()
    if re.search(r"https?://|www\.|[a-z0-9-]+\.(?:com|org|net|io|gg|tv)\b", lowered):
        return None
    if any(sep in title for sep in (" - ", " | ", " — ", " :: ", " • ")):
        return None
    label = engine._sanitize_app_label(title)
    if not label or engine._is_generic_name(label) or engine.name_matches_host(label):
        return None
    if len(label) > 80:
        return None
    return label


def generic_title_context(engine, current):
    for key in ("application.id", "window.app_id", "application.display_name", "application.name"):
        raw = (current.get(key) or "").strip()
        if raw and not engine._is_generic_name(raw) and not engine.name_matches_host(raw):
            return False
    for key in ("application.process.binary", "node.name", "media.name"):
        raw = (current.get(key) or "").strip().lower()
        if not raw:
            continue
        norm = engine._normalize_app_name(raw)
        if (
            engine._is_generic_name(raw)
            or raw in engine._MULTIPROCESS_CHILD_BINARIES
            or raw in {"chrome", "chromium", "chromium-browser", "electron", "wine", "wine64", "launcher", "helper"}
            or norm in {"chrome", "chromium", "electron", "renderer", "utility", "plugin host", "audio stream", "unknown"}
        ):
            return True
    return False


def app_name_from_pid(engine, pid):
    if not pid:
        return None
    exe_base = engine._proc_exe_basename(pid)
    try:
        with open(f"/proc/{pid}/comm", "r") as handle:
            comm = handle.read().strip()
    except OSError:
        comm = ""

    wrapper_set = {
        "bwrap", "flatpak", "snap", "snap-confine", "bash", "sh",
        "python", "python3", "wine", "wine64", "wineserver",
    }

    for candidate in (exe_base, comm):
        lowered = candidate.lower() if candidate else ""
        if lowered and lowered in engine._BINARY_DISPLAY_NAMES:
            return engine._BINARY_DISPLAY_NAMES[lowered]

    if exe_base and exe_base.lower() not in wrapper_set:
        return exe_base
    if comm and comm.lower() not in wrapper_set:
        return comm

    seen = set()
    cur = pid
    for _ in range(6):
        try:
            with open(f"/proc/{cur}/status", "r") as handle:
                ppid = None
                for line in handle:
                    if line.startswith("PPid:"):
                        ppid = line.split()[1]
                        break
        except OSError:
            return comm or exe_base or None
        if not ppid or ppid in seen or ppid == "0":
            return comm or exe_base or None
        seen.add(ppid)
        parent_exe = engine._proc_exe_basename(ppid)
        try:
            with open(f"/proc/{ppid}/comm", "r") as handle:
                parent_comm = handle.read().strip()
        except OSError:
            parent_comm = ""
        for candidate in (parent_exe, parent_comm):
            lowered = candidate.lower() if candidate else ""
            if lowered and lowered in engine._BINARY_DISPLAY_NAMES:
                return engine._BINARY_DISPLAY_NAMES[lowered]
        if parent_exe and parent_exe.lower() not in wrapper_set:
            return parent_exe
        if parent_comm and parent_comm.lower() not in wrapper_set:
            return parent_comm
        cur = ppid
    return comm or exe_base or None


def is_system_sound_stream(current):
    media_role = (current.get("media.role") or "").lower()
    if media_role in ("event", "notification", "phone-notification", "phone", "alert", "production"):
        return True
    binary = (current.get("application.process.binary") or "").lower()
    if binary in {"canberra-gtk-play", "canberra-gtk-module", "paplay", "aplay", "speaker-test", "notify-send", "kdialog", "kdedialog", "plasma-pa"}:
        return True
    app_name = (current.get("application.name") or "").lower()
    if app_name in {"libcanberra", "canberra", "plasma-pa", "speech-dispatcher", "org.freedesktop.notifications", "plasma-pulseaudio", "plasmashell", "kded", "kded5", "kded6", "org.kde.plasmashell", "org.kde.kded"}:
        return True
    node_name = (current.get("node.name") or "").lower()
    if "canberra" in node_name or "notification" in node_name:
        return True
    return False


def resolve_via_gio_env(engine, pid):
    if not pid:
        return None
    seen = set()
    cur = str(pid)
    for _ in range(10):
        if cur in seen or cur in ("0", ""):
            break
        seen.add(cur)
        env = engine._read_proc_env(cur)
        desktop_path = env.get("GIO_LAUNCHED_DESKTOP_FILE")
        if desktop_path and os.path.isfile(desktop_path):
            name, _exec, _hidden = engine._parse_desktop_file(desktop_path)
            if name:
                return name
        try:
            with open(f"/proc/{cur}/status", "r") as handle:
                ppid = None
                for line in handle:
                    if line.startswith("PPid:"):
                        ppid = line.split()[1]
                        break
        except OSError:
            return None
        if not ppid or ppid == "0":
            break
        cur = ppid
    return None


def gio_identity_candidate(engine, pid):
    for depth, cur in enumerate(engine._pid_lineage(pid)):
        env = engine._read_proc_env(cur)
        desktop_path = env.get("GIO_LAUNCHED_DESKTOP_FILE")
        if not desktop_path or not os.path.isfile(desktop_path):
            continue
        name, _exec, _hidden = engine._parse_desktop_file(desktop_path)
        if not name:
            continue
        desktop_id = os.path.splitext(os.path.basename(desktop_path))[0]
        return engine._candidate_from_raw("desktop", desktop_id, name, 130 - depth, "gio-desktop")
    return None


def sandbox_identity_candidate(engine, pid):
    if not pid:
        return None
    for depth, cur in enumerate(engine._pid_lineage(pid)):
        env = engine._read_proc_env(cur)
        flatpak_id = env.get("FLATPAK_ID")
        if not flatpak_id:
            try:
                with open(f"/proc/{cur}/root/.flatpak-info", "r") as handle:
                    for line in handle:
                        line = line.strip()
                        if line.startswith("name=") or line.startswith("application="):
                            flatpak_id = line.split("=", 1)[1].strip()
                            break
            except OSError:
                pass
        if flatpak_id:
            return engine._candidate_from_raw(
                "app",
                flatpak_id,
                engine._canonicalize_app_id(flatpak_id),
                126 - depth,
                "flatpak",
            )

        snap_name = env.get("SNAP_INSTANCE_NAME") or env.get("SNAP_NAME")
        if snap_name:
            return engine._candidate_from_raw(
                "snap",
                snap_name,
                snap_name.replace("-", " ").replace("_", " ").title(),
                122 - depth,
                "snap-env",
            )

        cgroup = engine._read_proc_cgroup(cur)
        matchers = (
            (r"app-flatpak-([A-Za-z0-9_.+-]+?)-\d+\.scope", "app", engine._canonicalize_app_id, 124, "flatpak-cgroup"),
            (r"app-([A-Za-z0-9_.+-]+?)\.slice", "app", engine._canonicalize_app_id, 116, "app-slice"),
            (r"snap\.([A-Za-z0-9_-]+)", "snap", lambda value: value.replace("-", " ").replace("_", " ").title(), 114, "snap-cgroup"),
        )
        for pattern, prefix, display_fn, score, source in matchers:
            match = re.search(pattern, cgroup)
            if not match:
                continue
            raw = match.group(1)
            return engine._candidate_from_raw(prefix, raw, display_fn(raw), score - depth, source)

        for env_key in ("GTK_APPLICATION_ID", "APP_ID", "XDG_CURRENT_DESKTOP_APP"):
            raw = env.get(env_key)
            if raw:
                return engine._candidate_from_raw(
                    "app",
                    raw,
                    engine._canonicalize_app_id(raw),
                    118 - depth,
                    env_key.lower(),
                )

        cmdline = engine._read_proc_cmdline(cur)
        if cmdline:
            for index, token in enumerate(cmdline[:-1]):
                if os.path.basename(token).lower() == "flatpak":
                    candidate = cmdline[index + 1]
                    if candidate == "run" and index + 2 < len(cmdline):
                        candidate = cmdline[index + 2]
                    if "." in candidate and not candidate.startswith("-"):
                        return engine._candidate_from_raw(
                            "app",
                            candidate,
                            engine._canonicalize_app_id(candidate),
                            112 - depth,
                            "flatpak-cmdline",
                        )
            mount_match = re.search(r"/tmp/\.mount_([^/]+)", cmdline[0])
            if mount_match:
                stripped = re.sub(r"[A-Za-z0-9]{4,8}$", "", mount_match.group(1)).rstrip("_-.")
                if stripped:
                    return engine._candidate_from_raw(
                        "path",
                        stripped,
                        stripped.replace("_", " ").replace("-", " ").title(),
                        92 - depth,
                        "appimage",
                    )
    return None


def path_identity_candidate(engine, pid):
    for depth, cur in enumerate(engine._pid_lineage(pid)):
        current_name = engine._proc_exe_basename(cur) or engine._proc_comm(cur)
        title = engine._infer_name_from_exe(cur, current_name=current_name)
        if title and not engine._is_generic_name(title) and not engine.name_matches_host(title):
            return engine._candidate_from_raw("path", title, title, 94 - depth, "exe-path")
    return None
