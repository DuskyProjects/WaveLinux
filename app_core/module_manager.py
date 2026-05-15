"""Lifecycle manager for WaveLinux feature modules."""

from __future__ import annotations

from collections import defaultdict, deque
import logging

from .health_bus import HealthBus


class ModuleManager:
    def __init__(self, ctx, *, feature_flags=None, health_bus=None):
        self.ctx = ctx
        self.feature_flags = feature_flags
        self.health_bus = health_bus or HealthBus()
        self._modules: dict[str, object] = {}
        self._order: list[str] = []
        self._disabled_snapshots: dict[str, object] = {}

    def register(self, module) -> None:
        module_id = str(getattr(module, "module_id", "") or "").strip().lower()
        if not module_id:
            raise ValueError("module_id is required")
        self._modules[module_id] = module
        self._order = self._topological_order()
        self.health_bus.update(module.health())

    def start_all(self) -> None:
        for module_id in self._order:
            module = self._modules[module_id]
            if self._is_start_disabled(module_id):
                try:
                    module.stop("Disabled by feature flags")
                except Exception:
                    logging.exception("Could not apply disabled state to module %s", module_id)
                self._mark_disabled(module, "Disabled by feature flags")
                continue
            self._start_module(module)
        for module_id in sorted(getattr(self.feature_flags, "force_restart_modules", set()) or set()):
            if module_id in self._modules:
                self.restart_module(module_id, "Forced restart by feature flags")

    def stop_all(self, reason: str) -> None:
        for module_id in reversed(self._order):
            module = self._modules[module_id]
            try:
                module.stop(reason)
            except Exception:
                logging.exception("Could not stop module %s", module_id)
                try:
                    module.mark_failed(f"Stop failed: {reason}")
                except Exception:
                    pass
            self.health_bus.update(module.health())

    def restart_module(self, module_id: str, reason: str) -> None:
        module_id = self._normalize_module_id(module_id)
        module = self._modules[module_id]
        if not getattr(module, "restartable", True):
            raise ValueError(f"module_not_restartable:{module_id}")
        snapshot = module.snapshot()
        module.stop(reason)
        replacement = module._spawn_replacement()
        self._modules[module_id] = replacement
        self._order = self._topological_order()
        self._start_module(replacement)
        self._restore_module(replacement, snapshot, action="restart")
        self.health_bus.update(replacement.health())

    def disable_module(self, module_id: str, reason: str) -> None:
        module_id = self._normalize_module_id(module_id)
        module = self._modules[module_id]
        if not getattr(module, "disableable", True):
            raise ValueError(f"module_not_disableable:{module_id}")
        snapshot = None
        try:
            snapshot = module.snapshot()
        except Exception:
            logging.exception("Could not snapshot module %s before disable", module_id)
        module.stop(reason)
        if snapshot is not None:
            self._disabled_snapshots[module_id] = snapshot
        self._mark_disabled(module, reason or "Disabled")

    def enable_module(self, module_id: str) -> None:
        module_id = self._normalize_module_id(module_id)
        module = self._modules[module_id]
        health = module.health()
        if health.state not in {"disabled", "stopped", "failed"}:
            self.health_bus.update(health)
            return
        snapshot = self._disabled_snapshots.pop(module_id, None)
        self._start_module(module)
        self._restore_module(module, snapshot, action="enable")
        self.health_bus.update(module.health())

    def module_health(self, module_id: str):
        module_id = self._normalize_module_id(module_id)
        health = self._modules[module_id].health()
        self.health_bus.update(health)
        return health

    def list_modules(self):
        out = []
        for module_id in self._order:
            module = self._modules[module_id]
            health = module.health()
            self.health_bus.update(health)
            out.append(health)
        return out

    def on_runtime_view(self, view_state) -> None:
        for module_id in self._order:
            module = self._modules[module_id]
            if module.health().state != "running":
                continue
            module.on_runtime_view(view_state)
            self.health_bus.update(module.health())

    def on_config_changed(self, config: dict) -> None:
        for module_id in self._order:
            module = self._modules[module_id]
            module.on_config_changed(config)
            self.health_bus.update(module.health())

    def _start_module(self, module) -> None:
        try:
            module.start(self.ctx)
        except Exception as exc:
            logging.exception("Could not start module %s", module.module_id)
            module.mark_failed(str(exc))
        self.health_bus.update(module.health())

    def _mark_disabled(self, module, summary: str) -> None:
        module._set_health("disabled", summary or "Disabled")
        self.health_bus.update(module.health())

    def _restore_module(self, module, snapshot, *, action: str) -> None:
        if snapshot is None:
            return
        try:
            module.restore(snapshot)
        except Exception as exc:
            logging.exception("Could not restore module %s after %s", module.module_id, action)
            module.mark_failed(f"Restore failed after {action}: {exc}")

    def _normalize_module_id(self, module_id: str) -> str:
        key = str(module_id or "").strip().lower()
        if key.endswith("_module"):
            key = key[:-7]
        if key not in self._modules:
            raise KeyError(f"unknown_module:{module_id}")
        return key

    def _is_start_disabled(self, module_id: str) -> bool:
        flags = self.feature_flags
        if flags is None:
            return False
        return flags.is_disabled(module_id)

    def _topological_order(self) -> list[str]:
        graph: dict[str, set[str]] = defaultdict(set)
        indegree: dict[str, int] = {}
        for module_id, module in self._modules.items():
            indegree[module_id] = 0
        for module_id, module in self._modules.items():
            for dep in tuple(getattr(module, "dependencies", ()) or ()):
                dep_id = str(dep or "").strip().lower()
                if dep_id not in self._modules:
                    continue
                if module_id not in graph[dep_id]:
                    graph[dep_id].add(module_id)
                    indegree[module_id] += 1
        queue_ids = deque(sorted(mid for mid, count in indegree.items() if count == 0))
        order: list[str] = []
        while queue_ids:
            module_id = queue_ids.popleft()
            order.append(module_id)
            for child in sorted(graph.get(module_id, ())):
                indegree[child] -= 1
                if indegree[child] == 0:
                    queue_ids.append(child)
        if len(order) != len(self._modules):
            raise ValueError("module_dependency_cycle")
        return order
