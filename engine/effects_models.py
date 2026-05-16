"""Data models for FX runtime state and verification."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any


@dataclass
class FxReadiness:
    code: str
    detail: str
    context: dict[str, Any] = field(default_factory=dict)

    def to_dict(self):
        return {
            "code": str(self.code or "").strip(),
            "detail": str(self.detail or "").strip(),
            "context": dict(self.context or {}),
        }


@dataclass
class FxRuntimeState:
    node_name: str
    mode: str = ""
    requested_effects: list[str] = field(default_factory=list)
    params_map: dict[str, dict[str, Any]] = field(default_factory=dict)
    capture_target: str = ""
    source: str = ""
    active_chain_source: str = ""
    active_chain_sink: str = ""
    default_source: str = ""
    proxy_sink_name: str = ""
    proxy_source_name: str = ""
    proxy_sink_module_id: str = ""
    proxy_source_module_id: str = ""
    proxy_sink_alive: bool = False
    proxy_source_alive: bool = False
    source_visible: bool = False
    active_chain_visible: bool = False
    loopbacks: list[str] = field(default_factory=list)
    live_loopbacks: dict[str, bool] = field(default_factory=dict)
    processes: list[str] = field(default_factory=list)
    live_processes: dict[str, bool] = field(default_factory=dict)
    status_state: str = ""
    status_message: str = ""
    status_error: str = ""
    status_generation: int = 0

    def to_dict(self):
        return asdict(self)


@dataclass
class FxVerificationResult:
    ready: bool
    requested: bool
    state: str = ""
    runtime: FxRuntimeState | None = None
    reasons: list[FxReadiness] = field(default_factory=list)

    def reason_codes(self):
        return [reason.code for reason in self.reasons]

    def to_dict(self):
        return {
            "ready": bool(self.ready),
            "requested": bool(self.requested),
            "state": str(self.state or "").strip(),
            "runtime": self.runtime.to_dict() if self.runtime is not None else {},
            "reasons": [reason.to_dict() for reason in self.reasons],
            "reason_codes": self.reason_codes(),
        }
