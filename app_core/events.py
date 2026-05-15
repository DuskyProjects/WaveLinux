"""Typed in-process events for module coordination."""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any, Callable


@dataclass(frozen=True)
class RuntimeViewUpdated:
    view_state: Any


@dataclass(frozen=True)
class FxStatusUpdated:
    status: Any


@dataclass(frozen=True)
class ConfigChanged:
    config: dict[str, Any]


@dataclass(frozen=True)
class ModuleFailed:
    module_id: str
    reason: str
    issues: tuple[str, ...] = ()


@dataclass(frozen=True)
class ModuleRestartRequested:
    module_id: str
    reason: str


@dataclass(frozen=True)
class DeviceTopologyChanged:
    added: tuple[str, ...] = ()
    removed: tuple[str, ...] = ()


@dataclass(frozen=True)
class SettingsTabOpened:
    tab_name: str


@dataclass(frozen=True)
class SettingsTabClosed:
    tab_name: str = ""


@dataclass(frozen=True)
class SettingsTabBecameActive:
    tab_name: str


@dataclass(frozen=True)
class AppShutdownRequested:
    reason: str


@dataclass
class EventBus:
    _subscribers: dict[type[Any], list[Callable[[Any], None]]] = field(
        default_factory=lambda: defaultdict(list)
    )

    def subscribe(self, event_type: type[Any], callback: Callable[[Any], None]) -> None:
        callbacks = self._subscribers[event_type]
        if callback not in callbacks:
            callbacks.append(callback)

    def unsubscribe(self, event_type: type[Any], callback: Callable[[Any], None]) -> None:
        callbacks = self._subscribers.get(event_type)
        if not callbacks:
            return
        try:
            callbacks.remove(callback)
        except ValueError:
            return

    def publish(self, event: Any) -> None:
        for callback in list(self._subscribers.get(type(event), ())):
            callback(event)

