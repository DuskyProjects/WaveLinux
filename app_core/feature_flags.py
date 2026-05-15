"""Local-only feature flags for module diagnostics and restart drills."""

from __future__ import annotations

from dataclasses import dataclass, field
import json
import os


def _parse_module_names(value) -> set[str]:
    if value is None:
        return set()
    if isinstance(value, str):
        parts = value.split(",")
    elif isinstance(value, (list, tuple, set)):
        parts = value
    else:
        return set()
    normalized = set()
    for raw in parts:
        item = str(raw or "").strip().lower()
        if not item:
            continue
        if item.endswith("_module"):
            item = item[:-7]
        normalized.add(item)
    return normalized


@dataclass
class FeatureFlags:
    disabled_modules: set[str] = field(default_factory=set)
    start_disabled_modules: set[str] = field(default_factory=set)
    force_restart_modules: set[str] = field(default_factory=set)
    source_path: str = ""

    def is_disabled(self, module_id: str) -> bool:
        key = str(module_id or "").strip().lower()
        return key in self.disabled_modules or key in self.start_disabled_modules


def _debug_config_path() -> str:
    return os.path.expanduser("~/.config/wavelinux/debug-modules.json")


def load_feature_flags(
    *,
    environ=None,
    path: str | None = None,
) -> FeatureFlags:
    environ = dict(os.environ if environ is None else environ)
    debug_path = os.path.abspath(path or _debug_config_path())
    payload: dict[str, object] = {}
    if os.path.exists(debug_path):
        try:
            with open(debug_path, "r", encoding="utf-8") as handle:
                loaded = json.load(handle)
            if isinstance(loaded, dict):
                payload = loaded
        except Exception:
            payload = {}

    disabled_modules = _parse_module_names(environ.get("WAVELINUX_DISABLE_MODULES"))
    disabled_modules.update(_parse_module_names(payload.get("disabled_modules")))

    start_disabled = _parse_module_names(environ.get("WAVELINUX_START_DISABLED_MODULES"))
    start_disabled.update(_parse_module_names(payload.get("start_disabled_modules")))

    force_restart = _parse_module_names(environ.get("WAVELINUX_FORCE_MODULE_RESTARTS"))
    force_restart.update(_parse_module_names(payload.get("force_restart_modules")))

    return FeatureFlags(
        disabled_modules=disabled_modules,
        start_disabled_modules=start_disabled,
        force_restart_modules=force_restart,
        source_path=debug_path,
    )

