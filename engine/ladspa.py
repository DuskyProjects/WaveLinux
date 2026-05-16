"""LADSPA path and plugin helpers for the PipeWire engine."""

from __future__ import annotations

import os


EFFECT_REQUIREMENTS = {
    "rnnoise": ("librnnoise_ladspa",),
    "compressor": ("sc4m_1916",),
    "gate": ("gate_1410",),
    "highpass": (),
    "eq": (),
    "limiter": (),
}


def env_flag_enabled(name, *, environ=None):
    env = os.environ if environ is None else environ
    return env.get(name, "").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }


def bundled_ladspa_entries(*, environ=None):
    env = os.environ if environ is None else environ
    return [
        path for path in env.get("WAVELINUX_BUNDLED_LADSPA_PATH", "").split(":") if path
    ]


def ladspa_env_entries(*, environ=None):
    env = os.environ if environ is None else environ
    env_entries = [path for path in env.get("LADSPA_PATH", "").split(":") if path]
    bundled_entries = bundled_ladspa_entries(environ=env)
    bundled_keys = {os.path.normpath(path) for path in bundled_entries}
    env_entries = [
        path for path in env_entries
        if os.path.normpath(path) not in bundled_keys
    ]

    deduped = []
    seen = set()
    for path in env_entries:
        key = os.path.normpath(path)
        if key in seen:
            continue
        deduped.append(path)
        seen.add(key)
    return deduped


def ladspa_roots(default_paths, *, environ=None):
    roots = []
    roots.extend(ladspa_env_entries(environ=environ))
    roots.extend(default_paths)
    env = os.environ if environ is None else environ
    if env_flag_enabled("WAVELINUX_ENABLE_BUNDLED_LADSPA", environ=env):
        roots.extend(bundled_ladspa_entries(environ=env))
    deduped = []
    seen = set()
    for root in roots:
        if root in seen:
            continue
        deduped.append(root)
        seen.add(root)
    return deduped


def pipewire_spawn_env(*, environ=None):
    env = dict(os.environ if environ is None else environ)
    ladspa_entries = ladspa_env_entries(environ=env)
    if env_flag_enabled("WAVELINUX_ENABLE_BUNDLED_LADSPA", environ=env):
        ladspa_entries.extend(bundled_ladspa_entries(environ=env))
    deduped = []
    seen = set()
    for path in ladspa_entries:
        key = os.path.normpath(path)
        if key in seen:
            continue
        deduped.append(path)
        seen.add(key)
    if ladspa_entries:
        env["LADSPA_PATH"] = ":".join(deduped)
    else:
        env.pop("LADSPA_PATH", None)
    return env


def probe_ladspa_plugins(roots):
    found = set()
    for root in roots:
        try:
            for entry in os.listdir(root):
                if entry.endswith(".so"):
                    found.add(entry[:-3])
        except OSError:
            continue
    return found


def ladspa_plugin_available(name, ladspa_plugins):
    if name in ladspa_plugins:
        return True
    low = name.lower()
    for plugin in ladspa_plugins:
        if plugin.lower() == low:
            return True
        if plugin.lower().startswith(low + "_"):
            return True
    return False


def ladspa_plugin_path(name, roots):
    target_lower = name.lower()
    for root in roots:
        try:
            entries = os.listdir(root)
        except OSError:
            continue
        for entry in entries:
            if not entry.endswith(".so"):
                continue
            stem = entry[:-3]
            if (
                stem == name
                or stem.lower() == target_lower
                or stem.lower().startswith(target_lower + "_")
            ):
                full = os.path.join(root, entry)
                if os.path.isfile(full):
                    return full
    return None


def effect_available(effect_id, ladspa_plugins, requirements=EFFECT_REQUIREMENTS):
    needed = requirements.get(effect_id, ())
    return all(ladspa_plugin_available(name, ladspa_plugins) for name in needed)
