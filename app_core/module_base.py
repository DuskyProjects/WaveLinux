"""Base feature-module contracts and helpers."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol


@dataclass
class ModuleSnapshot:
    module_id: str
    state: dict[str, Any] = field(default_factory=dict)


@dataclass
class ModuleHealth:
    module_id: str
    state: str
    summary: str = ""
    issues: list[str] = field(default_factory=list)
    restartable: bool = True


class FeatureModule(Protocol):
    module_id: str
    dependencies: tuple[str, ...]

    def start(self, ctx) -> None: ...
    def stop(self, reason: str) -> None: ...
    def snapshot(self) -> ModuleSnapshot: ...
    def restore(self, snapshot: ModuleSnapshot) -> None: ...
    def health(self) -> ModuleHealth: ...
    def on_runtime_view(self, view_state) -> None: ...
    def on_config_changed(self, config: dict) -> None: ...


class BaseFeatureModule:
    module_id = ""
    dependencies: tuple[str, ...] = ()
    restartable = True
    disableable = True

    def __init__(self, *spawn_args, **spawn_kwargs):
        self._ctx = None
        self._spawn_args = spawn_args
        self._spawn_kwargs = dict(spawn_kwargs)
        self._health = ModuleHealth(
            module_id=self.module_id,
            state="stopped",
            restartable=bool(self.restartable),
        )

    def start(self, ctx) -> None:
        self._ctx = ctx
        self._set_health("starting", "Starting")
        self.on_start(ctx)
        self._set_health("running", "Running")

    def stop(self, reason: str) -> None:
        self.on_stop(reason)
        self._set_health("stopped", reason or "Stopped")

    def snapshot(self) -> ModuleSnapshot:
        return ModuleSnapshot(module_id=self.module_id, state={})

    def restore(self, snapshot: ModuleSnapshot) -> None:
        _ = snapshot

    def health(self) -> ModuleHealth:
        return ModuleHealth(
            module_id=str(self.module_id or self._health.module_id),
            state=self._health.state,
            summary=self._health.summary,
            issues=list(self._health.issues),
            restartable=self._health.restartable,
        )

    def on_runtime_view(self, view_state) -> None:
        _ = view_state

    def on_config_changed(self, config: dict) -> None:
        _ = config

    def on_start(self, ctx) -> None:
        _ = ctx

    def on_stop(self, reason: str) -> None:
        _ = reason

    def _set_health(self, state: str, summary: str = "", *, issues=None) -> None:
        self._health = ModuleHealth(
            module_id=self.module_id,
            state=str(state or "").strip() or "stopped",
            summary=str(summary or "").strip(),
            issues=list(issues or []),
            restartable=bool(self.restartable),
        )

    def mark_degraded(self, summary: str, *, issues=None) -> None:
        self._set_health("degraded", summary, issues=issues)

    def mark_failed(self, summary: str, *, issues=None) -> None:
        self._set_health("failed", summary, issues=issues)

    def _spawn_replacement(self):
        return type(self)(*self._spawn_args, **self._spawn_kwargs)
