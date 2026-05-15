"""Shared application context passed to feature modules."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass
class AppContext:
    runtime: Any
    engine: Any
    config_store: Any
    event_bus: Any
    module_manager: Any
    diagnostics: Any
    main_window: Any = None

