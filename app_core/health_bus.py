"""Small registry for feature-module health state."""

from __future__ import annotations

from dataclasses import replace

from .module_base import ModuleHealth


class HealthBus:
    def __init__(self):
        self._health_by_module: dict[str, ModuleHealth] = {}

    def update(self, health: ModuleHealth) -> None:
        self._health_by_module[str(health.module_id)] = replace(health)

    def get(self, module_id: str) -> ModuleHealth | None:
        health = self._health_by_module.get(str(module_id or ""))
        return replace(health) if health is not None else None

    def list(self) -> list[ModuleHealth]:
        return [
            replace(health)
            for _, health in sorted(self._health_by_module.items())
        ]

