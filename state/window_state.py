"""Structured non-widget state for the WaveLinux main window."""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class ContentState:
    virtual_channels: list[str] = field(default_factory=list)
    hidden_nodes: set[str] = field(default_factory=set)
    scenes: dict[str, dict] = field(default_factory=dict)
    channel_order: list[str] = field(default_factory=list)
    submix_state: dict[str, dict] = field(default_factory=dict)
    active_effects: dict[str, list[str]] = field(default_factory=dict)
    effect_params: dict[str, dict] = field(default_factory=dict)


@dataclass
class DevicePolicyState:
    desired_mix_hw: dict[str, str | None] = field(
        default_factory=lambda: {"Monitor": None, "Stream": None}
    )
    desired_mix_volumes: dict[str, float] = field(
        default_factory=lambda: {"Monitor": 1.0, "Stream": 1.0}
    )
    preferred_monitor_hw_id: str = ""
    preferred_monitor_hw_name: str = ""
    restorable_monitor_hw_id: str = ""
    restorable_monitor_hw_name: str = ""
    last_good_monitor_hw_id: str = ""
    last_good_monitor_hw_name: str = ""
    preferred_selected_mic_id: str = ""
    preferred_selected_mic_name: str = ""
    restorable_selected_mic_id: str = ""
    restorable_selected_mic_name: str = ""
    last_good_selected_mic_id: str = ""
    last_good_selected_mic_name: str = ""
    selected_mic: str | None = None
    mic_selection_initialized: bool = False
    active_monitor_fallback: bool = False
    active_mic_fallback: bool = False


@dataclass
class RecoveryState:
    auto_recovery_state: dict[str, dict] = field(default_factory=dict)
    recent_recovery_status: dict[str, dict] = field(default_factory=dict)
    runtime_stopped: bool = False
    last_selected_mic_change_at: float = 0.0
    pactl_event_suppressed_until: float = 0.0


@dataclass
class UpdateState:
    pending_update_tag: str | None = None
    pending_verified_release: object | None = None
    pending_update_url: str = ""
    pending_update_asset_url: str = ""
    pending_update_asset_name: str = ""
    last_update_check_at: float | None = None
    last_update_error: dict | None = None
    last_update_attempt_result: str = "No update activity yet."
    install_state_cache: object | None = None
    install_state_cache_at: float = 0.0
    install_state_refresh_inflight: bool = False
    install_state_refresh_tabs: set[str] = field(default_factory=set)


@dataclass
class AppIdentityState:
    app_routing: dict[str, str] = field(default_factory=dict)
    app_volumes: dict[str, float] = field(default_factory=dict)
    app_last_seen: dict[str, int] = field(default_factory=dict)
    app_display_names: dict[str, str] = field(default_factory=dict)
    app_identity_overrides: dict[str, str] = field(default_factory=dict)
    app_label_overrides: dict[str, str] = field(default_factory=dict)
    forgotten_apps: set[str] = field(default_factory=set)
    prune_days: int = 14


@dataclass
class LifecycleState:
    quit_in_progress: bool = False
    shutting_down: bool = False
    runtime_pid_path: str = ""
    onboarding_completed: bool = True
    selected_setup_template: str = ""
    show_first_run_setup: bool = False
    pending_bluetooth_reconnect_macs: set[str] = field(default_factory=set)
    bluetooth_profile_reassert_retries: int = 0


@dataclass
class WaveLinuxWindowState:
    content: ContentState = field(default_factory=ContentState)
    device_policy: DevicePolicyState = field(default_factory=DevicePolicyState)
    recovery: RecoveryState = field(default_factory=RecoveryState)
    updates: UpdateState = field(default_factory=UpdateState)
    app_identity: AppIdentityState = field(default_factory=AppIdentityState)
    lifecycle: LifecycleState = field(default_factory=LifecycleState)
