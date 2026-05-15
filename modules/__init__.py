"""WaveLinux feature modules."""

from .app_routing_module import AppRoutingModule
from .device_policy_module import DevicePolicyModule
from .effects_module import EffectsModule
from .health_module import HealthModule
from .metering_module import MeteringModule
from .mixer_ui_module import MixerUiModule
from .runtime_module import RuntimeModule
from .scenes_module import ScenesModule
from .settings_ui_module import SettingsUiModule
from .stress_control_module import StressControlModule
from .updates_module import UpdatesModule

__all__ = [
    "AppRoutingModule",
    "DevicePolicyModule",
    "EffectsModule",
    "HealthModule",
    "MeteringModule",
    "MixerUiModule",
    "RuntimeModule",
    "ScenesModule",
    "SettingsUiModule",
    "StressControlModule",
    "UpdatesModule",
]
