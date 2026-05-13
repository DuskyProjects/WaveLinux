"""Structured health issues for the WaveLinux recovery center."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass(frozen=True)
class HealthIssue:
    code: str
    severity: str
    title: str
    detail: str
    primary_action: str = ""
    secondary_action: str = ""
    context: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class RecoveryStatus:
    node_name: str
    state: str
    retry_count: int
    next_retry_at: float | None = None
    diagnostics_path: str = ""
