"""Datamodels for the WaveLinux runtime control plane."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any
import time


@dataclass
class FxSpec:
    effects: list[str] = field(default_factory=list)
    params_map: dict[str, dict[str, float]] = field(default_factory=dict)
    generation: int = 0


@dataclass
class ChannelSpec:
    node_name: str
    capture_target: str = ""
    fx: FxSpec = field(default_factory=FxSpec)
    submix_state: dict[str, dict[str, Any]] = field(default_factory=dict)


@dataclass
class MixSpec:
    name: str
    hardware_sink: str | None = None
    sink_name: str | None = None
    source_name: str | None = None
    master_volume: float = 1.0


@dataclass
class AppRouteSpec:
    app_id: str
    display_name: str = ""
    sink_name: str | None = None


@dataclass
class DesiredState:
    schema_version: int = 1
    channels: dict[str, ChannelSpec] = field(default_factory=dict)
    mixes: dict[str, MixSpec] = field(default_factory=dict)
    app_routes: dict[str, AppRouteSpec] = field(default_factory=dict)
    virtual_channels: dict[str, str] = field(default_factory=dict)
    selected_mic: str | None = None


@dataclass
class RuntimeSinkView:
    name: str
    display_name: str
    is_internal: bool = False


@dataclass
class RuntimeChannelView:
    node_id: str
    name: str
    description: str
    media_class: str
    label: str
    channel_type: str
    icon: str
    is_mic: bool
    capture_target: str
    meter_source: str
    monitor_volume: float = 1.0
    monitor_mute: bool = False
    stream_volume: float = 1.0
    stream_mute: bool = False
    fx_effects: list[str] = field(default_factory=list)
    fx_running: bool = False


@dataclass
class RuntimeAppView:
    app_id: str
    app_name: str
    active_indices: list[str] = field(default_factory=list)
    current_sink: str | None = None
    current_volume: float | None = None


@dataclass
class ObservedState:
    timestamp: float = field(default_factory=time.time)
    fx_sources_by_channel: dict[str, str | None] = field(default_factory=dict)
    fx_effects_by_channel: dict[str, list[str]] = field(default_factory=dict)
    fx_params_by_channel: dict[str, dict[str, dict[str, float]]] = field(default_factory=dict)
    mix_hardware_routes: dict[str, str | None] = field(default_factory=dict)
    mix_master_volumes: dict[str, float] = field(default_factory=dict)
    app_routes: dict[str, str | None] = field(default_factory=dict)
    mic_inputs: list[RuntimeChannelView] = field(default_factory=list)
    virtual_channels: list[RuntimeChannelView] = field(default_factory=list)
    sinks: list[RuntimeSinkView] = field(default_factory=list)
    app_views: list[RuntimeAppView] = field(default_factory=list)
    channel_ids_by_name: dict[str, str] = field(default_factory=dict)
    stale_channel_ids: dict[str, str] = field(default_factory=dict)
    present_node_names: set[str] = field(default_factory=set)
    source_names: set[str] = field(default_factory=set)
    submix_owner_by_channel: dict[str, dict[str, str | None]] = field(default_factory=dict)
    submix_live_by_channel: dict[str, dict[str, bool]] = field(default_factory=dict)
    submix_source_by_channel: dict[str, dict[str, str | None]] = field(default_factory=dict)
    default_source: str | None = None
    health: dict[str, str] = field(default_factory=dict)
    snapshot: Any = None


@dataclass
class Action:
    kind: str
    payload: dict[str, Any] = field(default_factory=dict)


@dataclass
class OperationStatus:
    node_name: str = ""
    state: str = "idle"
    generation: int = 0
    message: str = ""
    updated_at: float = field(default_factory=time.time)
    error: str = ""
    diagnostics_path: str = ""


@dataclass
class RuntimeViewState:
    channels: dict[str, ChannelSpec] = field(default_factory=dict)
    mixes: dict[str, MixSpec] = field(default_factory=dict)
    apps: dict[str, str | None] = field(default_factory=dict)
    mic_inputs: list[RuntimeChannelView] = field(default_factory=list)
    virtual_channels: list[RuntimeChannelView] = field(default_factory=list)
    sinks: list[RuntimeSinkView] = field(default_factory=list)
    app_views: list[RuntimeAppView] = field(default_factory=list)
    present_node_names: set[str] = field(default_factory=set)
    default_source: str | None = None
    fx_status_by_channel: dict[str, OperationStatus] = field(default_factory=dict)
    health: dict[str, str] = field(default_factory=dict)
    pending_operations: dict[str, str] = field(default_factory=dict)
    node_count: int = 0
    app_count: int = 0
    observed_at: float = field(default_factory=time.time)


@dataclass
class SetChannelFx:
    node_name: str
    capture_target: str
    fx_spec: FxSpec


@dataclass
class ClearChannelFx:
    node_name: str
    generation: int = 0


@dataclass
class SetSubmixState:
    node_id: str
    mix_name: str
    volume: float
    mute: bool
    node_name: str = ""


@dataclass
class EnsureSubmixRoute:
    node_id: str
    node_name: str
    media_class: str
    mix_name: str
    initial_state: dict[str, Any] = field(default_factory=dict)


@dataclass
class RemoveNodeRouting:
    node_id: str


@dataclass
class SetMixHardwareRoute:
    mix_name: str
    sink_name: str | None


@dataclass
class SetMixVolume:
    mix_name: str
    volume: float


@dataclass
class SetAppRoute:
    app_id: str
    sink_name: str | None


@dataclass
class SetAppVolume:
    sink_input_index: str
    volume: float


@dataclass
class SetCardProfile:
    card_name: str
    profile_name: str


@dataclass
class SetSelectedMic:
    node_name: str | None


@dataclass
class RefreshNow:
    reason: str = ""


@dataclass
class RecoverChannel:
    node_name: str
