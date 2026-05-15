"""Core lifecycle and feature-module primitives for WaveLinux."""

from .context import AppContext
from .events import (
    AppShutdownRequested,
    ConfigChanged,
    DeviceTopologyChanged,
    EventBus,
    FxStatusUpdated,
    ModuleFailed,
    ModuleRestartRequested,
    RuntimeViewUpdated,
    SettingsTabBecameActive,
    SettingsTabClosed,
    SettingsTabOpened,
)
from .feature_flags import FeatureFlags, load_feature_flags
from .health_bus import HealthBus
from .module_base import BaseFeatureModule, FeatureModule, ModuleHealth, ModuleSnapshot
from .module_manager import ModuleManager

__all__ = [
    "AppContext",
    "AppShutdownRequested",
    "BaseFeatureModule",
    "ConfigChanged",
    "DeviceTopologyChanged",
    "EventBus",
    "FeatureFlags",
    "FeatureModule",
    "FxStatusUpdated",
    "HealthBus",
    "ModuleFailed",
    "ModuleHealth",
    "ModuleManager",
    "ModuleRestartRequested",
    "ModuleSnapshot",
    "RuntimeViewUpdated",
    "SettingsTabBecameActive",
    "SettingsTabClosed",
    "SettingsTabOpened",
    "load_feature_flags",
]
