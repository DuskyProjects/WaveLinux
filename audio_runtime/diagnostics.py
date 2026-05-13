"""Diagnostics, command journaling, and failure bundle export."""

from __future__ import annotations

from dataclasses import asdict, is_dataclass
from collections import deque
from pathlib import Path
from typing import Any
import json
import time


class RuntimeDiagnostics:
    """Persist small, high-signal diagnostic bundles for failed runtime ops."""

    def __init__(self, root_dir: str | None = None, max_commands: int = 300,
                 max_exports: int = 30):
        if root_dir is None:
            root_dir = "~/.config/wavelinux/diagnostics"
        self.root_dir = Path(root_dir).expanduser()
        self.root_dir.mkdir(parents=True, exist_ok=True)
        self._commands = deque(maxlen=max_commands)
        self._state_snapshots = deque(maxlen=30)
        self.max_exports = max(1, int(max_exports))
        self._prune_exports()

    def record_command(self, cmd, timeout, duration_ms, stdout, success):
        stdout_preview = ""
        if stdout:
            lines = stdout.splitlines()
            stdout_preview = "\n".join(lines[:4])[:400]
        self._commands.append({
            "timestamp": time.time(),
            "command": list(cmd),
            "timeout": timeout,
            "duration_ms": round(float(duration_ms), 2),
            "success": bool(success),
            "stdout_preview": stdout_preview,
        })

    def snapshot(self, label: str, payload: Any):
        self._state_snapshots.append({
            "timestamp": time.time(),
            "label": label,
            "payload": self._to_jsonable(payload),
        })

    def export_failure(self, reason: str, *, desired=None, observed=None,
                       actions=None, health=None, status=None) -> str:
        self._prune_exports(limit=max(0, self.max_exports - 1))
        stamp = time.strftime("%Y%m%d-%H%M%S")
        nanos = time.time_ns() % 1_000_000_000
        path = self.root_dir / f"runtime-failure-{stamp}-{nanos:09d}.json"
        bundle = {
            "reason": reason,
            "timestamp": time.time(),
            "desired": self._to_jsonable(desired),
            "observed": self._to_jsonable(observed),
            "actions": self._to_jsonable(actions),
            "health": self._to_jsonable(health),
            "status": self._to_jsonable(status),
            "recent_commands": list(self._commands),
            "recent_snapshots": list(self._state_snapshots),
        }
        path.write_text(json.dumps(bundle, indent=2, sort_keys=True))
        self._prune_exports()
        return str(path)

    def latest_commands(self):
        return list(self._commands)

    def _prune_exports(self, *, limit: int | None = None):
        limit = self.max_exports if limit is None else max(0, int(limit))
        exports = sorted(
            self.root_dir.glob("runtime-failure-*.json"),
            key=lambda item: (item.stat().st_mtime_ns, item.name),
            reverse=True,
        )
        for old in exports[limit:]:
            try:
                old.unlink()
            except OSError:
                continue

    @classmethod
    def _to_jsonable(cls, value: Any):
        if is_dataclass(value):
            return cls._to_jsonable(asdict(value))
        if isinstance(value, dict):
            return {str(k): cls._to_jsonable(v) for k, v in value.items()}
        if isinstance(value, (list, tuple, set, deque)):
            return [cls._to_jsonable(v) for v in value]
        if hasattr(value, "__dict__") and not isinstance(value, type):
            return cls._to_jsonable(vars(value))
        slots = getattr(value, "__slots__", None)
        if slots and not isinstance(value, type):
            payload = {}
            for slot in slots:
                if not hasattr(value, slot):
                    continue
                payload[slot] = cls._to_jsonable(getattr(value, slot))
            return payload
        if isinstance(value, (str, int, float, bool)) or value is None:
            return value
        return repr(value)
