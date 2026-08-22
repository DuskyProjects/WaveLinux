use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::mem;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, TryLockError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use directories::{BaseDirs, ProjectDirs};
use pipewire as pw;
use pw::{properties::properties, spa};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;
use wavelinux_model::{
    app_display_name, apply_graph_namespace, graph_prefix, graph_property_prefix,
    AcceleratorProviderStatus, AdaptiveLatencyStatus, AppMatcher, AppRoute, AppStateSnapshot,
    AppStream, AppVolumePreset, AudioCoreChannelStatus, AutoDeviceKind, AutoDeviceReason, Channel,
    ChannelInputMode, ChannelKind, DeviceInfo, Diagnostic, DiagnosticSeverity, EffectAvailability,
    EffectCatalog, EffectInstance, EffectRuntimeState, EffectRuntimeStatus, EngineStatus,
    FallbackHardwareProfile, HardwareProfile, HardwareProfileUiState, KnownApp, LatencyPolicy,
    LevelMeter, MeterTransportStatus, Mix, MixerConfig, MixerSettings, ModelError,
    PeripheralPluginStatus, PipeWireAudioHealthStatus, ResolvedAutoDevice, RoutingPolicy,
    RuntimeGraph, StreamerBindingProfile, StreamerDevicesConfig,
};
use wavelinux_pw::{
    a2dp_codec_rank_with_preferences, channel_bus_route_ids_from_routes,
    channel_has_active_effects, channel_mix_latency_msec,
    channel_mix_route_expected_for_active_routes, channel_mix_route_revision,
    channel_mix_route_uses_hardware_direct_monitoring, channel_mix_source_name,
    channel_uses_adaptive_latency_bridge, channel_uses_persistent_audio_core,
    effect_chain_adaptive_bridge_input_name, effect_chain_filter_output_name,
    effect_chain_input_name, effect_chain_node_name, effect_chain_source_name,
    effect_route_revision, input_route_revision, meter_sampling_enabled,
    meter_targets_for_config_with_devices, mix_monitor_route_revision_for_sink,
    mix_uses_persistent_audio_core, plan_bluetooth_a2dp_profiles, plan_ensure_graph,
    plan_ensure_graph_for_active_routes, plan_ensure_passthrough_mic_source,
    plan_kill_stale_processes, plan_move_app_stream, plan_move_app_stream_to_default,
    plan_move_capture_stream_to_source, plan_move_native_app_stream,
    plan_move_native_capture_stream, plan_route_channel_to_effect, plan_route_channel_to_mix,
    plan_route_effect_to_adaptive_bridge, plan_set_channel_bus_mute,
    plan_set_channel_bus_source_output_mute, plan_set_channel_bus_source_output_volume,
    plan_set_channel_bus_volume, plan_set_default_sink, plan_set_default_source,
    plan_set_managed_sink_mute, plan_set_managed_sink_volume,
    plan_set_mix_mute as plan_pw_set_mix_mute, plan_set_mix_volume as plan_pw_set_mix_volume,
    plan_set_native_stream_volume, plan_set_route_sink_input_mute,
    plan_set_route_sink_input_volume, plan_set_route_source_output_mute,
    plan_set_route_source_output_volume, plan_set_stream_mute, plan_set_stream_volume,
    plan_unload_modules, probe_effect_availability, render_filter_chain, AudioStateSnapshot,
    BluetoothAudioCard, ChannelBusRouteIds, CommandDomain, CommandOutput, CommandSpec,
    ManagedModule, MeterTarget, PipeWireRegistryCache, PlannedGraph, PwClient, PwError,
    RegistryEventKind, SinkInputRoute, SinkLevelState, SnapshotCommandTiming, SourceOutputRoute,
    StaleProcess, StreamRouteBackend, CHANNEL_CONFIG_REVISION,
    EFFECT_ADAPTIVE_BRIDGE_ROUTE_REVISION, EFFECT_CONFIG_REVISION,
};

mod configuration;
mod devices;
mod effects;
mod hardware_profiles;
mod health;
mod levels;
mod meters;
mod reconciliation;
mod registry_actor;
mod routing;

use hardware_profiles::{
    apply_profile_policy_to_devices, apply_profile_policy_to_graph, apply_profiles_to_devices,
    hardware_profile_by_id, hardware_profile_diagnostics, hardware_profile_ui_state,
    load_hardware_profile_catalog, remote_profile_sync_needed, sync_remote_profiles_for_devices,
    HardwareProfileCatalog,
};
#[cfg(test)]
use health::{
    cpu_pressure_between, parse_proc_load_pressure, parse_proc_pressure_total, parse_proc_stat_cpu,
    stall_pressure_between,
};
use health::{pipewire_health_deltas, CpuPressureSampler, PipeWireAudioHealthTracker};
use registry_actor::{run_native_registry_connection, NativeRegistryHooks};

const DEBUG_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
const DEBUG_LOG_ROTATED_FILES: usize = 4;
const LOG_VERSION_FILE: &str = "log-version";
const ENGINE_LOG_FILE: &str = "wavelinux-engine.log";
const LEGACY_APP_LOG_FILE: &str = "wavelinux.log";
const WAVELINUX5_MIGRATION_MARKER: &str = ".migration-from-wavelinux5.pending";
const EFFECT_CHAIN_LOG_SUFFIX: &str = ".log";
const AUDIO_CORE_PROCESS_KEY: &str = "__wavelinux6_audio_core__";
const AUDIO_CORE_MANIFEST_FILE: &str = "wavelinux6-audio-core.json";
const AUDIO_CORE_LOG_FILE: &str = "wavelinux6-audio-core.log";
const ADAPTIVE_QUANTUM_FLOORS_FILE: &str = "adaptive-quantum-floors.json";
const ADAPTIVE_QUANTUM_FLOORS_VERSION: u32 = 1;
const HOST_DIAGNOSTICS_TTL: Duration = Duration::from_secs(30);
const ACCELERATOR_STATUS_TTL: Duration = Duration::from_secs(30);
const EFFECT_AVAILABILITY_TTL: Duration = Duration::from_secs(30);
const HARDWARE_PROFILE_TTL: Duration = Duration::from_secs(15);
const REMOTE_PROFILE_SYNC_MIN_INTERVAL: Duration = Duration::from_secs(30);
const METER_RESTART_BACKOFF: Duration = Duration::from_secs(5);
const METER_IDLE_STOP_AFTER: Duration = Duration::from_millis(750);
// Match the visible meter floor. The previous -42 dBFS gate hid ordinary
// microphone levels even though the capture stream contained valid audio.
const METER_NOISE_FLOOR: f32 = 0.002;
const METER_STALE_AFTER: Duration = Duration::from_millis(120);
const METER_STALE_RELEASE_PER_SECOND: f32 = 0.08;
const METER_DISPLAY_FLOOR_DB: f32 = -54.0;
const METER_DISPLAY_CEILING_DB: f32 = 0.0;
const METER_DISPLAY_EXPONENT: f32 = 1.15;
const METER_STREAM_LATENCY: &str = "2400/48000";
const METER_MAINLOOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const EFFECT_GRAPH_SYNC_DEBOUNCE: Duration = Duration::from_millis(75);
const EFFECT_RECOVERY_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const EFFECT_CORE_READY_TIMEOUT: Duration = Duration::from_secs(3);
const EFFECT_CORE_ACK_TIMEOUT: Duration = Duration::from_secs(2);
const EFFECT_CORE_RETRY_MIN: Duration = Duration::from_millis(20);
const EFFECT_CORE_RETRY_MAX: Duration = Duration::from_millis(160);
const EFFECT_NODE_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const EFFECT_NODE_CLEAR_TIMEOUT: Duration = Duration::from_secs(2);
// One visible sample followed by the settled recheck below gives us two
// observations without adding a redundant third 100 ms startup delay.
const EFFECT_NODE_READY_STABLE_SAMPLES: usize = 1;
const EFFECT_NODE_READY_SETTLE: Duration = Duration::from_millis(100);
const EFFECT_ROUTE_READY_SETTLE: Duration = Duration::from_millis(300);
const EFFECT_ROUTE_LINK_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const EFFECT_ROUTE_LINK_SETTLE: Duration = Duration::from_millis(100);
const EFFECT_CHAIN_FAILURE_LOGS: usize = 8;
const GRAPH_REPAIR_DEBOUNCE: Duration = Duration::from_millis(650);
const ROUTE_HEALTH_REPAIR_BACKOFF: Duration = Duration::from_secs(10);
const UI_STATE_REFRESH_MAX_AGE: Duration = Duration::from_millis(4_000);
// Hotplug and stream routing are event-driven. This audit only recovers from a
// lost subscription event, so keep it infrequent enough that a slow host
// `pactl list` cannot create regular foreground contention under I/O load.
const DEFAULT_EVENT_WATCHDOG_INTERVAL: Duration = Duration::from_secs(120);
const ADAPTIVE_LATENCY_TICK_INTERVAL: Duration = Duration::from_secs(1);
// Pulse clients commonly emit new/change/remove bursts while negotiating a
// stream. Waiting for one short burst to settle avoids routing handles that no
// longer exist while keeping stable app routing below the 100 ms target.
const PLAYBACK_EVENT_SETTLE: Duration = Duration::from_millis(15);
const DEVICE_EVENT_SETTLE: Duration = Duration::from_millis(75);
const SLOW_REFRESH_LOG_THRESHOLD: Duration = Duration::from_millis(300);
const SEVERE_REFRESH_LOG_THRESHOLD: Duration = Duration::from_millis(1_500);
const ROUTINE_SLOW_REFRESH_LOG_INTERVAL: Duration = Duration::from_secs(60);
const HOST_COMMAND_ENV_REMOVE: &[&str] = &[
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "CEF_PATH",
    "CEF_ROOT",
    "GDK_BACKEND",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_EXTRA_MODULES",
    "GIO_MODULE_DIR",
    "GI_TYPELIB_PATH",
    "GSETTINGS_SCHEMA_DIR",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SCANNER",
    "GST_PLUGIN_SCANNER_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PTP_HELPER_1_0",
    "GST_REGISTRY_REUSE_PLUGIN_SCANNER",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GTK_PATH",
    "GTK_THEME",
    "LD_AUDIT",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "LIBRARY_PATH",
    "PERLLIB",
    "PYTHONHOME",
    "PYTHONPATH",
    "QT_PLUGIN_PATH",
    "WEBKIT_EXEC_PATH",
    "XDG_DATA_DIRS",
];
const FX_LOG_WARNING_WINDOW: Duration = Duration::from_secs(10 * 60);
const DSP_HELPER_ENV: &str = "WAVELINUX_DSP_HELPER";
const EFFECT_CHAIN_STOP_GRACE: Duration = Duration::from_secs(2);
const AUDIO_COMMAND_LOCK_TIMEOUT: Duration = Duration::from_secs(4);
const CAPTURE_MOVE_FAILURE_BACKOFF: Duration = Duration::from_secs(30);
const CAPTURE_MOVE_FAILURE_MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);
const APP_STREAM_MOVE_FAILURE_BACKOFF: Duration = Duration::from_secs(30);
const CLEANUP_MODULE_PASSES: usize = 6;
const CLEANUP_MODULE_SETTLE: Duration = Duration::from_millis(120);
const BLUETOOTH_MONITOR_ROUTE_SETTLE: Duration = Duration::from_millis(650);

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("{0}")]
    Model(#[from] ModelError),
    #[error("{0}")]
    PipeWire(#[from] PwError),
    #[error("config path unavailable")]
    ConfigPathUnavailable,
    #[error("io failed: {0}")]
    Io(String),
    #[error("json failed: {0}")]
    Json(String),
    #[error("lock poisoned")]
    LockPoisoned,
    #[error("audio graph is busy; try again in a moment")]
    AudioBusy,
}

impl From<std::io::Error> for EngineError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for EngineError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct EnginePaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub autostart_dir: PathBuf,
}

impl EnginePaths {
    pub fn from_xdg() -> Result<Self, EngineError> {
        let app_name = std::env::var("WAVELINUX_XDG_APP_NAME")
            .ok()
            .map(|value| value.trim().chars().take(64).collect::<String>())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "WaveLinux".into());
        let dirs = ProjectDirs::from("io.github", "DuskyProjects", &app_name)
            .ok_or(EngineError::ConfigPathUnavailable)?;
        let base_dirs = BaseDirs::new().ok_or(EngineError::ConfigPathUnavailable)?;
        let runtime_dir = runtime_base_dir().join(graph_prefix());
        create_private_runtime_dir(&runtime_dir)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            runtime_dir,
            autostart_dir: base_dirs.config_dir().join("autostart"),
        })
    }

    pub fn for_tests(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            runtime_dir: root.join("runtime"),
            autostart_dir: root.join("autostart"),
        }
    }

    fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    fn effect_chains_dir(&self) -> PathBuf {
        self.data_dir.join("effects")
    }

    fn adaptive_quantum_floors_file(&self) -> PathBuf {
        self.data_dir.join(ADAPTIVE_QUANTUM_FLOORS_FILE)
    }

    fn control_sockets_dir(&self) -> PathBuf {
        wavelinux_dsp::control_directory(&self.runtime_dir)
    }

    fn channel_control_socket(&self, channel_id: &str) -> PathBuf {
        wavelinux_dsp::channel_control_socket(&self.runtime_dir, &graph_prefix(), channel_id)
    }

    fn mix_control_socket(&self) -> PathBuf {
        wavelinux_dsp::mix_control_socket(&self.runtime_dir)
    }

    fn meter_stream_socket(&self) -> PathBuf {
        wavelinux_dsp::meter_stream_socket(&self.runtime_dir)
    }

    fn autostart_file(&self) -> PathBuf {
        self.autostart_dir
            .join(format!("{}.desktop", graph_prefix()))
    }

    fn log_file(&self) -> PathBuf {
        self.config_dir.join(ENGINE_LOG_FILE)
    }

    fn legacy_app_log_file(&self) -> PathBuf {
        self.config_dir.join(LEGACY_APP_LOG_FILE)
    }

    fn log_version_file(&self) -> PathBuf {
        self.config_dir.join(LOG_VERSION_FILE)
    }

    fn local_hardware_profiles_dir(&self) -> PathBuf {
        self.config_dir
            .join("hardware-profiles")
            .join("v1")
            .join("local")
    }

    fn wavelinux5_migration_marker(&self) -> PathBuf {
        self.config_dir.join(WAVELINUX5_MIGRATION_MARKER)
    }
}

fn runtime_base_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(unix)]
            let user = unsafe { libc::geteuid() }.to_string();
            #[cfg(not(unix))]
            let user = "user".to_string();
            std::env::temp_dir().join(format!("wavelinux-{user}"))
        })
}

fn create_private_runtime_dir(path: &Path) -> Result<(), EngineError> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub dry_run: bool,
    pub auto_repair_on_start: bool,
    pub poll_interval: Duration,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            dry_run: std::env::var("WAVELINUX_DRY_RUN").is_ok(),
            auto_repair_on_start: std::env::var("WAVELINUX_NO_AUTO_REPAIR").is_err(),
            poll_interval: DEFAULT_EVENT_WATCHDOG_INTERVAL,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineRevisions {
    pub state: u64,
    pub config: u64,
    pub graph: u64,
}

#[derive(Debug)]
struct EngineChangeSignal {
    state: AtomicU64,
    config: AtomicU64,
    graph: AtomicU64,
    wait_lock: Mutex<()>,
    changed: Condvar,
}

impl Default for EngineChangeSignal {
    fn default() -> Self {
        Self {
            state: AtomicU64::new(1),
            config: AtomicU64::new(1),
            graph: AtomicU64::new(1),
            wait_lock: Mutex::new(()),
            changed: Condvar::new(),
        }
    }
}

impl EngineChangeSignal {
    fn revisions(&self) -> EngineRevisions {
        EngineRevisions {
            state: self.state.load(Ordering::Acquire),
            config: self.config.load(Ordering::Acquire),
            graph: self.graph.load(Ordering::Acquire),
        }
    }

    fn notify_config(&self) {
        self.config.fetch_add(1, Ordering::AcqRel);
        self.notify_state();
    }

    fn notify_graph(&self) {
        self.graph.fetch_add(1, Ordering::AcqRel);
        self.notify_state();
    }

    fn notify_state(&self) {
        self.state.fetch_add(1, Ordering::AcqRel);
        self.changed.notify_all();
    }

    fn wait_after(&self, revision: u64, timeout: Duration) -> EngineRevisions {
        if self.state.load(Ordering::Acquire) != revision {
            return self.revisions();
        }
        let Ok(guard) = self.wait_lock.lock() else {
            return self.revisions();
        };
        if self.state.load(Ordering::Acquire) == revision {
            let _ = self.changed.wait_timeout(guard, timeout);
        }
        self.revisions()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepairReport {
    pub dry_run: bool,
    pub planned: PlannedGraph,
    pub outputs: Vec<CommandExecution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareProfilePrewarmReport {
    pub devices: usize,
    pub matched: usize,
    pub fetched: usize,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandExecution {
    pub command: CommandSpec,
    pub stdout: String,
    pub stderr: String,
    pub skipped: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EffectEndpointReadiness {
    source_ready: bool,
    input_ready: bool,
    processed_ready: bool,
}

impl EffectEndpointReadiness {
    fn ready(self) -> bool {
        self.source_ready && self.input_ready && self.processed_ready
    }
}

impl From<Result<CommandOutput, PwError>> for CommandExecution {
    fn from(result: Result<CommandOutput, PwError>) -> Self {
        match result {
            Ok(output) => Self {
                command: output.command,
                stdout: output.stdout,
                stderr: output.stderr,
                skipped: output.skipped,
                error: None,
            },
            Err(err) => Self {
                command: CommandSpec {
                    domain: wavelinux_pw::CommandDomain::Diagnostics,
                    program: String::new(),
                    args: Vec::new(),
                    description: "failed before command output was available".into(),
                },
                stdout: String::new(),
                stderr: String::new(),
                skipped: false,
                error: Some(err.to_string()),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SoundCheckReport {
    pub diagnostics: Vec<Diagnostic>,
    pub active_stream_count: usize,
    pub virtual_mix_count: usize,
    pub missing_effects: Vec<String>,
    pub debug_log_path: PathBuf,
    pub recent_log_lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphDebugReport {
    pub dry_run: bool,
    pub audio_graph_running: bool,
    pub planned: PlannedGraph,
    pub managed_modules: Vec<ManagedModule>,
    pub sink_input_routes: Vec<SinkInputRoute>,
    pub source_output_routes: Vec<SourceOutputRoute>,
    pub route_health: Vec<RouteHealthIssue>,
    pub stale_processes: Vec<StaleProcess>,
    pub graph: RuntimeGraph,
    pub diagnostics: Vec<Diagnostic>,
    pub debug_log_path: PathBuf,
    pub recent_log_lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteHealthIssue {
    pub module_id: Option<String>,
    pub role: String,
    pub channel_id: Option<String>,
    pub mix_id: Option<String>,
    pub source_name: Option<String>,
    pub sink_name: Option<String>,
    pub reason: RouteHealthReason,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RouteHealthReason {
    MissingSource,
    MissingSink,
    MissingSourceOutput,
    MissingSinkInput,
    StaleConfig,
    Duplicate,
    LevelMismatch,
}

#[derive(Debug)]
struct RuntimeCache {
    graph: RuntimeGraph,
    diagnostics: Vec<Diagnostic>,
    status: EngineStatus,
    sink_input_routes: Vec<SinkInputRoute>,
    source_output_routes: Vec<SourceOutputRoute>,
    bluetooth_monitor_routes: BTreeMap<String, BluetoothMonitorRouteSignature>,
    refreshed_at: Option<Instant>,
    initialized_bluetooth_cards: BTreeMap<String, String>,
}

impl RuntimeCache {
    fn new(dry_run: bool) -> Self {
        Self {
            graph: RuntimeGraph::default(),
            diagnostics: Vec::new(),
            bluetooth_monitor_routes: BTreeMap::new(),
            sink_input_routes: Vec::new(),
            source_output_routes: Vec::new(),
            refreshed_at: None,
            initialized_bluetooth_cards: BTreeMap::new(),
            status: EngineStatus {
                dry_run,
                healthy: true,
                audio_graph_running: false,
                message: if dry_run {
                    "Dry-run mode".into()
                } else {
                    "Ready".into()
                },
                last_refresh_unix: 0,
                adaptive_latency: AdaptiveLatencyStatus::default(),
                audio_core: Vec::new(),
                effects: Vec::new(),
                refresh: Default::default(),
                pipewire_audio_health: Default::default(),
                meter_transport: Default::default(),
                pipewire_registry: Default::default(),
                peripheral_plugins: Vec::new(),
                accelerator_providers: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BluetoothMonitorRouteSignature {
    output: String,
    serial: Option<String>,
    profile: Option<String>,
    codec: Option<String>,
}

#[derive(Debug, Default)]
struct RouteHealthRepairState {
    signature: Option<String>,
    attempted_at: Option<Instant>,
}

#[derive(Debug, Default)]
struct SlowRefreshLogState {
    last_logged_at: Option<Instant>,
    suppressed_refreshes: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct SlowRefreshLogDecision {
    suppressed_refreshes: u32,
}

#[derive(Debug, Default)]
struct TimedCache<T> {
    checked_at: Option<Instant>,
    value: T,
}

fn record_refresh_phase(
    phases: &mut Vec<(&'static str, u128)>,
    phase_started: &mut Instant,
    phase: &'static str,
) {
    phases.push((phase, phase_started.elapsed().as_millis()));
    *phase_started = Instant::now();
}

fn format_snapshot_command_timings(timings: &[SnapshotCommandTiming]) -> String {
    let mut selected = timings
        .iter()
        .filter(|timing| timing.elapsed_ms >= 25 || !timing.succeeded)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        selected = timings.iter().collect();
    }

    selected
        .into_iter()
        .map(|timing| {
            format!(
                "{}:{}ms:{}",
                timing.label,
                timing.elapsed_ms,
                if timing.succeeded { "ok" } else { "err" }
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn slow_refresh_log_decision(
    state: &mut SlowRefreshLogState,
    now: Instant,
    elapsed: Duration,
    snapshot_failed: bool,
    route_mutation_requested: bool,
) -> Option<SlowRefreshLogDecision> {
    if elapsed < SLOW_REFRESH_LOG_THRESHOLD {
        return None;
    }

    let urgent =
        elapsed >= SEVERE_REFRESH_LOG_THRESHOLD || snapshot_failed || route_mutation_requested;
    if !urgent
        && state.last_logged_at.is_some_and(|last_logged_at| {
            now.saturating_duration_since(last_logged_at) < ROUTINE_SLOW_REFRESH_LOG_INTERVAL
        })
    {
        state.suppressed_refreshes = state.suppressed_refreshes.saturating_add(1);
        return None;
    }

    let decision = SlowRefreshLogDecision {
        suppressed_refreshes: state.suppressed_refreshes,
    };
    state.suppressed_refreshes = 0;
    state.last_logged_at = Some(now);
    Some(decision)
}

#[derive(Debug)]
struct MeterSupervisor {
    dry_run: bool,
    process: Option<MeterProcess>,
    native_backend: bool,
    native_meters: Vec<LevelMeter>,
    targets: BTreeMap<String, MeterTarget>,
    target_revision: Option<MeterTargetRevision>,
    last_attempt_at: Option<Instant>,
    last_requested_at: Option<Instant>,
    last_activity_logged_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MeterTargetRevision {
    config: u64,
    graph: u64,
    audio_graph_running: bool,
}

impl MeterTargetRevision {
    fn new(revisions: EngineRevisions, audio_graph_running: bool) -> Self {
        Self {
            config: revisions.config,
            graph: revisions.graph,
            audio_graph_running,
        }
    }
}

#[derive(Debug, Default)]
struct MeterSupervisorUpdate {
    meters: Vec<LevelMeter>,
    native_backend: bool,
    started: usize,
    stopped: usize,
    failed: Vec<String>,
    sampled_sources: usize,
    active_targets: usize,
    max_level: f32,
    log_activity: bool,
}

#[derive(Debug)]
struct MeterProcess {
    source_names: BTreeSet<String>,
    samples: BTreeMap<String, Arc<AtomicMeterSample>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct MeterSample {
    peak_left: f32,
    peak_right: f32,
    updated_at: Option<Instant>,
}

#[derive(Debug, Clone, Deserialize)]
struct NativeCoreMeterReading {
    id: String,
    peak_left: f32,
    peak_right: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct NativeCoreMetersResponse {
    #[serde(default)]
    channels: Vec<NativeCoreMeterReading>,
    #[serde(default)]
    mixes: Vec<NativeCoreMeterReading>,
}

#[derive(Debug)]
struct MeterTransportTracker {
    protocol_version: AtomicU32,
    connected: AtomicBool,
    slot_count: AtomicUsize,
    last_sequence: AtomicU64,
    frames_received: AtomicU64,
    connections: AtomicU64,
    disconnects: AtomicU64,
    fallback_polls: AtomicU64,
    errors: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl Default for MeterTransportTracker {
    fn default() -> Self {
        Self {
            protocol_version: AtomicU32::new(0),
            connected: AtomicBool::new(false),
            slot_count: AtomicUsize::new(0),
            last_sequence: AtomicU64::new(0),
            frames_received: AtomicU64::new(0),
            connections: AtomicU64::new(0),
            disconnects: AtomicU64::new(0),
            fallback_polls: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }
}

impl MeterTransportTracker {
    fn connected(&self, slots: usize) {
        self.protocol_version.store(
            u32::from(wavelinux_dsp::METER_STREAM_PROTOCOL_VERSION),
            Ordering::Relaxed,
        );
        self.slot_count.store(slots, Ordering::Relaxed);
        self.connected.store(true, Ordering::Release);
        self.connections.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut error) = self.last_error.lock() {
            *error = None;
        }
    }

    fn frame_received(&self, sequence: u64) {
        self.last_sequence.store(sequence, Ordering::Relaxed);
        self.frames_received.fetch_add(1, Ordering::Relaxed);
    }

    fn disconnected(&self, error: Option<String>) {
        if self.connected.swap(false, Ordering::AcqRel) {
            self.disconnects.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(error) = error {
            self.errors.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut current) = self.last_error.lock() {
                *current = Some(error);
            }
        }
    }

    fn fallback_polled(&self) {
        self.fallback_polls.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> MeterTransportStatus {
        MeterTransportStatus {
            protocol_version: self.protocol_version.load(Ordering::Relaxed) as u16,
            connected: self.connected.load(Ordering::Acquire),
            slot_count: self.slot_count.load(Ordering::Relaxed),
            last_sequence: self.last_sequence.load(Ordering::Relaxed),
            frames_received: self.frames_received.load(Ordering::Relaxed),
            connections: self.connections.load(Ordering::Relaxed),
            disconnects: self.disconnects.load(Ordering::Relaxed),
            fallback_polls: self.fallback_polls.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            last_error: self.last_error.lock().ok().and_then(|error| error.clone()),
        }
    }
}

#[derive(Debug, Clone)]
struct CoreMeterTarget {
    node_id: String,
    slot_index: usize,
    gain: f32,
}

#[derive(Debug)]
pub struct CoreMeterStream {
    stream: UnixStream,
    header: wavelinux_dsp::MeterStreamHeader,
    frame_bytes: Vec<u8>,
    target_revision: Option<MeterTargetRevision>,
    targets: Vec<CoreMeterTarget>,
    last_sequence: u64,
}

#[derive(Debug)]
struct AtomicMeterSample {
    peak_left: AtomicU32,
    peak_right: AtomicU32,
    frames: AtomicU64,
    updated_micros: AtomicU64,
    clock_started_at: Instant,
}

impl Default for AtomicMeterSample {
    fn default() -> Self {
        Self {
            peak_left: AtomicU32::new(0.0_f32.to_bits()),
            peak_right: AtomicU32::new(0.0_f32.to_bits()),
            frames: AtomicU64::new(0),
            updated_micros: AtomicU64::new(0),
            clock_started_at: Instant::now(),
        }
    }
}

impl AtomicMeterSample {
    fn publish(&self, peak_left: f32, peak_right: f32, frames: u64) {
        self.peak_left.store(peak_left.to_bits(), Ordering::Relaxed);
        self.peak_right
            .store(peak_right.to_bits(), Ordering::Relaxed);
        self.frames.fetch_add(frames, Ordering::Relaxed);
        let updated_micros = duration_micros(self.clock_started_at.elapsed()).saturating_add(1);
        self.updated_micros.store(updated_micros, Ordering::Release);
    }

    fn snapshot(&self) -> MeterSample {
        let updated_micros = self.updated_micros.load(Ordering::Acquire);
        let updated_at = (updated_micros > 0).then(|| {
            let elapsed_micros = duration_micros(self.clock_started_at.elapsed());
            let age = elapsed_micros.saturating_sub(updated_micros.saturating_sub(1));
            Instant::now()
                .checked_sub(Duration::from_micros(age))
                .unwrap_or_else(Instant::now)
        });
        MeterSample {
            peak_left: f32::from_bits(self.peak_left.load(Ordering::Relaxed)),
            peak_right: f32::from_bits(self.peak_right.load(Ordering::Relaxed)),
            updated_at,
        }
    }
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

impl MeterSupervisor {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            process: None,
            native_backend: false,
            native_meters: Vec::new(),
            targets: BTreeMap::new(),
            target_revision: None,
            last_attempt_at: None,
            last_requested_at: None,
            last_activity_logged_at: None,
        }
    }

    fn reconcile(
        &mut self,
        targets: Vec<MeterTarget>,
        mark_requested: bool,
        target_revision: MeterTargetRevision,
    ) -> MeterSupervisorUpdate {
        let mut update = MeterSupervisorUpdate::default();
        if mark_requested {
            self.last_requested_at = Some(Instant::now());
        }
        if self.dry_run || !meter_sampling_enabled() {
            update.stopped += self.active_source_count();
            self.stop_all();
            self.target_revision = Some(target_revision);
            return update;
        }

        if self.native_backend {
            update.stopped += self.active_source_count();
            self.native_backend = false;
            self.native_meters.clear();
        }

        let targets = targets
            .into_iter()
            .map(|target| (target.node_id.clone(), target))
            .collect::<BTreeMap<_, _>>();
        let source_names = targets
            .values()
            .map(|target| target.source_name.clone())
            .collect::<BTreeSet<_>>();
        self.targets = targets;
        self.target_revision = Some(target_revision);
        let now = Instant::now();
        let (source_set_changed, process_exited) = self
            .process
            .as_mut()
            .map(|process| (process.source_names != source_names, process.has_exited()))
            .unwrap_or((false, false));
        if source_set_changed || process_exited {
            update.stopped += self.active_source_count();
            self.process.take();
            self.last_attempt_at = process_exited.then_some(now);
        }

        if source_names.is_empty() {
            self.last_attempt_at = None;
        } else if self.process.is_none()
            && self
                .last_attempt_at
                .is_none_or(|attempt| now.duration_since(attempt) >= METER_RESTART_BACKOFF)
        {
            self.last_attempt_at = Some(now);
            match MeterProcess::spawn(&source_names) {
                Ok(process) => {
                    update.started += process.source_names.len();
                    self.process = Some(process);
                    self.last_attempt_at = None;
                }
                Err(err) => update.failed.push(err.to_string()),
            }
        }

        self.populate_snapshot_update(&mut update, now);
        update
    }

    fn reconcile_native(
        &mut self,
        targets: Vec<MeterTarget>,
        meters: Vec<LevelMeter>,
        mark_requested: bool,
        target_revision: MeterTargetRevision,
    ) -> MeterSupervisorUpdate {
        let mut update = MeterSupervisorUpdate::default();
        if mark_requested {
            self.last_requested_at = Some(Instant::now());
        }
        if self.dry_run {
            update.stopped += self.active_source_count();
            self.stop_all();
            self.target_revision = Some(target_revision);
            return update;
        }
        if let Some(process) = self.process.take() {
            update.stopped += process.source_names.len();
        }
        if !self.native_backend {
            update.started = meters.len();
        }
        self.native_backend = true;
        self.native_meters = meters;
        self.targets = targets
            .into_iter()
            .map(|target| (target.node_id.clone(), target))
            .collect();
        self.target_revision = Some(target_revision);
        self.last_attempt_at = None;
        self.populate_snapshot_update(&mut update, Instant::now());
        update
    }

    fn snapshot_for_revision(
        &mut self,
        target_revision: MeterTargetRevision,
        mark_requested: bool,
    ) -> Option<MeterSupervisorUpdate> {
        if self.target_revision != Some(target_revision) {
            return None;
        }
        let now = Instant::now();
        let process_exited =
            !self.native_backend && self.process.as_mut().is_some_and(MeterProcess::has_exited);
        if process_exited {
            return None;
        }
        if !self.native_backend
            && self.process.is_none()
            && !self.targets.is_empty()
            && self
                .last_attempt_at
                .is_none_or(|attempt| now.duration_since(attempt) >= METER_RESTART_BACKOFF)
        {
            return None;
        }
        if mark_requested {
            self.last_requested_at = Some(now);
        }
        let mut update = MeterSupervisorUpdate::default();
        self.populate_snapshot_update(&mut update, now);
        Some(update)
    }

    fn populate_snapshot_update(&mut self, update: &mut MeterSupervisorUpdate, now: Instant) {
        update.meters = self.snapshot();
        update.native_backend = self.native_backend;
        update.sampled_sources = if self.native_backend {
            self.native_meters.len()
        } else {
            self.process
                .as_ref()
                .map(MeterProcess::sampled_source_count)
                .unwrap_or_default()
        };
        update.active_targets = update
            .meters
            .iter()
            .filter(|meter| meter.peak_left > 0.0 || meter.peak_right > 0.0)
            .count();
        update.max_level = update
            .meters
            .iter()
            .flat_map(|meter| [meter.peak_left, meter.peak_right])
            .fold(0.0_f32, f32::max);
        if update.active_targets > 0
            && self
                .last_activity_logged_at
                .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(30))
        {
            self.last_activity_logged_at = Some(now);
            update.log_activity = true;
        }
    }

    fn snapshot_or_stop_idle(&mut self) -> MeterSupervisorUpdate {
        let now = Instant::now();
        if self.requested_recently_at(now) {
            return MeterSupervisorUpdate {
                meters: self.snapshot(),
                ..MeterSupervisorUpdate::default()
            };
        }
        let stopped = self.active_source_count();
        self.stop_all();
        MeterSupervisorUpdate {
            stopped,
            ..MeterSupervisorUpdate::default()
        }
    }

    fn requested_recently(&self) -> bool {
        self.requested_recently_at(Instant::now())
    }

    fn requested_recently_at(&self, now: Instant) -> bool {
        self.last_requested_at
            .is_some_and(|requested_at| now.duration_since(requested_at) <= METER_IDLE_STOP_AFTER)
    }

    fn snapshot(&self) -> Vec<LevelMeter> {
        if self.native_backend {
            return self.native_meters.clone();
        }
        let Some(process) = self.process.as_ref() else {
            return Vec::new();
        };
        self.targets
            .values()
            .filter_map(|target| process.level_meter(target))
            .collect()
    }

    fn active_source_count(&self) -> usize {
        if self.native_backend {
            return self.native_meters.len();
        }
        self.process
            .as_ref()
            .map(|process| process.source_names.len())
            .unwrap_or_default()
    }

    fn stop_all(&mut self) {
        self.process.take();
        self.native_backend = false;
        self.native_meters.clear();
        self.targets.clear();
        self.target_revision = None;
        self.last_attempt_at = None;
        self.last_requested_at = None;
        self.last_activity_logged_at = None;
    }
}

impl MeterProcess {
    fn spawn(source_names: &BTreeSet<String>) -> Result<Self, std::io::Error> {
        let samples = source_names
            .iter()
            .map(|source_name| (source_name.clone(), Arc::new(AtomicMeterSample::default())))
            .collect::<BTreeMap<_, _>>();
        let endpoints = samples
            .iter()
            .map(|(source_name, sample)| {
                (
                    MeterEndpoint::from_source_name(source_name),
                    Arc::clone(sample),
                )
            })
            .collect::<Vec<_>>();
        let endpoint_context = endpoints
            .iter()
            .map(|(endpoint, _)| endpoint.describe())
            .collect::<Vec<_>>()
            .join("; ");
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name(format!("{}-meters", graph_prefix()))
            .spawn(move || {
                run_pipewire_meter_group(endpoints, reader_stop, ready_tx);
            })
            .map_err(std::io::Error::other)?;

        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(Self {
                source_names: source_names.clone(),
                samples,
                stop,
                worker: Some(worker),
            }),
            Ok(Err(err)) => {
                stop.store(true, Ordering::SeqCst);
                let _ = worker.join();
                Err(std::io::Error::other(format!("{err}; {endpoint_context}")))
            }
            Err(err) => {
                stop.store(true, Ordering::SeqCst);
                let _ = worker.join();
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("PipeWire meter startup timed out: {err}"),
                ))
            }
        }
    }

    fn has_exited(&mut self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(thread::JoinHandle::is_finished)
    }

    fn sampled_source_count(&self) -> usize {
        self.samples
            .values()
            .filter(|sample| sample.frames.load(Ordering::Relaxed) > 0)
            .count()
    }

    fn level_meter(&self, target: &MeterTarget) -> Option<LevelMeter> {
        let sample = self.samples.get(&target.source_name)?.snapshot();
        let now = Instant::now();
        let gain = if target.muted { 0.0 } else { target.gain }.clamp(0.0, 1.5);
        Some(LevelMeter {
            node_id: target.node_id.clone(),
            peak_left: meter_output_level(
                stale_adjusted_meter_peak(sample.peak_left, sample.updated_at, now),
                gain,
            ),
            peak_right: meter_output_level(
                stale_adjusted_meter_peak(sample.peak_right, sample.updated_at, now),
                gain,
            ),
        })
    }
}

impl Drop for MeterProcess {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_pipewire_meter_group(
    endpoints: Vec<(MeterEndpoint, Arc<AtomicMeterSample>)>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    if let Err(err) = run_pipewire_meter_group_inner(endpoints, stop, ready.clone()) {
        let _ = ready.send(Err(err));
    }
}

fn run_pipewire_meter_group_inner(
    endpoints: Vec<(MeterEndpoint, Arc<AtomicMeterSample>)>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|err| format!("PipeWire meter mainloop creation failed: {err}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|err| format!("PipeWire meter context creation failed: {err}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|err| format!("PipeWire meter core connection failed: {err}"))?;
    let mut streams = Vec::with_capacity(endpoints.len());
    let mut listeners = Vec::with_capacity(endpoints.len());
    for (endpoint, sample) in endpoints {
        let endpoint_context = endpoint.describe();
        let mut props = meter_stream_properties(&endpoint);
        if endpoint.dont_remix {
            props.insert(*pw::keys::STREAM_DONT_REMIX, "true");
        }
        if endpoint.dont_reconnect {
            props.insert(*pw::keys::NODE_DONT_RECONNECT, "true");
        }
        if endpoint.capture_sink_monitor {
            props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
        }

        let stream = pw::stream::StreamBox::new(&core, &format!("{}-meter", graph_prefix()), props)
            .map_err(|err| format!("PipeWire meter stream creation failed: {err}"))?;
        let listener = stream
            .add_local_listener_with_user_data(PipeWireMeterData {
                format: Default::default(),
                sample,
            })
            .param_changed(|_, user_data, id, param| {
                parse_meter_audio_format(id, param, &mut user_data.format);
            })
            .process(|stream, user_data| {
                process_pipewire_meter_buffer(stream, user_data);
            })
            .register()
            .map_err(|err| err.to_string())?;
        let format = meter_audio_format_pod()?;
        let mut params = [spa::pod::Pod::from_bytes(&format)
            .ok_or_else(|| "PipeWire meter format pod was invalid".to_string())?];
        let mut flags = pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS;
        if endpoint.dont_reconnect {
            flags |= pw::stream::StreamFlags::DONT_RECONNECT;
        }
        stream
            .connect(spa::utils::Direction::Input, None, flags, &mut params)
            .map_err(|err| {
                format!("PipeWire meter stream connect failed: {err}; {endpoint_context}")
            })?;
        listeners.push(listener);
        streams.push(stream);
    }
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        mainloop.loop_().iterate(METER_MAINLOOP_POLL_INTERVAL);
    }

    drop(listeners);
    drop(streams);

    Ok(())
}

fn meter_stream_properties(endpoint: &MeterEndpoint) -> pw::properties::PropertiesBox {
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_NAME => format!("{} VU Meter", app_display_name()),
        *pw::keys::NODE_NAME => format!("{}-meter-{}", graph_prefix(), safe_file_id(&endpoint.source_name)),
        *pw::keys::NODE_DESCRIPTION => format!("{} meter for {}", app_display_name(), endpoint.source_name),
        *pw::keys::NODE_VIRTUAL => "true",
        *pw::keys::NODE_PASSIVE => "true",
        *pw::keys::TARGET_OBJECT => endpoint.target_object.clone(),
    };
    props.insert("application.name", app_display_name());
    // Meters are display-only clients. A larger quantum avoids forcing the
    // microphone graph into extra low-latency wakeups while the UI is open.
    props.insert("node.latency", METER_STREAM_LATENCY);
    props.insert("node.dont-move", "true");
    props.insert("state.restore-props", "false");
    props.insert("state.restore-target", "false");
    props.insert(
        "module-stream-restore.id",
        meter_stream_restore_id(endpoint),
    );
    props.insert(graph_prop("managed"), "1");
    props.insert(graph_prop("role"), "meter");
    props
}

fn meter_stream_restore_id(endpoint: &MeterEndpoint) -> String {
    format!(
        "{}-meter-{}",
        graph_prefix(),
        safe_file_id(&endpoint.source_name)
    )
}

fn parse_meter_audio_format(
    id: u32,
    param: Option<&spa::pod::Pod>,
    format: &mut spa::param::audio::AudioInfoRaw,
) {
    let Some(param) = param else {
        return;
    };
    if id != spa::param::ParamType::Format.as_raw() {
        return;
    }
    let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param) else {
        return;
    };
    if media_type == spa::param::format::MediaType::Audio
        && media_subtype == spa::param::format::MediaSubtype::Raw
    {
        let _ = format.parse(param);
    }
}

fn process_pipewire_meter_buffer(stream: &pw::stream::Stream, user_data: &mut PipeWireMeterData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }
    let data = &mut datas[0];
    let chunk = data.chunk();
    let offset = chunk.offset() as usize;
    let size = chunk.size() as usize;
    let channels = user_data.format.channels().max(1) as usize;
    let Some(bytes) = data.data() else {
        return;
    };
    let Some(end) = offset.checked_add(size) else {
        return;
    };
    if end <= bytes.len() {
        consume_meter_interleaved_f32le(&bytes[offset..end], channels, &user_data.sample);
    }
}

fn meter_audio_format_pod() -> Result<Vec<u8>, String> {
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(48_000);
    audio_info.set_channels(2);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    Ok(spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|err| err.to_string())?
    .0
    .into_inner())
}

struct PipeWireMeterData {
    format: spa::param::audio::AudioInfoRaw,
    sample: Arc<AtomicMeterSample>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MeterEndpoint {
    source_name: String,
    target_object: String,
    capture_sink_monitor: bool,
    dont_reconnect: bool,
    dont_remix: bool,
}

impl MeterEndpoint {
    fn from_source_name(source_name: &str) -> Self {
        if let Some(sink_name) = source_name.strip_suffix(".monitor") {
            return Self {
                source_name: source_name.into(),
                target_object: sink_name.into(),
                capture_sink_monitor: true,
                dont_reconnect: true,
                dont_remix: true,
            };
        }

        Self {
            source_name: source_name.into(),
            target_object: source_name.into(),
            capture_sink_monitor: false,
            dont_reconnect: true,
            dont_remix: false,
        }
    }

    fn describe(&self) -> String {
        format!(
            "source={} target={} capture_sink={} dont_reconnect={} dont_remix={}",
            self.source_name,
            self.target_object,
            self.capture_sink_monitor,
            self.dont_reconnect,
            self.dont_remix
        )
    }
}

#[cfg(test)]
fn consume_meter_bytes(bytes: &[u8], pending: &mut Vec<u8>, sample: &Arc<AtomicMeterSample>) {
    pending.extend_from_slice(bytes);
    let frame_bytes = (pending.len() / 8) * 8;
    if frame_bytes == 0 {
        return;
    }

    consume_meter_interleaved_f32le(&pending[..frame_bytes], 2, sample);
    pending.drain(..frame_bytes);
}

fn consume_meter_interleaved_f32le(bytes: &[u8], channels: usize, sample: &Arc<AtomicMeterSample>) {
    if channels == 0 {
        return;
    }
    let sample_size = mem::size_of::<f32>();
    let frame_size = sample_size.saturating_mul(channels);
    if frame_size == 0 {
        return;
    }
    let frame_bytes = (bytes.len() / frame_size) * frame_size;
    if frame_bytes == 0 {
        return;
    }

    let mut sum_left = 0.0_f32;
    let mut sum_right = 0.0_f32;
    let mut frames = 0_u64;
    for frame in bytes[..frame_bytes].chunks_exact(frame_size) {
        let left = f32::from_le_bytes(frame[0..sample_size].try_into().unwrap_or_default());
        let right = if channels > 1 {
            f32::from_le_bytes(
                frame[sample_size..sample_size * 2]
                    .try_into()
                    .unwrap_or_default(),
            )
        } else {
            left
        };
        if left.is_finite() {
            sum_left += left * left;
        }
        if right.is_finite() {
            sum_right += right * right;
        }
        frames += 1;
    }

    let incoming_left = if frames > 0 {
        (sum_left / frames as f32).sqrt()
    } else {
        0.0
    };
    let incoming_right = if frames > 0 {
        (sum_right / frames as f32).sqrt()
    } else {
        0.0
    };

    sample.publish(
        gate_meter_peak(incoming_left),
        gate_meter_peak(incoming_right),
        frames,
    );
}

fn channel_id_from_bus_meter_id(node_id: &str) -> Option<&str> {
    node_id
        .strip_prefix("channel:")?
        .split_once(":mix:")
        .map(|(channel_id, _)| channel_id)
        .filter(|channel_id| !channel_id.is_empty())
}

fn level_meters_from_native_response(
    targets: &[MeterTarget],
    response: NativeCoreMetersResponse,
) -> Vec<LevelMeter> {
    let channels = response
        .channels
        .into_iter()
        .map(|meter| (meter.id.clone(), meter))
        .collect::<BTreeMap<_, _>>();
    let mixes = response
        .mixes
        .into_iter()
        .map(|meter| (meter.id.clone(), meter))
        .collect::<BTreeMap<_, _>>();

    targets
        .iter()
        .filter_map(|target| {
            let bus_channel_id = channel_id_from_bus_meter_id(&target.node_id);
            let (reading, gain) = if let Some(channel_id) = bus_channel_id {
                (
                    channels.get(channel_id)?,
                    if target.muted { 0.0 } else { target.gain },
                )
            } else if let Some(reading) = channels.get(&target.node_id) {
                (reading, if target.muted { 0.0 } else { target.gain })
            } else {
                // Native mix meters already include bus, master-volume, and
                // mute state. Applying the target gain here would square the
                // configured mix volume.
                (mixes.get(&target.node_id)?, 1.0)
            };
            Some(LevelMeter {
                node_id: target.node_id.clone(),
                peak_left: meter_output_level(reading.peak_left, gain),
                peak_right: meter_output_level(reading.peak_right, gain),
            })
        })
        .collect()
}

fn meter_output_level(peak: f32, gain: f32) -> f32 {
    if gain <= 0.0 {
        return 0.0;
    }
    let level = (peak * gain).clamp(0.0, 1.0);
    if level < METER_NOISE_FLOOR {
        return 0.0;
    }

    let db = 20.0 * level.log10();
    let normalized = ((db - METER_DISPLAY_FLOOR_DB)
        / (METER_DISPLAY_CEILING_DB - METER_DISPLAY_FLOOR_DB))
        .clamp(0.0, 1.0);
    normalized.powf(METER_DISPLAY_EXPONENT)
}

fn stale_adjusted_meter_peak(peak: f32, updated_at: Option<Instant>, now: Instant) -> f32 {
    let Some(updated_at) = updated_at else {
        return 0.0;
    };
    let peak = gate_meter_peak(peak);
    if peak == 0.0 {
        return 0.0;
    }
    let stale_age = now
        .saturating_duration_since(updated_at)
        .checked_sub(METER_STALE_AFTER)
        .unwrap_or_default();
    if stale_age.is_zero() {
        return peak;
    }

    let adjusted = peak * METER_STALE_RELEASE_PER_SECOND.powf(stale_age.as_secs_f32());
    gate_meter_peak(adjusted)
}

fn gate_meter_peak(peak: f32) -> f32 {
    if !peak.is_finite() {
        return 0.0;
    }
    let peak = peak.clamp(0.0, 1.0);
    if peak < METER_NOISE_FLOOR {
        0.0
    } else {
        peak
    }
}

fn plan_channel_bus_volume_commands(
    sink_input_id: Option<&str>,
    source_output_id: Option<&str>,
    volume: f32,
) -> Vec<CommandSpec> {
    let mut commands = Vec::new();
    if let Some(sink_input_id) = sink_input_id {
        commands.push(plan_set_channel_bus_volume(sink_input_id, volume));
        if let Some(source_output_id) = source_output_id {
            commands.push(plan_set_channel_bus_source_output_volume(
                source_output_id,
                1.0,
            ));
        }
    } else if let Some(source_output_id) = source_output_id {
        commands.push(plan_set_channel_bus_source_output_volume(
            source_output_id,
            volume,
        ));
    }
    commands
}

fn plan_channel_bus_mute_commands(
    sink_input_id: Option<&str>,
    source_output_id: Option<&str>,
    muted: bool,
) -> Vec<CommandSpec> {
    let mut commands = Vec::new();
    if let Some(sink_input_id) = sink_input_id {
        commands.push(plan_set_channel_bus_mute(sink_input_id, muted));
    }
    if let Some(source_output_id) = source_output_id {
        commands.push(plan_set_channel_bus_source_output_mute(
            source_output_id,
            muted,
        ));
    }
    commands
}

fn graph_sink_level_commands(
    config: &MixerConfig,
    sink_levels: &BTreeMap<String, SinkLevelState>,
) -> Vec<CommandSpec> {
    let mut commands = Vec::new();
    for mix in config
        .mixes
        .iter()
        .filter(|mix| !mix_uses_persistent_audio_core(mix))
    {
        let desired_percent = (mix.volume.clamp(0.0, 1.0) * 100.0).round() as u8;
        let current = sink_levels.get(&mix.virtual_sink_name);
        if current.and_then(|level| level.volume_percent) != Some(desired_percent) {
            commands.push(plan_pw_set_mix_volume(mix, mix.volume));
        }
        if current.map(|level| level.muted) != Some(mix.muted) {
            commands.push(plan_pw_set_mix_mute(mix, mix.muted));
        }
    }

    for channel in &config.channels {
        let current = sink_levels.get(&channel.virtual_sink_name);
        if current.and_then(|level| level.volume_percent) != Some(100) {
            commands.push(plan_set_managed_sink_volume(
                &channel.virtual_sink_name,
                1.0,
            ));
        }
        if current.map(|level| level.muted) != Some(false) {
            commands.push(plan_set_managed_sink_mute(
                &channel.virtual_sink_name,
                false,
            ));
        }
    }
    commands
}

fn managed_route_level_commands(
    config: &MixerConfig,
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
) -> Vec<CommandSpec> {
    let mut commands = Vec::new();

    for sink_input in sink_inputs {
        let Some(expected) = expected_managed_route_level_for_parts(
            config,
            sink_input.role.as_deref(),
            sink_input.channel_id.as_deref(),
            sink_input.mix_id.as_deref(),
        ) else {
            continue;
        };
        if route_mute_mismatch(sink_input.muted, expected.muted) {
            commands.push(plan_set_route_sink_input_mute(
                &sink_input.id,
                expected.muted,
            ));
        }
        if route_volume_mismatch(sink_input.volume_percent, expected.sink_input_percent) {
            commands.push(plan_set_route_sink_input_volume(
                &sink_input.id,
                f32::from(expected.sink_input_percent) / 100.0,
            ));
        }
    }

    for source_output in source_outputs {
        let Some(expected) = expected_managed_route_level_for_parts(
            config,
            source_output.role.as_deref(),
            source_output.channel_id.as_deref(),
            source_output.mix_id.as_deref(),
        ) else {
            continue;
        };
        if route_mute_mismatch(source_output.muted, expected.muted) {
            commands.push(plan_set_route_source_output_mute(
                &source_output.id,
                expected.muted,
            ));
        }
        if route_volume_mismatch(source_output.volume_percent, expected.source_output_percent) {
            commands.push(plan_set_route_source_output_volume(
                &source_output.id,
                f32::from(expected.source_output_percent) / 100.0,
            ));
        }
    }

    commands
}

#[derive(Debug)]
struct EffectChainProcess {
    program: String,
    child: Child,
    config_revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AudioSubscriptionEvent {
    PlaybackStream,
    CaptureStream,
    Device,
}

#[derive(Debug, Default, Deserialize)]
struct AudioCoreDiagnosticsResponse {
    #[serde(default)]
    protocol_version: u16,
    #[serde(default)]
    route_id: String,
    #[serde(default)]
    core_topology_revision: String,
    #[serde(default = "default_sample_rate_hz")]
    sample_rate_hz: u32,
    #[serde(default)]
    target_latency_msec: u16,
    #[serde(default)]
    current_buffer_frames: u64,
    #[serde(default)]
    captured_frames: u64,
    #[serde(default)]
    rendered_frames: u64,
    #[serde(default)]
    dropped_frames: u64,
    #[serde(default)]
    underrun_frames: u64,
    #[serde(default)]
    capture_callbacks: u64,
    #[serde(default)]
    worker_running: bool,
    #[serde(default)]
    worker_blocks: u64,
    #[serde(default)]
    worker_queue_frames: u64,
    #[serde(default)]
    worker_queue_capacity_frames: u64,
    #[serde(default)]
    worker_overrun_frames: u64,
    #[serde(default)]
    accelerator_provider: Option<String>,
    #[serde(default)]
    accelerator_active_states: u32,
    #[serde(default)]
    accelerator_provider_pids: Vec<u32>,
    #[serde(default)]
    accelerator_provider_blocks: u64,
    #[serde(default)]
    accelerator_fallback_blocks: u64,
    #[serde(default)]
    accelerator_deadline_misses: u64,
    #[serde(default)]
    accelerator_invalid_results: u64,
    #[serde(default)]
    accelerator_stale_results: u64,
    #[serde(default)]
    accelerator_disabled_states: u32,
    #[serde(default)]
    accelerator_startup_failures: Vec<String>,
    #[serde(default)]
    accelerator_last_failure: Option<String>,
    #[serde(default)]
    last_process_micros: u64,
    #[serde(default)]
    max_process_micros: u64,
    #[serde(default)]
    chain_swaps: u64,
    #[serde(default)]
    non_finite_blocks: u64,
    #[serde(default)]
    non_finite_samples: u64,
    #[serde(default)]
    non_finite_effect_mask: u64,
    #[serde(default)]
    chain_recoveries: u64,
    #[serde(default)]
    chain_swap_replacements: u64,
    #[serde(default)]
    retired_chain_overflows: u64,
    #[serde(default)]
    submitted_generation: u64,
    #[serde(default)]
    acknowledged_generation: u64,
    #[serde(default)]
    submitted_route_generation: u64,
    #[serde(default)]
    applied_route_generation: u64,
    #[serde(default)]
    input_target_node_name: Option<String>,
    #[serde(default)]
    output_target_node_names: Vec<String>,
    #[serde(default)]
    route_target_error: Option<String>,
    #[serde(default = "default_rate_correction")]
    rate_correction: f64,
}

fn default_sample_rate_hz() -> u32 {
    wavelinux_model::SAMPLE_RATE_HZ
}

fn default_rate_correction() -> f64 {
    1.0
}

#[derive(Debug)]
pub struct WaveLinuxEngine {
    paths: EnginePaths,
    app_version: String,
    options: EngineOptions,
    pw: PwClient,
    startup_defaults: DefaultDevices,
    config: RwLock<MixerConfig>,
    persisted_config_revision: Mutex<Option<String>>,
    runtime: RwLock<RuntimeCache>,
    change_signal: EngineChangeSignal,
    meter_supervisor: Mutex<MeterSupervisor>,
    meter_transport: MeterTransportTracker,
    effect_chain_processes: Mutex<BTreeMap<String, EffectChainProcess>>,
    effect_chain_revisions: Mutex<BTreeMap<String, String>>,
    audio_event_subscription: Mutex<Option<Child>>,
    pipewire_registry: PipeWireRegistryCache,
    peripheral_plugins: Mutex<BTreeMap<String, PeripheralPluginStatus>>,
    pipewire_health_subscription: Mutex<Option<Child>>,
    pipewire_profiler_subscription: Mutex<Option<Child>>,
    pipewire_audio_health: PipeWireAudioHealthTracker,
    runtime_refresh: Mutex<()>,
    host_diagnostics: Mutex<TimedCache<Vec<Diagnostic>>>,
    effect_availability: Mutex<TimedCache<Vec<EffectAvailability>>>,
    hardware_profiles: Arc<Mutex<TimedCache<HardwareProfileCatalog>>>,
    accelerator_status: Mutex<TimedCache<Vec<AcceleratorProviderStatus>>>,
    remote_profile_sync: Arc<Mutex<RemoteProfileSyncState>>,
    slow_refresh_log: Mutex<SlowRefreshLogState>,
    audio_commands: Mutex<()>,
    effect_sync_active: AtomicBool,
    capture_move_failures: Mutex<BTreeMap<String, CaptureMoveFailure>>,
    app_stream_move_failures: Mutex<BTreeMap<String, Instant>>,
    effect_updates: Mutex<BTreeMap<String, Arc<EffectUpdateSlot>>>,
    effect_config_writes: Mutex<()>,
    deferred_graph_repair: Mutex<DeferredGraphRepair>,
    route_health_repair: Mutex<RouteHealthRepairState>,
    adaptive_latency: Mutex<AdaptiveLatencyController>,
    adaptive_quantum: Mutex<AdaptiveQuantumController>,
    adaptive_pipewire_health_counters: Mutex<PipeWireAudioHealthStatus>,
    cpu_pressure_sampler: Mutex<CpuPressureSampler>,
    audio_core_underrun_counters: Mutex<BTreeMap<String, u64>>,
    adaptive_core_discontinuity_counters: Mutex<BTreeMap<String, u64>>,
    startup_repair_pending: AtomicBool,
    startup_initialization_in_progress: AtomicBool,
    stop: AtomicBool,
}

#[derive(Debug, Clone)]
struct PendingEffectUpdate {
    generation: u64,
    channel: Channel,
}

#[derive(Debug)]
struct EffectApplyAcknowledgement {
    generation: u64,
    config_revision: String,
    chain_swaps: u64,
}

#[derive(Debug)]
struct EffectUpdateState {
    status: EffectRuntimeStatus,
    desired: PendingEffectUpdate,
    in_flight_generation: Option<u64>,
    worker_running: bool,
    coalesced_requests: u64,
    recovery_not_before: Option<Instant>,
}

#[derive(Debug)]
struct EffectEnqueueDecision {
    generation: u64,
    previous_acknowledged: u64,
    coalesced: bool,
    start_worker: bool,
    control_socket: String,
}

#[derive(Debug)]
struct EffectAttemptCompletion {
    superseded: bool,
    final_state: EffectRuntimeState,
    final_error: Option<String>,
}

impl EffectUpdateState {
    fn enqueue(&mut self, channel: Channel) -> Result<EffectEnqueueDecision, String> {
        let generation = self
            .desired
            .generation
            .checked_add(1)
            .ok_or_else(|| "effect generation counter is exhausted".to_string())?;
        let coalesced = self.worker_running;
        if coalesced {
            self.coalesced_requests = self.coalesced_requests.saturating_add(1);
        }
        self.desired = PendingEffectUpdate {
            generation,
            channel: channel.clone(),
        };
        self.status.selected_effect_count = channel.effects.len();
        self.status.desired_enabled = channel_effects_desired_enabled(&channel);
        self.status.desired_generation = generation;
        self.status.coalesced_requests = self.coalesced_requests;
        self.status.pending = true;
        self.status.last_error = None;
        self.recovery_not_before = None;
        self.status.resolve_state();
        let decision = EffectEnqueueDecision {
            generation,
            previous_acknowledged: self.status.applied_generation,
            coalesced,
            start_worker: !self.worker_running,
            control_socket: self.status.control_socket.clone(),
        };
        self.worker_running = true;
        Ok(decision)
    }

    fn begin_latest(&mut self) -> PendingEffectUpdate {
        let desired = self.desired.clone();
        self.in_flight_generation = Some(desired.generation);
        self.status.in_flight_generation = Some(desired.generation);
        desired
    }

    fn finish_attempt(
        &mut self,
        attempted_generation: u64,
        result: &Result<EffectApplyAcknowledgement, String>,
    ) -> EffectAttemptCompletion {
        self.in_flight_generation = None;
        self.status.in_flight_generation = None;
        if self.desired.generation != attempted_generation {
            return EffectAttemptCompletion {
                superseded: true,
                final_state: self.status.state,
                final_error: None,
            };
        }
        match result {
            Ok(ack) => {
                self.status.applied_generation = ack.generation;
                self.status.core_healthy = true;
                self.status.pending = false;
                self.status.last_error = None;
                self.recovery_not_before = None;
            }
            Err(error) => {
                self.status.core_healthy = false;
                self.status.pending = false;
                self.status.last_error = Some(error.clone());
                self.recovery_not_before = Some(Instant::now() + EFFECT_RECOVERY_RETRY_INTERVAL);
            }
        }
        self.status.resolve_state();
        self.worker_running = false;
        EffectAttemptCompletion {
            superseded: false,
            final_state: self.status.state,
            final_error: self.status.last_error.clone(),
        }
    }

    fn reserve_recovery_worker(&mut self, now: Instant) -> bool {
        if self.worker_running
            || !self.status.core_healthy
            || !self.status.pending
            || self.status.applied_generation == self.desired.generation
            || self
                .recovery_not_before
                .is_some_and(|not_before| now < not_before)
        {
            return false;
        }

        self.worker_running = true;
        self.status.pending = true;
        self.status.resolve_state();
        true
    }

    fn record_worker_spawn_failure(&mut self, error: String) {
        self.worker_running = false;
        self.in_flight_generation = None;
        self.status.in_flight_generation = None;
        self.status.pending = false;
        self.status.core_healthy = false;
        self.status.last_error = Some(error);
        self.recovery_not_before = Some(Instant::now() + EFFECT_RECOVERY_RETRY_INTERVAL);
        self.status.resolve_state();
    }
}

#[derive(Debug)]
struct EffectUpdateSlot {
    state: Mutex<EffectUpdateState>,
}

impl EffectUpdateSlot {
    fn new(channel: Channel, control_socket: &Path) -> Self {
        let desired_enabled = channel_effects_desired_enabled(&channel);
        let mut status = EffectRuntimeStatus {
            channel_id: channel.id.clone(),
            selected_effect_count: channel.effects.len(),
            desired_enabled,
            desired_generation: 1,
            pending: desired_enabled,
            control_socket: control_socket.to_string_lossy().into_owned(),
            ..EffectRuntimeStatus::default()
        };
        status.resolve_state();
        Self {
            state: Mutex::new(EffectUpdateState {
                desired: PendingEffectUpdate {
                    generation: 1,
                    channel,
                },
                status,
                in_flight_generation: None,
                worker_running: false,
                coalesced_requests: 0,
                recovery_not_before: None,
            }),
        }
    }
}

#[derive(Debug, Default)]
struct DeferredGraphRepair {
    generation: u64,
}

struct EffectSyncActiveGuard<'a> {
    active: &'a AtomicBool,
}

impl Drop for EffectSyncActiveGuard<'_> {
    fn drop(&mut self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone)]
struct CaptureMoveFailure {
    failed_at: Instant,
    attempts: u32,
    signature: String,
}

#[derive(Debug, Default)]
struct RemoteProfileSyncState {
    in_flight: bool,
    last_started: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdaptiveLatencySignal {
    Clean,
    CpuPressure,
    PipeWireTrouble,
    AudioTrouble,
}

#[derive(Debug)]
struct AdaptiveLatencyController {
    active_level: usize,
    last_change: Instant,
    clean_since: Option<Instant>,
    pipewire_trouble_since: Option<Instant>,
    last_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdaptiveQuantumFloorCache {
    version: u32,
    floors: BTreeMap<String, u32>,
}

impl Default for AdaptiveQuantumFloorCache {
    fn default() -> Self {
        Self {
            version: ADAPTIVE_QUANTUM_FLOORS_VERSION,
            floors: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default)]
struct AdaptiveQuantumController {
    output_signature: String,
    applied_quantum_frames: u32,
    last_decrease: Option<(Instant, u32)>,
    learned_floors: BTreeMap<String, u32>,
}

impl AdaptiveQuantumController {
    fn with_learned_floors(learned_floors: BTreeMap<String, u32>) -> Self {
        Self {
            learned_floors,
            ..Self::default()
        }
    }

    fn update(
        &mut self,
        desired_quantum_frames: u32,
        underrun_delta: u64,
        output_signature: &str,
        now: Instant,
    ) -> (u32, u32, bool) {
        if self.output_signature != output_signature {
            self.output_signature = output_signature.to_string();
            self.applied_quantum_frames = 0;
            self.last_decrease = None;
        }

        let mut learned_new_floor = false;
        if underrun_delta > 0 {
            if let Some((decreased_at, previous_quantum)) = self.last_decrease {
                if now.duration_since(decreased_at) <= Duration::from_secs(3)
                    && self.output_signature != "<no-monitor-output>"
                {
                    let floor = self
                        .learned_floors
                        .entry(self.output_signature.clone())
                        .or_default();
                    if previous_quantum > *floor {
                        *floor = previous_quantum;
                        learned_new_floor = true;
                    }
                    self.last_decrease = None;
                }
            }
        }

        let floor = self
            .learned_floors
            .get(&self.output_signature)
            .copied()
            .unwrap_or(0);
        let applied = desired_quantum_frames.max(floor);
        if applied < self.applied_quantum_frames {
            self.last_decrease = Some((now, self.applied_quantum_frames));
        }
        self.applied_quantum_frames = applied;
        (applied, floor, learned_new_floor)
    }
}

impl Default for AdaptiveLatencyController {
    fn default() -> Self {
        Self {
            active_level: 0,
            last_change: Instant::now(),
            clean_since: None,
            pipewire_trouble_since: None,
            last_reason: "initial".into(),
        }
    }
}

impl AdaptiveLatencyController {
    fn update(
        &mut self,
        settings: &wavelinux_model::AdaptiveLatencySettings,
        signal: AdaptiveLatencySignal,
        cpu_pressure: f32,
        pipewire_warning_delta: u64,
        underrun_delta: u64,
        now: Instant,
    ) -> AdaptiveLatencyStatus {
        let levels = normalized_adaptive_levels(settings);
        if !settings.enabled || levels.is_empty() {
            self.active_level = 0;
            self.last_reason = "disabled".into();
            return AdaptiveLatencyStatus {
                enabled: false,
                target_msec: settings.min_msec,
                active_level: 0,
                min_msec: settings.min_msec,
                max_msec: settings.max_msec,
                buffer_fill_msec: None,
                last_reason: self.last_reason.clone(),
                underrun_delta: 0,
                pipewire_warning_delta: 0,
                cpu_pressure: 0.0,
                pipewire_quantum_frames: 0,
                pipewire_quantum_floor_frames: 0,
            };
        }

        self.active_level = self.active_level.min(levels.len().saturating_sub(1));
        match signal {
            AdaptiveLatencySignal::AudioTrouble => {
                self.clean_since = None;
                self.pipewire_trouble_since = None;
                if now.duration_since(self.last_change) >= Duration::from_millis(250) {
                    let next = self
                        .active_level
                        .saturating_add(2)
                        .min(levels.len().saturating_sub(1));
                    if next != self.active_level {
                        self.active_level = next;
                        self.last_change = now;
                        self.last_reason = "audio_trouble".into();
                    }
                }
            }
            AdaptiveLatencySignal::PipeWireTrouble => {
                self.clean_since = None;
                let trouble_since = *self.pipewire_trouble_since.get_or_insert(now);
                if now.duration_since(trouble_since) >= Duration::from_secs(2)
                    && now.duration_since(self.last_change) >= Duration::from_secs(1)
                {
                    let next = self
                        .active_level
                        .saturating_add(1)
                        .min(levels.len().saturating_sub(1));
                    if next != self.active_level {
                        self.active_level = next;
                        self.last_change = now;
                        self.last_reason = "pipewire_trouble".into();
                    }
                }
            }
            AdaptiveLatencySignal::CpuPressure => {
                self.clean_since = None;
                self.pipewire_trouble_since = None;
                if now.duration_since(self.last_change) >= Duration::from_secs(1) {
                    let next = self
                        .active_level
                        .saturating_add(1)
                        .min(levels.len().saturating_sub(1));
                    if next != self.active_level {
                        self.active_level = next;
                        self.last_change = now;
                        self.last_reason = "cpu_pressure".into();
                    }
                }
            }
            AdaptiveLatencySignal::Clean => {
                self.pipewire_trouble_since = None;
                let clean_since = *self.clean_since.get_or_insert(now);
                if now.duration_since(clean_since) >= Duration::from_secs(30)
                    && self.active_level > 0
                    && now.duration_since(self.last_change) >= Duration::from_secs(15)
                {
                    self.active_level -= 1;
                    self.last_change = now;
                    self.last_reason = "clean_recovery".into();
                }
            }
        }

        AdaptiveLatencyStatus {
            enabled: true,
            target_msec: levels[self.active_level],
            active_level: self.active_level,
            min_msec: settings.min_msec,
            max_msec: settings.max_msec,
            buffer_fill_msec: None,
            last_reason: self.last_reason.clone(),
            underrun_delta,
            pipewire_warning_delta,
            cpu_pressure,
            pipewire_quantum_frames: wavelinux_dsp::adaptive_pipewire_quantum_frames(
                levels[self.active_level],
            ),
            pipewire_quantum_floor_frames: 0,
        }
    }
}

impl WaveLinuxEngine {
    pub fn from_xdg() -> Result<Arc<Self>, EngineError> {
        Self::from_xdg_for_app_version(env!("CARGO_PKG_VERSION"))
    }

    pub fn from_xdg_for_app_version(app_version: &str) -> Result<Arc<Self>, EngineError> {
        let paths = EnginePaths::from_xdg()?;
        maintain_logs_for_paths(&paths, app_version)?;
        Self::new_with_app_version(paths, EngineOptions::default(), app_version)
    }

    pub fn new(paths: EnginePaths, options: EngineOptions) -> Result<Arc<Self>, EngineError> {
        Self::new_with_app_version(paths, options, env!("CARGO_PKG_VERSION"))
    }

    pub fn new_with_app_version(
        paths: EnginePaths,
        options: EngineOptions,
        app_version: &str,
    ) -> Result<Arc<Self>, EngineError> {
        fs::create_dir_all(&paths.config_dir)?;
        fs::create_dir_all(&paths.data_dir)?;
        fs::create_dir_all(paths.local_hardware_profiles_dir())?;
        create_private_runtime_dir(&paths.runtime_dir)?;
        create_private_runtime_dir(&paths.control_sockets_dir())?;
        let loaded_config = load_config(&paths)?;
        let config = loaded_config.clone().normalized()?;
        if config != loaded_config {
            write_json(&paths.config_file(), &config)?;
        }
        let (adaptive_quantum, adaptive_quantum_load_error) =
            match load_adaptive_quantum_floors(&paths.adaptive_quantum_floors_file()) {
                Ok(floors) => (AdaptiveQuantumController::with_learned_floors(floors), None),
                Err(error) => (
                    AdaptiveQuantumController::default(),
                    Some(error.to_string()),
                ),
            };
        let pw = PwClient::new(options.dry_run);
        let startup_defaults = DefaultDevices::capture(&pw);
        let engine = Arc::new(Self {
            app_version: app_version.trim().to_string(),
            pw,
            startup_defaults,
            runtime: RwLock::new(RuntimeCache::new(options.dry_run)),
            config: RwLock::new(config),
            persisted_config_revision: Mutex::new(None),
            change_signal: EngineChangeSignal::default(),
            meter_supervisor: Mutex::new(MeterSupervisor::new(options.dry_run)),
            meter_transport: MeterTransportTracker::default(),
            effect_chain_processes: Mutex::new(BTreeMap::new()),
            effect_chain_revisions: Mutex::new(BTreeMap::new()),
            audio_event_subscription: Mutex::new(None),
            pipewire_registry: PipeWireRegistryCache::default(),
            peripheral_plugins: Mutex::new(BTreeMap::new()),
            pipewire_health_subscription: Mutex::new(None),
            pipewire_profiler_subscription: Mutex::new(None),
            pipewire_audio_health: PipeWireAudioHealthTracker::default(),
            runtime_refresh: Mutex::new(()),
            host_diagnostics: Mutex::new(TimedCache::default()),
            effect_availability: Mutex::new(TimedCache::default()),
            hardware_profiles: Arc::new(Mutex::new(TimedCache::default())),
            accelerator_status: Mutex::new(TimedCache::default()),
            remote_profile_sync: Arc::new(Mutex::new(RemoteProfileSyncState::default())),
            slow_refresh_log: Mutex::new(SlowRefreshLogState::default()),
            audio_commands: Mutex::new(()),
            effect_sync_active: AtomicBool::new(false),
            capture_move_failures: Mutex::new(BTreeMap::new()),
            app_stream_move_failures: Mutex::new(BTreeMap::new()),
            effect_updates: Mutex::new(BTreeMap::new()),
            effect_config_writes: Mutex::new(()),
            deferred_graph_repair: Mutex::new(DeferredGraphRepair::default()),
            route_health_repair: Mutex::new(RouteHealthRepairState::default()),
            adaptive_latency: Mutex::new(AdaptiveLatencyController::default()),
            adaptive_quantum: Mutex::new(adaptive_quantum),
            adaptive_pipewire_health_counters: Mutex::new(PipeWireAudioHealthStatus::default()),
            cpu_pressure_sampler: Mutex::new(CpuPressureSampler::default()),
            audio_core_underrun_counters: Mutex::new(BTreeMap::new()),
            adaptive_core_discontinuity_counters: Mutex::new(BTreeMap::new()),
            startup_repair_pending: AtomicBool::new(false),
            startup_initialization_in_progress: AtomicBool::new(false),
            paths,
            options,
            stop: AtomicBool::new(false),
        });
        if let Some(error) = adaptive_quantum_load_error {
            engine.log_engine_event(
                "latency.quantum_cache",
                format!("ignored invalid learned-floor cache: {error}"),
            );
        }
        engine.initialize_effect_update_slots()?;
        engine.persist_config()?;
        engine.rebuild_effect_chain_configs()?;
        if let Ok(config) = engine.read_config() {
            engine.log_engine_event(
                "engine.start",
                format!(
                    "dry_run={} auto_repair_on_start={} poll_ms={} restore_on_launch={} lock_default_input={} lock_default_output={} startup_sink={} startup_source={} meter_supervisor={}",
                    engine.options.dry_run,
                    engine.options.auto_repair_on_start,
                    engine.options.poll_interval.as_millis(),
                    should_restore_audio_graph_on_launch(
                        &graph_prefix(),
                        config.settings.restore_audio_graph_on_launch,
                    ),
                    config.settings.lock_default_input,
                    config.settings.lock_default_output,
                    engine.startup_defaults.sink.as_deref().unwrap_or("<none>"),
                    engine.startup_defaults.source.as_deref().unwrap_or("<none>"),
                    if graph_prefix() == "wavelinux6" {
                        "native-core"
                    } else if meter_sampling_enabled() {
                        "pipewire-stream"
                    } else {
                        "disabled"
                    },
                ),
            );
        }
        engine.log_runtime_identity();
        let configured_restore_on_launch = engine
            .read_config()
            .map(|config| config.settings.restore_audio_graph_on_launch)
            .unwrap_or(false);
        let restore_on_launch =
            should_restore_audio_graph_on_launch(&graph_prefix(), configured_restore_on_launch);
        let startup_graph_reusable = engine.options.auto_repair_on_start
            && restore_on_launch
            && engine.startup_audio_graph_reusable().unwrap_or(false);
        let startup_cleanup = if startup_graph_reusable {
            engine.log_engine_event(
                "startup.cleanup",
                "existing WaveLinux audio graph is current; skipping startup rebuild",
            );
            Vec::new()
        } else {
            engine.cleanup_startup_audio_graph()?
        };
        if !startup_cleanup.is_empty() {
            engine.log_command_executions("startup.cleanup", &startup_cleanup);
        }
        if engine.options.auto_repair_on_start && restore_on_launch {
            engine
                .startup_initialization_in_progress
                .store(true, Ordering::Release);
            if let Ok(mut runtime) = engine.runtime.write() {
                runtime.status.audio_graph_running = startup_graph_reusable;
                runtime.status.message = if startup_graph_reusable {
                    "Connecting to existing audio graph".into()
                } else {
                    "Starting audio engine".into()
                };
            }
            if startup_graph_reusable {
                engine.log_engine_event(
                    "repair.startup",
                    "existing audio graph matched current profiles and routes; scheduling a background refresh",
                );
            } else {
                engine.startup_repair_pending.store(true, Ordering::Release);
                engine.log_engine_event(
                    "repair.startup",
                    "scheduled initial audio graph repair on the background worker",
                );
            }
        }
        #[cfg(not(test))]
        engine.schedule_hardware_profile_prewarm();
        Ok(engine)
    }

    fn log_runtime_identity(&self) {
        self.log_engine_event(
            "engine.identity",
            format!(
                "app={} version={} graph_prefix={} graph_property_prefix={} appimage={} current_exe={} config_dir={} data_dir={} runtime_dir={} latest_installed_appimage={}",
                app_display_name(),
                self.app_version,
                graph_prefix(),
                graph_property_prefix(),
                std::env::var("APPIMAGE").unwrap_or_else(|_| "<none>".into()),
                std::env::current_exe()
                    .ok()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "<unknown>".into()),
                self.paths.config_dir.display(),
                self.paths.data_dir.display(),
                self.paths.runtime_dir.display(),
                latest_installed_appimage_summary(&self.paths.data_dir)
                    .unwrap_or_else(|| "<none>".into()),
            ),
        );
    }

    fn runtime_identity_diagnostics(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        let appimage_path = std::env::var_os("APPIMAGE").map(PathBuf::from);
        let appimage_display = appimage_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<none>".into());

        diagnostics.push(Diagnostic {
            code: "runtime.identity.version".into(),
            severity: DiagnosticSeverity::Info,
            message: format!(
                "{} {} using graph namespace {}",
                app_display_name(),
                self.app_version,
                graph_prefix()
            ),
            action: None,
        });
        diagnostics.push(Diagnostic {
            code: "runtime.identity.paths".into(),
            severity: DiagnosticSeverity::Info,
            message: format!(
                "AppImage={appimage_display}; config={}; data={}; runtime={}",
                self.paths.config_dir.display(),
                self.paths.data_dir.display(),
                self.paths.runtime_dir.display()
            ),
            action: None,
        });

        if let Some(path) = appimage_path.as_deref() {
            if let Some(path_version) = appimage_version_from_path(path) {
                if path_version != self.app_version {
                    diagnostics.push(Diagnostic {
                        code: "runtime.identity.appimage_version_mismatch".into(),
                        severity: DiagnosticSeverity::Warning,
                        message: format!(
                            "Running AppImage file version {path_version}, but app reports {}",
                            self.app_version
                        ),
                        action: Some(
                            "Reinstall WaveLinux 6 so the launcher and AppImage point at the same build"
                                .into(),
                        ),
                    });
                }
            }
        }

        if let Some((latest_version, latest_path)) = latest_installed_appimage(&self.paths.data_dir)
        {
            if latest_version != self.app_version {
                diagnostics.push(Diagnostic {
                    code: "runtime.identity.installed_appimage_stale".into(),
                    severity: DiagnosticSeverity::Warning,
                    message: format!(
                        "Installed WaveLinux 6 AppImage is {latest_version} at {}, while app reports {}",
                        latest_path.display(),
                        self.app_version
                    ),
                    action: Some("Run scripts/install-local.sh after building the latest AppImage".into()),
                });
            }
        }

        diagnostics
    }

    pub fn spawn_background(self: &Arc<Self>) -> thread::JoinHandle<()> {
        let engine = Arc::clone(self);
        thread::spawn(move || {
            let (audio_event_tx, audio_event_rx) = mpsc::sync_channel(16);
            let audio_event_reader = engine.start_audio_event_subscription(audio_event_tx);
            if audio_event_reader.is_some()
                && engine
                    .pipewire_registry
                    .wait_initialized(Duration::from_secs(1))
            {
                let status = engine.pipewire_registry.status();
                engine.log_engine_event(
                    "pipewire.registry",
                    format!(
                        "ready generation={} objects={} nodes={} devices={} ports={} links={} metadata={}",
                        status.generation,
                        status.object_count,
                        status.node_count,
                        status.device_count,
                        status.port_count,
                        status.link_count,
                        status.metadata_count,
                    ),
                );
            }
            let pipewire_health_reader = engine.start_pipewire_health_subscription();
            let pipewire_profiler_reader = engine.start_pipewire_profiler_subscription();
            let repair_pending = engine.startup_repair_pending.swap(false, Ordering::AcqRel);
            let startup_result = if repair_pending && !engine.stop.load(Ordering::SeqCst) {
                engine.repair_audio_graph().map(|_| ())
            } else {
                engine.refresh_runtime()
            };
            engine
                .startup_initialization_in_progress
                .store(false, Ordering::Release);
            engine.change_signal.notify_state();
            if let Err(err) = startup_result {
                engine.log_engine_event(
                    "repair.startup",
                    format!("initial audio graph startup failed: {err}"),
                );
            }
            let adaptive_worker = if engine.options.dry_run {
                None
            } else {
                let adaptive_engine = Arc::clone(&engine);
                thread::Builder::new()
                    .name("wavelinux-adaptive-latency".into())
                    .spawn(move || {
                        let mut previous_error = None;
                        while !adaptive_engine.stop.load(Ordering::SeqCst) {
                            let started = Instant::now();
                            match adaptive_engine.refresh_adaptive_latency_live() {
                                Ok(()) => previous_error = None,
                                Err(error) => {
                                    let message = error.to_string();
                                    if previous_error.as_deref() != Some(message.as_str()) {
                                        adaptive_engine.log_engine_event(
                                            "latency.adaptive",
                                            format!("control tick failed: {message}"),
                                        );
                                        previous_error = Some(message);
                                    }
                                }
                            }
                            adaptive_engine.recover_effect_updates_if_ready();
                            let remaining =
                                ADAPTIVE_LATENCY_TICK_INTERVAL.saturating_sub(started.elapsed());
                            if !remaining.is_zero() {
                                thread::sleep(remaining);
                            }
                        }
                    })
                    .ok()
            };
            while !engine.stop.load(Ordering::SeqCst) {
                let event = match audio_event_rx.recv_timeout(engine.options.poll_interval) {
                    Ok(event) => {
                        let settle = match event {
                            AudioSubscriptionEvent::PlaybackStream
                            | AudioSubscriptionEvent::CaptureStream => PLAYBACK_EVENT_SETTLE,
                            AudioSubscriptionEvent::Device => DEVICE_EVENT_SETTLE,
                        };
                        thread::sleep(settle);
                        coalesce_audio_subscription_events(event, &audio_event_rx)
                    }
                    // This is a low-frequency safety audit, not the old two-second
                    // refresh loop. Normal operation is driven by graph events.
                    Err(mpsc::RecvTimeoutError::Timeout) => AudioSubscriptionEvent::Device,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        thread::sleep(engine.options.poll_interval);
                        AudioSubscriptionEvent::Device
                    }
                };
                if !engine.stop.load(Ordering::SeqCst) {
                    let result = match event {
                        AudioSubscriptionEvent::PlaybackStream => engine.refresh_playback_streams(),
                        AudioSubscriptionEvent::CaptureStream => engine.refresh_runtime(),
                        AudioSubscriptionEvent::Device => engine.refresh_runtime(),
                    };
                    if let Err(err) = result {
                        engine.log_engine_event(
                            "runtime.events",
                            format!("event={event:?} refresh_failed={err}"),
                        );
                    }
                }
            }
            engine.stop_audio_event_subscription();
            engine.stop_pipewire_health_subscription();
            engine.stop_pipewire_profiler_subscription();
            if let Some(reader) = audio_event_reader {
                let _ = reader.join();
            }
            if let Some(reader) = pipewire_health_reader {
                let _ = reader.join();
            }
            if let Some(reader) = pipewire_profiler_reader {
                let _ = reader.join();
            }
            if let Some(worker) = adaptive_worker {
                let _ = worker.join();
            }
        })
    }

    pub fn stop_background(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.change_signal.notify_state();
        self.stop_audio_event_subscription();
        self.stop_pipewire_health_subscription();
        self.stop_pipewire_profiler_subscription();
        self.stop_meter_supervisor();
    }

    pub fn revisions(&self) -> EngineRevisions {
        self.change_signal.revisions()
    }

    pub fn wait_for_change(&self, revision: u64, timeout: Duration) -> EngineRevisions {
        self.change_signal.wait_after(revision, timeout)
    }

    pub fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn start_audio_event_subscription(
        self: &Arc<Self>,
        events: mpsc::SyncSender<AudioSubscriptionEvent>,
    ) -> Option<thread::JoinHandle<()>> {
        if self.options.dry_run {
            return None;
        }
        let engine = Arc::clone(self);
        thread::Builder::new()
            .name("wavelinux-pipewire-registry".into())
            .spawn(move || {
                let mut reconnecting = false;
                let mut connected_once = false;
                while !engine.stop.load(Ordering::SeqCst) {
                    engine.pipewire_registry.mark_connected(reconnecting);
                    let batch_engine = Arc::clone(&engine);
                    let batch_events = events.clone();
                    let reconnect_bootstrap = reconnecting;
                    let hooks = NativeRegistryHooks {
                        cache: engine.pipewire_registry.clone(),
                        stopping: {
                            let engine = Arc::clone(&engine);
                            Arc::new(move || engine.stop.load(Ordering::SeqCst))
                        },
                        on_batch: Arc::new(move |batch| {
                            if batch.changed_objects == 0 && !batch.initial {
                                return;
                            }
                            if batch.initial {
                                batch_engine.change_signal.notify_state();
                                if reconnect_bootstrap {
                                    let _ = batch_events.try_send(AudioSubscriptionEvent::Device);
                                }
                            }
                            for event in batch.events {
                                let event = match event {
                                    RegistryEventKind::PlaybackStream => {
                                        AudioSubscriptionEvent::PlaybackStream
                                    }
                                    RegistryEventKind::CaptureStream => {
                                        AudioSubscriptionEvent::CaptureStream
                                    }
                                    RegistryEventKind::Device => AudioSubscriptionEvent::Device,
                                };
                                let _ = batch_events.try_send(event);
                            }
                        }),
                    };
                    if !connected_once {
                        engine.log_engine_event(
                            "pipewire.registry",
                            "subscribing to native in-process PipeWire registry actor",
                        );
                    }
                    let result = run_native_registry_connection(hooks);
                    let initialized = engine.pipewire_registry.status().initialized;
                    connected_once |= initialized;
                    if engine.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let detail = result
                        .err()
                        .unwrap_or_else(|| "native PipeWire registry loop ended".into());
                    engine.pipewire_registry.mark_disconnected(&detail);
                    if !connected_once {
                        engine.pipewire_registry.mark_unavailable(format!(
                            "native PipeWire registry unavailable: {detail}"
                        ));
                        engine.log_engine_event(
                            "pipewire.registry",
                            format!(
                                "native registry unavailable ({detail}); using Pulse event compatibility"
                            ),
                        );
                        if let Some(reader) =
                            engine.start_pulse_audio_event_subscription(events.clone())
                        {
                            let _ = reader.join();
                        }
                        break;
                    }
                    engine.log_engine_event(
                        "pipewire.registry",
                        format!("{detail}; reconnecting native registry"),
                    );
                    thread::sleep(Duration::from_millis(500));
                    if !engine.stop.load(Ordering::SeqCst) {
                        reconnecting = true;
                    }
                }
            })
            .ok()
    }

    fn start_pulse_audio_event_subscription(
        self: &Arc<Self>,
        events: mpsc::SyncSender<AudioSubscriptionEvent>,
    ) -> Option<thread::JoinHandle<()>> {
        let mut child = match host_command("pactl")
            .arg("subscribe")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                self.log_engine_event(
                    "runtime.events",
                    format!("failed to subscribe to Pulse audio events: {err}"),
                );
                return None;
            }
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            self.log_engine_event(
                "runtime.events",
                "Pulse audio event subscription did not provide stdout",
            );
            return None;
        };
        if let Ok(mut subscription) = self.audio_event_subscription.lock() {
            *subscription = Some(child);
        } else {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }

        self.log_engine_event(
            "runtime.events",
            "subscribed to Pulse audio graph events for immediate stream routing",
        );
        let engine = Arc::clone(self);
        thread::Builder::new()
            .name("wavelinux-audio-events".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    if engine.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(line) = line else {
                        break;
                    };
                    if let Some(event) = parse_audio_subscription_event(&line) {
                        let _ = events.try_send(event);
                    }
                }
            })
            .ok()
    }

    fn stop_audio_event_subscription(&self) {
        let child = self
            .audio_event_subscription
            .lock()
            .ok()
            .and_then(|mut subscription| subscription.take());
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn start_pipewire_health_subscription(self: &Arc<Self>) -> Option<thread::JoinHandle<()>> {
        if self.options.dry_run {
            return None;
        }

        let mut child = match host_command("journalctl")
            .args([
                "--user",
                "--follow",
                "--lines=0",
                "--output=cat",
                "-u",
                "pipewire.service",
                "-u",
                "pipewire-pulse.service",
                "-u",
                "wireplumber.service",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                self.log_engine_event(
                    "pipewire.health",
                    format!("live journal monitor unavailable: {err}"),
                );
                return None;
            }
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        if let Ok(mut subscription) = self.pipewire_health_subscription.lock() {
            *subscription = Some(child);
        } else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }

        self.pipewire_audio_health.set_monitor_available(true);
        self.log_engine_event(
            "pipewire.health",
            "subscribed to live PipeWire and WirePlumber warning events",
        );
        let engine = Arc::clone(self);
        thread::Builder::new()
            .name("wavelinux-audio-health".into())
            .spawn(move || {
                let owned_prefix = graph_prefix();
                for line in BufReader::new(stdout).lines() {
                    if engine.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(line) = line else {
                        break;
                    };
                    if engine
                        .pipewire_audio_health
                        .observe_line(&line, &owned_prefix)
                    {
                        engine.change_signal.notify_state();
                    }
                }
                engine.pipewire_audio_health.set_monitor_available(false);
            })
            .ok()
    }

    fn stop_pipewire_health_subscription(&self) {
        let child = self
            .pipewire_health_subscription
            .lock()
            .ok()
            .and_then(|mut subscription| subscription.take());
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.pipewire_audio_health.set_monitor_available(false);
    }

    fn start_pipewire_profiler_subscription(self: &Arc<Self>) -> Option<thread::JoinHandle<()>> {
        if self.options.dry_run {
            return None;
        }

        let (child, stdout) = match spawn_pipewire_profiler_monitor() {
            Ok(monitor) => monitor,
            Err(error) => {
                self.log_engine_event(
                    "pipewire.profiler",
                    format!("direct profiler unavailable: {error}"),
                );
                return None;
            }
        };
        if let Ok(mut subscription) = self.pipewire_profiler_subscription.lock() {
            *subscription = Some(child);
        } else {
            return None;
        }
        self.pipewire_audio_health.reset_profiler_baseline();
        self.pipewire_audio_health.set_profiler_available(true);
        self.log_engine_event(
            "pipewire.profiler",
            "subscribed to direct PipeWire profiler counters",
        );

        let engine = Arc::clone(self);
        thread::Builder::new()
            .name("wavelinux-pipewire-profiler".into())
            .spawn(move || {
                let owned_prefix = graph_prefix();
                for line in BufReader::new(stdout).lines() {
                    if engine.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(line) = line else {
                        break;
                    };
                    if engine
                        .pipewire_audio_health
                        .observe_profiler_line(&line, &owned_prefix)
                    {
                        engine.log_engine_event(
                            "pipewire.profiler",
                            format!("new direct audio error: {}", line.trim()),
                        );
                        engine.change_signal.notify_state();
                    }
                }
                engine.pipewire_audio_health.set_profiler_available(false);
            })
            .ok()
    }

    fn stop_pipewire_profiler_subscription(&self) {
        let child = self
            .pipewire_profiler_subscription
            .lock()
            .ok()
            .and_then(|mut subscription| subscription.take());
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.pipewire_audio_health.set_profiler_available(false);
    }

    pub fn prewarm_hardware_profiles(&self) -> Result<HardwareProfilePrewarmReport, EngineError> {
        prewarm_hardware_profiles_for_paths(
            &self.paths,
            &self.pw,
            &self.read_config()?.device_policy,
        )
    }

    #[cfg(not(test))]
    fn schedule_hardware_profile_prewarm(self: &Arc<Self>) {
        let engine = Arc::clone(self);
        thread::spawn(move || match engine.prewarm_hardware_profiles() {
            Ok(report) => engine.log_engine_event(
                "hardware.profile.prewarm",
                format!(
                    "startup devices={} matched={} fetched={} diagnostics={}",
                    report.devices,
                    report.matched,
                    report.fetched,
                    report.diagnostics.len()
                ),
            ),
            Err(err) => engine.log_engine_event(
                "hardware.profile.prewarm",
                format!("startup prewarm failed: {err}"),
            ),
        });
    }

    pub fn get_state(&self) -> Result<AppStateSnapshot, EngineError> {
        if !self
            .startup_initialization_in_progress
            .load(Ordering::Acquire)
        {
            let _ = self.refresh_runtime_if_stale(UI_STATE_REFRESH_MAX_AGE);
        }
        self.cached_state()
    }

    pub fn observe_state(&self) -> Result<AppStateSnapshot, EngineError> {
        self.refresh_cached_meters()?;
        self.cached_state()
    }

    /// Return the current immutable UI snapshot without initiating host queries.
    pub fn cached_state_snapshot(&self) -> Result<AppStateSnapshot, EngineError> {
        self.cached_state()
    }

    fn initialize_effect_update_slots(&self) -> Result<(), EngineError> {
        let config = self.read_config()?.clone();
        let mut slots = self
            .effect_updates
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        slots.clear();
        for channel in config.channels {
            let socket = self.paths.channel_control_socket(&channel.id);
            slots.insert(
                channel.id.clone(),
                Arc::new(EffectUpdateSlot::new(channel, &socket)),
            );
        }
        Ok(())
    }

    fn effect_update_slot(&self, channel: &Channel) -> Result<Arc<EffectUpdateSlot>, EngineError> {
        let mut slots = self
            .effect_updates
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        Ok(Arc::clone(slots.entry(channel.id.clone()).or_insert_with(
            || {
                Arc::new(EffectUpdateSlot::new(
                    channel.clone(),
                    &self.paths.channel_control_socket(&channel.id),
                ))
            },
        )))
    }

    fn desired_effect_generation(&self, channel_id: &str) -> u64 {
        self.effect_updates
            .lock()
            .ok()
            .and_then(|slots| slots.get(channel_id).cloned())
            .and_then(|slot| slot.state.lock().ok().map(|state| state.desired.generation))
            .unwrap_or(1)
    }

    fn effect_runtime_statuses(&self, config: &MixerConfig) -> Vec<EffectRuntimeStatus> {
        let slots = self.effect_updates.lock().ok();
        let mut statuses = config
            .channels
            .iter()
            .map(|channel| {
                slots
                    .as_ref()
                    .and_then(|slots| slots.get(&channel.id))
                    .and_then(|slot| slot.state.lock().ok().map(|state| state.status.clone()))
                    .unwrap_or_else(|| {
                        let desired_enabled = channel_effects_desired_enabled(channel);
                        let mut status = EffectRuntimeStatus {
                            channel_id: channel.id.clone(),
                            selected_effect_count: channel.effects.len(),
                            desired_enabled,
                            desired_generation: 1,
                            pending: desired_enabled,
                            control_socket: self
                                .paths
                                .channel_control_socket(&channel.id)
                                .to_string_lossy()
                                .into_owned(),
                            ..EffectRuntimeStatus::default()
                        };
                        status.resolve_state();
                        status
                    })
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|left, right| left.channel_id.cmp(&right.channel_id));
        statuses
    }

    fn cached_state(&self) -> Result<AppStateSnapshot, EngineError> {
        let config = self.read_config()?.clone();
        let runtime = self.read_runtime()?;
        let mut engine_status = runtime.status.clone();
        engine_status.effects = self.effect_runtime_statuses(&config);
        engine_status.meter_transport = self.meter_transport.snapshot();
        engine_status.pipewire_registry = self.pipewire_registry.status();
        engine_status.peripheral_plugins = self
            .peripheral_plugins
            .lock()
            .map(|plugins| plugins.values().cloned().collect())
            .unwrap_or_default();
        engine_status.accelerator_providers =
            self.accelerator_provider_statuses(&engine_status.audio_core)?;
        Ok(AppStateSnapshot {
            config,
            graph: runtime.graph.clone(),
            diagnostics: runtime.diagnostics.clone(),
            engine: engine_status,
            catalog: EffectCatalog::default(),
        })
    }

    pub fn set_peripheral_plugin_status(&self, status: PeripheralPluginStatus) {
        if let Ok(mut plugins) = self.peripheral_plugins.lock() {
            plugins.insert(status.kind.clone(), status);
        }
    }

    fn accelerator_provider_statuses(
        &self,
        audio_core: &[AudioCoreChannelStatus],
    ) -> Result<Vec<AcceleratorProviderStatus>, EngineError> {
        let mut cache = self
            .accelerator_status
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        if cache_expired(cache.checked_at, ACCELERATOR_STATUS_TTL) {
            cache.value = [
                wavelinux_accelerator::AcceleratorProvider::Cuda,
                wavelinux_accelerator::AcceleratorProvider::OpenVino,
                wavelinux_accelerator::AcceleratorProvider::MiGraphX,
            ]
            .into_iter()
            .map(|provider| {
                let probe = wavelinux_accelerator::probe_provider_pack(provider);
                let qualification = probe.qualification.as_ref();
                AcceleratorProviderStatus {
                    provider: provider.as_str().into(),
                    protocol_version: wavelinux_accelerator::ACCELERATOR_PROTOCOL_VERSION,
                    installed: probe.installed,
                    valid: probe.valid,
                    qualified: probe.qualified,
                    active: false,
                    pack_version: probe.pack_version,
                    model_sha256: probe.model_sha256,
                    hardware_fingerprint: probe.hardware_fingerprint,
                    tested_unix: qualification
                        .map(|record| record.tested_unix)
                        .filter(|timestamp| *timestamp > 0),
                    blocks: qualification.map(|record| record.blocks),
                    numerical_max_abs_error: qualification
                        .map(|record| record.numerical_max_abs_error),
                    deadline_misses: qualification.map(|record| record.deadline_misses),
                    discontinuities: qualification.map(|record| record.discontinuities),
                    added_latency_msec: qualification.map(|record| record.added_latency_msec),
                    cpu_reduction_percent: qualification.map(|record| record.cpu_reduction_percent),
                    fallback_validated: qualification
                        .is_some_and(|record| record.fallback_validated),
                    live_workload_validated: qualification
                        .is_some_and(|record| record.live_workload_validated),
                    detail: probe.detail,
                }
            })
            .collect();
            cache.checked_at = Some(Instant::now());
        }
        let mut statuses = cache.value.clone();
        for status in &mut statuses {
            let matching = audio_core.iter().filter(|core| {
                core.accelerator_provider.as_deref() == Some(status.provider.as_str())
            });
            let (active_states, provider_blocks, fallback_blocks) =
                matching.fold((0_u32, 0_u64, 0_u64), |totals, core| {
                    (
                        totals.0.saturating_add(core.accelerator_active_states),
                        totals.1.saturating_add(core.accelerator_provider_blocks),
                        totals.2.saturating_add(core.accelerator_fallback_blocks),
                    )
                });
            status.active = active_states > 0;
            if status.active || fallback_blocks > 0 {
                status.detail = format!(
                    "{}; live states={active_states}, provider blocks={provider_blocks}, CPU fallbacks={fallback_blocks}",
                    status.detail
                );
            }
        }
        Ok(statuses)
    }

    pub fn refresh_runtime(&self) -> Result<(), EngineError> {
        let _runtime_refresh = self.lock_runtime_refresh()?;
        self.refresh_runtime_unlocked()
    }

    fn refresh_playback_streams(&self) -> Result<(), EngineError> {
        let _runtime_refresh = self.lock_runtime_refresh()?;
        let started = Instant::now();
        let config = self.read_config()?.clone();
        let (mut graph, audio_graph_running) = {
            let runtime = self.read_runtime()?;
            (runtime.graph.clone(), runtime.status.audio_graph_running)
        };

        graph.app_streams = if let Some((snapshot, _)) = self
            .pipewire_registry
            .audio_state_snapshot(Some(&config), Vec::new())
        {
            snapshot.graph.app_streams
        } else {
            self.pw.list_app_streams(Some(&config), &graph.outputs)?
        };
        let mut routed_count = 0_usize;
        if audio_graph_running && !self.stop.load(Ordering::SeqCst) {
            let routable = fast_routable_streams_for_graph(&config, &graph);
            let routed_stream_ids = self.route_configured_streams(&config, &routable)?;
            routed_count = routed_stream_ids.len();
            if !routed_stream_ids.is_empty() {
                for stream in &mut graph.app_streams {
                    if routed_stream_ids.contains(&stream.id) {
                        stream.routed_channel_id =
                            route_stream_to_configured_channel(&config, stream)
                                .map(|channel| channel.id.clone());
                    }
                }
            }
            let _ = self.apply_configured_stream_volumes(&config, &graph.app_streams)?;
        }
        self.remember_observed_apps(&graph.app_streams)?;

        let stream_count = graph.app_streams.len();
        let changed = {
            let mut runtime = self.write_runtime()?;
            if runtime.graph.app_streams == graph.app_streams {
                false
            } else {
                runtime.graph.app_streams = graph.app_streams;
                true
            }
        };
        if changed {
            self.change_signal.notify_graph();
        }
        if routed_count > 0 {
            let command_elapsed_ms = started.elapsed().as_millis();
            self.log_engine_event(
                "route.streams.fast",
                format!(
                    "moved={routed_count} command_elapsed_ms={command_elapsed_ms} estimated_event_to_route_ms={}",
                    command_elapsed_ms.saturating_add(PLAYBACK_EVENT_SETTLE.as_millis()),
                ),
            );
        }
        if started.elapsed() >= Duration::from_millis(100) {
            self.log_engine_event(
                "runtime.refresh.fast",
                format!(
                    "elapsed_ms={} streams={} changed={changed}",
                    started.elapsed().as_millis(),
                    stream_count
                ),
            );
        }
        Ok(())
    }

    fn refresh_runtime_if_stale(&self, max_age: Duration) -> Result<(), EngineError> {
        if self.runtime_refreshed_within(max_age)? {
            return Ok(());
        }
        // UI state polling should never wait behind a full graph refresh. When a
        // refresh is already running, callers get the last cached runtime state.
        let _runtime_refresh = match self.runtime_refresh.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(_)) => return Err(EngineError::LockPoisoned),
            Err(TryLockError::WouldBlock) => {
                self.log_engine_event(
                    "runtime.refresh",
                    "refresh already in progress; returning cached state",
                );
                return Ok(());
            }
        };
        if self.runtime_refreshed_within(max_age)? {
            return Ok(());
        }
        self.refresh_runtime_unlocked()
    }

    fn refresh_runtime_unlocked(&self) -> Result<(), EngineError> {
        let started = Instant::now();
        let mut phase_started = Instant::now();
        let mut refresh_phases = Vec::new();
        let config = self.read_config()?.clone();
        let (audio_state, mut snapshot_command_timings) =
            self.audio_state_snapshot_for_config_timed(Some(&config))?;
        let mut repair_seed_state = Some(audio_state.clone());
        let mut graph = audio_state.graph;
        let mut route_snapshot = audio_state.routes;
        let mut active_sink = audio_state.active_playback_sink;
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "snapshot");
        let mut audio_graph_running = graph_has_wavelinux_nodes(&graph);
        let mut fast_routed_streams = false;
        if audio_graph_running && !self.stop.load(Ordering::SeqCst) {
            let fast_streams = fast_routable_streams_for_graph(&config, &graph);
            let routed_stream_ids = self.route_configured_streams(&config, &fast_streams)?;
            fast_routed_streams = !routed_stream_ids.is_empty();
            if !routed_stream_ids.is_empty() {
                repair_seed_state = None;
                for stream in &mut graph.app_streams {
                    if routed_stream_ids.contains(&stream.id) {
                        stream.routed_channel_id =
                            route_stream_to_configured_channel(&config, stream)
                                .map(|channel| channel.id.clone());
                    }
                }
                self.log_engine_event(
                    "route.streams.fast",
                    format!(
                        "moved={} elapsed_ms={}",
                        routed_stream_ids.len(),
                        phase_started.elapsed().as_millis()
                    ),
                );
            }
        }
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "app_routes_fast");
        let mut bluetooth_cards = self
            .bluetooth_audio_cards_for_devices(
                audio_state.bluetooth_cards,
                &graph.inputs,
                &graph.outputs,
            )
            .unwrap_or_default();
        let mut default_source = audio_state.default_source;
        let mut default_sink = audio_state.default_sink;
        let mut managed_modules = route_snapshot.managed_modules;
        let mut source_outputs = route_snapshot.source_output_routes;
        let mut sink_inputs = route_snapshot.sink_input_routes;
        let mut desired_audio_config = effective_config_with_profiled_devices(
            &config,
            &graph.inputs,
            &graph.outputs,
            &bluetooth_cards,
            default_source.as_deref(),
            default_sink.as_deref(),
            active_sink.as_deref(),
        );
        desired_audio_config = self.config_with_unhealthy_effects_bypassed(&desired_audio_config);
        let mut audio_config =
            config_with_unavailable_effects_bypassed(&desired_audio_config, &graph);
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "devices");
        if !self.stop.load(Ordering::SeqCst)
            && self.bluetooth_a2dp_repair_needed(&bluetooth_cards, false)?
        {
            self.log_engine_event(
                "bluetooth.a2dp",
                "restoring Bluetooth playback to A2DP before routing decisions",
            );
            let _audio_commands = self.lock_audio_commands()?;
            let outputs = self.ensure_bluetooth_a2dp_profiles(false)?;
            self.log_command_executions("bluetooth.a2dp", &outputs);
            if outputs
                .iter()
                .any(|output| !output.skipped && output.error.is_none())
            {
                thread::sleep(Duration::from_millis(250));
                let (next_state, timings) =
                    self.audio_state_snapshot_for_config_timed(Some(&config))?;
                repair_seed_state = Some(next_state.clone());
                let next_bluetooth_cards = next_state.bluetooth_cards;
                default_source = next_state.default_source;
                default_sink = next_state.default_sink;
                active_sink = next_state.active_playback_sink;
                source_outputs = next_state.routes.source_output_routes;
                sink_inputs = next_state.routes.sink_input_routes;
                graph = next_state.graph;
                snapshot_command_timings.extend(timings);
                bluetooth_cards = self
                    .bluetooth_audio_cards_for_devices(
                        next_bluetooth_cards,
                        &graph.inputs,
                        &graph.outputs,
                    )
                    .unwrap_or_default();
                desired_audio_config = effective_config_with_profiled_devices(
                    &config,
                    &graph.inputs,
                    &graph.outputs,
                    &bluetooth_cards,
                    default_source.as_deref(),
                    default_sink.as_deref(),
                    active_sink.as_deref(),
                );
                desired_audio_config =
                    self.config_with_unhealthy_effects_bypassed(&desired_audio_config);
                audio_config =
                    config_with_unavailable_effects_bypassed(&desired_audio_config, &graph);
                audio_graph_running = graph_has_wavelinux_nodes(&graph);
            }
        }
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "bluetooth");
        let auto_device_route_repair_needed = auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &graph.inputs,
                outputs: &graph.outputs,
                bluetooth_cards: &bluetooth_cards,
                default_source: default_source.as_deref(),
                default_sink: default_sink.as_deref(),
                active_sink: active_sink.as_deref(),
                managed_modules: &managed_modules,
                source_outputs: &source_outputs,
                sink_inputs: &sink_inputs,
            },
        );
        let effect_sync_active = self.effect_sync_active.load(Ordering::SeqCst);
        let active_effect_route_repair_needed = !effect_sync_active
            && active_effect_routes_need_repair(
                &desired_audio_config,
                &graph,
                &managed_modules,
                &source_outputs,
                &sink_inputs,
            );
        let active_app_channel_ids = active_app_channel_ids_for_graph(&audio_config, &graph);
        let active_mix_ids =
            active_mix_ids_for_routes(&audio_config, &graph, &source_outputs, &sink_inputs);
        let route_health = route_health_issues_for_active_routes(
            &audio_config,
            &graph,
            &managed_modules,
            &source_outputs,
            &sink_inputs,
            &active_app_channel_ids,
            &active_mix_ids,
        );
        let active_route_repair_needed = audio_graph_running
            && !app_routing_graph_ready(
                &audio_config,
                &graph,
                &managed_modules,
                &source_outputs,
                &sink_inputs,
            );
        let route_health_repair_needed = audio_graph_running
            && !route_health.is_empty()
            && self.route_health_repair_allowed(&route_health);
        let realtime_fallback_channel_ids =
            self.realtime_fallback_sync_channel_ids_for_runtime_prefix(&config, &graph_prefix());
        let realtime_fallback_repair_needed = !realtime_fallback_channel_ids.is_empty();
        let default_device_lock_repair_needed = self.confirmed_default_device_lock_repair_needed(
            &audio_config,
            default_source.as_deref(),
            default_sink.as_deref(),
        );
        let auto_device_repair_needed =
            auto_device_route_repair_needed || default_device_lock_repair_needed;
        let bluetooth_monitor_route_refresh_needed = audio_graph_running
            && self
                .read_runtime()
                .map(|runtime| {
                    bluetooth_monitor_route_refresh_needed(
                        &runtime,
                        &audio_config,
                        &graph.outputs,
                        &managed_modules,
                    )
                })
                .unwrap_or(false);
        let incremental_mix_route_repair_needed = !auto_device_repair_needed
            && !active_effect_route_repair_needed
            && !realtime_fallback_repair_needed
            && !bluetooth_monitor_route_refresh_needed
            && (active_route_repair_needed || route_health_repair_needed)
            && route_changes_are_incremental_mix_only(
                &audio_config,
                IncrementalMixRouteView {
                    graph: &graph,
                    managed_modules: &managed_modules,
                    source_outputs: &source_outputs,
                    sink_inputs: &sink_inputs,
                },
                &active_app_channel_ids,
                &active_mix_ids,
                &route_health,
            );
        let persistent_core_target_repair_needed = graph_prefix() == "wavelinux6"
            && self.audio_core_process_is_tracked()
            && !active_effect_route_repair_needed
            && !realtime_fallback_repair_needed
            && !bluetooth_monitor_route_refresh_needed
            && !route_health_repair_needed
            && persistent_core_target_routes_need_sync(
                &audio_config,
                &source_outputs,
                &sink_inputs,
            )
            && app_routing_graph_ready_without_persistent_targets(
                &audio_config,
                &graph,
                &managed_modules,
                &source_outputs,
                &sink_inputs,
            );
        let mut route_mutations_deferred = false;
        let mut route_mutation_requested = false;
        if audio_graph_running
            && !self.stop.load(Ordering::SeqCst)
            && (auto_device_repair_needed
                || active_effect_route_repair_needed
                || active_route_repair_needed
                || route_health_repair_needed
                || realtime_fallback_repair_needed
                || bluetooth_monitor_route_refresh_needed)
        {
            route_mutation_requested = true;
            let default_lock_only_repair = default_device_lock_repair_needed
                && !auto_device_route_repair_needed
                && !active_effect_route_repair_needed
                && !active_route_repair_needed
                && !route_health_repair_needed
                && !realtime_fallback_repair_needed
                && !bluetooth_monitor_route_refresh_needed;
            let reason = if default_lock_only_repair {
                "default audio device selection changed; restoring app-facing default only"
            } else if bluetooth_monitor_route_refresh_needed
                && !auto_device_repair_needed
                && !active_effect_route_repair_needed
                && !active_route_repair_needed
                && !route_health_repair_needed
                && !realtime_fallback_repair_needed
            {
                "Bluetooth monitor route changed or duplicated; rebuilding final output route"
            } else if realtime_fallback_repair_needed
                && !auto_device_repair_needed
                && !active_effect_route_repair_needed
                && !active_route_repair_needed
                && !route_health_repair_needed
            {
                "realtime FX fallback triggered; rebuilding affected effect chains"
            } else if persistent_core_target_repair_needed {
                "hardware target changed; retargeting the persistent audio core live"
            } else if incremental_mix_route_repair_needed {
                "active application mix changed; synchronizing live routes"
            } else if route_health_repair_needed {
                "managed audio route is stale or detached; repairing audio routes"
            } else if active_route_repair_needed {
                "active audio mix route changed; repairing audio routes"
            } else if active_effect_route_repair_needed && !auto_device_repair_needed {
                "active effect route changed while graph was running; repairing audio routes"
            } else {
                "auto hardware device changed while graph was running; repairing audio routes"
            };
            self.log_engine_event("hotplug.device", reason);
            let audio_commands = self.try_lock_audio_commands_for_refresh("hotplug.device")?;
            if let Some(_audio_commands) = audio_commands {
                if !self.stop.load(Ordering::SeqCst) {
                    let mut outputs = Vec::new();
                    if default_lock_only_repair {
                        outputs.extend(self.apply_default_device_locks(&audio_config)?);
                    } else if bluetooth_monitor_route_refresh_needed {
                        outputs
                            .extend(self.repair_bluetooth_monitor_routes_unlocked(&audio_config)?);
                    }
                    if realtime_fallback_repair_needed {
                        self.log_engine_event(
                            "effects.fallback",
                            format!(
                                "recent realtime underrun; syncing channels: {}",
                                realtime_fallback_channel_ids
                                    .iter()
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        );
                        for (channel_id, path) in self
                            .preserve_realtime_fallback_logs_for_runtime_prefix(
                                &config,
                                &realtime_fallback_channel_ids,
                                &graph_prefix(),
                            )
                        {
                            self.log_engine_event(
                                "effects.fallback",
                                format!(
                                    "preserved failed FX log channel={channel_id} path={}",
                                    path.display()
                                ),
                            );
                        }
                        self.rebuild_effect_chain_configs()?;
                        outputs.extend(
                            self.sync_effect_channels_unlocked(&realtime_fallback_channel_ids)?,
                        );
                        self.clear_realtime_fallback_trigger_logs_for_runtime_prefix(
                            &config,
                            &realtime_fallback_channel_ids,
                            &graph_prefix(),
                        );
                    }
                    if persistent_core_target_repair_needed {
                        self.rebuild_effect_chain_configs_from_config(
                            &audio_config,
                            &graph_prefix(),
                        )?;
                        outputs.extend(self.sync_persistent_audio_core_targets()?);
                        if default_device_lock_repair_needed {
                            outputs.extend(self.apply_default_device_locks(&audio_config)?);
                        }
                    } else if incremental_mix_route_repair_needed {
                        outputs.extend(self.sync_active_mix_routes_unlocked(
                            &audio_config,
                            IncrementalMixRouteView {
                                graph: &graph,
                                managed_modules: &managed_modules,
                                source_outputs: &source_outputs,
                                sink_inputs: &sink_inputs,
                            },
                            &active_app_channel_ids,
                            &active_mix_ids,
                            &route_health,
                        )?);
                    } else if active_effect_route_repair_needed
                        || active_route_repair_needed
                        || route_health_repair_needed
                    {
                        outputs.extend(
                            self.repair_audio_graph_unlocked_from_snapshot(
                                repair_seed_state.take(),
                            )?
                            .outputs,
                        );
                    } else if !default_lock_only_repair
                        && (auto_device_route_repair_needed || default_device_lock_repair_needed)
                    {
                        outputs.extend(
                            self.repair_auto_device_routes_unlocked(repair_seed_state.take())?,
                        );
                    }
                    self.log_command_executions("hotplug.device", &outputs);
                    if default_lock_only_repair {
                        default_source = self.pw.default_source().ok().flatten();
                        default_sink = self.pw.default_sink().ok().flatten();
                        active_sink = self.pw.active_playback_sink().ok().flatten();
                    } else {
                        let (next_state, timings) =
                            self.audio_state_snapshot_for_config_timed(Some(&config))?;
                        let next_bluetooth_cards = next_state.bluetooth_cards;
                        default_source = next_state.default_source;
                        default_sink = next_state.default_sink;
                        active_sink = next_state.active_playback_sink;
                        route_snapshot = next_state.routes;
                        graph = next_state.graph;
                        snapshot_command_timings.extend(timings);
                        bluetooth_cards = self
                            .bluetooth_audio_cards_for_devices(
                                next_bluetooth_cards,
                                &graph.inputs,
                                &graph.outputs,
                            )
                            .unwrap_or_default();
                        managed_modules = route_snapshot.managed_modules;
                        source_outputs = route_snapshot.source_output_routes;
                        sink_inputs = route_snapshot.sink_input_routes;
                    }
                    desired_audio_config = effective_config_with_profiled_devices(
                        &config,
                        &graph.inputs,
                        &graph.outputs,
                        &bluetooth_cards,
                        default_source.as_deref(),
                        default_sink.as_deref(),
                        active_sink.as_deref(),
                    );
                    desired_audio_config =
                        self.config_with_unhealthy_effects_bypassed(&desired_audio_config);
                    audio_config =
                        config_with_unavailable_effects_bypassed(&desired_audio_config, &graph);
                    audio_graph_running = graph_has_wavelinux_nodes(&graph);
                }
            } else {
                route_mutations_deferred = true;
            }
        }
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "repair");
        if audio_graph_running && !self.stop.load(Ordering::SeqCst) && !route_mutations_deferred {
            self.persist_followed_monitor_output_selection(&config, &audio_config)?;
            let graph_ready_for_apps = app_routing_graph_ready(
                &audio_config,
                &graph,
                &managed_modules,
                &source_outputs,
                &sink_inputs,
            );
            let rescued_streams = self.move_unready_routed_streams_to_default(
                &audio_config,
                &graph,
                &managed_modules,
                &source_outputs,
                &sink_inputs,
            )?;
            let routed_streams = if graph_ready_for_apps {
                !self
                    .route_configured_streams(&audio_config, &graph.app_streams)?
                    .is_empty()
            } else {
                self.log_engine_event(
                    "route.streams",
                    "audio graph is not ready for app routing; leaving apps on real outputs",
                );
                false
            };
            let updated_volumes =
                self.apply_configured_stream_volumes(&config, &graph.app_streams)?;
            let moved_capture_streams = if graph_ready_for_apps {
                self.move_capture_streams_to_locked_default_input(
                    &audio_config,
                    &source_outputs,
                    &graph.inputs,
                    &bluetooth_cards,
                )?
            } else {
                false
            };
            if runtime_route_resnapshot_needed(
                fast_routed_streams,
                rescued_streams,
                routed_streams,
                updated_volumes,
                moved_capture_streams,
            ) {
                let (next_state, timings) =
                    self.audio_state_snapshot_for_config_timed(Some(&config))?;
                graph = next_state.graph;
                active_sink = next_state.active_playback_sink;
                source_outputs = next_state.routes.source_output_routes;
                sink_inputs = next_state.routes.sink_input_routes;
                snapshot_command_timings.extend(timings);
                audio_graph_running = graph_has_wavelinux_nodes(&graph);
            }
        }
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "routes");
        graph.meters = if self.stop.load(Ordering::SeqCst) {
            Vec::new()
        } else {
            self.meter_snapshot_or_stop_idle()?
        };
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "meters");
        self.remember_observed_apps(&graph.app_streams)?;
        let mut auto_devices = resolved_auto_devices_for_config(
            &config,
            &graph.inputs,
            &graph.outputs,
            &bluetooth_cards,
            default_source.as_deref(),
            default_sink.as_deref(),
            active_sink.as_deref(),
        );
        let audio_core_status = self.collect_audio_core_status(&audio_config);
        let mut adaptive_audio_core_status = audio_core_status.clone();
        apply_audio_core_discontinuity_deltas(
            &mut adaptive_audio_core_status,
            &self.adaptive_core_discontinuity_counters,
        );
        let mut diagnostics = self.host_diagnostics()?;
        diagnostics.extend(self.effect_chain_diagnostics_with_core(
            &audio_config,
            &graph,
            &audio_core_status,
        ));
        let adaptive_latency_status = self.adaptive_latency_status(
            &config.settings.adaptive_latency,
            &adaptive_audio_core_status,
        )?;
        self.send_adaptive_latency_targets(&config, &adaptive_latency_status);
        let healthy = diagnostics
            .iter()
            .all(|item| item.severity != DiagnosticSeverity::Error);
        let mut runtime = self.write_runtime()?;
        stabilize_auto_device_reasons(&runtime.graph.auto_devices, &mut auto_devices);
        let graph_changed = runtime.graph != graph;
        let diagnostics_changed = runtime.diagnostics != diagnostics;
        let previous_status = runtime.status.clone();
        self.log_auto_device_changes(&runtime.graph.auto_devices, &auto_devices);
        graph.auto_devices = auto_devices;
        runtime.graph = graph;
        runtime.diagnostics = diagnostics;
        runtime.sink_input_routes = sink_inputs;
        runtime.source_output_routes = source_outputs;
        runtime.bluetooth_monitor_routes =
            bluetooth_monitor_route_signatures(&audio_config, &runtime.graph.outputs);
        runtime.status.healthy = healthy;
        runtime.status.audio_graph_running = audio_graph_running;
        runtime.status.last_refresh_unix = OffsetDateTime::now_utc().unix_timestamp();
        runtime.status.adaptive_latency = adaptive_latency_status;
        runtime.status.audio_core = audio_core_status;
        runtime.status.pipewire_audio_health = self.pipewire_audio_health.snapshot();
        runtime.status.pipewire_registry = self.pipewire_registry.status();
        runtime.refreshed_at = Some(Instant::now());
        runtime.status.message = if healthy {
            if self.options.dry_run {
                "Dry-run mode".into()
            } else if audio_graph_running {
                "Audio graph running".into()
            } else {
                "Audio graph stopped".into()
            }
        } else {
            "Host audio dependencies are missing".into()
        };
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "state");
        let elapsed = started.elapsed();
        let snapshot_failed = snapshot_command_timings
            .iter()
            .any(|timing| !timing.succeeded);
        {
            let refresh = &mut runtime.status.refresh;
            let elapsed_msec = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
            refresh.total_refreshes = refresh.total_refreshes.saturating_add(1);
            refresh.last_total_msec = elapsed_msec;
            refresh.peak_total_msec = refresh.peak_total_msec.max(elapsed_msec);
            refresh.last_phase_msec = refresh_phases
                .iter()
                .map(|(phase, elapsed_ms)| {
                    (
                        (*phase).to_string(),
                        (*elapsed_ms).min(u128::from(u64::MAX)) as u64,
                    )
                })
                .collect();
            refresh.snapshot_commands =
                snapshot_command_timings.len().min(u32::MAX as usize) as u32;
            refresh.snapshot_failures = snapshot_command_timings
                .iter()
                .filter(|timing| !timing.succeeded)
                .count()
                .min(u32::MAX as usize) as u32;
            if route_mutation_requested {
                refresh.route_mutations = refresh.route_mutations.saturating_add(1);
            }
            if route_mutations_deferred {
                refresh.deferred_route_mutations =
                    refresh.deferred_route_mutations.saturating_add(1);
            }
        }
        let slow_refresh_decision = {
            let mut state = self
                .slow_refresh_log
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            slow_refresh_log_decision(
                &mut state,
                Instant::now(),
                elapsed,
                snapshot_failed,
                route_mutation_requested || route_mutations_deferred,
            )
        };
        if let Some(decision) = slow_refresh_decision {
            let phases = refresh_phases
                .iter()
                .map(|(phase, elapsed_ms)| format!("{phase}={elapsed_ms}ms"))
                .collect::<Vec<_>>()
                .join(" ");
            let snapshot_commands = format_snapshot_command_timings(&snapshot_command_timings);
            self.log_engine_event(
                "runtime.refresh",
                format!(
                    "slow_refresh_ms={} suppressed_refreshes={} inputs={} outputs={} streams={} meters={} graph_running={} phases={} snapshot_commands={}",
                    elapsed.as_millis(),
                    decision.suppressed_refreshes,
                    runtime.graph.inputs.len(),
                    runtime.graph.outputs.len(),
                    runtime.graph.app_streams.len(),
                    runtime.graph.meters.len(),
                    runtime.status.audio_graph_running,
                    phases,
                    snapshot_commands,
                ),
            );
        }
        let status_changed = runtime.status != previous_status;
        drop(runtime);
        if graph_changed {
            self.change_signal.notify_graph();
        } else if diagnostics_changed || status_changed {
            self.change_signal.notify_state();
        }
        Ok(())
    }

    fn route_health_repair_allowed(&self, issues: &[RouteHealthIssue]) -> bool {
        let signature = route_health_signature(issues);
        let summary = route_health_summary(issues);
        if self.effect_sync_active.load(Ordering::SeqCst) {
            self.log_engine_event(
                "route.health",
                format!(
                    "issues={} suppressed=effects_sync {}",
                    issues.len(),
                    summary
                ),
            );
            return false;
        }
        let mut state = match self.route_health_repair.lock() {
            Ok(state) => state,
            Err(_) => {
                self.log_engine_event(
                    "route.health",
                    format!(
                        "issues={} lock_poisoned=true; allowing repair {}",
                        issues.len(),
                        summary
                    ),
                );
                return true;
            }
        };

        if state.signature.as_deref() == Some(signature.as_str())
            && state
                .attempted_at
                .is_some_and(|attempted_at| attempted_at.elapsed() < ROUTE_HEALTH_REPAIR_BACKOFF)
        {
            self.log_engine_event(
                "route.health",
                format!(
                    "issues={} repeated=true suppressed=true {}",
                    issues.len(),
                    summary
                ),
            );
            return false;
        }

        state.signature = Some(signature);
        state.attempted_at = Some(Instant::now());
        self.log_engine_event(
            "route.health",
            format!("issues={} suppressed=false {}", issues.len(), summary),
        );
        self.log_engine_event(
            "repair.routes",
            format!("trigger=route_health issues={} {}", issues.len(), summary),
        );
        true
    }

    fn log_auto_device_changes(
        &self,
        previous: &[ResolvedAutoDevice],
        next: &[ResolvedAutoDevice],
    ) {
        for device in next {
            let prior = previous
                .iter()
                .find(|prior| auto_device_slot_matches(prior, device));
            let changed = prior.is_none_or(|prior| {
                prior.device_id != device.device_id || prior.reason != device.reason
            });
            if !changed {
                continue;
            }
            let event = match device.kind {
                AutoDeviceKind::Input => "auto.input",
                AutoDeviceKind::Output => "auto.output",
            };
            self.log_engine_event(
                event,
                format!(
                    "channel={} mix={} previous={} selected={} description={} priority={} reason={}",
                    device.channel_id.as_deref().unwrap_or("-"),
                    device.mix_id.as_deref().unwrap_or("-"),
                    prior
                        .and_then(|prior| prior.device_id.as_deref())
                        .unwrap_or("-"),
                    device.device_id.as_deref().unwrap_or("-"),
                    device.device_description.as_deref().unwrap_or("-"),
                    device
                        .priority
                        .map(|priority| priority.to_string())
                        .unwrap_or_else(|| "-".into()),
                    auto_device_reason_label(&device.reason),
                ),
            );
        }
    }

    pub fn run_diagnostics(&self) -> Result<SoundCheckReport, EngineError> {
        let state = self.get_state()?;
        let mut diagnostics = state.diagnostics.clone();
        let config = self.effective_config_for_audio_graph(&state.config);
        let route_snapshot = self.pw.route_snapshot().unwrap_or_default();
        let managed_modules = route_snapshot.managed_modules;
        let source_outputs = route_snapshot.source_output_routes;
        let sink_input_routes = route_snapshot.sink_input_routes;
        let active_app_channel_ids = active_app_channel_ids_for_graph(&config, &state.graph);
        let active_mix_ids =
            active_mix_ids_for_routes(&config, &state.graph, &source_outputs, &sink_input_routes);
        let route_health = route_health_issues_for_active_routes(
            &config,
            &state.graph,
            &managed_modules,
            &source_outputs,
            &sink_input_routes,
            &active_app_channel_ids,
            &active_mix_ids,
        );
        diagnostics.extend(graph_diagnostics(&state.config, &state.graph));
        diagnostics.extend(route_diagnostics(&config, &state.graph, &managed_modules));
        diagnostics.extend(route_health_diagnostics(&route_health));
        diagnostics.extend(hardware_profile_diagnostics(&state.graph));
        if let Ok(catalog) = self.hardware_profiles() {
            diagnostics.extend(catalog.diagnostics);
        }
        diagnostics.extend(self.effect_chain_diagnostics(&config, &state.graph));
        let missing_effects = state
            .graph
            .effect_availability
            .iter()
            .filter(|effect| !effect.available)
            .map(|effect| effect.effect_id.clone())
            .collect::<Vec<_>>();
        if !meter_sampling_enabled() {
            diagnostics.push(Diagnostic {
                code: "meters.pipewire_stream.disabled".into(),
                severity: DiagnosticSeverity::Info,
                message: "PipeWire VU meter supervisor is disabled".into(),
                action: Some(
                    "Install PipeWire host tools or unset WAVELINUX_DISABLE_METERS to show live fader meters".into(),
                ),
            });
        }
        self.log_engine_event(
            "diagnostics.run",
            format!(
                "diagnostics={} streams={} mixes={} missing_effects={}",
                diagnostics.len(),
                state.graph.app_streams.len(),
                state.config.mixes.len(),
                missing_effects.len(),
            ),
        );
        Ok(SoundCheckReport {
            diagnostics,
            active_stream_count: state.graph.app_streams.len(),
            virtual_mix_count: state.config.mixes.len(),
            missing_effects,
            debug_log_path: self.paths.log_file(),
            recent_log_lines: self.recent_log_lines(80),
        })
    }

    pub fn get_graph_debug_report(&self) -> Result<GraphDebugReport, EngineError> {
        let config = self.effective_config_for_audio_graph(&self.read_config()?.clone());
        let mut graph = self.snapshot_for_config(Some(&config))?;
        let audio_graph_running = graph_has_wavelinux_nodes(&graph);
        graph.meters = self.meter_snapshot_or_stop_idle()?;
        let mut diagnostics = self.host_diagnostics()?;
        let route_snapshot = self.pw.route_snapshot().unwrap_or_default();
        let managed_modules = route_snapshot.managed_modules;
        let sink_input_routes = route_snapshot.sink_input_routes;
        let source_output_routes = route_snapshot.source_output_routes;
        let active_app_channel_ids = active_app_channel_ids_for_graph(&config, &graph);
        let active_mix_ids =
            active_mix_ids_for_routes(&config, &graph, &source_output_routes, &sink_input_routes);
        let planned =
            plan_ensure_graph_for_active_routes(&config, &active_app_channel_ids, &active_mix_ids);
        let route_health = route_health_issues_for_active_routes(
            &config,
            &graph,
            &managed_modules,
            &source_output_routes,
            &sink_input_routes,
            &active_app_channel_ids,
            &active_mix_ids,
        );
        diagnostics.extend(graph_diagnostics(&config, &graph));
        diagnostics.extend(route_diagnostics(&config, &graph, &managed_modules));
        diagnostics.extend(route_health_diagnostics(&route_health));
        diagnostics.extend(hardware_profile_diagnostics(&graph));
        if let Ok(catalog) = self.hardware_profiles() {
            diagnostics.extend(catalog.diagnostics);
        }
        diagnostics.extend(self.effect_chain_diagnostics(&config, &graph));
        Ok(GraphDebugReport {
            dry_run: self.options.dry_run,
            audio_graph_running,
            planned,
            managed_modules,
            sink_input_routes,
            source_output_routes,
            route_health,
            stale_processes: self.pw.stale_processes().unwrap_or_default(),
            graph,
            diagnostics,
            debug_log_path: self.paths.log_file(),
            recent_log_lines: self.recent_log_lines(120),
        })
    }

    pub fn cleanup_audio_graph(&self) -> Result<Vec<CommandExecution>, EngineError> {
        self.log_engine_event("cleanup.full", "requested full graph cleanup");
        self.stop_meter_supervisor();
        let restore_default_output = self
            .read_config()
            .map(|config| config.settings.lock_default_output)
            .unwrap_or(false);
        let outputs = {
            let _audio_commands = self.lock_audio_commands()?;
            self.stop_all_tracked_effect_chain_processes();
            let mut outputs = self.cleanup_stale_processes()?;
            outputs.extend(self.cleanup_all_modules_until_clear()?);
            outputs.extend(self.restore_startup_default_devices(restore_default_output));
            outputs
        };
        self.log_command_executions("cleanup.full", &outputs);
        let _ = self.refresh_runtime();
        Ok(outputs)
    }

    pub fn cleanup_stale_audio_graph(&self) -> Result<Vec<CommandExecution>, EngineError> {
        self.log_engine_event("cleanup.stale", "requested stale graph cleanup");
        let config = self.read_config()?.clone();
        let graph = self.snapshot_for_config(Some(&config))?;
        let active_app_channel_ids = active_app_channel_ids_for_graph(&config, &graph);
        let route_snapshot = self.pw.route_snapshot().unwrap_or_default();
        let active_mix_ids = active_mix_ids_for_routes(
            &config,
            &graph,
            &route_snapshot.source_output_routes,
            &route_snapshot.sink_input_routes,
        );
        let _audio_commands = self.lock_audio_commands()?;
        let outputs = self.cleanup_stale_modules_for_config(
            &config,
            &active_app_channel_ids,
            &active_mix_ids,
            false,
        )?;
        self.log_command_executions("cleanup.stale", &outputs);
        Ok(outputs)
    }

    fn startup_audio_graph_reusable(&self) -> Result<bool, EngineError> {
        let config = self.read_config()?.clone();
        let (graph, _) = self.snapshot_for_config_timed(Some(&config))?;
        if !graph_has_wavelinux_nodes(&graph) {
            return Ok(false);
        }

        let stale_processes = self.stale_audio_processes_excluding_active()?;
        if !stale_processes.is_empty() {
            self.log_engine_event(
                "startup.cleanup",
                format!(
                    "existing graph has {} stale WaveLinux audio helper(s); forcing rebuild",
                    stale_processes.len()
                ),
            );
            return Ok(false);
        }

        let bluetooth_cards = self.bluetooth_audio_cards().unwrap_or_default();
        let default_source = self.pw.default_source().ok().flatten();
        let default_sink = self.pw.default_sink().ok().flatten();
        let active_sink = self.pw.active_playback_sink().ok().flatten();
        let route_snapshot = self.pw.route_snapshot().unwrap_or_default();
        let managed_modules = route_snapshot.managed_modules;
        let source_outputs = route_snapshot.source_output_routes;
        let sink_inputs = route_snapshot.sink_input_routes;
        let mut effective_config = effective_config_with_profiled_devices(
            &config,
            &graph.inputs,
            &graph.outputs,
            &bluetooth_cards,
            default_source.as_deref(),
            default_sink.as_deref(),
            active_sink.as_deref(),
        );
        effective_config = self.config_with_unhealthy_effects_bypassed(&effective_config);

        if !plan_bluetooth_a2dp_profiles(&bluetooth_cards, &BTreeMap::new(), true).is_empty() {
            return Ok(false);
        }

        if auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &graph.inputs,
                outputs: &graph.outputs,
                bluetooth_cards: &bluetooth_cards,
                default_source: default_source.as_deref(),
                default_sink: default_sink.as_deref(),
                active_sink: active_sink.as_deref(),
                managed_modules: &managed_modules,
                source_outputs: &source_outputs,
                sink_inputs: &sink_inputs,
            },
        ) {
            return Ok(false);
        }

        let graph_has_blocking_diagnostic = graph_diagnostics(&effective_config, &graph)
            .iter()
            .any(|diagnostic| {
                matches!(diagnostic.severity, DiagnosticSeverity::Error)
                    || diagnostic.code.starts_with("graph.effect_")
            });
        if graph_has_blocking_diagnostic {
            return Ok(false);
        }

        if !app_routing_graph_ready(
            &effective_config,
            &graph,
            &managed_modules,
            &source_outputs,
            &sink_inputs,
        ) {
            self.log_engine_event(
                "startup.cleanup",
                "existing graph routes do not match current config; forcing rebuild",
            );
            return Ok(false);
        }

        let active_app_channel_ids = active_app_channel_ids_for_graph(&effective_config, &graph);
        let active_mix_ids =
            active_mix_ids_for_routes(&effective_config, &graph, &source_outputs, &sink_inputs);
        Ok(
            route_diagnostics(&effective_config, &graph, &managed_modules).is_empty()
                && route_health_issues_for_active_routes(
                    &effective_config,
                    &graph,
                    &managed_modules,
                    &source_outputs,
                    &sink_inputs,
                    &active_app_channel_ids,
                    &active_mix_ids,
                )
                .is_empty(),
        )
    }

    fn cleanup_startup_audio_graph(&self) -> Result<Vec<CommandExecution>, EngineError> {
        let graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        let has_wavelinux_nodes = graph_has_wavelinux_nodes(&graph);
        let has_managed_modules = self
            .pw
            .managed_modules()
            .map(|modules| !modules.is_empty())
            .unwrap_or(false);
        let has_stale_processes = self
            .pw
            .stale_processes()
            .map(|processes| !processes.is_empty())
            .unwrap_or(false);

        if !has_wavelinux_nodes && !has_managed_modules && !has_stale_processes {
            self.log_engine_event(
                "startup.cleanup",
                "no previous WaveLinux audio graph was present",
            );
            return Ok(Vec::new());
        }

        self.log_engine_event(
            "startup.cleanup",
            format!(
                "removing previous WaveLinux audio graph before launch nodes={} modules={} processes={}",
                has_wavelinux_nodes, has_managed_modules, has_stale_processes
            ),
        );
        self.stop_meter_supervisor();
        let restore_default_output = self
            .read_config()
            .map(|config| config.settings.lock_default_output)
            .unwrap_or(false);
        let _audio_commands = self.lock_audio_commands()?;
        self.stop_all_tracked_effect_chain_processes();
        let mut outputs = self.cleanup_stale_processes()?;
        outputs.extend(self.cleanup_all_modules_until_clear()?);
        outputs.extend(self.restore_startup_default_devices(restore_default_output));
        Ok(outputs)
    }

    fn route_configured_streams(
        &self,
        config: &MixerConfig,
        streams: &[AppStream],
    ) -> Result<BTreeSet<String>, EngineError> {
        let routes = streams
            .iter()
            .filter_map(|stream| {
                let channel = route_stream_to_configured_channel(config, stream)?;
                if stream.routed_channel_id.as_deref() == Some(channel.id.as_str()) {
                    return None;
                }
                if self.app_stream_move_recently_failed(&stream.id) {
                    return None;
                }
                Some((stream.clone(), channel.clone()))
            })
            .collect::<Vec<_>>();

        if routes.is_empty() {
            return Ok(BTreeSet::new());
        }

        let _audio_commands = self.lock_audio_commands()?;
        let mut routed_stream_ids = BTreeSet::new();
        for (stream, channel) in routes {
            let native_node_id = match self
                .pipewire_registry
                .playback_route_backend(&stream.id, &channel.virtual_sink_name)
            {
                Some(StreamRouteBackend::Native(route)) => Some((
                    route.stream_node_id,
                    route.target_object_serial,
                    route.target_node_name,
                )),
                Some(StreamRouteBackend::Unavailable(detail)) => {
                    self.log_engine_event(
                        "route.streams",
                        format!("stream={} deferred={detail}", stream.id),
                    );
                    continue;
                }
                Some(StreamRouteBackend::PulseCompatibility) | None => None,
            };
            let command = if let Some((node_id, serial, target)) = native_node_id.as_ref() {
                plan_move_native_app_stream(*node_id, serial, target)
            } else {
                plan_move_app_stream(&stream.id, &channel)
            };
            let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
            let output = ignore_stale_stream_command(output, &stream.id);
            if output.skipped && output.stderr.contains("disappeared") {
                continue;
            }

            let move_succeeded = output.error.is_none() && !output.skipped;
            self.remember_app_stream_move_result(&stream.id, &output)?;
            self.log_command_executions("route.streams", std::slice::from_ref(&output));
            if move_succeeded {
                routed_stream_ids.insert(stream.id.clone());
                if let Some(target_volume) = configured_volume_update_for_stream(config, &stream) {
                    let volume_command = native_node_id
                        .as_ref()
                        .map(|(node_id, _, _)| {
                            plan_set_native_stream_volume(*node_id, target_volume)
                        })
                        .unwrap_or_else(|| plan_set_stream_volume(&stream.id, target_volume));
                    let volume_output = command_execution_with_spec(
                        volume_command.clone(),
                        self.pw.execute(volume_command),
                    );
                    let volume_output = ignore_stale_stream_command(volume_output, &stream.id);
                    if volume_output.skipped && volume_output.stderr.contains("disappeared") {
                        continue;
                    }
                    self.log_command_executions(
                        "route.streams",
                        std::slice::from_ref(&volume_output),
                    );
                }
            }
        }
        Ok(routed_stream_ids)
    }

    fn move_capture_streams_to_locked_default_input(
        &self,
        config: &MixerConfig,
        source_outputs: &[SourceOutputRoute],
        profiled_inputs: &[DeviceInfo],
        bluetooth_cards: &[BluetoothAudioCard],
    ) -> Result<bool, EngineError> {
        let _audio_commands = self.lock_audio_commands()?;
        let outputs = self.execute_capture_stream_moves_unlocked_with_devices(
            config,
            source_outputs,
            profiled_inputs,
            bluetooth_cards,
        )?;
        for output in &outputs {
            if output.skipped && output.stderr.contains("disappeared") {
                continue;
            }
            self.log_command_executions("default.input", std::slice::from_ref(output));
        }
        Ok(outputs.iter().any(|output| !output.skipped))
    }

    fn execute_capture_stream_moves_unlocked(
        &self,
        config: &MixerConfig,
        source_outputs: &[SourceOutputRoute],
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let bluetooth_cards = self.bluetooth_audio_cards().unwrap_or_default();
        let profiled_inputs = self.profiled_inputs().unwrap_or_default();
        self.execute_capture_stream_moves_unlocked_with_devices(
            config,
            source_outputs,
            &profiled_inputs,
            &bluetooth_cards,
        )
    }

    fn execute_capture_stream_moves_unlocked_with_devices(
        &self,
        config: &MixerConfig,
        source_outputs: &[SourceOutputRoute],
        profiled_inputs: &[DeviceInfo],
        bluetooth_cards: &[BluetoothAudioCard],
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let mut commands =
            capture_stream_move_commands_to_locked_default_input(config, source_outputs);
        let planned_capture_ids = commands
            .iter()
            .filter_map(|command| command.args.get(1).cloned())
            .collect::<BTreeSet<_>>();
        let fallback_source = best_hardware_input(profiled_inputs, bluetooth_cards);
        commands.extend(
            capture_stream_move_commands_for_bluetooth_protection(
                source_outputs,
                fallback_source.as_deref(),
                bluetooth_cards,
            )
            .into_iter()
            .filter(|command| {
                command
                    .args
                    .get(1)
                    .is_none_or(|source_output_id| !planned_capture_ids.contains(source_output_id))
            }),
        );
        self.prune_capture_move_failures()?;
        commands.retain(|command| {
            let Some(source_output_id) = command.args.get(1) else {
                return true;
            };
            let signature = capture_move_signature_for_command(command, source_outputs);
            !self.capture_move_recently_failed(source_output_id, &signature)
        });
        if commands.is_empty() {
            return Ok(Vec::new());
        }

        self.log_engine_event(
            "default.input",
            format!(
                "moving {} active capture stream(s) to the controlled WaveLinux microphone",
                commands.len()
            ),
        );
        let results = commands
            .into_iter()
            .filter_map(|pulse_command| {
                let source_output_id = pulse_command.args.get(1)?.clone();
                let target_source = pulse_command.args.get(2)?.clone();
                let signature = capture_move_signature_for_command(&pulse_command, source_outputs);
                let command = match self
                    .pipewire_registry
                    .capture_route_backend(&source_output_id, &target_source)
                {
                    Some(StreamRouteBackend::Native(route)) => plan_move_native_capture_stream(
                        route.stream_node_id,
                        &route.target_object_serial,
                        &route.target_node_name,
                    ),
                    Some(StreamRouteBackend::Unavailable(detail)) => {
                        self.log_engine_event(
                            "default.input",
                            format!("stream={source_output_id} deferred={detail}"),
                        );
                        return None;
                    }
                    Some(StreamRouteBackend::PulseCompatibility) => pulse_command,
                    None => {
                        self.log_engine_event(
                            "default.input",
                            format!(
                                "stream={source_output_id} skipped because it is no longer present in the PipeWire registry"
                            ),
                        );
                        return None;
                    }
                };
                let result = self.pw.execute(command.clone());
                let output = command_execution_with_spec(command, result);
                let output = ignore_stale_stream_command(output, &source_output_id);
                Some((source_output_id, signature, output))
            })
            .collect::<Vec<_>>();
        self.remember_failed_capture_moves(&results)?;
        Ok(results.into_iter().map(|(_, _, output)| output).collect())
    }

    fn capture_move_recently_failed(&self, source_output_id: &str, signature: &str) -> bool {
        // PipeWire source-output ids can be reused after route changes, so the
        // failure key includes the current route signature as well as the id.
        self.capture_move_failures
            .lock()
            .ok()
            .and_then(|failures| failures.get(source_output_id).cloned())
            .is_some_and(|failure| {
                failure.signature == signature
                    && failure.failed_at.elapsed() < capture_move_failure_backoff(failure.attempts)
            })
    }

    fn prune_capture_move_failures(&self) -> Result<(), EngineError> {
        let mut failures = self
            .capture_move_failures
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        failures.retain(|_, failure| {
            failure.failed_at.elapsed() < capture_move_failure_backoff(failure.attempts)
        });
        Ok(())
    }

    fn remember_failed_capture_moves(
        &self,
        results: &[(String, String, CommandExecution)],
    ) -> Result<(), EngineError> {
        let mut failures = self
            .capture_move_failures
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        let now = Instant::now();
        for (source_output_id, signature, output) in results {
            if output.error.is_some() {
                let attempts = failures
                    .get(source_output_id)
                    .filter(|failure| failure.signature == *signature)
                    .map(|failure| failure.attempts.saturating_add(1))
                    .unwrap_or(1);
                failures.insert(
                    source_output_id.clone(),
                    CaptureMoveFailure {
                        failed_at: now,
                        attempts,
                        signature: signature.clone(),
                    },
                );
            } else {
                failures.remove(source_output_id);
            }
        }
        Ok(())
    }

    fn app_stream_move_recently_failed(&self, stream_id: &str) -> bool {
        self.app_stream_move_failures
            .lock()
            .ok()
            .and_then(|failures| failures.get(stream_id).copied())
            .is_some_and(|failed_at| failed_at.elapsed() < APP_STREAM_MOVE_FAILURE_BACKOFF)
    }

    fn remember_app_stream_move_result(
        &self,
        stream_id: &str,
        output: &CommandExecution,
    ) -> Result<(), EngineError> {
        let mut failures = self
            .app_stream_move_failures
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        failures.retain(|_, failed_at| failed_at.elapsed() < APP_STREAM_MOVE_FAILURE_BACKOFF);
        if output.error.is_some() {
            failures.insert(stream_id.to_string(), Instant::now());
        } else {
            failures.remove(stream_id);
        }
        Ok(())
    }

    fn move_unready_routed_streams_to_default(
        &self,
        config: &MixerConfig,
        graph: &RuntimeGraph,
        managed_modules: &[ManagedModule],
        source_outputs: &[SourceOutputRoute],
        sink_inputs: &[SinkInputRoute],
    ) -> Result<bool, EngineError> {
        let stream_ids = graph
            .app_streams
            .iter()
            .filter(|stream| {
                stream.routed_channel_id.is_some()
                    && (app_stream_is_transient_event(stream)
                        || !stream_route_ready(
                            config,
                            graph,
                            managed_modules,
                            source_outputs,
                            sink_inputs,
                            stream,
                        ))
            })
            .map(|stream| stream.id.clone())
            .collect::<Vec<_>>();
        if stream_ids.is_empty() {
            return Ok(false);
        }

        self.log_engine_event(
            "route.streams",
            format!(
                "moving {} app stream(s) to the default output until WaveLinux routing is ready",
                stream_ids.len()
            ),
        );
        let _audio_commands = self.lock_audio_commands()?;
        for stream_id in stream_ids {
            let command = plan_move_app_stream_to_default(&stream_id);
            let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
            let output = ignore_stale_stream_command(output, &stream_id);
            if output.skipped && output.stderr.contains("disappeared") {
                self.log_engine_event(
                    "route.streams",
                    format!("stream {stream_id} disappeared before fallback routing; ignoring stale state"),
                );
                continue;
            }
            self.remember_app_stream_move_result(&stream_id, &output)?;
            self.log_command_executions("route.streams", &[output]);
        }
        Ok(true)
    }

    fn apply_configured_stream_volumes(
        &self,
        config: &MixerConfig,
        streams: &[AppStream],
    ) -> Result<bool, EngineError> {
        let updates = streams
            .iter()
            .filter_map(|stream| {
                configured_volume_update_for_stream(config, stream)
                    .map(|volume| (stream.id.clone(), volume))
            })
            .collect::<Vec<_>>();

        if updates.is_empty() {
            return Ok(false);
        }

        self.log_engine_event(
            "route.volumes",
            format!(
                "applying {} offline app volume preset(s): {}",
                updates.len(),
                updates
                    .iter()
                    .map(|(stream_id, volume)| format!("{stream_id}->{:.0}%", volume * 100.0))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        );
        let _audio_commands = self.lock_audio_commands()?;
        for (stream_id, volume) in updates {
            let command = plan_set_stream_volume(&stream_id, volume);
            let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
            let output = ignore_stale_stream_command(output, &stream_id);
            if output.skipped && output.stderr.contains("disappeared") {
                self.log_engine_event(
                    "route.volumes",
                    format!(
                        "stream {stream_id} disappeared before volume preset; ignoring stale state"
                    ),
                );
                continue;
            }
            self.log_command_executions("route.volumes", &[output]);
        }
        Ok(true)
    }

    fn remember_observed_apps(&self, streams: &[AppStream]) -> Result<bool, EngineError> {
        if streams.is_empty() {
            return Ok(false);
        }

        let seen_unix = OffsetDateTime::now_utc().unix_timestamp();
        let mut remembered = Vec::new();
        {
            let mut config = self.write_config()?;
            for stream in streams {
                if app_stream_is_transient_event(stream) {
                    continue;
                }
                if let Some(app) = config.remember_app_stream(stream, seen_unix)? {
                    remembered.push(app.display_name);
                }
            }
        }

        if remembered.is_empty() {
            return Ok(false);
        }

        self.persist_config()?;
        self.log_engine_event(
            "apps.remember",
            format!(
                "remembered_or_updated={} apps={}",
                remembered.len(),
                remembered.join(",")
            ),
        );
        Ok(true)
    }

    fn apply_graph_levels(
        &self,
        config: &MixerConfig,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let (state, _) = self
            .pw
            .audio_state_snapshot_with_effect_availability_timed(None, Vec::new());
        let mut commands = graph_sink_level_commands(config, &state.sink_levels);
        commands.extend(managed_route_level_commands(
            config,
            &state.routes.source_output_routes,
            &state.routes.sink_input_routes,
        ));
        if !commands.is_empty() {
            self.log_engine_event(
                "route.levels",
                format!("repairing {} graph level(s)", commands.len()),
            );
        }
        let mut outputs = commands
            .into_iter()
            .map(|command| {
                let result = self.pw.execute(command.clone());
                command_execution_with_stale_stream_skip(command, result)
            })
            .collect::<Vec<_>>();
        // Keep this call for non-snapshot callers whose route handles can change
        // while levels are being restored. It becomes a no-op once the first
        // snapshot-derived commands have converged.
        if outputs.iter().any(|output| output.error.is_some()) {
            outputs.extend(self.apply_managed_route_levels(config)?);
        }
        Ok(outputs)
    }

    fn apply_managed_route_levels(
        &self,
        config: &MixerConfig,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        let sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
        let commands = managed_route_level_commands(config, &source_outputs, &sink_inputs);
        if !commands.is_empty() {
            self.log_engine_event(
                "route.levels",
                format!("repairing {} managed route level(s)", commands.len()),
            );
        }
        Ok(commands
            .into_iter()
            .map(|command| {
                let result = self.pw.execute(command.clone());
                command_execution_with_stale_stream_skip(command, result)
            })
            .collect())
    }

    fn apply_default_device_locks(
        &self,
        config: &MixerConfig,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let mut commands = Vec::new();
        if config.settings.lock_default_output {
            if let Some(channel) = default_output_channel(config) {
                commands.push(plan_set_default_sink(&channel.virtual_sink_name));
            }
        }
        if config.settings.lock_default_input {
            if let Some(source) = default_input_source(config) {
                commands.push(plan_set_default_source(&source));
            }
        }

        Ok(self
            .pw
            .execute_all(commands)
            .into_iter()
            .map(command_execution)
            .collect())
    }

    fn confirmed_default_device_lock_repair_needed(
        &self,
        config: &MixerConfig,
        observed_source: Option<&str>,
        observed_sink: Option<&str>,
    ) -> bool {
        if !default_device_lock_repair_needed(config, observed_source, observed_sink) {
            return false;
        }
        let input_repair = default_input_lock_repair_needed(config, observed_source)
            && self.pw.default_source().map_or(true, |current| {
                default_input_lock_repair_needed(config, current.as_deref())
            });
        let output_repair = default_output_lock_repair_needed(config, observed_sink)
            && self.pw.default_sink().map_or(true, |current| {
                default_output_lock_repair_needed(config, current.as_deref())
            });
        input_repair || output_repair
    }

    fn restore_startup_default_devices(
        &self,
        restore_default_output: bool,
    ) -> Vec<CommandExecution> {
        let mut commands = Vec::new();
        let bluetooth_cards = self.bluetooth_audio_cards().unwrap_or_default();
        commands.extend(plan_bluetooth_a2dp_profiles(
            &bluetooth_cards,
            &BTreeMap::new(),
            true,
        ));
        if restore_default_output {
            if let Some(sink) = self.startup_defaults.sink.as_deref() {
                commands.push(CommandSpec::new(
                    CommandDomain::Route,
                    "pactl",
                    ["set-default-sink", sink],
                    format!("restore default output to {sink}"),
                ));
            }
        }
        if let Some(source) = self.startup_defaults.source.as_deref() {
            if bluetooth_input_would_force_hfp(source, &bluetooth_cards) {
                self.log_engine_event(
                    "cleanup.bluetooth",
                    format!(
                        "skipped restoring Bluetooth default input {source} to keep A2DP active"
                    ),
                );
            } else {
                commands.push(CommandSpec::new(
                    CommandDomain::Route,
                    "pactl",
                    ["set-default-source", source],
                    format!("restore default input to {source}"),
                ));
            }
        }

        self.pw
            .execute_all(commands)
            .into_iter()
            .map(command_execution)
            .collect()
    }

    fn bluetooth_a2dp_repair_needed(
        &self,
        bluetooth_cards: &[BluetoothAudioCard],
        force_all_a2dp: bool,
    ) -> Result<bool, EngineError> {
        let mut runtime = self
            .runtime
            .write()
            .map_err(|_| EngineError::LockPoisoned)?;
        prune_initialized_bluetooth_cards(&mut runtime, bluetooth_cards);
        Ok(!plan_bluetooth_a2dp_profiles(
            bluetooth_cards,
            &runtime.initialized_bluetooth_cards,
            force_all_a2dp,
        )
        .is_empty())
    }

    fn ensure_bluetooth_a2dp_profiles(
        &self,
        force_all_a2dp: bool,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let bluetooth_cards = self.bluetooth_audio_cards()?;
        self.ensure_bluetooth_a2dp_profiles_for_cards(&bluetooth_cards, force_all_a2dp)
    }

    fn ensure_bluetooth_a2dp_profiles_for_cards(
        &self,
        bluetooth_cards: &[BluetoothAudioCard],
        force_all_a2dp: bool,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let commands = {
            let mut runtime = self
                .runtime
                .write()
                .map_err(|_| EngineError::LockPoisoned)?;
            prune_initialized_bluetooth_cards(&mut runtime, bluetooth_cards);
            let commands = plan_bluetooth_a2dp_profiles(
                bluetooth_cards,
                &runtime.initialized_bluetooth_cards,
                force_all_a2dp,
            );
            for card in bluetooth_cards {
                if let Some(pref) = &card.preferred_a2dp_profile {
                    runtime
                        .initialized_bluetooth_cards
                        .insert(card.name.clone(), pref.clone());
                }
            }
            commands
        };
        Ok(self
            .pw
            .execute_all(commands)
            .into_iter()
            .map(command_execution)
            .collect())
    }

    fn sanitize_hardware_input_for_bluetooth_a2dp(
        &self,
        source_device: Option<String>,
    ) -> Option<String> {
        let source = source_device?;
        let cards = self.bluetooth_audio_cards().unwrap_or_default();
        if bluetooth_input_would_force_hfp(&source, &cards) {
            self.log_engine_event(
                "bluetooth.input",
                format!(
                    "ignored Bluetooth input {source} because A2DP is available for the same headset"
                ),
            );
            None
        } else {
            Some(source)
        }
    }

    fn bluetooth_audio_cards(&self) -> Result<Vec<BluetoothAudioCard>, EngineError> {
        let cards = self.pw.bluetooth_audio_cards()?;
        let inputs = self.pw.list_inputs().unwrap_or_default();
        let outputs = self.pw.list_outputs().unwrap_or_default();
        self.bluetooth_audio_cards_for_devices(cards, &inputs, &outputs)
    }

    fn bluetooth_audio_cards_for_devices(
        &self,
        mut cards: Vec<BluetoothAudioCard>,
        inputs: &[DeviceInfo],
        outputs: &[DeviceInfo],
    ) -> Result<Vec<BluetoothAudioCard>, EngineError> {
        if let Ok(catalog) = self.hardware_profiles() {
            let mut profiled_inputs = inputs.to_vec();
            let mut profiled_outputs = outputs.to_vec();
            if let Ok(config) = self.read_config() {
                apply_profile_policy_to_devices(
                    &mut profiled_inputs,
                    &catalog,
                    &config.device_policy,
                );
                apply_profile_policy_to_devices(
                    &mut profiled_outputs,
                    &catalog,
                    &config.device_policy,
                );
            } else {
                apply_profiles_to_devices(&mut profiled_inputs, &catalog);
                apply_profiles_to_devices(&mut profiled_outputs, &catalog);
            }

            for card in &mut cards {
                let preferred_codecs = find_preferred_codecs_for_card(
                    card,
                    &profiled_inputs,
                    &profiled_outputs,
                    &catalog,
                );
                if !preferred_codecs.is_empty() {
                    if let Some(new_preferred) = card
                        .profiles
                        .iter()
                        .filter(|p| {
                            p.available
                                && p.sinks > 0
                                && (p.name.to_ascii_lowercase().contains("a2dp")
                                    || p.description.to_ascii_lowercase().contains("a2dp"))
                        })
                        .max_by_key(|p| {
                            (
                                a2dp_codec_rank_with_preferences(
                                    &p.name,
                                    &p.description,
                                    &preferred_codecs,
                                ),
                                p.priority,
                            )
                        })
                        .map(|p| p.name.clone())
                    {
                        card.preferred_a2dp_profile = Some(new_preferred);
                    }
                }
            }
        }
        Ok(cards)
    }

    fn effective_config_for_audio_graph(&self, config: &MixerConfig) -> MixerConfig {
        let bluetooth_cards = self.bluetooth_audio_cards().unwrap_or_default();
        let inputs = self.profiled_inputs().unwrap_or_default();
        let outputs = self.profiled_outputs().unwrap_or_default();
        let default_sink = self.pw.default_sink().ok().flatten();
        let active_sink = self.pw.active_playback_sink().ok().flatten();
        let default_source = self.pw.default_source().ok().flatten();
        let effective = effective_config_with_profiled_devices(
            config,
            &inputs,
            &outputs,
            &bluetooth_cards,
            default_source.as_deref(),
            default_sink.as_deref(),
            active_sink.as_deref(),
        );
        self.config_with_unhealthy_effects_bypassed(&effective)
    }

    fn profiled_inputs(&self) -> Result<Vec<DeviceInfo>, EngineError> {
        let mut inputs = self.pw.list_inputs()?;
        let config = self.read_config()?.clone();
        self.ensure_remote_profiles_for_devices(&inputs, &config.device_policy)?;
        if let Ok(catalog) = self.hardware_profiles() {
            apply_profile_policy_to_devices(&mut inputs, &catalog, &config.device_policy);
        }
        Ok(inputs)
    }

    fn profiled_outputs(&self) -> Result<Vec<DeviceInfo>, EngineError> {
        let mut outputs = self.pw.list_outputs()?;
        let config = self.read_config()?.clone();
        self.ensure_remote_profiles_for_devices(&outputs, &config.device_policy)?;
        if let Ok(catalog) = self.hardware_profiles() {
            apply_profile_policy_to_devices(&mut outputs, &catalog, &config.device_policy);
        }
        Ok(outputs)
    }

    fn rebuild_effect_chain_configs(&self) -> Result<Vec<PathBuf>, EngineError> {
        self.rebuild_effect_chain_configs_for_runtime_prefix(&graph_prefix())
    }

    fn rebuild_effect_chain_configs_for_runtime_prefix(
        &self,
        runtime_prefix: &str,
    ) -> Result<Vec<PathBuf>, EngineError> {
        let config = self.config_with_unhealthy_effects_bypassed_for_runtime_prefix(
            &self.read_config()?.clone(),
            runtime_prefix,
        );
        self.rebuild_effect_chain_configs_from_config(&config, runtime_prefix)
    }

    fn rebuild_effect_chain_configs_from_config(
        &self,
        config: &MixerConfig,
        runtime_prefix: &str,
    ) -> Result<Vec<PathBuf>, EngineError> {
        let _writes = self
            .effect_config_writes
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        let dir = self.paths.effect_chains_dir();
        let control_dir = self.paths.control_sockets_dir();
        fs::create_dir_all(&dir)?;
        create_private_runtime_dir(&control_dir)?;

        let catalog = EffectCatalog::default();
        let mut written = Vec::new();
        let mut desired = BTreeSet::new();
        let mut desired_sockets = BTreeSet::new();
        let mut manifest_channels = Vec::new();
        for channel in config.channels.iter().filter(|channel| {
            channel_uses_persistent_audio_core(channel)
                || channel.effects.iter().any(|effect| !effect.bypassed)
        }) {
            let filter_channel = channel_with_effect_enable_applied(channel);
            let file_name = effect_chain_file_name(&channel.id, "conf");
            desired.insert(file_name.clone());
            let path = dir.join(&file_name);
            let tmp_path = dir.join(format!(".{}.{}.tmp", file_name, Uuid::new_v4().simple()));
            fs::write(&tmp_path, render_filter_chain(&filter_channel, &catalog))?;
            fs::rename(&tmp_path, &path)?;
            written.push(path);

            let file_name = effect_chain_file_name(&channel.id, "json");
            desired.insert(file_name.clone());
            let path = dir.join(&file_name);
            let mut dsp_config = dsp_channel_config(channel);
            dsp_config.generation = self.desired_effect_generation(&channel.id);
            dsp_config.adaptive_latency =
                dsp_adaptive_latency_config(&config.settings.adaptive_latency);
            let socket_path = self.paths.channel_control_socket(&channel.id);
            let socket_name = socket_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("wavelinux6-chain-channel.sock")
                .to_string();
            desired_sockets.insert(socket_name.clone());
            dsp_config.control_socket_path = Some(socket_path.to_string_lossy().into_owned());
            if runtime_prefix == "wavelinux6" {
                manifest_channels.push(dsp_config.clone());
            }
            write_json(&path, &dsp_config)?;
            written.push(path);

            if channel_uses_adaptive_latency_bridge(channel) {
                let file_name = effect_chain_file_name(&channel.id, "bridge.json");
                desired.insert(file_name.clone());
                let path = dir.join(&file_name);
                let bridge_config =
                    dsp_adaptive_bridge_config(channel, config, &self.paths.runtime_dir);
                write_json(&path, &bridge_config)?;
                written.push(path);
            }
        }

        if runtime_prefix == "wavelinux6" {
            manifest_channels.sort_by(|left, right| left.channel_id.cmp(&right.channel_id));
            let learned_quantum_floors = self
                .adaptive_quantum
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?
                .learned_floors
                .clone();
            let mut manifest_mixes = config
                .mixes
                .iter()
                .map(|mix| {
                    let mut dsp_mix = dsp_mix_config(mix, config);
                    dsp_mix.pipewire_quantum_frames =
                        learned_quantum_floor_for_mix(mix, &learned_quantum_floors);
                    dsp_mix
                })
                .collect::<Vec<_>>();
            manifest_mixes.sort_by(|left, right| left.mix_id.cmp(&right.mix_id));
            let manifest_content = serde_json::to_string(&(&manifest_channels, &manifest_mixes))?;
            desired_sockets.insert(wavelinux_dsp::MIX_CONTROL_SOCKET_FILE.into());
            let manifest = wavelinux_dsp::DspCoreManifest::new(
                content_revision(&manifest_content),
                manifest_channels,
            )
            .with_runtime_root(self.paths.runtime_dir.to_string_lossy().into_owned())
            .with_mixes(
                manifest_mixes,
                Some(
                    self.paths
                        .mix_control_socket()
                        .to_string_lossy()
                        .into_owned(),
                ),
            );
            let manifest_path = dir.join(AUDIO_CORE_MANIFEST_FILE);
            write_json(&manifest_path, &manifest)?;
            written.push(manifest_path);
        }

        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with(&format!("{}-chain-", graph_prefix()))
                && (name.ends_with(".conf")
                    || name.ends_with(".json")
                    || name.ends_with(".bridge.json"))
                && !desired.contains(name)
            {
                fs::remove_file(path)?;
            }
        }
        for entry in fs::read_dir(&control_dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with(&format!("{}-chain-", graph_prefix()))
                && name.ends_with(".sock")
                && !desired_sockets.contains(name)
            {
                fs::remove_file(path)?;
            }
        }
        Ok(written)
    }

    fn start_effect_chain_processes(
        &self,
        config: &MixerConfig,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        if graph_prefix() == "wavelinux6" {
            let mut outputs = vec![self.start_persistent_audio_core_process()];
            if outputs.iter().all(|output| output.error.is_none()) {
                outputs.extend(self.sync_persistent_audio_core_targets()?);
                outputs.extend(
                    config
                        .channels
                        .iter()
                        .filter(|channel| channel_uses_persistent_audio_core(channel))
                        .map(|channel| self.start_effect_chain_process(channel)),
                );
            }
            return Ok(outputs);
        }
        let mut outputs = Vec::new();
        for channel in config.channels.iter().filter(|channel| {
            channel_uses_persistent_audio_core(channel)
                || channel.effects.iter().any(|effect| !effect.bypassed)
        }) {
            outputs.push(self.start_effect_chain_process(channel));
        }
        Ok(outputs)
    }

    fn sync_persistent_audio_core_targets(&self) -> Result<Vec<CommandExecution>, EngineError> {
        let manifest_path = self
            .paths
            .effect_chains_dir()
            .join(AUDIO_CORE_MANIFEST_FILE);
        let mut manifest: wavelinux_dsp::DspCoreManifest = read_json(&manifest_path)?;
        manifest
            .resolve_control_socket_paths()
            .map_err(EngineError::Io)?;
        manifest.validate().map_err(EngineError::Io)?;
        let mut outputs = Vec::with_capacity(manifest.channels.len() + manifest.mixes.len());
        for channel in &manifest.channels {
            if !channel.input_target_capable && channel.input_target_node_name.is_none() {
                continue;
            }
            let target = channel.input_target_node_name.as_deref();
            let args = match target {
                Some(target) => vec![
                    "set_input_target".to_string(),
                    channel.channel_id.clone(),
                    target.to_string(),
                ],
                None => vec!["clear_input_target".to_string(), channel.channel_id.clone()],
            };
            let command = CommandSpec::new(
                CommandDomain::Effects,
                dsp_helper_program(),
                args,
                format!(
                    "retarget '{}' without restarting audio",
                    channel.channel_name
                ),
            );
            if self.options.dry_run {
                outputs.push(skipped_command(command));
                continue;
            }
            let result = send_audio_core_input_target(
                &self.paths.channel_control_socket(&channel.channel_id),
                &channel.channel_id,
                target,
            )
            .map(|response| CommandOutput {
                command: command.clone(),
                stdout: response.to_string(),
                stderr: "persistent input target update applied".into(),
                skipped: true,
            })
            .map_err(PwError::Io);
            outputs.push(command_execution_with_spec(command, result));
        }
        for mix in &manifest.mixes {
            let command = CommandSpec::new(
                CommandDomain::Effects,
                dsp_helper_program(),
                std::iter::once("set_output_targets".to_string())
                    .chain(std::iter::once(mix.mix_id.clone()))
                    .chain(mix.output_target_node_names.iter().cloned())
                    .collect::<Vec<_>>(),
                format!("retarget '{}' without restarting audio", mix.mix_name),
            );
            if self.options.dry_run {
                outputs.push(skipped_command(command));
                continue;
            }
            let result = send_audio_core_output_targets(
                &self.paths.mix_control_socket(),
                &mix.mix_id,
                &mix.output_target_node_names,
            )
            .map(|response| CommandOutput {
                command: command.clone(),
                stdout: response.to_string(),
                stderr: "persistent output target update applied".into(),
                skipped: true,
            })
            .map_err(PwError::Io);
            outputs.push(command_execution_with_spec(command, result));
        }
        Ok(outputs)
    }

    fn start_effect_chain_process(&self, channel: &Channel) -> CommandExecution {
        if channel_uses_persistent_audio_core(channel) {
            return self.update_persistent_audio_core_channel(channel);
        }
        let path = self
            .paths
            .effect_chains_dir()
            .join(effect_chain_file_name(&channel.id, "conf"));
        let (program, args) = effect_chain_launch_command(
            channel,
            &path,
            wavelinux_dsp::AudioRuntimeMode::from_env(),
            graph_prefix() == "wavelinux6",
        );
        let command = CommandSpec::new(
            CommandDomain::Effects,
            program.clone(),
            args.clone(),
            format!("start '{}' effect chain", channel.name),
        );
        let log_path = self.effect_chain_log_path(channel);
        let json_path = path.with_extension("json");
        let config_revision =
            audio_core_channel_revision_from_path(&json_path).unwrap_or_else(|_| {
                content_revision(&fs::read_to_string(&json_path).unwrap_or_default())
            });
        let native_core = program.ends_with("wavelinux6-audio-core")
            && args.iter().any(|arg| arg == "--run-native");

        let result = if self.options.dry_run {
            Ok(CommandOutput {
                command: command.clone(),
                stdout: String::new(),
                stderr: String::new(),
                skipped: true,
            })
        } else if self.effect_chain_process_is_tracked(&channel.id)
            && self.effect_chain_nodes_visible(channel)
        {
            let current_revision = self.tracked_effect_chain_config_revision(&channel.id);
            if native_core && current_revision.as_deref() != Some(config_revision.as_str()) {
                let socket_path = self.paths.channel_control_socket(&channel.id);
                match send_effect_chain_swap(
                    &socket_path,
                    &channel.id,
                    &json_path,
                    &config_revision,
                    self.desired_effect_generation(&channel.id),
                ) {
                    Ok(response) => {
                        self.remember_effect_chain_config_revision(
                            &channel.id,
                            config_revision.clone(),
                        );
                        Ok(CommandOutput {
                            command: command.clone(),
                            stdout: response.to_string(),
                            stderr: "native chain swap queued without restarting audio".into(),
                            skipped: true,
                        })
                    }
                    Err(err) => Err(PwError::Io(format!(
                        "native chain update was rejected; existing audio remains active: {err}"
                    ))),
                }
            } else {
                Ok(CommandOutput {
                    command: command.clone(),
                    stdout: String::new(),
                    stderr: "effect helper is already running at the requested revision".into(),
                    skipped: true,
                })
            }
        } else {
            if self.effect_chain_process_is_tracked(&channel.id) {
                self.stop_tracked_effect_chain_process(&channel.id);
            }
            let stdout = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_path);
            let stderr = OpenOptions::new().create(true).append(true).open(&log_path);
            match (stdout, stderr) {
                (Ok(stdout), Ok(stderr)) => {
                    let mut child = host_command(&program);
                    child
                        .args(&args)
                        .stdin(Stdio::null())
                        .stdout(Stdio::from(stdout))
                        .stderr(Stdio::from(stderr));
                    #[cfg(unix)]
                    {
                        child.process_group(0);
                    }
                    child
                        .spawn()
                        .and_then(|child_process| {
                            let pid = child_process.id();
                            let mut processes =
                                self.effect_chain_processes.lock().map_err(|_| {
                                    std::io::Error::other("effect process lock poisoned")
                                })?;
                            if let Some(mut previous) = processes.insert(
                                channel.id.clone(),
                                EffectChainProcess {
                                    program: program.clone(),
                                    child: child_process,
                                    config_revision: config_revision.clone(),
                                },
                            ) {
                                let previous_pid = previous.child.id();
                                let _ = terminate_effect_chain_child(
                                    &previous.program,
                                    &mut previous.child,
                                    EFFECT_CHAIN_STOP_GRACE,
                                );
                                self.log_engine_event(
                                    "effects.process",
                                    format!("replaced tracked {} pid={previous_pid}", channel.id),
                                );
                            }
                            self.remember_effect_chain_config_revision(
                                &channel.id,
                                config_revision.clone(),
                            );
                            Ok(CommandOutput {
                                command: command.clone(),
                                stdout: String::new(),
                                stderr: format!("{} pid={pid}", log_path.display()),
                                skipped: false,
                            })
                        })
                        .map_err(|err| {
                            if err.kind() == std::io::ErrorKind::NotFound {
                                PwError::CommandNotFound(program.clone())
                            } else {
                                PwError::Io(err.to_string())
                            }
                        })
                }
                (Err(err), _) | (_, Err(err)) => Err(PwError::Io(err.to_string())),
            }
        };
        command_execution(result)
    }

    fn start_persistent_audio_core_process(&self) -> CommandExecution {
        let manifest_path = self
            .paths
            .effect_chains_dir()
            .join(AUDIO_CORE_MANIFEST_FILE);
        let program = dsp_helper_program();
        let args = vec![
            "--run-core".to_string(),
            "--manifest".to_string(),
            manifest_path.to_string_lossy().to_string(),
        ];
        let command = CommandSpec::new(
            CommandDomain::Effects,
            program.clone(),
            args.clone(),
            "start persistent WaveLinux 6 audio core",
        );
        if self.options.dry_run {
            return skipped_command(command);
        }

        self.reap_effect_chain_processes();
        let topology_revision = audio_core_topology_revision(&manifest_path).unwrap_or_default();
        let running_revision = self
            .effect_chain_processes
            .lock()
            .ok()
            .and_then(|processes| {
                processes
                    .get(AUDIO_CORE_PROCESS_KEY)
                    .map(|process| process.config_revision.clone())
            });
        if running_revision.as_deref() == Some(topology_revision.as_str()) {
            match self.wait_for_persistent_audio_core_ready(&manifest_path) {
                Ok(ready) => {
                    return CommandExecution {
                    command,
                    stdout: String::new(),
                    stderr: format!(
                        "persistent audio core is already running; readiness acknowledged for {ready} endpoints"
                    ),
                    skipped: true,
                    error: None,
                    };
                }
                Err(error) => {
                    self.log_engine_event(
                        "effects.ready",
                        format!("tracked audio core failed readiness; restarting: {error}"),
                    );
                    self.mark_all_effect_cores_unavailable(&error);
                    self.stop_tracked_effect_chain_process(AUDIO_CORE_PROCESS_KEY);
                }
            }
        } else if running_revision.is_some() {
            self.stop_tracked_effect_chain_process(AUDIO_CORE_PROCESS_KEY);
        }

        if running_revision.is_none() {
            let existing_manifest = read_json::<wavelinux_dsp::DspCoreManifest>(&manifest_path);
            if let Ok(existing_manifest) = existing_manifest {
                if let Some(channel) = existing_manifest.channels.first() {
                    let socket_path = self.paths.channel_control_socket(&channel.channel_id);
                    if let Ok(response) =
                        query_audio_core_diagnostics(&socket_path, &channel.channel_id)
                    {
                        if response.core_topology_revision == topology_revision {
                            match self.wait_for_persistent_audio_core_ready(&manifest_path) {
                                Ok(ready) => {
                                    self.refresh_persistent_effect_revisions();
                                    return CommandExecution {
                                        command,
                                        stdout: String::new(),
                                        stderr: format!(
                                            "adopted existing persistent audio core; readiness acknowledged for {ready} endpoints"
                                        ),
                                        skipped: true,
                                        error: None,
                                    };
                                }
                                Err(error) => self.log_engine_event(
                                    "effects.ready",
                                    format!(
                                        "existing audio core was incomplete; requesting a controlled restart: {error}"
                                    ),
                                ),
                            }
                        } else {
                            self.log_engine_event(
                                "effects.ready",
                                format!(
                                    "existing audio core topology {} does not match {}; requesting a controlled restart",
                                    response.core_topology_revision, topology_revision
                                ),
                            );
                        }
                        if let Err(error) = request_audio_core_shutdown(
                            &socket_path,
                            &channel.channel_id,
                            EFFECT_CORE_READY_TIMEOUT,
                        ) {
                            return command_execution(Err(PwError::Io(error)));
                        }
                    }
                }
            }
        }

        let log_path = self.paths.config_dir.join(AUDIO_CORE_LOG_FILE);
        let result = (|| -> Result<CommandOutput, PwError> {
            let stdout = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&log_path)
                .map_err(|err| PwError::Io(err.to_string()))?;
            let stderr = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .map_err(|err| PwError::Io(err.to_string()))?;
            let mut child = host_command(&program);
            child
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr));
            #[cfg(unix)]
            child.process_group(0);
            let child_process = child.spawn().map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    PwError::CommandNotFound(program.clone())
                } else {
                    PwError::Io(err.to_string())
                }
            })?;
            let pid = child_process.id();
            self.effect_chain_processes
                .lock()
                .map_err(|_| PwError::Io("effect process lock poisoned".into()))?
                .insert(
                    AUDIO_CORE_PROCESS_KEY.into(),
                    EffectChainProcess {
                        program: program.clone(),
                        child: child_process,
                        config_revision: topology_revision,
                    },
                );
            let ready = self
                .wait_for_persistent_audio_core_ready(&manifest_path)
                .map_err(PwError::Io)?;
            self.refresh_persistent_effect_revisions();
            Ok(CommandOutput {
                command: command.clone(),
                stdout: String::new(),
                stderr: format!(
                    "{} pid={pid} readiness_endpoints={ready}",
                    log_path.display()
                ),
                skipped: false,
            })
        })();
        if let Err(error) = &result {
            let error = format!("audio core startup failed: {error}");
            self.mark_all_effect_cores_unavailable(&error);
            self.stop_tracked_effect_chain_process(AUDIO_CORE_PROCESS_KEY);
        }
        command_execution(result)
    }

    fn wait_for_persistent_audio_core_ready(&self, manifest_path: &Path) -> Result<usize, String> {
        let mut manifest: wavelinux_dsp::DspCoreManifest = read_json(manifest_path)
            .map_err(|err| format!("failed to read audio-core readiness manifest: {err}"))?;
        manifest.resolve_control_socket_paths()?;
        manifest.validate()?;
        let mut ready = 0_usize;
        for channel in &manifest.channels {
            let socket_path = self.paths.channel_control_socket(&channel.channel_id);
            let response = wait_for_audio_core_ready(
                &socket_path,
                &channel.channel_id,
                EFFECT_CORE_READY_TIMEOUT,
            )?;
            if response.core_topology_revision != wavelinux_dsp::core_topology_revision(&manifest) {
                return Err(format!(
                    "audio core topology {} does not match expected {}",
                    response.core_topology_revision,
                    wavelinux_dsp::core_topology_revision(&manifest)
                ));
            }
            self.observe_effect_core_ready(channel, &response);
            self.log_engine_event(
                "effects.ready",
                format!(
                    "channel_id={} desired_generation={} acknowledged_generation={} protocol={} resolved_control_socket={}",
                    channel.channel_id,
                    channel.generation,
                    response.acknowledged_generation,
                    response.protocol_version,
                    socket_path.display(),
                ),
            );
            ready = ready.saturating_add(1);
        }
        if !manifest.mixes.is_empty() {
            let socket_path = self.paths.mix_control_socket();
            for mix in &manifest.mixes {
                let response = wait_for_audio_core_ready(
                    &socket_path,
                    &mix.mix_id,
                    EFFECT_CORE_READY_TIMEOUT,
                )?;
                self.log_engine_event(
                    "effects.ready",
                    format!(
                        "mix_id={} protocol={} resolved_control_socket={}",
                        mix.mix_id,
                        response.protocol_version,
                        socket_path.display(),
                    ),
                );
                ready = ready.saturating_add(1);
            }
        }
        self.change_signal.notify_state();
        Ok(ready)
    }

    fn observe_effect_core_ready(
        &self,
        config: &wavelinux_dsp::DspChannelConfig,
        response: &AudioCoreDiagnosticsResponse,
    ) {
        self.observe_effect_core_diagnostics(&config.channel_id, Ok(response));
    }

    fn mark_all_effect_cores_unavailable(&self, error: &str) {
        let slots = self
            .effect_updates
            .lock()
            .map(|slots| slots.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for slot in slots {
            if let Ok(mut state) = slot.state.lock() {
                state.status.core_healthy = false;
                state.status.pending = false;
                state.status.last_error = Some(error.to_string());
                state.status.resolve_state();
            }
        }
        self.change_signal.notify_state();
    }

    fn observe_effect_core_diagnostics(
        &self,
        channel_id: &str,
        observation: Result<&AudioCoreDiagnosticsResponse, &str>,
    ) {
        let slot = self
            .effect_updates
            .lock()
            .ok()
            .and_then(|slots| slots.get(channel_id).cloned());
        let Some(slot) = slot else {
            return;
        };
        let Ok(mut state) = slot.state.lock() else {
            return;
        };
        let previous = state.status.clone();
        match observation {
            Ok(response) => {
                state.status.core_healthy = true;
                if response.acknowledged_generation == state.status.desired_generation {
                    state.status.applied_generation = response.acknowledged_generation;
                    state.status.pending = false;
                    state.status.last_error = None;
                    state.recovery_not_before = None;
                } else {
                    state.status.pending = true;
                    state.status.last_error = Some(format!(
                        "audio core acknowledged generation {}, latest desired generation is {}",
                        response.acknowledged_generation, state.status.desired_generation
                    ));
                }
            }
            Err(error) => {
                state.status.core_healthy = false;
                state.status.pending = false;
                state.status.last_error = Some(error.to_string());
            }
        }
        state.status.resolve_state();
        if state.status != previous {
            self.change_signal.notify_state();
        }
    }

    fn update_persistent_audio_core_channel(&self, channel: &Channel) -> CommandExecution {
        if !self.audio_core_process_is_tracked() {
            return self.start_persistent_audio_core_process();
        }
        let json_path = self
            .paths
            .effect_chains_dir()
            .join(effect_chain_file_name(&channel.id, "json"));
        let socket_path = self.paths.channel_control_socket(&channel.id);
        let config_revision =
            audio_core_channel_revision_from_path(&json_path).unwrap_or_else(|_| {
                content_revision(&fs::read_to_string(&json_path).unwrap_or_default())
            });
        let command = CommandSpec::new(
            CommandDomain::Effects,
            dsp_helper_program(),
            ["swap_chain", channel.id.as_str()],
            format!("update '{}' in persistent audio core", channel.name),
        );
        if self
            .tracked_effect_chain_config_revision(&channel.id)
            .as_deref()
            == Some(config_revision.as_str())
        {
            return CommandExecution {
                command,
                stdout: String::new(),
                stderr: "persistent audio core already has this channel revision".into(),
                skipped: true,
                error: None,
            };
        }
        match send_effect_chain_swap(
            &socket_path,
            &channel.id,
            &json_path,
            &config_revision,
            self.desired_effect_generation(&channel.id),
        ) {
            Ok(response) => {
                self.remember_effect_chain_config_revision(&channel.id, config_revision);
                CommandExecution {
                    command,
                    stdout: response.to_string(),
                    stderr: "native chain swap queued without restarting audio".into(),
                    skipped: true,
                    error: None,
                }
            }
            Err(err) => CommandExecution {
                command,
                stdout: String::new(),
                stderr: String::new(),
                skipped: true,
                error: Some(format!(
                    "native chain update was rejected; existing audio remains active: {err}"
                )),
            },
        }
    }

    fn effect_chain_log_path(&self, channel: &Channel) -> PathBuf {
        if channel_uses_persistent_audio_core(channel) {
            return self.paths.config_dir.join(AUDIO_CORE_LOG_FILE);
        }
        self.paths
            .config_dir
            .join(effect_chain_file_name(&channel.id, "log"))
    }

    fn effect_chain_failure_log_prefix(&self, channel: &Channel) -> String {
        format!("{}.failure.", effect_chain_file_name(&channel.id, "log"))
    }

    fn effect_chain_failure_log_path(&self, channel: &Channel) -> PathBuf {
        let timestamp = OffsetDateTime::now_utc().unix_timestamp_nanos();
        self.paths.config_dir.join(format!(
            "{}{timestamp}.log",
            self.effect_chain_failure_log_prefix(channel)
        ))
    }

    fn preserve_realtime_fallback_logs_for_runtime_prefix(
        &self,
        config: &MixerConfig,
        channel_ids: &BTreeSet<String>,
        runtime_prefix: &str,
    ) -> Vec<(String, PathBuf)> {
        if runtime_prefix != "wavelinux6" {
            return Vec::new();
        }

        config
            .channels
            .iter()
            .filter(|channel| channel_ids.contains(&channel.id))
            .filter(|channel| {
                channel
                    .effects
                    .iter()
                    .any(|effect| !effect.bypassed && realtime_fallback_effect(&effect.effect_id))
            })
            .filter(|channel| self.effect_chain_log_mentions_realtime_underrun(channel))
            .filter_map(|channel| {
                self.preserve_effect_chain_failure_log(channel)
                    .map(|path| (channel.id.clone(), path))
            })
            .collect()
    }

    fn clear_realtime_fallback_trigger_logs_for_runtime_prefix(
        &self,
        config: &MixerConfig,
        channel_ids: &BTreeSet<String>,
        runtime_prefix: &str,
    ) {
        if runtime_prefix != "wavelinux6" {
            return;
        }

        for channel in config
            .channels
            .iter()
            .filter(|channel| channel_ids.contains(&channel.id))
            .filter(|channel| self.effect_chain_log_mentions_realtime_underrun(channel))
        {
            let _ = fs::write(self.effect_chain_log_path(channel), "");
        }
    }

    fn preserve_effect_chain_failure_log(&self, channel: &Channel) -> Option<PathBuf> {
        let source = self.effect_chain_log_path(channel);
        let metadata = fs::metadata(&source).ok()?;
        if !metadata.is_file() || metadata.len() == 0 {
            return None;
        }

        let target = self.effect_chain_failure_log_path(channel);
        fs::create_dir_all(&self.paths.config_dir).ok()?;
        fs::copy(&source, &target).ok()?;
        self.preserve_effect_chain_failure_artifact(channel, &target, "conf");
        self.preserve_effect_chain_failure_artifact(channel, &target, "json");
        self.trim_effect_chain_failure_logs(channel);
        Some(target)
    }

    fn preserve_effect_chain_failure_artifact(
        &self,
        channel: &Channel,
        failure_log_path: &Path,
        suffix: &str,
    ) {
        let source = self
            .paths
            .effect_chains_dir()
            .join(effect_chain_file_name(&channel.id, suffix));
        let Ok(metadata) = fs::metadata(&source) else {
            return;
        };
        if !metadata.is_file() || metadata.len() == 0 {
            return;
        }
        let _ = fs::copy(source, failure_log_path.with_extension(suffix));
    }

    fn effect_chain_failure_log_entries(&self, channel: &Channel) -> Vec<(SystemTime, PathBuf)> {
        let prefix = self.effect_chain_failure_log_prefix(channel);
        let Ok(entries) = fs::read_dir(&self.paths.config_dir) else {
            return Vec::new();
        };

        let mut logs = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                if !name.starts_with(&prefix) || !name.ends_with(".log") {
                    return None;
                }
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                Some((modified, path))
            })
            .collect::<Vec<_>>();

        logs.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        logs
    }

    fn matching_effect_chain_failure_log_path(&self, channel: &Channel) -> Option<PathBuf> {
        let current = dsp_channel_config(channel);
        self.effect_chain_failure_log_entries(channel)
            .into_iter()
            .filter(|(modified, path)| effect_chain_failure_log_is_active(path, *modified))
            .find(|(_, path)| self.effect_chain_failure_artifact_matches_channel(path, &current))
            .map(|(_, path)| path)
    }

    fn effect_chain_failure_artifact_matches_channel(
        &self,
        failure_log_path: &Path,
        current: &wavelinux_dsp::DspChannelConfig,
    ) -> bool {
        let artifact_path = failure_log_path.with_extension("json");
        let Ok(failed) = read_json::<wavelinux_dsp::DspChannelConfig>(&artifact_path) else {
            return false;
        };
        failed.sample_rate_hz == current.sample_rate_hz
            && failed.latency_frames == current.latency_frames
            && failed.effects == current.effects
    }

    fn active_effect_chain_failure_log_path(&self, channel: &Channel) -> Option<PathBuf> {
        self.matching_effect_chain_failure_log_path(channel)
    }

    fn active_effect_chain_failure_artifact_path(
        &self,
        channel: &Channel,
        suffix: &str,
    ) -> Option<PathBuf> {
        self.active_effect_chain_failure_log_path(channel)
            .map(|path| path.with_extension(suffix))
            .filter(|path| path.exists())
    }

    fn trim_effect_chain_failure_logs(&self, channel: &Channel) {
        let prefix = self.effect_chain_failure_log_prefix(channel);
        let Ok(entries) = fs::read_dir(&self.paths.config_dir) else {
            return;
        };
        let mut logs = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = path.file_name()?.to_str()?;
                if !name.starts_with(&prefix) || !name.ends_with(".log") {
                    return None;
                }
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                Some((modified, path))
            })
            .collect::<Vec<_>>();

        logs.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        for (_, path) in logs.into_iter().skip(EFFECT_CHAIN_FAILURE_LOGS) {
            remove_effect_chain_failure_artifacts(&path);
        }
    }

    fn recent_effect_chain_failure_summary(&self, channel: &Channel) -> Option<String> {
        let current_log_path = self.effect_chain_log_path(channel);
        let channel_filter =
            channel_uses_persistent_audio_core(channel).then_some(channel.id.as_str());
        [
            Some(current_log_path),
            self.active_effect_chain_failure_log_path(channel),
        ]
        .into_iter()
        .flatten()
        .find_map(|path| effect_chain_log_failure_summary(&path, channel_filter))
    }

    fn collect_audio_core_status(&self, config: &MixerConfig) -> Vec<AudioCoreChannelStatus> {
        let mut queried = self.query_audio_core_status(config);
        apply_audio_core_discontinuity_deltas(&mut queried, &self.audio_core_underrun_counters);
        queried
    }

    fn collect_adaptive_audio_core_status(
        &self,
        config: &MixerConfig,
    ) -> Vec<AudioCoreChannelStatus> {
        let mut queried = self.query_audio_core_status(config);
        apply_audio_core_discontinuity_deltas(
            &mut queried,
            &self.adaptive_core_discontinuity_counters,
        );
        queried
    }

    fn query_audio_core_status(&self, config: &MixerConfig) -> Vec<AudioCoreChannelStatus> {
        let mut queried = config
            .channels
            .iter()
            .filter(|channel| channel_uses_persistent_audio_core(channel))
            .map(|channel| {
                let socket_path = self.paths.channel_control_socket(&channel.id);
                match query_audio_core_diagnostics(&socket_path, &channel.id) {
                    Ok(response) => {
                        self.observe_effect_core_diagnostics(&channel.id, Ok(&response));
                        AudioCoreChannelStatus {
                            channel_id: channel.id.clone(),
                            online: true,
                            sample_rate_hz: response.sample_rate_hz,
                            target_latency_msec: response.target_latency_msec,
                            current_buffer_frames: response.current_buffer_frames,
                            buffer_fill_msec: response.current_buffer_frames as f32 * 1000.0
                                / response.sample_rate_hz.max(1) as f32,
                            captured_frames: response.captured_frames,
                            rendered_frames: response.rendered_frames,
                            dropped_frames: response.dropped_frames,
                            underrun_frames: response.underrun_frames,
                            underrun_delta: 0,
                            capture_callbacks: response.capture_callbacks,
                            worker_running: response.worker_running,
                            worker_blocks: response.worker_blocks,
                            worker_queue_frames: response.worker_queue_frames,
                            worker_queue_capacity_frames: response.worker_queue_capacity_frames,
                            worker_overrun_frames: response.worker_overrun_frames,
                            accelerator_provider: response.accelerator_provider,
                            accelerator_active_states: response.accelerator_active_states,
                            accelerator_provider_pids: response.accelerator_provider_pids,
                            accelerator_provider_blocks: response.accelerator_provider_blocks,
                            accelerator_fallback_blocks: response.accelerator_fallback_blocks,
                            accelerator_deadline_misses: response.accelerator_deadline_misses,
                            accelerator_invalid_results: response.accelerator_invalid_results,
                            accelerator_stale_results: response.accelerator_stale_results,
                            accelerator_disabled_states: response.accelerator_disabled_states,
                            accelerator_startup_failures: response.accelerator_startup_failures,
                            accelerator_last_failure: response.accelerator_last_failure,
                            last_process_micros: response.last_process_micros,
                            max_process_micros: response.max_process_micros,
                            chain_swaps: response.chain_swaps,
                            non_finite_blocks: response.non_finite_blocks,
                            non_finite_samples: response.non_finite_samples,
                            non_finite_effect_mask: response.non_finite_effect_mask,
                            chain_recoveries: response.chain_recoveries,
                            chain_swap_replacements: response.chain_swap_replacements,
                            retired_chain_overflows: response.retired_chain_overflows,
                            submitted_generation: response.submitted_generation,
                            acknowledged_generation: response.acknowledged_generation,
                            submitted_route_generation: response.submitted_route_generation,
                            applied_route_generation: response.applied_route_generation,
                            input_target_node_name: response.input_target_node_name,
                            output_target_node_names: response.output_target_node_names,
                            route_target_error: response.route_target_error,
                            rate_correction: response.rate_correction,
                            error: None,
                        }
                    }
                    Err(error) => {
                        self.observe_effect_core_diagnostics(&channel.id, Err(&error));
                        AudioCoreChannelStatus {
                            channel_id: channel.id.clone(),
                            error: Some(error),
                            ..AudioCoreChannelStatus::default()
                        }
                    }
                }
            })
            .collect::<Vec<_>>();
        if config.mixes.iter().any(mix_uses_persistent_audio_core) {
            let socket_path = self.paths.mix_control_socket();
            queried.extend(config.mixes.iter().map(|mix| {
                let status_id = format!("mix:{}", mix.id);
                match query_audio_core_diagnostics(&socket_path, &mix.id) {
                    Ok(response) => AudioCoreChannelStatus {
                        channel_id: status_id,
                        online: true,
                        sample_rate_hz: response.sample_rate_hz,
                        target_latency_msec: response.target_latency_msec,
                        current_buffer_frames: response.current_buffer_frames,
                        buffer_fill_msec: response.current_buffer_frames as f32 * 1000.0
                            / response.sample_rate_hz.max(1) as f32,
                        captured_frames: response.captured_frames,
                        rendered_frames: response.rendered_frames,
                        dropped_frames: response.dropped_frames,
                        underrun_frames: response.underrun_frames,
                        underrun_delta: 0,
                        capture_callbacks: response.capture_callbacks,
                        worker_running: response.worker_running,
                        worker_blocks: response.worker_blocks,
                        worker_queue_frames: response.worker_queue_frames,
                        worker_queue_capacity_frames: response.worker_queue_capacity_frames,
                        worker_overrun_frames: response.worker_overrun_frames,
                        accelerator_provider: response.accelerator_provider,
                        accelerator_active_states: response.accelerator_active_states,
                        accelerator_provider_pids: response.accelerator_provider_pids,
                        accelerator_provider_blocks: response.accelerator_provider_blocks,
                        accelerator_fallback_blocks: response.accelerator_fallback_blocks,
                        accelerator_deadline_misses: response.accelerator_deadline_misses,
                        accelerator_invalid_results: response.accelerator_invalid_results,
                        accelerator_stale_results: response.accelerator_stale_results,
                        accelerator_disabled_states: response.accelerator_disabled_states,
                        accelerator_startup_failures: response.accelerator_startup_failures,
                        accelerator_last_failure: response.accelerator_last_failure,
                        last_process_micros: response.last_process_micros,
                        max_process_micros: response.max_process_micros,
                        chain_swaps: response.chain_swaps,
                        non_finite_blocks: response.non_finite_blocks,
                        non_finite_samples: response.non_finite_samples,
                        non_finite_effect_mask: response.non_finite_effect_mask,
                        chain_recoveries: response.chain_recoveries,
                        chain_swap_replacements: response.chain_swap_replacements,
                        retired_chain_overflows: response.retired_chain_overflows,
                        submitted_generation: response.submitted_generation,
                        acknowledged_generation: response.acknowledged_generation,
                        submitted_route_generation: response.submitted_route_generation,
                        applied_route_generation: response.applied_route_generation,
                        input_target_node_name: response.input_target_node_name,
                        output_target_node_names: response.output_target_node_names,
                        route_target_error: response.route_target_error,
                        rate_correction: response.rate_correction,
                        error: None,
                    },
                    Err(error) => AudioCoreChannelStatus {
                        channel_id: status_id,
                        error: Some(error),
                        ..AudioCoreChannelStatus::default()
                    },
                }
            }));
        }
        queried.sort_by(|left, right| left.channel_id.cmp(&right.channel_id));
        queried
    }

    fn effect_chain_diagnostics(
        &self,
        config: &MixerConfig,
        graph: &RuntimeGraph,
    ) -> Vec<Diagnostic> {
        let audio_core = self.collect_audio_core_status(config);
        self.effect_chain_diagnostics_with_core(config, graph, &audio_core)
    }

    fn effect_chain_diagnostics_with_core(
        &self,
        config: &MixerConfig,
        graph: &RuntimeGraph,
        audio_core: &[AudioCoreChannelStatus],
    ) -> Vec<Diagnostic> {
        let availability = graph
            .effect_availability
            .iter()
            .map(|effect| (effect.effect_id.as_str(), effect))
            .collect::<BTreeMap<_, _>>();
        let catalog = EffectCatalog::default();
        let mut diagnostics = Vec::new();
        diagnostics.extend(audio_core_integrity_diagnostics(audio_core));

        for channel in config
            .channels
            .iter()
            .filter(|channel| channel.effects.iter().any(|effect| !effect.bypassed))
        {
            let path = self
                .paths
                .effect_chains_dir()
                .join(effect_chain_file_name(&channel.id, "conf"));
            let exists = path.exists();
            diagnostics.push(Diagnostic {
                code: format!("effects.chain.{}", channel.id),
                severity: if exists {
                    DiagnosticSeverity::Info
                } else {
                    DiagnosticSeverity::Warning
                },
                message: if exists {
                    format!("{} FX chain config is ready", channel.name)
                } else {
                    format!("{} FX chain config is missing", channel.name)
                },
                action: if exists {
                    None
                } else {
                    Some("Change an effect to rebuild effect configs".into())
                },
            });

            let source_name = effect_chain_source_name(channel);
            let source_visible = graph.inputs.iter().any(|input| input.name == source_name);
            diagnostics.push(Diagnostic {
                code: format!("effects.source.{}", channel.id),
                severity: if source_visible {
                    DiagnosticSeverity::Info
                } else {
                    DiagnosticSeverity::Warning
                },
                message: if source_visible {
                    format!("{} FX source is visible", channel.name)
                } else {
                    format!("{} FX source is not visible", channel.name)
                },
                action: if source_visible {
                    None
                } else {
                    Some(
                        "Repair the audio graph or bypass the channel FX to keep raw audio routed"
                            .into(),
                    )
                },
            });

            let live_core_status = audio_core
                .iter()
                .find(|status| status.channel_id == channel.id && status.online);
            let realtime_underrun_log = live_core_status.map_or_else(
                || self.effect_chain_log_mentions_realtime_underrun(channel),
                |status| status.underrun_delta > 0,
            );
            let preserved_failure_log = self.active_effect_chain_failure_log_path(channel);
            if realtime_underrun_log || preserved_failure_log.is_some() {
                let current_log_path = self.effect_chain_log_path(channel);
                let log_path = preserved_failure_log.as_ref().unwrap_or(&current_log_path);
                let failure_summary = live_core_status
                    .map(|status| {
                        format!(
                            ": underrun_delta={} dropped_frames={} process_us={} buffer_ms={:.1}",
                            status.underrun_delta,
                            status.dropped_frames,
                            status.last_process_micros,
                            status.buffer_fill_msec
                        )
                    })
                    .or_else(|| {
                        self.recent_effect_chain_failure_summary(channel)
                            .map(|summary| format!(": {summary}"))
                    })
                    .unwrap_or_default();
                let conf_path = self.active_effect_chain_failure_artifact_path(channel, "conf");
                let json_path = self.active_effect_chain_failure_artifact_path(channel, "json");
                let mut action = format!(
                    "WaveLinux 6 bypassed heavy FX to keep audio alive; inspect {} before reenabling the affected effect",
                    log_path.display()
                );
                if let Some(conf_path) = conf_path {
                    action.push_str(&format!(
                        "; generated PipeWire config: {}",
                        conf_path.display()
                    ));
                }
                if let Some(json_path) = json_path {
                    action.push_str(&format!("; generated DSP config: {}", json_path.display()));
                }
                diagnostics.push(Diagnostic {
                    code: format!("effects.underrun.{}", channel.id),
                    severity: DiagnosticSeverity::Warning,
                    message: format!(
                        "{} FX chain is missing realtime deadlines{}",
                        channel.name, failure_summary
                    ),
                    action: Some(action),
                });
            }
            if self.effect_chain_recent_log_mentions_clipping(channel) {
                diagnostics.push(Diagnostic {
                    code: format!("effects.clipping.{}", channel.id),
                    severity: DiagnosticSeverity::Warning,
                    message: format!("{} FX input is clipping", channel.name),
                    action: Some(
                        "Lower the hardware mic gain slightly or keep a limiter at the end of the voice chain"
                            .into(),
                    ),
                });
            }

            for effect in channel.effects.iter().filter(|effect| !effect.bypassed) {
                let Some(effect_availability) = availability.get(effect.effect_id.as_str()) else {
                    continue;
                };
                if effect_availability.available {
                    continue;
                }

                let effect_name = catalog
                    .effects
                    .iter()
                    .find(|definition| definition.id == effect.effect_id)
                    .map(|definition| definition.name.as_str())
                    .unwrap_or(effect.effect_id.as_str());
                diagnostics.push(Diagnostic {
                    code: format!("effects.missing.{}.{}", channel.id, effect.instance_id),
                    severity: DiagnosticSeverity::Warning,
                    message: format!("{} on {} is unavailable", effect_name, channel.name),
                    action: Some(effect_availability.detail.clone()),
                });
            }
        }

        diagnostics
    }

    fn effect_chain_log_mentions_realtime_underrun(&self, channel: &Channel) -> bool {
        let channel_filter =
            channel_uses_persistent_audio_core(channel).then_some(channel.id.as_str());
        effect_chain_log_mentions_recent(
            &self.effect_chain_log_path(channel),
            &["underrun detected", "processing too slow"],
            channel_filter,
        )
        .is_some()
            || effect_chain_log_recent_native_underrun(
                &self.effect_chain_log_path(channel),
                channel_filter,
            )
            .is_some()
    }

    fn effect_chain_has_active_realtime_failure(&self, channel: &Channel) -> bool {
        self.effect_chain_log_mentions_realtime_underrun(channel)
            || self.active_effect_chain_failure_log_path(channel).is_some()
    }

    fn effect_chain_recent_log_mentions_clipping(&self, channel: &Channel) -> bool {
        self.effect_chain_recent_log_mentions(channel, &["clipping detected"])
    }

    fn effect_chain_recent_log_mentions(&self, channel: &Channel, markers: &[&str]) -> bool {
        let channel_filter =
            channel_uses_persistent_audio_core(channel).then_some(channel.id.as_str());
        if effect_chain_log_mentions_recent(
            &self.effect_chain_log_path(channel),
            markers,
            channel_filter,
        )
        .is_some()
        {
            return true;
        }
        self.active_effect_chain_failure_log_path(channel)
            .as_deref()
            .is_some_and(|path| effect_chain_log_mentions(path, markers, channel_filter))
    }

    fn config_with_unhealthy_effects_bypassed(&self, config: &MixerConfig) -> MixerConfig {
        self.config_with_unhealthy_effects_bypassed_for_runtime_prefix(config, &graph_prefix())
    }

    fn config_with_unhealthy_effects_bypassed_for_runtime_prefix(
        &self,
        config: &MixerConfig,
        runtime_prefix: &str,
    ) -> MixerConfig {
        if runtime_prefix != "wavelinux6" {
            // Legacy runtimes keep warnings diagnostic-only so their behavior is
            // not changed while WaveLinux 6 is being migrated.
            return config.clone();
        }

        let mut effective = config.clone();
        for channel in &mut effective.channels {
            if !self.effect_chain_has_active_realtime_failure(channel) {
                continue;
            }
            bypass_realtime_fallback_effects(channel);
        }
        effective
    }

    fn realtime_fallback_sync_channel_ids_for_runtime_prefix(
        &self,
        config: &MixerConfig,
        runtime_prefix: &str,
    ) -> BTreeSet<String> {
        if runtime_prefix != "wavelinux6" {
            return BTreeSet::new();
        }

        config
            .channels
            .iter()
            .filter(|channel| {
                channel
                    .effects
                    .iter()
                    .any(|effect| !effect.bypassed && realtime_fallback_effect(&effect.effect_id))
            })
            .filter(|channel| self.effect_chain_log_mentions_realtime_underrun(channel))
            .map(|channel| channel.id.clone())
            .collect()
    }

    fn cleanup_modules(
        &self,
        mut should_unload: impl FnMut(&ManagedModule) -> bool,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let modules = self.pw.managed_modules()?;
        Ok(self.cleanup_modules_from_snapshot(&modules, |module| should_unload(module)))
    }

    fn cleanup_modules_from_snapshot(
        &self,
        modules: &[ManagedModule],
        mut should_unload: impl FnMut(&ManagedModule) -> bool,
    ) -> Vec<CommandExecution> {
        let modules = modules
            .iter()
            .filter(|module| should_unload(module))
            .cloned()
            .collect::<Vec<_>>();
        self.pw
            .execute_all(plan_unload_modules(&modules))
            .into_iter()
            .map(command_execution)
            .collect()
    }

    fn cleanup_all_modules_until_clear(&self) -> Result<Vec<CommandExecution>, EngineError> {
        let mut outputs = Vec::new();

        for pass in 1..=CLEANUP_MODULE_PASSES {
            let modules = self.pw.managed_modules()?;
            if modules.is_empty() {
                if pass > 1 {
                    self.log_engine_event(
                        "cleanup.modules",
                        format!("managed modules cleared after {} pass(es)", pass - 1),
                    );
                }
                return Ok(outputs);
            }

            self.log_engine_event(
                "cleanup.modules",
                format!("pass={pass} managed_modules={}", modules.len()),
            );
            outputs.extend(
                self.pw
                    .execute_all(plan_unload_modules(&modules))
                    .into_iter()
                    .map(command_execution),
            );
            thread::sleep(CLEANUP_MODULE_SETTLE);
        }

        let survivors = self.pw.managed_modules()?;
        if !survivors.is_empty() {
            let summary = survivors
                .iter()
                .map(|module| {
                    format!(
                        "{}:{}",
                        module.module_id,
                        module.role.as_deref().unwrap_or("unknown"),
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            self.log_engine_event(
                "cleanup.modules",
                format!("managed modules still present after cleanup: {summary}"),
            );
        }

        Ok(outputs)
    }

    fn channel_bus_route_ids_unlocked(&self, channel_id: &str, mix_id: &str) -> ChannelBusRouteIds {
        let cached = self.read_runtime().ok().and_then(|runtime| {
            if !runtime.status.audio_graph_running {
                return None;
            }
            let route_ids = channel_bus_route_ids_from_routes(
                channel_id,
                mix_id,
                &runtime.sink_input_routes,
                &runtime.source_output_routes,
            );
            (!route_ids.is_empty()).then_some(route_ids)
        });

        cached.unwrap_or_else(|| {
            self.pw
                .find_channel_bus_route_ids(channel_id, mix_id)
                .unwrap_or_default()
        })
    }

    fn execute_channel_bus_volume_unlocked(
        &self,
        channel_id: &str,
        mix_id: &str,
        volume: f32,
    ) -> Vec<CommandExecution> {
        if graph_prefix() == "wavelinux6" {
            return vec![self.execute_native_mix_control_unlocked(
                "set_mix_bus",
                mix_id,
                Some(channel_id),
                serde_json::json!({ "volume": volume }),
            )];
        }
        let route_ids = self.channel_bus_route_ids_unlocked(channel_id, mix_id);

        plan_channel_bus_volume_commands(
            route_ids.sink_input_id.as_deref(),
            route_ids.source_output_id.as_deref(),
            volume,
        )
        .into_iter()
        .map(|command| {
            let result = self.pw.execute(command.clone());
            command_execution_with_stale_stream_skip(command, result)
        })
        .collect()
    }

    fn execute_channel_bus_mute_unlocked(
        &self,
        channel_id: &str,
        mix_id: &str,
        muted: bool,
    ) -> Vec<CommandExecution> {
        if graph_prefix() == "wavelinux6" {
            return vec![self.execute_native_mix_control_unlocked(
                "set_mix_bus",
                mix_id,
                Some(channel_id),
                serde_json::json!({ "muted": muted }),
            )];
        }
        let route_ids = self.channel_bus_route_ids_unlocked(channel_id, mix_id);

        plan_channel_bus_mute_commands(
            route_ids.sink_input_id.as_deref(),
            route_ids.source_output_id.as_deref(),
            muted,
        )
        .into_iter()
        .map(|command| {
            let result = self.pw.execute(command.clone());
            command_execution_with_stale_stream_skip(command, result)
        })
        .collect()
    }

    fn execute_native_mix_control_unlocked(
        &self,
        control_command: &str,
        mix_id: &str,
        channel_id: Option<&str>,
        fields: serde_json::Value,
    ) -> CommandExecution {
        let command = CommandSpec::new(
            CommandDomain::Level,
            dsp_helper_program(),
            [control_command, mix_id],
            match channel_id {
                Some(channel_id) => {
                    format!("update native mix {mix_id} bus {channel_id}")
                }
                None => format!("update native mix {mix_id}"),
            },
        );
        let mut payload = serde_json::json!({
            "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
            "command": control_command,
            "request_id": Uuid::new_v4().to_string(),
            "mix_id": mix_id,
        });
        if let Some(channel_id) = channel_id {
            payload["channel_id"] = serde_json::Value::String(channel_id.to_string());
        }
        if let (Some(payload), Some(fields)) = (payload.as_object_mut(), fields.as_object()) {
            payload.extend(fields.clone());
        }
        let socket_path = self.paths.mix_control_socket();
        let result = send_core_control_request(&socket_path, &payload)
            .map(|response| CommandOutput {
                command: command.clone(),
                stdout: response.to_string(),
                stderr: String::new(),
                skipped: false,
            })
            .map_err(PwError::Io);
        command_execution(result)
    }

    fn reap_effect_chain_processes(&self) {
        let mut exited = Vec::new();
        let Ok(mut processes) = self.effect_chain_processes.lock() else {
            self.log_engine_event(
                "effects.process",
                "failed to reap effect helpers; lock poisoned",
            );
            return;
        };

        processes.retain(|channel_id, process| {
            let pid = process.child.id();
            match process.child.try_wait() {
                Ok(Some(status)) => {
                    exited.push(format!("{channel_id} pid={pid} status={status}"));
                    false
                }
                Ok(None) => true,
                Err(err) => {
                    exited.push(format!("{channel_id} pid={pid} wait_error={err}"));
                    false
                }
            }
        });
        drop(processes);

        for message in exited {
            self.log_engine_event("effects.process", format!("reaped {message}"));
        }
    }

    fn active_effect_chain_pids(&self) -> BTreeSet<String> {
        self.reap_effect_chain_processes();
        self.effect_chain_processes
            .lock()
            .map(|processes| {
                processes
                    .values()
                    .map(|process| process.child.id().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn active_effect_chain_config_markers(&self) -> BTreeSet<String> {
        self.reap_effect_chain_processes();
        if self.audio_core_process_is_tracked() {
            return self
                .effect_chain_revisions
                .lock()
                .map(|revisions| {
                    revisions
                        .keys()
                        .map(|channel_id| effect_chain_file_name(channel_id, "conf"))
                        .collect()
                })
                .unwrap_or_default();
        }
        self.effect_chain_processes
            .lock()
            .map(|processes| {
                processes
                    .keys()
                    .map(|channel_id| effect_chain_file_name(channel_id, "conf"))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn effect_chain_process_is_tracked(&self, channel_id: &str) -> bool {
        if graph_prefix() == "wavelinux6" && channel_id != AUDIO_CORE_PROCESS_KEY {
            return self.audio_core_process_is_tracked();
        }
        self.reap_effect_chain_processes();
        self.effect_chain_processes
            .lock()
            .map(|processes| processes.contains_key(channel_id))
            .unwrap_or(false)
    }

    fn tracked_effect_chain_config_revision(&self, channel_id: &str) -> Option<String> {
        self.effect_chain_revisions
            .lock()
            .ok()
            .and_then(|revisions| revisions.get(channel_id).cloned())
    }

    fn remember_effect_chain_config_revision(&self, channel_id: &str, revision: String) {
        if let Ok(mut revisions) = self.effect_chain_revisions.lock() {
            revisions.insert(channel_id.to_string(), revision);
        }
    }

    fn refresh_persistent_effect_revisions(&self) {
        let Ok(config) = self.read_config() else {
            return;
        };
        let revisions = config
            .channels
            .iter()
            .filter_map(|channel| {
                let path = self
                    .paths
                    .effect_chains_dir()
                    .join(effect_chain_file_name(&channel.id, "json"));
                audio_core_channel_revision_from_path(&path)
                    .ok()
                    .map(|revision| (channel.id.clone(), revision))
            })
            .collect::<BTreeMap<_, _>>();
        if let Ok(mut current) = self.effect_chain_revisions.lock() {
            *current = revisions;
        }
    }

    fn audio_core_process_is_tracked(&self) -> bool {
        self.reap_effect_chain_processes();
        self.effect_chain_processes
            .lock()
            .map(|processes| processes.contains_key(AUDIO_CORE_PROCESS_KEY))
            .unwrap_or(false)
    }

    fn effect_chain_nodes_visible(&self, channel: &Channel) -> bool {
        let graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        effect_chain_endpoint_readiness_for_graph(&graph, channel).ready()
    }

    fn stop_tracked_effect_chain_process(&self, channel_id: &str) {
        let child = self
            .effect_chain_processes
            .lock()
            .ok()
            .and_then(|mut processes| processes.remove(channel_id));
        let Some(mut process) = child else {
            return;
        };
        if channel_id == AUDIO_CORE_PROCESS_KEY {
            if let Ok(mut revisions) = self.effect_chain_revisions.lock() {
                revisions.clear();
            }
        }

        let pid = process.child.id();
        match terminate_effect_chain_child(
            &process.program,
            &mut process.child,
            EFFECT_CHAIN_STOP_GRACE,
        ) {
            Ok(status) => {
                self.log_engine_event(
                    "effects.process",
                    format!("stopped tracked {channel_id} pid={pid} status={status}"),
                );
            }
            Err(err) => {
                self.log_engine_event(
                    "effects.process",
                    format!("failed to stop tracked {channel_id} pid={pid}: {err}"),
                );
            }
        }
    }

    fn stop_all_tracked_effect_chain_processes(&self) {
        let channel_ids = self
            .effect_chain_processes
            .lock()
            .map(|processes| processes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for channel_id in channel_ids {
            self.stop_tracked_effect_chain_process(&channel_id);
        }
    }

    fn cleanup_stale_processes(&self) -> Result<Vec<CommandExecution>, EngineError> {
        let processes = self.stale_audio_processes_excluding_active()?;
        let outputs = self
            .pw
            .execute_all(plan_kill_stale_processes(&processes))
            .into_iter()
            .map(command_execution)
            .collect();
        if !processes.is_empty() {
            thread::sleep(Duration::from_millis(50));
            self.reap_effect_chain_processes();
        }
        Ok(outputs)
    }

    fn stale_audio_processes_excluding_active(&self) -> Result<Vec<StaleProcess>, EngineError> {
        let active_effect_pids = self.active_effect_chain_pids();
        let active_effect_config_markers = self.active_effect_chain_config_markers();
        Ok(self
            .pw
            .stale_processes()?
            .into_iter()
            .filter(|process| {
                !stale_process_is_active_effect_child(
                    process,
                    &active_effect_pids,
                    &active_effect_config_markers,
                )
            })
            .collect())
    }

    fn cleanup_stale_modules_for_config(
        &self,
        config: &MixerConfig,
        active_app_channel_ids: &BTreeSet<String>,
        active_mix_ids: &BTreeSet<String>,
        preserve_stale_monitor_routes: bool,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let modules = self.pw.managed_modules()?;
        self.cleanup_stale_modules_for_config_from_snapshot(
            config,
            active_app_channel_ids,
            active_mix_ids,
            preserve_stale_monitor_routes,
            &modules,
        )
    }

    fn cleanup_stale_modules_for_config_from_snapshot(
        &self,
        config: &MixerConfig,
        active_app_channel_ids: &BTreeSet<String>,
        active_mix_ids: &BTreeSet<String>,
        preserve_stale_monitor_routes: bool,
        modules: &[ManagedModule],
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let mut outputs = self.cleanup_stale_processes()?;
        let mut seen = BTreeSet::new();
        outputs.extend(self.cleanup_modules_from_snapshot(modules, |module| {
            if preserve_stale_monitor_routes && module.role.as_deref() == Some("mix_monitor") {
                return false;
            }
            if module_is_stale_for_active_routes(
                module,
                config,
                active_app_channel_ids,
                active_mix_ids,
            ) {
                return true;
            }

            module_dedupe_key_for_config(module, config).is_some_and(|key| !seen.insert(key))
        }));
        Ok(outputs)
    }

    fn cleanup_stale_auto_device_modules_for_config_from_snapshot(
        &self,
        config: &MixerConfig,
        active_mix_ids: &BTreeSet<String>,
        preserve_stale_monitor_routes: bool,
        modules: &[ManagedModule],
    ) -> Vec<CommandExecution> {
        let mut seen = BTreeSet::new();
        self.cleanup_modules_from_snapshot(modules, |module| {
            if !module_is_auto_device_route(module) {
                return false;
            }
            if preserve_stale_monitor_routes && module.role.as_deref() == Some("mix_monitor") {
                return false;
            }
            if module.role.as_deref() == Some("mix_monitor")
                && module
                    .mix_id
                    .as_deref()
                    .is_none_or(|mix_id| !active_mix_ids.contains(mix_id))
            {
                return true;
            }
            if module_is_stale_for_config(module, config) {
                return true;
            }

            module_dedupe_key_for_config(module, config).is_some_and(|key| !seen.insert(key))
        })
    }

    fn preload_monitor_output_routes_for_config(
        &self,
        config: &MixerConfig,
        active_mix_ids: &BTreeSet<String>,
        initial_state: &AudioStateSnapshot,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let plan = plan_ensure_graph_for_active_routes(config, &BTreeSet::new(), active_mix_ids);
        let mut existing_state = initial_state.clone();
        let monitor_commands = plan
            .commands
            .into_iter()
            .filter(command_is_mix_monitor_route)
            .collect::<Vec<_>>();
        if monitor_commands.iter().any(|command| {
            command_targets_bluetooth_sink(command)
                && !monitor_route_endpoints_available(command, &existing_state.graph)
        }) {
            for _ in 0..6 {
                thread::sleep(Duration::from_millis(200));
                existing_state.graph = self
                    .pw
                    .snapshot_for_config_with_effect_availability(None, Vec::new());
                if monitor_commands.iter().all(|command| {
                    !command_targets_bluetooth_sink(command)
                        || monitor_route_endpoints_available(command, &existing_state.graph)
                }) {
                    break;
                }
            }
            if monitor_commands.iter().all(|command| {
                !command_targets_bluetooth_sink(command)
                    || monitor_route_endpoints_available(command, &existing_state.graph)
            }) {
                existing_state = self
                    .pw
                    .audio_state_snapshot_with_effect_availability_timed(None, Vec::new())
                    .0;
            }
        }
        if monitor_commands.iter().any(|command| {
            command_targets_bluetooth_sink(command)
                && monitor_route_endpoints_available(command, &existing_state.graph)
                && !repair_command_is_satisfied(
                    command,
                    &existing_state.graph,
                    &existing_state.routes.source_output_routes,
                    &existing_state.routes.sink_input_routes,
                    &existing_state.routes.managed_modules,
                )
        }) {
            self.log_engine_event(
                "hotplug.output",
                "Bluetooth monitor output is visible; waiting for A2DP transport to settle",
            );
            thread::sleep(BLUETOOTH_MONITOR_ROUTE_SETTLE);
            existing_state = self
                .pw
                .audio_state_snapshot_with_effect_availability_timed(None, Vec::new())
                .0;
        }
        let mut skipped = Vec::new();
        let commands = monitor_commands
            .into_iter()
            .filter_map(|command| {
                if !monitor_route_endpoints_available(&command, &existing_state.graph) {
                    skipped.push(skipped_command_with_stderr(
                        command,
                        "monitor output is not visible yet; preserving existing monitor route",
                    ));
                    return None;
                }
                (!repair_command_is_satisfied(
                    &command,
                    &existing_state.graph,
                    &existing_state.routes.source_output_routes,
                    &existing_state.routes.sink_input_routes,
                    &existing_state.routes.managed_modules,
                ))
                .then_some(command)
            })
            .collect::<Vec<_>>();

        let mut outputs = skipped;
        outputs.extend(
            self.pw
                .execute_all(commands)
                .into_iter()
                .map(command_execution),
        );
        Ok(outputs)
    }

    fn apply_start_at_login(&self, enabled: bool) -> Result<(), EngineError> {
        let autostart_file = self.paths.autostart_file();
        if enabled {
            fs::create_dir_all(&self.paths.autostart_dir)?;
            fs::write(&autostart_file, render_autostart_desktop_entry())?;
        } else if autostart_file.exists() {
            fs::remove_file(autostart_file)?;
        }
        Ok(())
    }

    fn update_config<T>(
        &self,
        update: impl FnOnce(&mut MixerConfig) -> Result<T, ModelError>,
    ) -> Result<Result<T, EngineError>, EngineError> {
        let result = {
            let mut config = self.write_config()?;
            update(&mut config).map_err(EngineError::from)
        };
        if result.is_ok() {
            self.persist_config()?;
        }
        Ok(result)
    }

    fn persist_config(&self) -> Result<(), EngineError> {
        let config = self.read_config()?.clone();
        let serialized = serde_json::to_string(&config)?;
        let revision = content_revision(&serialized);
        let mut persisted = self
            .persisted_config_revision
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        if persisted.as_deref() == Some(revision.as_str()) && self.paths.config_file().is_file() {
            return Ok(());
        }
        write_json(&self.paths.config_file(), &config)?;
        *persisted = Some(revision);
        drop(persisted);
        self.change_signal.notify_config();
        Ok(())
    }

    fn persist_followed_monitor_output_selection(
        &self,
        saved_config: &MixerConfig,
        effective_config: &MixerConfig,
    ) -> Result<(), EngineError> {
        if !saved_config.settings.monitor_follows_default_output {
            return Ok(());
        }
        let Some(output) = effective_config
            .mixes
            .iter()
            .find(|mix| mix.id == "monitor")
            .and_then(|mix| mix.monitor_output.clone())
            .filter(|output| is_restorable_device(output))
        else {
            return Ok(());
        };
        let saved_output = saved_config
            .mixes
            .iter()
            .find(|mix| mix.id == "monitor")
            .and_then(|mix| mix.monitor_output.as_deref());
        if saved_output == Some(output.as_str())
            && saved_config.device_policy.preferred_output.as_deref() == Some(output.as_str())
        {
            return Ok(());
        }

        let mut changed = false;
        {
            let mut config = self.write_config()?;
            if let Some(mix) = config.mixes.iter_mut().find(|mix| mix.id == "monitor") {
                if mix.monitor_output.as_deref() != Some(output.as_str()) {
                    mix.set_outputs(vec![output.clone()]);
                    changed = true;
                }
            }
            if config.device_policy.preferred_output.as_deref() != Some(output.as_str()) {
                config.device_policy.preferred_output = Some(output.clone());
                changed = true;
            }
            if config.device_policy.active_output_fallback {
                config.device_policy.active_output_fallback = false;
                changed = true;
            }
        }
        if changed {
            self.persist_config()?;
            self.log_engine_event(
                "hotplug.output",
                format!("persisted followed monitor output: {output}"),
            );
        }
        Ok(())
    }

    fn read_config(&self) -> Result<std::sync::RwLockReadGuard<'_, MixerConfig>, EngineError> {
        self.config.read().map_err(|_| EngineError::LockPoisoned)
    }

    fn write_config(&self) -> Result<std::sync::RwLockWriteGuard<'_, MixerConfig>, EngineError> {
        self.config.write().map_err(|_| EngineError::LockPoisoned)
    }

    fn read_runtime(&self) -> Result<std::sync::RwLockReadGuard<'_, RuntimeCache>, EngineError> {
        self.runtime.read().map_err(|_| EngineError::LockPoisoned)
    }

    fn write_runtime(&self) -> Result<std::sync::RwLockWriteGuard<'_, RuntimeCache>, EngineError> {
        self.runtime.write().map_err(|_| EngineError::LockPoisoned)
    }

    fn runtime_refreshed_within(&self, max_age: Duration) -> Result<bool, EngineError> {
        Ok(self
            .read_runtime()?
            .refreshed_at
            .is_some_and(|refreshed_at| refreshed_at.elapsed() <= max_age))
    }

    fn lock_audio_commands(&self) -> Result<MutexGuard<'_, ()>, EngineError> {
        let started = Instant::now();
        loop {
            match self.audio_commands.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(_)) => return Err(EngineError::LockPoisoned),
                Err(TryLockError::WouldBlock)
                    if started.elapsed() >= AUDIO_COMMAND_LOCK_TIMEOUT =>
                {
                    self.log_engine_event(
                        "audio.lock",
                        format!(
                            "timed out after {}ms waiting for graph mutation lock",
                            started.elapsed().as_millis()
                        ),
                    );
                    return Err(EngineError::AudioBusy);
                }
                Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn try_lock_audio_commands_for_refresh(
        &self,
        area: &str,
    ) -> Result<Option<MutexGuard<'_, ()>>, EngineError> {
        // Deferred repair/effect sync jobs must requeue instead of waiting here;
        // blocking can deadlock user-visible state behind an in-flight mutation.
        match self.audio_commands.try_lock() {
            Ok(guard) => Ok(Some(guard)),
            Err(TryLockError::Poisoned(_)) => Err(EngineError::LockPoisoned),
            Err(TryLockError::WouldBlock) => {
                let message = if area == "effects.sync" {
                    "graph mutation already in progress; deferring effect route sync"
                } else {
                    "graph mutation already in progress; deferring automatic route repair"
                };
                self.log_engine_event(area, message);
                Ok(None)
            }
        }
    }

    fn lock_runtime_refresh(&self) -> Result<MutexGuard<'_, ()>, EngineError> {
        self.runtime_refresh
            .lock()
            .map_err(|_| EngineError::LockPoisoned)
    }

    fn snapshot_for_config(
        &self,
        config: Option<&MixerConfig>,
    ) -> Result<RuntimeGraph, EngineError> {
        Ok(self.snapshot_for_config_timed(config)?.0)
    }

    fn snapshot_for_config_timed(
        &self,
        config: Option<&MixerConfig>,
    ) -> Result<(RuntimeGraph, Vec<SnapshotCommandTiming>), EngineError> {
        let effect_availability = self.effect_availability()?;
        let (mut graph, timings) = if let Some((snapshot, generation)) = self
            .pipewire_registry
            .audio_state_snapshot(config, effect_availability.clone())
        {
            (
                snapshot.graph,
                vec![SnapshotCommandTiming {
                    label: format!("pipewire registry generation {generation}"),
                    elapsed_ms: 0,
                    succeeded: true,
                }],
            )
        } else {
            self.pw
                .snapshot_for_config_with_effect_availability_timed(config, effect_availability)
        };
        let profile_policy = match config {
            Some(config) => config.device_policy.clone(),
            None => self.read_config()?.device_policy.clone(),
        };
        let devices = graph
            .inputs
            .iter()
            .chain(graph.outputs.iter())
            .cloned()
            .collect::<Vec<_>>();
        self.ensure_remote_profiles_for_devices(&devices, &profile_policy)?;
        if let Ok(catalog) = self.hardware_profiles() {
            apply_profile_policy_to_graph(&mut graph, &catalog, &profile_policy);
        }
        Ok((graph, timings))
    }

    fn audio_state_snapshot_for_config_timed(
        &self,
        config: Option<&MixerConfig>,
    ) -> Result<(AudioStateSnapshot, Vec<SnapshotCommandTiming>), EngineError> {
        let effect_availability = self.effect_availability()?;
        let (mut snapshot, timings) = if let Some((snapshot, generation)) = self
            .pipewire_registry
            .audio_state_snapshot(config, effect_availability.clone())
        {
            (
                snapshot,
                vec![SnapshotCommandTiming {
                    label: format!("pipewire registry generation {generation}"),
                    elapsed_ms: 0,
                    succeeded: true,
                }],
            )
        } else {
            self.pw
                .audio_state_snapshot_with_effect_availability_timed(config, effect_availability)
        };
        let profile_policy = match config {
            Some(config) => config.device_policy.clone(),
            None => self.read_config()?.device_policy.clone(),
        };
        let devices = snapshot
            .graph
            .inputs
            .iter()
            .chain(snapshot.graph.outputs.iter())
            .cloned()
            .collect::<Vec<_>>();
        self.ensure_remote_profiles_for_devices(&devices, &profile_policy)?;
        if let Ok(catalog) = self.hardware_profiles() {
            apply_profile_policy_to_graph(&mut snapshot.graph, &catalog, &profile_policy);
        }
        Ok((snapshot, timings))
    }

    fn host_diagnostics(&self) -> Result<Vec<Diagnostic>, EngineError> {
        let mut cache = self
            .host_diagnostics
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        if cache_expired(cache.checked_at, HOST_DIAGNOSTICS_TTL) {
            let mut diagnostics = self.pw.diagnostics();
            diagnostics.extend(self.runtime_identity_diagnostics());
            diagnostics.extend(pipewire_audio_health_diagnostics(
                &self.pipewire_audio_health.snapshot(),
            ));
            diagnostics.extend(dsp_runtime_diagnostics());
            cache.value = diagnostics;
            cache.checked_at = Some(Instant::now());
        }
        Ok(cache.value.clone())
    }

    fn adaptive_latency_status(
        &self,
        settings: &wavelinux_model::AdaptiveLatencySettings,
        audio_core: &[AudioCoreChannelStatus],
    ) -> Result<AdaptiveLatencyStatus, EngineError> {
        let pipewire_health = self.pipewire_audio_health.snapshot();
        let (pipewire_warning_delta, owned_pipewire_warning_delta) = {
            let mut previous = self
                .adaptive_pipewire_health_counters
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            let (warning_delta, owned_delta) = pipewire_health_deltas(&previous, &pipewire_health);
            *previous = pipewire_health;
            (warning_delta, owned_delta)
        };
        let cpu_pressure = self
            .cpu_pressure_sampler
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?
            .sample()
            .unwrap_or(0.0);
        let (signal, cpu_pressure, pipewire_warning_delta, underrun_delta) =
            adaptive_latency_signal(
                settings,
                audio_core,
                cpu_pressure,
                pipewire_warning_delta,
                owned_pipewire_warning_delta,
            );
        let mut controller = self
            .adaptive_latency
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        let now = Instant::now();
        let mut status = controller.update(
            settings,
            signal,
            cpu_pressure,
            pipewire_warning_delta,
            underrun_delta,
            now,
        );
        drop(controller);
        let output_signature = adaptive_monitor_output_signature(audio_core);
        let (quantum_frames, quantum_floor_frames, learned_floor_cache) = {
            let mut controller = self
                .adaptive_quantum
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            let (quantum_frames, quantum_floor_frames, learned_new_floor) = controller.update(
                status.pipewire_quantum_frames,
                status.underrun_delta,
                &output_signature,
                now,
            );
            let cache = learned_new_floor.then(|| AdaptiveQuantumFloorCache {
                version: ADAPTIVE_QUANTUM_FLOORS_VERSION,
                floors: controller.learned_floors.clone(),
            });
            (quantum_frames, quantum_floor_frames, cache)
        };
        if let Some(cache) = learned_floor_cache {
            match write_json(&self.paths.adaptive_quantum_floors_file(), &cache) {
                Ok(()) => self.log_engine_event(
                    "latency.quantum_cache",
                    format!(
                        "learned output_signature={} floor_frames={}",
                        output_signature, quantum_floor_frames
                    ),
                ),
                Err(error) => self.log_engine_event(
                    "latency.quantum_cache",
                    format!("could not persist learned floor: {error}"),
                ),
            }
        }
        status.pipewire_quantum_frames = quantum_frames;
        status.pipewire_quantum_floor_frames = quantum_floor_frames;
        status.buffer_fill_msec = audio_core
            .iter()
            .filter(|channel| channel.online)
            .map(|channel| channel.buffer_fill_msec)
            .max_by(f32::total_cmp);
        Ok(status)
    }

    fn send_adaptive_latency_targets(&self, config: &MixerConfig, status: &AdaptiveLatencyStatus) {
        if !status.enabled {
            return;
        }
        for channel in config
            .channels
            .iter()
            .filter(|channel| channel_uses_persistent_audio_core(channel))
        {
            let socket_path = self.paths.channel_control_socket(&channel.id);
            send_adaptive_latency_target(
                &socket_path,
                &channel.id,
                status.target_msec,
                status.pipewire_quantum_frames,
                &status.last_reason,
            );
        }
        if config.mixes.iter().any(mix_uses_persistent_audio_core) {
            let socket_path = self.paths.mix_control_socket();
            for mix in &config.mixes {
                send_adaptive_latency_target(
                    &socket_path,
                    &mix.id,
                    status.target_msec,
                    status.pipewire_quantum_frames,
                    &status.last_reason,
                );
            }
        }
    }

    fn refresh_adaptive_latency_live(&self) -> Result<(), EngineError> {
        let config = self.read_config()?.clone();
        let audio_core = self.collect_adaptive_audio_core_status(&config);
        let status =
            self.adaptive_latency_status(&config.settings.adaptive_latency, &audio_core)?;
        self.send_adaptive_latency_targets(&config, &status);

        let (target_changed, trouble_detected) = {
            let mut runtime = self.write_runtime()?;
            let target_changed = runtime.status.adaptive_latency.target_msec != status.target_msec
                || runtime.status.adaptive_latency.last_reason != status.last_reason
                || runtime.status.adaptive_latency.pipewire_quantum_frames
                    != status.pipewire_quantum_frames
                || runtime
                    .status
                    .adaptive_latency
                    .pipewire_quantum_floor_frames
                    != status.pipewire_quantum_floor_frames;
            let trouble_detected = status.underrun_delta > 0;
            runtime.status.adaptive_latency = status.clone();
            runtime.status.audio_core = audio_core;
            runtime.status.pipewire_audio_health = self.pipewire_audio_health.snapshot();
            (target_changed, trouble_detected)
        };
        if target_changed || trouble_detected {
            self.log_engine_event(
                "latency.adaptive",
                format!(
                    "target_ms={} level={} quantum_frames={} quantum_floor_frames={} reason={} discontinuity_frames={} cpu_pressure={:.3} buffer_ms={}",
                    status.target_msec,
                    status.active_level,
                    status.pipewire_quantum_frames,
                    status.pipewire_quantum_floor_frames,
                    status.last_reason,
                    status.underrun_delta,
                    status.cpu_pressure,
                    status
                        .buffer_fill_msec
                        .map(|value| format!("{value:.1}"))
                        .unwrap_or_else(|| "n/a".into()),
                ),
            );
            self.change_signal.notify_state();
        }
        Ok(())
    }

    fn effect_availability(&self) -> Result<Vec<EffectAvailability>, EngineError> {
        let mut cache = self
            .effect_availability
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        if cache_expired(cache.checked_at, EFFECT_AVAILABILITY_TTL) {
            cache.value = probe_effect_availability(&EffectCatalog::default());
            cache.checked_at = Some(Instant::now());
        }
        Ok(cache.value.clone())
    }

    pub fn refresh_effect_availability(&self) -> Result<Vec<EffectAvailability>, EngineError> {
        let mut cache = self
            .effect_availability
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        cache.value = probe_effect_availability(&EffectCatalog::default());
        cache.checked_at = Some(Instant::now());
        Ok(cache.value.clone())
    }

    fn ensure_remote_profiles_for_devices(
        &self,
        devices: &[DeviceInfo],
        policy: &wavelinux_model::DevicePolicy,
    ) -> Result<(), EngineError> {
        let catalog = {
            let mut cache = self
                .hardware_profiles
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            if cache_expired(cache.checked_at, HARDWARE_PROFILE_TTL) {
                cache.value = load_hardware_profile_catalog(&self.paths);
                cache.checked_at = Some(Instant::now());
            }
            cache.value.clone()
        };
        if !remote_profile_sync_needed(&self.paths, devices, policy, &catalog) {
            return Ok(());
        }

        {
            let mut state = self
                .remote_profile_sync
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            if state.in_flight
                || state
                    .last_started
                    .is_some_and(|started| started.elapsed() < REMOTE_PROFILE_SYNC_MIN_INTERVAL)
            {
                return Ok(());
            }
            state.in_flight = true;
            state.last_started = Some(Instant::now());
        }

        let paths = self.paths.clone();
        let devices = devices.to_vec();
        let policy = policy.clone();
        let hardware_profiles = Arc::clone(&self.hardware_profiles);
        let remote_profile_sync = Arc::clone(&self.remote_profile_sync);
        thread::spawn(move || {
            let report = sync_remote_profiles_for_devices(&paths, &devices, &policy, &catalog);
            if report.changed || !report.diagnostics.is_empty() {
                if let Ok(mut cache) = hardware_profiles.lock() {
                    if report.changed {
                        cache.value = load_hardware_profile_catalog(&paths);
                        cache.checked_at = Some(Instant::now());
                    }
                    if !report.diagnostics.is_empty() {
                        cache.value.diagnostics.extend(report.diagnostics.clone());
                        cache.checked_at.get_or_insert_with(Instant::now);
                    }
                }
                log_engine_event_to_paths(
                    &paths,
                    "hardware.profile.remote",
                    format!(
                        "matched={} fetched={} diagnostics={}",
                        report.matched,
                        report.fetched,
                        report.diagnostics.len()
                    ),
                );
            }
            if let Ok(mut state) = remote_profile_sync.lock() {
                state.in_flight = false;
            }
        });
        Ok(())
    }

    fn hardware_profiles(&self) -> Result<HardwareProfileCatalog, EngineError> {
        let mut cache = self
            .hardware_profiles
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        if cache_expired(cache.checked_at, HARDWARE_PROFILE_TTL) {
            cache.value = load_hardware_profile_catalog(&self.paths);
            cache.checked_at = Some(Instant::now());
            self.log_engine_event(
                "hardware.profile",
                format!(
                    "loaded profiles={} diagnostics={} local_dir={}",
                    cache.value.profiles.len(),
                    cache.value.diagnostics.len(),
                    self.paths.local_hardware_profiles_dir().display(),
                ),
            );
        }
        Ok(cache.value.clone())
    }

    fn reload_hardware_profiles_cache(&self) -> Result<(), EngineError> {
        let mut cache = self
            .hardware_profiles
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        cache.value = load_hardware_profile_catalog(&self.paths);
        cache.checked_at = Some(Instant::now());
        Ok(())
    }

    fn write_local_hardware_profile_override(
        &self,
        profile: &HardwareProfile,
    ) -> Result<PathBuf, EngineError> {
        let dir = self
            .paths
            .local_hardware_profiles_dir()
            .join("wavelinux-user-overrides");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "{}.json",
            safe_hardware_profile_file_id(&profile.id)
        ));
        fs::write(&path, serde_json::to_string_pretty(profile)?)?;
        Ok(path)
    }

    fn refresh_meter_supervisor(
        &self,
        config: &MixerConfig,
        graph: &RuntimeGraph,
        audio_graph_running: bool,
        mark_requested: bool,
    ) -> Result<Vec<LevelMeter>, EngineError> {
        let target_revision = MeterTargetRevision::new(self.revisions(), audio_graph_running);
        let targets = if audio_graph_running {
            meter_targets_for_config_with_devices(config, &graph.inputs)
        } else {
            Vec::new()
        };
        let requested = mark_requested
            || self
                .meter_supervisor
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?
                .requested_recently();
        let native_meters = (graph_prefix() == "wavelinux6" && audio_graph_running && requested)
            .then(|| self.native_core_level_meters(&targets));
        let update = {
            let mut supervisor = self
                .meter_supervisor
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            if let Some(Ok(meters)) = native_meters {
                supervisor.reconcile_native(targets, meters, mark_requested, target_revision)
            } else if requested {
                supervisor.reconcile(targets, mark_requested, target_revision)
            } else {
                supervisor.snapshot_or_stop_idle()
            }
        };

        self.log_meter_supervisor_update(&update);
        Ok(update.meters)
    }

    fn native_core_level_meters(&self, targets: &[MeterTarget]) -> Result<Vec<LevelMeter>, String> {
        let socket_path = self.paths.mix_control_socket();
        let response = send_core_control_request(
            &socket_path,
            &serde_json::json!({
                "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
                "command": "get_meters",
                "request_id": Uuid::new_v4().to_string(),
            }),
        )?;
        let response: NativeCoreMetersResponse = serde_json::from_value(response)
            .map_err(|err| format!("invalid meter response: {err}"))?;
        Ok(level_meters_from_native_response(targets, response))
    }

    fn log_meter_supervisor_update(&self, update: &MeterSupervisorUpdate) {
        if update.started > 0 || update.stopped > 0 || !update.failed.is_empty() {
            self.log_engine_event(
                "meters.supervisor",
                format!(
                    "backend={} started={} stopped={} failed={} active={}",
                    if update.native_backend {
                        "native-core"
                    } else {
                        "pipewire-fallback"
                    },
                    update.started,
                    update.stopped,
                    update.failed.len(),
                    update.meters.len(),
                ),
            );
            for failure in update.failed.iter().take(8) {
                self.log_engine_event("meters.supervisor", format!("failed {failure}"));
            }
        }

        if update.log_activity {
            self.log_engine_event(
                "meters.activity",
                format!(
                    "backend={} sampled_targets={} active_targets={} max_level={:.3}",
                    if update.native_backend {
                        "native-core"
                    } else {
                        "pipewire-fallback"
                    },
                    update.sampled_sources,
                    update.active_targets,
                    update.max_level,
                ),
            );
        }
    }

    fn refresh_cached_meters(&self) -> Result<(), EngineError> {
        let meters = self.meter_snapshot_or_stop_idle()?;
        let mut runtime = self.write_runtime()?;
        if runtime.status.audio_graph_running {
            runtime.graph.meters = meters;
        } else if !runtime.graph.meters.is_empty() {
            runtime.graph.meters.clear();
        }
        Ok(())
    }

    fn meter_snapshot_or_stop_idle(&self) -> Result<Vec<LevelMeter>, EngineError> {
        let update = {
            let mut supervisor = self
                .meter_supervisor
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            supervisor.snapshot_or_stop_idle()
        };

        if update.stopped > 0 {
            self.log_engine_event(
                "meters.supervisor",
                format!("stopped={} idle=true", update.stopped),
            );
        }

        Ok(update.meters)
    }

    fn stop_meter_supervisor(&self) {
        if let Ok(mut supervisor) = self.meter_supervisor.lock() {
            let stopped = supervisor.active_source_count();
            supervisor.stop_all();
            if stopped > 0 {
                self.log_engine_event("meters.supervisor", format!("stopped={stopped}"));
            }
        }
    }

    fn repair_audio_graph_if_running(self: &Arc<Self>) -> Result<(), EngineError> {
        if self.audio_graph_running_cached() {
            self.log_engine_event(
                "repair.auto",
                "config changed while audio graph was running; scheduling graph repair",
            );
            self.schedule_audio_graph_repair();
        } else {
            self.log_engine_event(
                "repair.auto",
                "config changed while audio graph was stopped; repair skipped",
            );
        }
        Ok(())
    }

    fn schedule_audio_graph_repair(self: &Arc<Self>) {
        let generation = match self.deferred_graph_repair.lock() {
            Ok(mut repair) => {
                repair.generation = repair.generation.saturating_add(1);
                repair.generation
            }
            Err(_) => {
                self.log_engine_event("repair.auto", "failed to schedule graph repair");
                return;
            }
        };
        let engine = Arc::clone(self);
        let _ = thread::Builder::new()
            .name("wavelinux-graph-repair".into())
            .spawn(move || {
                thread::sleep(GRAPH_REPAIR_DEBOUNCE);
                if engine.stop.load(Ordering::SeqCst) {
                    return;
                }
                let should_run = match engine.deferred_graph_repair.lock() {
                    Ok(repair) => repair.generation == generation,
                    Err(_) => false,
                };
                if !should_run {
                    return;
                }
                if !engine.audio_graph_running_cached() {
                    engine.log_engine_event(
                        "repair.auto",
                        "deferred graph repair skipped; graph is no longer running",
                    );
                    return;
                }
                engine.log_engine_event("repair.auto", "running deferred graph repair");
                let audio_commands = match engine.try_lock_audio_commands_for_refresh("repair.auto")
                {
                    Ok(Some(guard)) => guard,
                    Ok(None) => {
                        engine.log_engine_event(
                            "repair.auto",
                            "deferred graph repair requeued; graph mutation is still running",
                        );
                        engine.schedule_audio_graph_repair();
                        return;
                    }
                    Err(err) => {
                        engine.log_engine_event(
                            "repair.auto",
                            format!("deferred repair failed before start: {err}"),
                        );
                        return;
                    }
                };
                engine.log_engine_event("repair.start", "requested audio graph repair");
                let result = engine.repair_audio_graph_unlocked();
                drop(audio_commands);
                let _ = engine.refresh_runtime();
                match result {
                    Ok(report) => {
                        engine.log_command_executions("repair.auto", &report.outputs);
                    }
                    Err(err) => {
                        engine.log_engine_event(
                            "repair.auto",
                            format!("deferred repair failed: {err}"),
                        );
                    }
                }
            });
    }

    #[cfg(test)]
    fn sync_effect_channels(
        &self,
        channel_ids: &BTreeSet<String>,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let outputs = {
            let _audio_commands = self.lock_audio_commands()?;
            self.sync_effect_channels_unlocked(channel_ids)?
        };
        let _ = self.refresh_runtime();
        Ok(outputs)
    }

    fn try_sync_effect_channels(
        &self,
        channel_ids: &BTreeSet<String>,
    ) -> Result<Option<Vec<CommandExecution>>, EngineError> {
        let outputs = {
            // Effect route sync shares the graph mutation lock with hotplug and
            // repair. Returning None lets the scheduler requeue the same work.
            let Some(_audio_commands) = self.try_lock_audio_commands_for_refresh("effects.sync")?
            else {
                return Ok(None);
            };
            self.sync_effect_channels_unlocked(channel_ids)?
        };
        let _ = self.refresh_runtime();
        Ok(Some(outputs))
    }

    fn sync_effect_channels_unlocked(
        &self,
        channel_ids: &BTreeSet<String>,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let _effect_sync_active = self.mark_effect_sync_active();
        let config = self.effective_config_for_audio_graph(&self.read_config()?.clone());
        let channels = config
            .channels
            .iter()
            .filter(|channel| channel_ids.contains(&channel.id))
            .collect::<Vec<_>>();
        if channels.is_empty() {
            return Ok(Vec::new());
        }
        if channels
            .iter()
            .all(|channel| channel_uses_persistent_audio_core(channel))
        {
            return self.sync_persistent_audio_core_channels_unlocked(&config, &channels);
        }

        let mut outputs = self.cleanup_modules(|module| {
            matches!(
                module.role.as_deref(),
                Some("channel_to_mix") | Some("channel_to_effect") | Some("mic_passthrough")
            ) && module
                .channel_id
                .as_deref()
                .is_some_and(|channel_id| channel_ids.contains(channel_id))
        })?;
        if !outputs.is_empty() {
            thread::sleep(CLEANUP_MODULE_SETTLE);
        }

        let stale_processes = self.pw.stale_processes()?;
        let effect_processes = stale_processes
            .into_iter()
            .filter(|process| {
                channels.iter().any(|channel| {
                    process.command.contains(&format!(
                        "{}-chain-{}.conf",
                        graph_prefix(),
                        safe_file_id(&channel.id)
                    ))
                })
            })
            .collect::<Vec<_>>();
        outputs.extend(
            self.pw
                .execute_all(plan_kill_stale_processes(&effect_processes))
                .into_iter()
                .map(command_execution),
        );
        if !effect_processes.is_empty() {
            thread::sleep(Duration::from_millis(50));
            self.reap_effect_chain_processes();
        }
        let mut uncleared_effect_channels = BTreeSet::new();
        if !effect_processes.is_empty() {
            for channel in channels.iter().copied() {
                if !self.wait_for_effect_nodes_to_clear(channel) {
                    uncleared_effect_channels.insert(channel.id.clone());
                    self.log_engine_event(
                        "effects.sync",
                        format!(
                            "{} old FX nodes were still visible before restart; routing this pass from the raw channel monitor",
                            channel.name
                        ),
                    );
                }
            }
        }

        let mut linked_effect_channel_ids = BTreeSet::new();
        for channel in channels {
            let mut route_channel = (*channel).clone();
            if channel_has_active_effects(channel) {
                let start_output = self.start_effect_chain_process(channel);
                let start_failed = start_output.error.is_some();
                outputs.push(start_output);
                if uncleared_effect_channels.contains(&channel.id)
                    || start_failed
                    || !self.wait_for_effect_nodes_ready_for_routing(channel)
                {
                    self.log_engine_event(
                        "effects.sync",
                        format!(
                            "{} FX nodes did not appear; falling back to the raw channel monitor",
                            channel.name
                        ),
                    );
                    for effect in &mut route_channel.effects {
                        effect.bypassed = true;
                    }
                }
            }

            if channel_has_active_effects(&route_channel) {
                linked_effect_channel_ids.insert(route_channel.id.clone());
                outputs.extend(
                    self.pw
                        .execute_all(plan_route_channel_to_effect(
                            &route_channel,
                            &config.settings,
                        ))
                        .into_iter()
                        .map(command_execution),
                );
                outputs.extend(
                    self.pw
                        .execute_all(plan_route_effect_to_adaptive_bridge(
                            &route_channel,
                            &config.settings,
                        ))
                        .into_iter()
                        .map(command_execution),
                );
            } else {
                outputs.extend(
                    self.pw
                        .execute_all(plan_ensure_passthrough_mic_source(&route_channel))
                        .into_iter()
                        .map(command_execution),
                );
            }

            for mix in config.mixes.iter().filter(|mix| {
                channel
                    .mix_buses
                    .get(&mix.id)
                    .is_some_and(|bus| bus.enabled)
            }) {
                outputs.extend(
                    self.pw
                        .execute_all(plan_route_channel_to_mix(
                            &route_channel,
                            mix,
                            &config.settings,
                        ))
                        .into_iter()
                        .map(command_execution),
                );
                if let Some(bus) = channel.mix_buses.get(&mix.id) {
                    outputs.extend(self.execute_channel_bus_volume_unlocked(
                        &channel.id,
                        &mix.id,
                        bus.volume,
                    ));
                    outputs.extend(self.execute_channel_bus_mute_unlocked(
                        &channel.id,
                        &mix.id,
                        bus.muted,
                    ));
                }
            }
        }

        let route_issues = self.wait_for_effect_routes_linked(&config, &linked_effect_channel_ids);
        if !route_issues.is_empty() {
            self.log_engine_event(
                "effects.sync",
                format!(
                    "FX loopbacks did not link cleanly after targeted sync: {}; running full repair",
                    route_health_summary(&route_issues)
                ),
            );
            outputs.extend(self.repair_audio_graph_unlocked()?.outputs);
        }

        Ok(outputs)
    }

    fn sync_persistent_audio_core_channels_unlocked(
        &self,
        config: &MixerConfig,
        channels: &[&Channel],
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let mut outputs = Vec::new();
        let mut routes_need_sync = false;
        for channel in channels {
            let nodes_were_visible = self.effect_chain_nodes_visible(channel);
            routes_need_sync |= !nodes_were_visible;
            let start_output = self.start_effect_chain_process(channel);
            let start_failed = start_output.error.is_some();
            outputs.push(start_output);
            if start_failed {
                continue;
            }
            if !nodes_were_visible && !self.wait_for_effect_nodes_ready_for_routing(channel) {
                self.log_engine_event(
                    "effects.sync",
                    format!(
                        "{} persistent audio-core endpoints did not become ready; existing routes were preserved",
                        channel.name
                    ),
                );
                continue;
            }
        }

        // A native chain swap does not alter endpoints or routing. Keeping the
        // existing graph untouched avoids redundant Pulse commands and link
        // activation while the core crossfades to the prepared chain.
        if !routes_need_sync {
            return Ok(outputs);
        }

        let graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        let managed_modules = self.pw.managed_modules().unwrap_or_default();
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        let sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
        let mut route_commands = Vec::new();
        for channel in channels {
            route_commands.extend(plan_route_channel_to_effect(channel, &config.settings));
            for mix in config.mixes.iter().filter(|mix| {
                channel
                    .mix_buses
                    .get(&mix.id)
                    .is_some_and(|bus| bus.enabled)
            }) {
                route_commands.extend(plan_route_channel_to_mix(channel, mix, &config.settings));
            }
        }
        route_commands.retain(|command| {
            !repair_command_is_satisfied(
                command,
                &graph,
                &source_outputs,
                &sink_inputs,
                &managed_modules,
            )
        });
        outputs.extend(
            self.pw
                .execute_all(route_commands)
                .into_iter()
                .map(command_execution),
        );

        for channel in channels {
            for mix in config.mixes.iter().filter(|mix| {
                channel
                    .mix_buses
                    .get(&mix.id)
                    .is_some_and(|bus| bus.enabled)
            }) {
                if let Some(bus) = channel.mix_buses.get(&mix.id) {
                    outputs.extend(self.execute_channel_bus_volume_unlocked(
                        &channel.id,
                        &mix.id,
                        bus.volume,
                    ));
                    outputs.extend(self.execute_channel_bus_mute_unlocked(
                        &channel.id,
                        &mix.id,
                        bus.muted,
                    ));
                }
            }
        }

        let channel_ids = channels
            .iter()
            .map(|channel| channel.id.clone())
            .collect::<BTreeSet<_>>();
        let route_issues = self.wait_for_effect_routes_linked(config, &channel_ids);
        if !route_issues.is_empty() {
            self.log_engine_event(
                "effects.sync",
                format!(
                    "persistent audio-core routes are not fully linked yet: {}",
                    route_health_summary(&route_issues)
                ),
            );
        }
        Ok(outputs)
    }

    fn mark_effect_sync_active(&self) -> EffectSyncActiveGuard<'_> {
        self.effect_sync_active.store(true, Ordering::SeqCst);
        EffectSyncActiveGuard {
            active: &self.effect_sync_active,
        }
    }

    fn wait_for_effect_nodes(&self, channel: &Channel) -> bool {
        if self.options.dry_run {
            return true;
        }
        let started = Instant::now();
        let mut ready_samples = 0;
        while started.elapsed() < EFFECT_NODE_WAIT_TIMEOUT {
            if self.effect_chain_endpoint_readiness(channel).ready() {
                ready_samples += 1;
                if ready_samples >= EFFECT_NODE_READY_STABLE_SAMPLES {
                    return true;
                }
                thread::sleep(EFFECT_NODE_READY_SETTLE);
                continue;
            }
            ready_samples = 0;
            thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn wait_for_effect_nodes_ready_for_routing(&self, channel: &Channel) -> bool {
        if !self.wait_for_effect_nodes(channel) {
            return false;
        }
        if self.options.dry_run {
            return true;
        }
        thread::sleep(EFFECT_ROUTE_READY_SETTLE);
        self.effect_chain_endpoint_readiness(channel).ready()
    }

    fn wait_for_persistent_core_nodes_ready_for_routing(
        &self,
        channels: &[&Channel],
        mixes: &[Mix],
    ) -> bool {
        if self.options.dry_run || channels.is_empty() {
            return true;
        }

        let started = Instant::now();
        let mut ready_samples = 0;
        while started.elapsed() < EFFECT_NODE_WAIT_TIMEOUT {
            let inputs = self.pw.list_inputs().unwrap_or_default();
            let outputs = self.pw.list_outputs().unwrap_or_default();
            let all_ready = channels.iter().all(|channel| {
                effect_chain_endpoint_readiness_for_devices(&inputs, &outputs, channel).ready()
            }) && mixes.iter().all(|mix| {
                inputs
                    .iter()
                    .any(|input| input.name == mix.virtual_source_name)
            });
            if all_ready {
                ready_samples += 1;
                if ready_samples >= EFFECT_NODE_READY_STABLE_SAMPLES {
                    // The native core publishes every channel from one process.
                    // One shared settle period is enough before loading routes.
                    thread::sleep(EFFECT_NODE_READY_SETTLE);
                    let settled_inputs = self.pw.list_inputs().unwrap_or_default();
                    let settled_outputs = self.pw.list_outputs().unwrap_or_default();
                    return channels.iter().all(|channel| {
                        effect_chain_endpoint_readiness_for_devices(
                            &settled_inputs,
                            &settled_outputs,
                            channel,
                        )
                        .ready()
                    }) && mixes.iter().all(|mix| {
                        settled_inputs
                            .iter()
                            .any(|input| input.name == mix.virtual_source_name)
                    });
                }
            } else {
                ready_samples = 0;
            }
            thread::sleep(EFFECT_NODE_READY_SETTLE);
        }
        false
    }

    fn wait_for_effect_routes_linked(
        &self,
        config: &MixerConfig,
        channel_ids: &BTreeSet<String>,
    ) -> Vec<RouteHealthIssue> {
        if self.options.dry_run || channel_ids.is_empty() {
            return Vec::new();
        }

        let started = Instant::now();
        let mut issues = Vec::new();
        while started.elapsed() < EFFECT_ROUTE_LINK_WAIT_TIMEOUT {
            let state = self
                .pw
                .audio_state_snapshot_with_effect_availability_timed(None, Vec::new())
                .0;
            issues = effect_route_health_issues_for_channels(
                config,
                &state.graph,
                &state.routes.managed_modules,
                &state.routes.source_output_routes,
                &state.routes.sink_input_routes,
                channel_ids,
            );
            if issues.is_empty() {
                return issues;
            }
            thread::sleep(EFFECT_ROUTE_LINK_SETTLE);
        }
        issues
    }

    fn wait_for_effect_nodes_to_clear(&self, channel: &Channel) -> bool {
        if self.options.dry_run {
            return true;
        }
        let source_name = effect_chain_source_name(channel);
        let input_name = effect_chain_input_name(channel);
        let started = Instant::now();
        while started.elapsed() < EFFECT_NODE_CLEAR_TIMEOUT {
            let (source_visible, input_visible) =
                self.effect_chain_endpoint_visibility(&source_name, &input_name);
            if !source_visible && !input_visible {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn effect_chain_endpoint_readiness(&self, channel: &Channel) -> EffectEndpointReadiness {
        let inputs = self.pw.list_inputs().unwrap_or_default();
        let outputs = self.pw.list_outputs().unwrap_or_default();
        effect_chain_endpoint_readiness_for_devices(&inputs, &outputs, channel)
    }

    fn effect_chain_endpoint_visibility(
        &self,
        source_name: &str,
        input_name: &str,
    ) -> (bool, bool) {
        let inputs = self.pw.list_inputs().unwrap_or_default();
        let outputs = self.pw.list_outputs().unwrap_or_default();
        (
            inputs.iter().any(|source| source.name == source_name),
            outputs.iter().any(|sink| sink.name == input_name),
        )
    }

    fn schedule_effect_graph_sync(self: &Arc<Self>, channel_id: String) {
        let channel = match self.read_config().ok().and_then(|config| {
            config
                .channels
                .iter()
                .find(|item| item.id == channel_id)
                .cloned()
        }) {
            Some(channel) => channel,
            None => {
                self.log_engine_event(
                    "effects.sync",
                    format!("channel_id={channel_id} request_failed=channel_not_found"),
                );
                return;
            }
        };
        let slot = match self.effect_update_slot(&channel) {
            Ok(slot) => slot,
            Err(err) => {
                self.log_engine_event(
                    "effects.sync",
                    format!("channel_id={channel_id} request_failed={err}"),
                );
                return;
            }
        };
        let decision = {
            let Ok(mut state) = slot.state.lock() else {
                self.log_engine_event(
                    "effects.sync",
                    format!("channel_id={channel_id} request_failed=state_lock_poisoned"),
                );
                return;
            };
            match state.enqueue(channel.clone()) {
                Ok(decision) => decision,
                Err(error) => {
                    state.status.pending = false;
                    state.status.last_error = Some(error.clone());
                    state.status.resolve_state();
                    self.log_engine_event(
                        "effects.sync",
                        format!("channel_id={channel_id} request_failed={error}"),
                    );
                    self.change_signal.notify_state();
                    return;
                }
            }
        };
        self.change_signal.notify_state();
        self.log_engine_event(
            "effects.sync",
            format!(
                "channel_id={} desired_generation={} previous_acknowledged_generation={} selected_effect_count={} desired_enabled={} resolved_control_socket={} request_{} final_effect_status=red",
                channel.id,
                decision.generation,
                decision.previous_acknowledged,
                channel.effects.len(),
                channel_effects_desired_enabled(&channel),
                decision.control_socket,
                if decision.coalesced { "coalesced" } else { "queued" },
            ),
        );
        if !decision.start_worker {
            return;
        }
        self.spawn_effect_update_worker(slot, &channel.id, decision.generation);
    }

    fn recover_effect_updates_if_ready(self: &Arc<Self>) {
        if self.stop.load(Ordering::SeqCst) || !self.audio_graph_running_cached() {
            return;
        }
        let slots = self
            .effect_updates
            .lock()
            .map(|slots| {
                slots
                    .iter()
                    .map(|(channel_id, slot)| (channel_id.clone(), Arc::clone(slot)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        for (channel_id, slot) in slots {
            let recovery = slot.state.lock().ok().and_then(|mut state| {
                state.reserve_recovery_worker(Instant::now()).then(|| {
                    (
                        state.desired.generation,
                        state.status.applied_generation,
                        state.status.selected_effect_count,
                        state.status.desired_enabled,
                        state.status.control_socket.clone(),
                    )
                })
            });
            let Some((generation, previous_acknowledged, selected_count, enabled, socket)) =
                recovery
            else {
                continue;
            };

            self.log_engine_event(
                "effects.sync",
                format!(
                    "channel_id={channel_id} desired_generation={generation} previous_acknowledged_generation={previous_acknowledged} selected_effect_count={selected_count} desired_enabled={enabled} resolved_control_socket={socket} request_recovery=true final_effect_status=red",
                ),
            );
            self.change_signal.notify_state();
            self.spawn_effect_update_worker(slot, &channel_id, generation);
        }
    }

    fn spawn_effect_update_worker(
        self: &Arc<Self>,
        slot: Arc<EffectUpdateSlot>,
        channel_id: &str,
        generation: u64,
    ) {
        let engine = Arc::clone(self);
        let worker_slot = Arc::clone(&slot);
        let thread_name = format!("wavelinux-fx-{}", safe_file_id(channel_id));
        if let Err(err) = thread::Builder::new().name(thread_name).spawn(move || {
            engine.run_effect_update_worker(worker_slot);
        }) {
            if let Ok(mut state) = slot.state.lock() {
                state.record_worker_spawn_failure(format!("failed to start effect worker: {err}"));
            }
            self.change_signal.notify_state();
            self.log_engine_event(
                "effects.sync",
                format!(
                    "channel_id={channel_id} desired_generation={generation} request_failed=worker_spawn error={err}",
                ),
            );
        }
    }

    fn run_effect_update_worker(self: &Arc<Self>, slot: Arc<EffectUpdateSlot>) {
        thread::sleep(EFFECT_GRAPH_SYNC_DEBOUNCE);
        loop {
            if self.stop.load(Ordering::SeqCst) {
                if let Ok(mut state) = slot.state.lock() {
                    state.worker_running = false;
                    state.in_flight_generation = None;
                    state.status.in_flight_generation = None;
                    state.status.pending = false;
                    state.status.last_error = Some("WaveLinux is stopping".into());
                    state.status.resolve_state();
                }
                self.change_signal.notify_state();
                return;
            }

            let desired = {
                let Ok(mut state) = slot.state.lock() else {
                    return;
                };
                state.begin_latest()
            };
            let socket = self.paths.channel_control_socket(&desired.channel.id);
            self.log_engine_event(
                "effects.sync",
                format!(
                    "channel_id={} desired_generation={} selected_effect_count={} desired_enabled={} resolved_control_socket={} request_started",
                    desired.channel.id,
                    desired.generation,
                    desired.channel.effects.len(),
                    channel_effects_desired_enabled(&desired.channel),
                    socket.display(),
                ),
            );
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.apply_effect_update(&desired)
            }))
            .unwrap_or_else(|panic| {
                Err(format!("effect worker panicked: {}", panic_payload(panic)))
            });

            let completion = {
                let Ok(mut state) = slot.state.lock() else {
                    return;
                };
                state.finish_attempt(desired.generation, &result)
            };
            self.change_signal.notify_state();

            if completion.superseded {
                self.log_engine_event(
                    "effects.sync",
                    format!(
                        "channel_id={} desired_generation={} request_superseded=true",
                        desired.channel.id, desired.generation
                    ),
                );
                thread::sleep(EFFECT_GRAPH_SYNC_DEBOUNCE);
                continue;
            }

            match result {
                Ok(ack) => self.log_engine_event(
                    "effects.sync",
                    format!(
                        "channel_id={} desired_generation={} request_acknowledged={} config_revision={} chain_swaps={} final_effect_status={}",
                        desired.channel.id,
                        desired.generation,
                        ack.generation,
                        ack.config_revision,
                        ack.chain_swaps,
                        effect_runtime_state_name(completion.final_state),
                    ),
                ),
                Err(error) => self.log_engine_event(
                    "effects.sync",
                    format!(
                        "channel_id={} desired_generation={} request_failed={} final_effect_status={} error={}",
                        desired.channel.id,
                        desired.generation,
                        true,
                        effect_runtime_state_name(completion.final_state),
                        completion.final_error.unwrap_or(error),
                    ),
                ),
            }
            return;
        }
    }

    fn apply_effect_update(
        &self,
        desired: &PendingEffectUpdate,
    ) -> Result<EffectApplyAcknowledgement, String> {
        if !channel_uses_persistent_audio_core(&desired.channel) {
            self.rebuild_effect_chain_configs()
                .map_err(|err| err.to_string())?;
            if self.audio_graph_running_cached() {
                let channel_ids = BTreeSet::from([desired.channel.id.clone()]);
                match self
                    .try_sync_effect_channels(&channel_ids)
                    .map_err(|err| err.to_string())?
                {
                    Some(outputs) => self.log_command_executions("effects.sync", &outputs),
                    None => return Err("legacy effect graph mutation is busy".into()),
                }
            }
            return Ok(EffectApplyAcknowledgement {
                generation: desired.generation,
                config_revision: "legacy-filter-chain".into(),
                chain_swaps: 0,
            });
        }

        let (config_path, config_revision) =
            self.write_effect_runtime_config(&desired.channel, desired.generation)?;
        if !self.audio_graph_running_cached() {
            return Err("audio graph is stopped; selected effects remain saved".into());
        }
        let socket_path = self.paths.channel_control_socket(&desired.channel.id);
        let ready = wait_for_audio_core_ready(
            &socket_path,
            &desired.channel.id,
            EFFECT_CORE_READY_TIMEOUT,
        )?;
        if ready.acknowledged_generation == desired.generation {
            return Ok(EffectApplyAcknowledgement {
                generation: desired.generation,
                config_revision,
                chain_swaps: ready.chain_swaps,
            });
        }
        if ready.submitted_generation < desired.generation {
            let response = send_effect_chain_swap(
                &socket_path,
                &desired.channel.id,
                &config_path,
                &config_revision,
                desired.generation,
            )?;
            let queued_generation = response
                .get("graph_revision")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| "audio core did not return a queued generation".to_string())?;
            if queued_generation != desired.generation {
                return Err(format!(
                    "audio core queued generation {queued_generation}, expected {}",
                    desired.generation
                ));
            }
        }
        let acknowledged = wait_for_effect_generation_ack(
            &socket_path,
            &desired.channel.id,
            desired.generation,
            EFFECT_CORE_ACK_TIMEOUT,
        )?;
        Ok(EffectApplyAcknowledgement {
            generation: acknowledged.acknowledged_generation,
            config_revision,
            chain_swaps: acknowledged.chain_swaps,
        })
    }

    fn write_effect_runtime_config(
        &self,
        channel: &Channel,
        generation: u64,
    ) -> Result<(PathBuf, String), String> {
        let settings = self
            .read_config()
            .map_err(|err| err.to_string())?
            .settings
            .adaptive_latency
            .clone();
        let _writes = self
            .effect_config_writes
            .lock()
            .map_err(|_| "effect config write lock poisoned".to_string())?;
        let path = self
            .paths
            .effect_chains_dir()
            .join(effect_chain_file_name(&channel.id, "json"));
        let existing = read_json::<wavelinux_dsp::DspChannelConfig>(&path).ok();
        let mut dsp_config = dsp_channel_config(channel);
        if dsp_config.input_target_node_name.is_none() {
            dsp_config.input_target_node_name =
                existing.and_then(|config| config.input_target_node_name);
        }
        dsp_config.generation = generation;
        dsp_config.adaptive_latency = dsp_adaptive_latency_config(&settings);
        dsp_config.control_socket_path = Some(
            self.paths
                .channel_control_socket(&channel.id)
                .to_string_lossy()
                .into_owned(),
        );
        let revision = audio_core_channel_processing_revision(&dsp_config);
        write_json(&path, &dsp_config).map_err(|err| err.to_string())?;
        Ok((path, revision))
    }

    fn audio_graph_running_cached(&self) -> bool {
        self.read_runtime()
            .map(|runtime| runtime.status.audio_graph_running)
            .unwrap_or(false)
    }

    fn log_engine_event(&self, area: &str, message: impl AsRef<str>) {
        log_engine_event_to_paths(&self.paths, area, message);
    }

    fn log_command_executions(&self, area: &str, outputs: &[CommandExecution]) {
        if outputs.is_empty() {
            return;
        }
        let failed = outputs
            .iter()
            .filter(|output| output.error.is_some())
            .count();
        let skipped = outputs.iter().filter(|output| output.skipped).count();
        self.log_engine_event(
            area,
            format!(
                "commands={} failed={} skipped={}",
                outputs.len(),
                failed,
                skipped,
            ),
        );
        let notable_outputs = outputs
            .iter()
            .filter(|output| output.error.is_some())
            .chain(
                outputs
                    .iter()
                    .filter(|output| output.error.is_none() && !output.skipped),
            )
            .take(24);
        for output in notable_outputs {
            self.log_engine_event(
                area,
                format!(
                    "{} status={} command={}",
                    output.command.description,
                    output
                        .error
                        .as_deref()
                        .map(|error| format!("error:{error}"))
                        .unwrap_or_else(|| "ok".into()),
                    output.command.shell_line(),
                ),
            );
        }
    }

    fn recent_log_lines(&self, limit: usize) -> Vec<String> {
        let Ok(data) = fs::read_to_string(self.paths.log_file()) else {
            return Vec::new();
        };
        let mut lines = data
            .lines()
            .rev()
            .take(limit)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        lines.reverse();
        lines
    }
}

fn log_engine_event_to_paths(paths: &EnginePaths, area: &str, message: impl AsRef<str>) {
    let path = paths.log_file();
    let _ = fs::create_dir_all(&paths.config_dir);
    let _ = rotate_log_if_oversize(&path, DEBUG_LOG_MAX_BYTES);
    let _ = trim_rotated_logs(&path);

    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{timestamp} [{area}] {}", message.as_ref());
    }
}

fn maintain_logs_for_paths(paths: &EnginePaths, app_version: &str) -> Result<(), EngineError> {
    fs::create_dir_all(&paths.config_dir)?;
    let previous_version = fs::read_to_string(paths.log_version_file())
        .ok()
        .map(|version| version.trim().to_string());
    let version = app_version.trim();
    let version_changed = previous_version.as_deref() != Some(version);

    let mut log_paths = current_log_paths(paths)?;
    log_paths.sort();
    log_paths.dedup();

    for path in log_paths {
        if version_changed {
            rotate_log(&path)?;
        } else {
            rotate_log_if_oversize(&path, DEBUG_LOG_MAX_BYTES)?;
        }
        trim_rotated_logs(&path)?;
    }

    fs::write(paths.log_version_file(), format!("{version}\n"))?;
    Ok(())
}

fn current_log_paths(paths: &EnginePaths) -> Result<Vec<PathBuf>, EngineError> {
    let mut paths_to_check = vec![paths.log_file(), paths.legacy_app_log_file()];
    if !paths.config_dir.exists() {
        return Ok(paths_to_check);
    }

    for entry in fs::read_dir(&paths.config_dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with(&format!("{}-chain-", graph_prefix()))
            && name.ends_with(EFFECT_CHAIN_LOG_SUFFIX)
        {
            paths_to_check.push(path);
        }
    }

    Ok(paths_to_check)
}

fn rotate_log_if_oversize(path: &Path, max_bytes: u64) -> Result<bool, EngineError> {
    if fs::metadata(path)
        .map(|metadata| metadata.len() > max_bytes)
        .unwrap_or(false)
    {
        rotate_log(path)
    } else {
        Ok(false)
    }
}

fn rotate_log(path: &Path) -> Result<bool, EngineError> {
    if !fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
    {
        return Ok(false);
    }

    let oldest = rotated_log_path(path, DEBUG_LOG_ROTATED_FILES);
    if oldest.exists() {
        fs::remove_file(&oldest)?;
    }

    for index in (1..DEBUG_LOG_ROTATED_FILES).rev() {
        let source = rotated_log_path(path, index);
        if source.exists() {
            let target = rotated_log_path(path, index + 1);
            if target.exists() {
                fs::remove_file(&target)?;
            }
            fs::rename(source, target)?;
        }
    }

    fs::rename(path, rotated_log_path(path, 1))?;
    Ok(true)
}

fn trim_rotated_logs(path: &Path) -> Result<(), EngineError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(base_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    if !parent.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let entry_path = entry.path();
        let Some(name) = entry_path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(index) = rotated_log_index(name, base_name) else {
            continue;
        };
        if index > DEBUG_LOG_ROTATED_FILES {
            fs::remove_file(entry_path)?;
        }
    }
    Ok(())
}

fn rotated_log_path(path: &Path, index: usize) -> PathBuf {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return path.with_extension(format!("log.{index}"));
    };
    path.with_file_name(format!("{file_name}.{index}"))
}

fn rotated_log_index(file_name: &str, base_name: &str) -> Option<usize> {
    file_name
        .strip_prefix(base_name)?
        .strip_prefix('.')?
        .parse()
        .ok()
}

fn latest_installed_appimage_summary(data_dir: &Path) -> Option<String> {
    latest_installed_appimage(data_dir)
        .map(|(version, path)| format!("{version}:{}", path.display()))
}

fn latest_installed_appimage(data_dir: &Path) -> Option<(String, PathBuf)> {
    let entries = fs::read_dir(data_dir).ok()?;
    let mut candidates = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter_map(|path| appimage_version_from_path(&path).map(|version| (version, path)))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        appimage_version_key(&left.0)
            .cmp(&appimage_version_key(&right.0))
            .then_with(|| left.1.cmp(&right.1))
    });
    candidates.pop()
}

fn appimage_version_from_path(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("WaveLinux6_")?
        .strip_suffix("_amd64.AppImage")
        .map(ToOwned::to_owned)
}

fn appimage_version_key(version: &str) -> (u64, u64, u64, String) {
    let mut parts = version
        .split(|ch: char| !ch.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok());
    (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        version.to_string(),
    )
}

pub fn prewarm_hardware_profiles_from_xdg() -> Result<HardwareProfilePrewarmReport, EngineError> {
    let paths = EnginePaths::from_xdg()?;
    let config = load_config(&paths)?.normalized()?;
    let pw = PwClient::new(false);
    prewarm_hardware_profiles_for_paths(&paths, &pw, &config.device_policy)
}

fn prewarm_hardware_profiles_for_paths(
    paths: &EnginePaths,
    pw: &PwClient,
    policy: &wavelinux_model::DevicePolicy,
) -> Result<HardwareProfilePrewarmReport, EngineError> {
    fs::create_dir_all(paths.local_hardware_profiles_dir())?;
    let mut diagnostics = Vec::new();
    let mut devices = match pw.list_inputs() {
        Ok(devices) => devices,
        Err(err) => {
            diagnostics.push(Diagnostic {
                code: "hardware.profile.prewarm.inputs".into(),
                severity: DiagnosticSeverity::Warning,
                message: format!("Could not inspect audio inputs during profile prewarm: {err}"),
                action: Some("WaveLinux will try again when it starts".into()),
            });
            Vec::new()
        }
    };
    match pw.list_outputs() {
        Ok(outputs) => devices.extend(outputs),
        Err(err) => diagnostics.push(Diagnostic {
            code: "hardware.profile.prewarm.outputs".into(),
            severity: DiagnosticSeverity::Warning,
            message: format!("Could not inspect audio outputs during profile prewarm: {err}"),
            action: Some("WaveLinux will try again when it starts".into()),
        }),
    }

    let mut catalog = load_hardware_profile_catalog(paths);
    let report = sync_remote_profiles_for_devices(paths, &devices, policy, &catalog);
    if report.changed {
        catalog = load_hardware_profile_catalog(paths);
    }
    let matched = count_catalog_hardware_profile_matches(&devices, &catalog);
    diagnostics.extend(report.diagnostics.clone());
    log_engine_event_to_paths(
        paths,
        "hardware.profile.prewarm",
        format!(
            "devices={} matched={} remote_matched={} fetched={} diagnostics={}",
            devices.len(),
            matched,
            report.matched,
            report.fetched,
            diagnostics.len()
        ),
    );
    Ok(HardwareProfilePrewarmReport {
        devices: devices.len(),
        matched,
        fetched: report.fetched,
        diagnostics,
    })
}

fn count_catalog_hardware_profile_matches(
    devices: &[DeviceInfo],
    catalog: &HardwareProfileCatalog,
) -> usize {
    let mut matched_devices = devices.to_vec();
    apply_profiles_to_devices(&mut matched_devices, catalog);
    matched_devices
        .iter()
        .filter(|device| device.matched_profile_id.is_some())
        .count()
}

#[derive(Debug, Clone, Default)]
struct DefaultDevices {
    sink: Option<String>,
    source: Option<String>,
}

impl DefaultDevices {
    fn capture(pw: &PwClient) -> Self {
        Self {
            sink: pw
                .default_sink()
                .ok()
                .flatten()
                .filter(|device| is_restorable_device(device)),
            source: pw
                .default_source()
                .ok()
                .flatten()
                .filter(|device| is_restorable_device(device)),
        }
    }
}

fn effective_config_with_auto_devices(
    config: &MixerConfig,
    inputs: &[DeviceInfo],
    outputs: &[DeviceInfo],
    auto_input: Option<String>,
    auto_output: Option<String>,
    bluetooth_cards: &[BluetoothAudioCard],
) -> MixerConfig {
    let mut effective = config.clone();
    effective.device_policy.active_input_fallback = false;
    effective.device_policy.active_output_fallback = false;

    for channel in effective
        .channels
        .iter_mut()
        .filter(|channel| channel.kind.uses_hardware_slot())
    {
        let Some(source) = channel.source_device.as_deref() else {
            continue;
        };
        let bluetooth_blocked = bluetooth_input_would_force_hfp(source, bluetooth_cards);
        let unavailable = selected_input_is_unavailable(inputs, source, bluetooth_cards);
        if bluetooth_blocked || unavailable {
            effective.device_policy.restorable_input = Some(source.to_owned());
            channel.source_device = None;
            effective.device_policy.active_input_fallback = true;
        }
    }

    if let Some(auto_input) = auto_input {
        for channel in effective
            .channels
            .iter_mut()
            .filter(|channel| channel.kind.uses_hardware_slot() && channel.source_device.is_none())
        {
            channel.source_device = Some(auto_input.clone());
        }
        effective.device_policy.preferred_input = Some(auto_input);
    }

    if effective.settings.monitor_follows_default_output {
        if let Some(auto_output) = auto_output {
            if let Some(mix) = effective.mixes.iter_mut().find(|mix| mix.id == "monitor") {
                mix.set_outputs(vec![auto_output.clone()]);
                effective.device_policy.preferred_output = Some(auto_output);
            }
        }
    } else if let Some(auto_output) = auto_output {
        if let Some(mix) = effective.mixes.iter_mut().find(|mix| mix.id == "monitor") {
            let selected_outputs = mix.outputs();
            if selected_outputs
                .iter()
                .any(|output| selected_output_is_unavailable(outputs, output))
            {
                effective.device_policy.restorable_output = mix.monitor_output.clone();
                mix.set_outputs(vec![auto_output.clone()]);
                effective.device_policy.preferred_output = Some(auto_output);
                effective.device_policy.active_output_fallback = true;
            }
        }
    }

    effective
}

fn selected_input_is_unavailable(
    inputs: &[DeviceInfo],
    source: &str,
    bluetooth_cards: &[BluetoothAudioCard],
) -> bool {
    !inputs.is_empty()
        && !inputs
            .iter()
            .any(|input| input_device_can_route_source(input, source, bluetooth_cards))
}

fn input_device_can_route_source(
    input: &DeviceInfo,
    source: &str,
    bluetooth_cards: &[BluetoothAudioCard],
) -> bool {
    audio_endpoint_names_match(&input.id, source)
        && input_device_can_be_opened(input)
        && !input.is_virtual
        && is_restorable_device(&input.id)
        && !looks_like_monitor_source(input)
        && !bluetooth_input_would_force_hfp(&input.id, bluetooth_cards)
}

fn selected_output_is_unavailable(outputs: &[DeviceInfo], output: &str) -> bool {
    !outputs.is_empty()
        && !outputs
            .iter()
            .any(|device| output_device_can_route_sink(device, output))
}

fn output_device_can_route_sink(device: &DeviceInfo, output: &str) -> bool {
    audio_endpoint_names_match(&device.id, output)
        && device.is_available
        && !device.is_virtual
        && is_restorable_device(&device.id)
}

#[derive(Debug, Clone)]
struct AutoDeviceChoice {
    device_id: String,
    priority: u8,
    reason: AutoDeviceReason,
}

fn best_hardware_input_choice(
    inputs: &[DeviceInfo],
    bluetooth_cards: &[BluetoothAudioCard],
) -> Option<AutoDeviceChoice> {
    inputs
        .iter()
        .filter(|input| input_device_can_auto_select(input, bluetooth_cards))
        .max_by_key(|input| (hardware_input_priority(input), input.is_default))
        .map(|input| AutoDeviceChoice {
            device_id: input.id.clone(),
            priority: hardware_input_priority(input),
            reason: AutoDeviceReason::Priority,
        })
}

fn best_hardware_input(
    inputs: &[DeviceInfo],
    bluetooth_cards: &[BluetoothAudioCard],
) -> Option<String> {
    best_hardware_input_choice(inputs, bluetooth_cards).map(|choice| choice.device_id)
}

fn preferred_hardware_input_choice(
    inputs: &[DeviceInfo],
    default_source: Option<&str>,
    bluetooth_cards: &[BluetoothAudioCard],
) -> Option<AutoDeviceChoice> {
    let best_choice = best_hardware_input_choice(inputs, bluetooth_cards);
    let default_choice = default_source
        .and_then(|source| {
            inputs.iter().find(|input| {
                audio_endpoint_names_match(&input.id, source)
                    && input_device_can_auto_select(input, bluetooth_cards)
            })
        })
        .map(|default_input| AutoDeviceChoice {
            device_id: default_input.id.clone(),
            priority: hardware_input_priority(default_input),
            reason: AutoDeviceReason::SystemDefault,
        });

    match (best_choice, default_choice) {
        (Some(best), Some(default)) if default.priority >= best.priority => Some(default),
        (Some(best), _) => Some(best),
        (None, Some(default)) => Some(default),
        (None, None) => None,
    }
}

fn preferred_hardware_input(
    inputs: &[DeviceInfo],
    default_source: Option<&str>,
    bluetooth_cards: &[BluetoothAudioCard],
) -> Option<String> {
    preferred_hardware_input_choice(inputs, default_source, bluetooth_cards)
        .map(|choice| choice.device_id)
}

fn input_device_can_auto_select(
    input: &DeviceInfo,
    bluetooth_cards: &[BluetoothAudioCard],
) -> bool {
    !input.is_virtual
        && input_device_can_be_opened(input)
        && is_restorable_device(&input.id)
        && !looks_like_monitor_source(input)
        && !bluetooth_input_would_force_hfp(&input.id, bluetooth_cards)
        && input
            .active_routing_policy
            .as_ref()
            .is_none_or(|policy| policy.allow_auto_select_input)
}

fn input_device_can_be_opened(input: &DeviceInfo) -> bool {
    input.is_available || input_has_safe_availability_override(input)
}

fn best_monitor_output_choice(outputs: &[DeviceInfo]) -> Option<AutoDeviceChoice> {
    outputs
        .iter()
        .filter(|output| output_device_can_auto_select(output))
        .max_by_key(|output| (monitor_output_priority(output), output.is_default))
        .map(|output| AutoDeviceChoice {
            device_id: output.id.clone(),
            priority: monitor_output_priority(output),
            reason: AutoDeviceReason::Priority,
        })
}

fn best_monitor_output(outputs: &[DeviceInfo]) -> Option<String> {
    best_monitor_output_choice(outputs).map(|choice| choice.device_id)
}

fn output_device_can_auto_select(output: &DeviceInfo) -> bool {
    output.is_available
        && !output.is_virtual
        && is_restorable_device(&output.id)
        && output
            .active_routing_policy
            .as_ref()
            .is_none_or(|policy| policy.allow_auto_select_output)
}

fn preferred_monitor_output_choice(
    outputs: &[DeviceInfo],
    default_sink: Option<&str>,
    active_sink: Option<&str>,
) -> Option<AutoDeviceChoice> {
    let best_output = best_monitor_output(outputs);
    outputs
        .iter()
        .filter(|output| {
            best_output
                .as_deref()
                .is_some_and(|best| output_device_can_route_sink(output, best))
                || default_sink.is_some_and(|sink| output_device_can_route_sink(output, sink))
                || active_sink.is_some_and(|sink| output_device_can_route_sink(output, sink))
        })
        .max_by_key(|output| {
            (
                monitor_output_priority(output),
                output.is_default,
                active_sink.is_some_and(|sink| output_device_can_route_sink(output, sink)),
                default_sink.is_some_and(|sink| output_device_can_route_sink(output, sink)),
            )
        })
        .map(|output| {
            let reason = if active_sink
                .is_some_and(|sink| output_device_can_route_sink(output, sink))
            {
                AutoDeviceReason::ActiveOutput
            } else if default_sink.is_some_and(|sink| output_device_can_route_sink(output, sink)) {
                AutoDeviceReason::SystemDefault
            } else {
                AutoDeviceReason::Priority
            };
            AutoDeviceChoice {
                device_id: output.id.clone(),
                priority: monitor_output_priority(output),
                reason,
            }
        })
}

fn preferred_monitor_output(
    outputs: &[DeviceInfo],
    default_sink: Option<&str>,
    active_sink: Option<&str>,
) -> Option<String> {
    preferred_monitor_output_choice(outputs, default_sink, active_sink)
        .map(|choice| choice.device_id)
}

fn bluetooth_input_would_force_hfp(source: &str, cards: &[BluetoothAudioCard]) -> bool {
    let Some(source_key) = bluetooth_endpoint_device_key(source) else {
        return false;
    };
    cards.iter().any(|card| {
        card.a2dp_available()
            && normalize_bluetooth_device_key(&card.device_key) == source_key
            && source.trim().starts_with("bluez_input.")
    })
}

fn find_preferred_codecs_for_card(
    card: &BluetoothAudioCard,
    inputs: &[DeviceInfo],
    outputs: &[DeviceInfo],
    catalog: &HardwareProfileCatalog,
) -> Vec<String> {
    let card_key = normalize_bluetooth_device_key(&card.device_key);
    let mut seen = BTreeSet::new();
    let mut codecs = Vec::new();
    for device in inputs
        .iter()
        .chain(outputs.iter())
        .filter(|device| bluetooth_device_matches_card_key(device, &card_key))
    {
        let Some(profile) = device
            .matched_profile_id
            .as_deref()
            .and_then(|profile_id| hardware_profile_by_id(catalog, profile_id))
        else {
            continue;
        };
        for codec in &profile.codec_policy.preferred_a2dp_codecs {
            let codec = codec.trim().replace('-', "_").to_ascii_lowercase();
            if !codec.is_empty() && seen.insert(codec.clone()) {
                codecs.push(codec);
            }
        }
    }
    codecs
}

fn prune_initialized_bluetooth_cards(
    runtime: &mut RuntimeCache,
    bluetooth_cards: &[BluetoothAudioCard],
) {
    let live_card_names = bluetooth_cards
        .iter()
        .map(|card| card.name.clone())
        .collect::<BTreeSet<_>>();
    runtime
        .initialized_bluetooth_cards
        .retain(|card_name, _| live_card_names.contains(card_name));
}

fn bluetooth_device_matches_card_key(device: &DeviceInfo, card_key: &str) -> bool {
    if card_key.is_empty() {
        return false;
    }
    if bluetooth_endpoint_device_key(&device.id).as_deref() == Some(card_key) {
        return true;
    }
    if bluetooth_endpoint_device_key(&device.name).as_deref() == Some(card_key) {
        return true;
    }
    ["api.bluez5.address", "device.string", "bluez5.address"]
        .iter()
        .filter_map(|key| device.pipewire_properties.get(*key))
        .any(|value| normalize_bluetooth_device_key(value) == card_key)
}

fn hardware_input_priority(input: &DeviceInfo) -> u8 {
    let class_priority = hardware_input_class_priority(input);
    if let Some(priority) = input
        .active_routing_policy
        .as_ref()
        .and_then(|policy| policy.input_priority)
    {
        return priority.max(class_priority);
    }
    class_priority
}

fn hardware_input_class_priority(input: &DeviceInfo) -> u8 {
    let text = device_search_text_with_properties(input);

    if input.bus == Some(wavelinux_model::DeviceBus::Usb) || text.contains("usb") {
        return 80;
    }
    if input.bus == Some(wavelinux_model::DeviceBus::Bluetooth)
        || text.contains("bluez")
        || text.contains("bluetooth")
    {
        return 30;
    }
    if text.contains("jack")
        || text.contains("headset")
        || text.contains("headphone")
        || text.contains("linein")
        || text.contains("line-in")
        || text.contains("front mic")
        || text.contains("rear mic")
    {
        return 65;
    }
    if text.contains("built-in")
        || text.contains("built in")
        || text.contains("internal")
        || text.contains("digital microphone")
        || text.contains("dmic")
        || text.contains("hda")
        || text.contains("pci")
    {
        return 40;
    }
    if text.contains("mic") || text.contains("microphone") || text.contains("analog") {
        return 35;
    }
    1
}

fn monitor_output_priority(output: &DeviceInfo) -> u8 {
    let class_priority = monitor_output_class_priority(output);
    if let Some(priority) = output
        .active_routing_policy
        .as_ref()
        .and_then(|policy| policy.output_priority)
    {
        return priority.max(class_priority);
    }
    class_priority
}

fn monitor_output_class_priority(output: &DeviceInfo) -> u8 {
    let text = device_search_text_with_properties(output);

    if output.bus == Some(wavelinux_model::DeviceBus::Usb) || text.contains("usb") {
        return 80;
    }
    if output.bus == Some(wavelinux_model::DeviceBus::Bluetooth)
        || text.contains("bluez")
        || text.contains("bluetooth")
    {
        return 70;
    }
    if text.contains("headphone")
        || text.contains("headset")
        || text.contains("lineout")
        || text.contains("line-out")
        || text.contains("aux")
        || text.contains("jack")
    {
        return 60;
    }
    if text.contains("speaker") {
        return 35;
    }
    if text.contains("analog") {
        return 45;
    }
    if text.contains("hdmi") || text.contains("displayport") {
        return 10;
    }
    1
}

fn device_search_text(device: &DeviceInfo) -> String {
    format!("{} {} {}", device.id, device.name, device.description).to_ascii_lowercase()
}

fn device_search_text_with_properties(device: &DeviceInfo) -> String {
    let properties = [
        "device.api",
        "device.icon_name",
        "device.profile.description",
        "device.profile.name",
        "media.class",
        "node.nick",
    ]
    .iter()
    .filter_map(|key| device.pipewire_properties.get(*key))
    .cloned()
    .collect::<Vec<_>>()
    .join(" ");
    format!("{} {}", device_search_text(device), properties).to_ascii_lowercase()
}

fn input_has_safe_availability_override(input: &DeviceInfo) -> bool {
    if input.is_available {
        return false;
    }
    if device_has_explicit_unavailable_active_port(input) {
        return false;
    }
    if matches!(
        input.bus,
        Some(wavelinux_model::DeviceBus::Pci) | Some(wavelinux_model::DeviceBus::Platform)
    ) {
        return false;
    }

    let text = device_search_text_with_properties(input);
    let usbish = input.bus == Some(wavelinux_model::DeviceBus::Usb) || text.contains("usb");
    let profiled = input.matched_profile_id.is_some()
        && matches!(
            input.profile_confidence,
            Some(wavelinux_model::ProfileConfidence::Medium)
                | Some(wavelinux_model::ProfileConfidence::High)
        );
    let has_stable_identity = input.vendor_id.is_some()
        || input.product_id.is_some()
        || input.alsa_card.is_some()
        || input.alsa_device.is_some();

    (usbish || profiled) && has_stable_identity && !text.contains("hda") && !text.contains("pci")
}

fn device_has_explicit_unavailable_active_port(device: &DeviceInfo) -> bool {
    let active = device
        .active_port
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let matches = device
        .ports
        .iter()
        .filter(|port| {
            active.is_none_or(|active| port.name == active || port.description == active)
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return false;
    }
    matches
        .iter()
        .any(|port| port_availability_is_unavailable(&port.availability))
}

fn port_availability_is_unavailable(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "not available" | "unavailable" | "no"
    )
}

fn looks_like_monitor_source(input: &DeviceInfo) -> bool {
    let text = device_search_text(input);
    text.contains(".monitor") || text.contains("monitor of")
}

fn effective_config_with_profiled_devices(
    config: &MixerConfig,
    inputs: &[DeviceInfo],
    outputs: &[DeviceInfo],
    bluetooth_cards: &[BluetoothAudioCard],
    default_source: Option<&str>,
    default_sink: Option<&str>,
    active_sink: Option<&str>,
) -> MixerConfig {
    let auto_input = preferred_hardware_input(inputs, default_source, bluetooth_cards);
    let auto_output = preferred_monitor_output(outputs, default_sink, active_sink);
    let mut effective = effective_config_with_auto_devices(
        config,
        inputs,
        outputs,
        auto_input,
        auto_output,
        bluetooth_cards,
    );
    effective = config_with_unavailable_hardware_direct_monitoring_disabled(
        effective,
        inputs,
        bluetooth_cards,
    );
    effective.settings.runtime_latency_policy = Some(active_latency_policy_for_config(
        &effective, inputs, outputs,
    ));
    effective
}

fn resolved_auto_devices_for_config(
    config: &MixerConfig,
    inputs: &[DeviceInfo],
    outputs: &[DeviceInfo],
    bluetooth_cards: &[BluetoothAudioCard],
    default_source: Option<&str>,
    default_sink: Option<&str>,
    active_sink: Option<&str>,
) -> Vec<ResolvedAutoDevice> {
    let input_choice = preferred_hardware_input_choice(inputs, default_source, bluetooth_cards);
    let output_choice = preferred_monitor_output_choice(outputs, default_sink, active_sink);
    let mut devices = Vec::new();

    for channel in config
        .channels
        .iter()
        .filter(|channel| channel.kind.uses_hardware_slot() && channel.source_device.is_none())
    {
        devices.push(resolved_auto_device(
            AutoDeviceKind::Input,
            Some(channel.id.clone()),
            None,
            input_choice.as_ref(),
            inputs,
        ));
    }

    if config.settings.monitor_follows_default_output {
        if let Some(mix) = config.mixes.iter().find(|mix| mix.id == "monitor") {
            devices.push(resolved_auto_device(
                AutoDeviceKind::Output,
                None,
                Some(mix.id.clone()),
                output_choice.as_ref(),
                outputs,
            ));
        }
    }

    devices
}

fn effective_config_with_runtime_auto_devices(
    config: &MixerConfig,
    graph: &RuntimeGraph,
) -> MixerConfig {
    let mut effective = config.clone();
    for auto_device in &graph.auto_devices {
        let Some(device_id) = auto_device.device_id.as_deref() else {
            continue;
        };
        match auto_device.kind {
            AutoDeviceKind::Input => {
                let Some(channel_id) = auto_device.channel_id.as_deref() else {
                    continue;
                };
                if let Some(channel) = effective.channels.iter_mut().find(|channel| {
                    channel.id == channel_id
                        && channel.kind.uses_hardware_slot()
                        && channel.source_device.is_none()
                }) {
                    channel.source_device = Some(device_id.to_owned());
                }
            }
            AutoDeviceKind::Output => {
                if !effective.settings.monitor_follows_default_output {
                    continue;
                }
                let Some(mix_id) = auto_device.mix_id.as_deref() else {
                    continue;
                };
                if let Some(mix) = effective.mixes.iter_mut().find(|mix| mix.id == mix_id) {
                    mix.set_outputs(vec![device_id.to_owned()]);
                }
            }
        }
    }
    effective
}

fn resolved_auto_device(
    kind: AutoDeviceKind,
    channel_id: Option<String>,
    mix_id: Option<String>,
    choice: Option<&AutoDeviceChoice>,
    devices: &[DeviceInfo],
) -> ResolvedAutoDevice {
    let device = choice.and_then(|choice| {
        devices
            .iter()
            .find(|device| audio_endpoint_names_match(&device.id, &choice.device_id))
    });
    ResolvedAutoDevice {
        kind,
        channel_id,
        mix_id,
        device_id: choice.map(|choice| choice.device_id.clone()),
        device_name: device.map(|device| device.name.clone()),
        device_description: device.map(|device| device.description.clone()),
        priority: choice.map(|choice| choice.priority),
        reason: choice
            .map(|choice| choice.reason)
            .unwrap_or(AutoDeviceReason::Unavailable),
    }
}

fn config_with_unavailable_hardware_direct_monitoring_disabled(
    mut config: MixerConfig,
    inputs: &[DeviceInfo],
    bluetooth_cards: &[BluetoothAudioCard],
) -> MixerConfig {
    if config.settings.hardware_direct_mic_monitoring
        && !hardware_direct_monitoring_wave_xlr_available(&config, inputs, bluetooth_cards)
    {
        config.settings.hardware_direct_mic_monitoring = false;
    }
    config
}

fn hardware_direct_monitoring_wave_xlr_available(
    config: &MixerConfig,
    inputs: &[DeviceInfo],
    bluetooth_cards: &[BluetoothAudioCard],
) -> bool {
    config
        .channels
        .iter()
        .filter(|channel| channel.kind.uses_hardware_slot())
        .filter_map(|channel| channel.source_device.as_deref())
        .any(|source| {
            inputs.iter().any(|input| {
                device_is_wave_xlr(input)
                    && input_device_can_route_source(input, source, bluetooth_cards)
            })
        })
}

fn device_is_wave_xlr(device: &DeviceInfo) -> bool {
    if device
        .matched_profile_id
        .as_deref()
        .is_some_and(|profile| profile.eq_ignore_ascii_case("elgato.wave-xlr"))
    {
        return true;
    }

    let vendor_id = normalize_usb_id(device.vendor_id.as_deref());
    let product_id = normalize_usb_id(device.product_id.as_deref());
    if vendor_id.as_deref() == Some("0fd9") && product_id.as_deref() == Some("007d") {
        return true;
    }

    let compact = device_search_text(device)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    compact.contains("elgato") && compact.contains("wavexlr")
}

fn normalize_usb_id(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    let hex = value
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if hex.is_empty() {
        return None;
    }
    if hex.len() > 4 {
        Some(hex[hex.len() - 4..].to_string())
    } else {
        Some(format!("{hex:0>4}"))
    }
}

fn config_with_unavailable_effects_bypassed(
    config: &MixerConfig,
    graph: &RuntimeGraph,
) -> MixerConfig {
    let mut effective = config.clone();

    for channel in effective
        .channels
        .iter_mut()
        .filter(|channel| channel_has_active_effects(channel))
    {
        if effect_chain_endpoint_readiness_for_graph(graph, channel).ready() {
            continue;
        }
        for effect in &mut channel.effects {
            effect.bypassed = true;
        }
    }

    effective
}

fn active_latency_policy_for_config(
    config: &MixerConfig,
    inputs: &[DeviceInfo],
    outputs: &[DeviceInfo],
) -> LatencyPolicy {
    let fallback = &config
        .device_policy
        .fallback_hardware_profile
        .latency_policy;
    let mut output_policy = LatencyPolicy::default();
    let mut saw_output_policy = false;

    for mix in &config.mixes {
        for output in mix.outputs() {
            if let Some(policy) = outputs
                .iter()
                .find(|device| device.id == output)
                .and_then(|device| device.active_latency_policy.as_ref())
            {
                merge_latency_policy_floor(&mut output_policy, policy);
                saw_output_policy = true;
            }
        }
    }

    if saw_output_policy {
        fill_latency_policy_defaults(&mut output_policy, fallback);
        return output_policy;
    }

    let mut input_policy = LatencyPolicy::default();
    let mut saw_input_policy = false;

    for channel in config
        .channels
        .iter()
        .filter(|channel| channel.kind.uses_hardware_slot())
    {
        if let Some(policy) = channel
            .source_device
            .as_deref()
            .and_then(|source| inputs.iter().find(|input| input.id == source))
            .and_then(|device| device.active_latency_policy.as_ref())
        {
            merge_latency_policy_floor(&mut input_policy, policy);
            saw_input_policy = true;
        }
    }

    if !saw_input_policy {
        return fallback.clone();
    }

    fill_latency_policy_defaults(&mut input_policy, fallback);
    input_policy
}

fn merge_latency_policy_floor(target: &mut LatencyPolicy, policy: &LatencyPolicy) {
    target.stable_msec = max_optional_u16(target.stable_msec, policy.stable_msec);
    target.low_latency_msec = max_optional_u16(target.low_latency_msec, policy.low_latency_msec);
    target.bluetooth_floor_msec =
        max_optional_u16(target.bluetooth_floor_msec, policy.bluetooth_floor_msec);
}

fn max_optional_u16(left: Option<u16>, right: Option<u16>) -> Option<u16> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}

fn fill_latency_policy_defaults(policy: &mut LatencyPolicy, fallback: &LatencyPolicy) {
    if policy.stable_msec.is_none() {
        policy.stable_msec = fallback.stable_msec;
    }
    if policy.low_latency_msec.is_none() {
        policy.low_latency_msec = fallback.low_latency_msec;
    }
    if policy.bluetooth_floor_msec.is_none() {
        policy.bluetooth_floor_msec = fallback.bluetooth_floor_msec;
    }
}

struct ProfiledDeviceRepairView<'a> {
    inputs: &'a [DeviceInfo],
    outputs: &'a [DeviceInfo],
    bluetooth_cards: &'a [BluetoothAudioCard],
    default_source: Option<&'a str>,
    default_sink: Option<&'a str>,
    active_sink: Option<&'a str>,
    managed_modules: &'a [ManagedModule],
    source_outputs: &'a [SourceOutputRoute],
    sink_inputs: &'a [SinkInputRoute],
}

fn auto_device_route_repair_needed_for_profiled_devices(
    config: &MixerConfig,
    view: ProfiledDeviceRepairView<'_>,
) -> bool {
    let effective_config = effective_config_with_profiled_devices(
        config,
        view.inputs,
        view.outputs,
        view.bluetooth_cards,
        view.default_source,
        view.default_sink,
        view.active_sink,
    );
    if view
        .managed_modules
        .iter()
        .any(|module| auto_device_module_is_stale_for_config(module, &effective_config))
    {
        return true;
    }
    let auto_input =
        preferred_hardware_input(view.inputs, view.default_source, view.bluetooth_cards);
    let auto_output = preferred_monitor_output(view.outputs, view.default_sink, view.active_sink);
    auto_device_route_repair_needed(
        &effective_config,
        auto_input.as_deref(),
        auto_output.as_deref(),
        view.managed_modules,
        view.source_outputs,
        view.sink_inputs,
    )
}

fn bluetooth_monitor_route_signatures(
    config: &MixerConfig,
    outputs: &[DeviceInfo],
) -> BTreeMap<String, BluetoothMonitorRouteSignature> {
    config
        .mixes
        .iter()
        .flat_map(|mix| {
            mix.outputs()
                .into_iter()
                .filter(|output| output.starts_with("bluez_output."))
                .filter_map(move |output| {
                    let device = outputs
                        .iter()
                        .find(|device| output_device_can_route_sink(device, &output))?;
                    Some((
                        format!("{}:{}", mix.id, output),
                        BluetoothMonitorRouteSignature {
                            output: device.id.clone(),
                            serial: device
                                .pipewire_properties
                                .get("object.serial")
                                .cloned()
                                .or_else(|| device.pipewire_properties.get("object.id").cloned()),
                            profile: device.active_profile.clone(),
                            codec: device.active_codec.clone(),
                        },
                    ))
                })
        })
        .collect()
}

fn bluetooth_monitor_route_refresh_needed(
    runtime: &RuntimeCache,
    config: &MixerConfig,
    outputs: &[DeviceInfo],
    managed_modules: &[ManagedModule],
) -> bool {
    let signatures = bluetooth_monitor_route_signatures(config, outputs);
    signatures.iter().any(|(route_key, signature)| {
        let mix_id = route_key
            .split_once(':')
            .map(|(mix_id, _)| mix_id)
            .unwrap_or(route_key.as_str());
        let route_count = managed_modules
            .iter()
            .filter(|module| {
                module.role.as_deref() == Some("mix_monitor")
                    && module.mix_id.as_deref() == Some(mix_id)
                    && module
                        .sink_name
                        .as_deref()
                        .is_some_and(|sink| audio_endpoint_names_match(sink, &signature.output))
            })
            .count();
        if route_count != 1 {
            return true;
        }

        runtime
            .bluetooth_monitor_routes
            .get(route_key)
            .is_none_or(|previous| previous != signature)
    })
}

fn auto_device_module_is_stale_for_config(module: &ManagedModule, config: &MixerConfig) -> bool {
    module_is_auto_device_route(module) && module_is_stale_for_config(module, config)
}

fn module_is_auto_device_route(module: &ManagedModule) -> bool {
    matches!(
        module.role.as_deref(),
        Some("input_to_channel") | Some("mix_monitor")
    )
}

fn managed_module_is_loopback_route(module: &ManagedModule) -> bool {
    matches!(
        module.role.as_deref(),
        Some("input_to_channel")
            | Some("mix_monitor")
            | Some("channel_to_effect")
            | Some("channel_to_mix")
    )
}

fn managed_module_is_incremental_mix_route(module: &ManagedModule) -> bool {
    matches!(
        module.role.as_deref(),
        Some("channel_to_mix") | Some("mix_monitor")
    )
}

fn managed_loopback_has_live_source_output(
    module: &ManagedModule,
    role: Option<&str>,
    channel_id: Option<&str>,
    mix_id: Option<&str>,
    source_name: Option<&str>,
    source_outputs: &[SourceOutputRoute],
) -> bool {
    find_managed_loopback_source_output(
        module,
        role,
        channel_id,
        mix_id,
        source_name,
        source_outputs,
    )
    .is_some()
}

fn find_managed_loopback_source_output<'a>(
    module: &ManagedModule,
    role: Option<&str>,
    channel_id: Option<&str>,
    mix_id: Option<&str>,
    source_name: Option<&str>,
    source_outputs: &'a [SourceOutputRoute],
) -> Option<&'a SourceOutputRoute> {
    source_outputs.iter().find(|route| {
        source_output_matches_loopback(route, module, role, channel_id, mix_id, source_name)
    })
}

fn source_output_matches_loopback(
    route: &SourceOutputRoute,
    module: &ManagedModule,
    role: Option<&str>,
    channel_id: Option<&str>,
    mix_id: Option<&str>,
    source_name: Option<&str>,
) -> bool {
    let module_matches = route
        .module_id
        .as_deref()
        .is_some_and(|module_id| module_id == module.module_id.as_str());
    let route_matches = route.role.as_deref() == role
        && route.channel_id.as_deref() == channel_id
        && route.mix_id.as_deref() == mix_id;
    let source_matches = source_name.is_none_or(|source| {
        route
            .source_name
            .as_deref()
            .is_some_and(|actual| audio_endpoint_names_match(actual, source))
    });

    (module_matches || route_matches) && source_matches
}

fn managed_loopback_has_live_sink_input(
    module: &ManagedModule,
    role: Option<&str>,
    channel_id: Option<&str>,
    mix_id: Option<&str>,
    sink_name: Option<&str>,
    sink_inputs: &[SinkInputRoute],
) -> bool {
    find_managed_loopback_sink_input(module, role, channel_id, mix_id, sink_name, sink_inputs)
        .is_some()
}

fn find_managed_loopback_sink_input<'a>(
    module: &ManagedModule,
    role: Option<&str>,
    channel_id: Option<&str>,
    mix_id: Option<&str>,
    sink_name: Option<&str>,
    sink_inputs: &'a [SinkInputRoute],
) -> Option<&'a SinkInputRoute> {
    sink_inputs.iter().find(|route| {
        sink_input_matches_loopback(route, module, role, channel_id, mix_id, sink_name)
    })
}

fn sink_input_matches_loopback(
    route: &SinkInputRoute,
    module: &ManagedModule,
    role: Option<&str>,
    channel_id: Option<&str>,
    mix_id: Option<&str>,
    sink_name: Option<&str>,
) -> bool {
    let module_matches = route
        .module_id
        .as_deref()
        .is_some_and(|module_id| module_id == module.module_id.as_str());
    let route_matches = route.role.as_deref() == role
        && route.channel_id.as_deref() == channel_id
        && route.mix_id.as_deref() == mix_id;
    let sink_matches = sink_name.is_none_or(|sink| {
        route
            .sink_name
            .as_deref()
            .or(route.target_object.as_deref())
            .or(route.sink.as_deref())
            .is_some_and(|actual| audio_endpoint_names_match(actual, sink))
    });

    (module_matches || route_matches) && sink_matches
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManagedRouteLevel {
    muted: bool,
    sink_input_percent: u8,
    source_output_percent: u8,
}

fn expected_managed_route_level(
    config: &MixerConfig,
    module: &ManagedModule,
) -> Option<ManagedRouteLevel> {
    expected_managed_route_level_for_parts(
        config,
        module.role.as_deref(),
        module.channel_id.as_deref(),
        module.mix_id.as_deref(),
    )
}

fn expected_managed_route_level_for_parts(
    config: &MixerConfig,
    role: Option<&str>,
    channel_id: Option<&str>,
    mix_id: Option<&str>,
) -> Option<ManagedRouteLevel> {
    match role? {
        "channel_to_mix" => {
            let channel_id = channel_id?;
            let mix_id = mix_id?;
            let bus = config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)?
                .mix_buses
                .get(mix_id)?;
            bus.enabled.then(|| ManagedRouteLevel {
                muted: bus.muted,
                sink_input_percent: volume_to_percent(bus.volume),
                source_output_percent: 100,
            })
        }
        "input_to_channel" | "channel_to_effect" | "mix_monitor" => Some(ManagedRouteLevel {
            muted: false,
            sink_input_percent: 100,
            source_output_percent: 100,
        }),
        _ => None,
    }
}

fn managed_route_level_mismatch(
    config: &MixerConfig,
    module: &ManagedModule,
    source_output: &SourceOutputRoute,
    sink_input: &SinkInputRoute,
) -> bool {
    let Some(expected) = expected_managed_route_level(config, module) else {
        return false;
    };

    route_mute_mismatch(source_output.muted, expected.muted)
        || route_mute_mismatch(sink_input.muted, expected.muted)
        || route_volume_mismatch(source_output.volume_percent, expected.source_output_percent)
        || route_volume_mismatch(sink_input.volume_percent, expected.sink_input_percent)
}

fn route_mute_mismatch(actual: Option<bool>, expected: bool) -> bool {
    actual.is_some_and(|actual| actual != expected)
}

fn route_volume_mismatch(actual: Option<u8>, expected: u8) -> bool {
    actual.is_some_and(|actual| actual.abs_diff(expected) > 1)
}

fn volume_to_percent(volume: f32) -> u8 {
    (volume.clamp(0.0, 1.0) * 100.0).round() as u8
}

fn auto_device_route_repair_needed(
    config: &MixerConfig,
    auto_input: Option<&str>,
    auto_output: Option<&str>,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
) -> bool {
    auto_input_repair_needed(config, auto_input, managed_modules, source_outputs)
        || auto_output_repair_needed(
            config,
            auto_output,
            managed_modules,
            source_outputs,
            sink_inputs,
        )
}

fn native_input_target_route_ready(
    channel: &Channel,
    expected_source: &str,
    source_outputs: &[SourceOutputRoute],
) -> bool {
    source_outputs.iter().any(|route| {
        route.role.as_deref() == Some("input_target")
            && route.channel_id.as_deref() == Some(channel.id.as_str())
            && [route.source_name.as_deref(), route.target_object.as_deref()]
                .into_iter()
                .flatten()
                .any(|source| audio_endpoint_names_match(source, expected_source))
    })
}

fn native_mix_output_target_route_ready(
    mix: &Mix,
    expected_sink: &str,
    sink_inputs: &[SinkInputRoute],
) -> bool {
    sink_inputs.iter().any(|route| {
        route.role.as_deref() == Some("mix_output_target")
            && route.mix_id.as_deref() == Some(mix.id.as_str())
            && [route.sink_name.as_deref(), route.target_object.as_deref()]
                .into_iter()
                .flatten()
                .any(|sink| audio_endpoint_names_match(sink, expected_sink))
    })
}

fn default_device_lock_repair_needed(
    config: &MixerConfig,
    default_source: Option<&str>,
    default_sink: Option<&str>,
) -> bool {
    default_input_lock_repair_needed(config, default_source)
        || default_output_lock_repair_needed(config, default_sink)
}

fn auto_input_repair_needed(
    config: &MixerConfig,
    auto_input: Option<&str>,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
) -> bool {
    let Some(auto_input) = auto_input.filter(|device| is_restorable_device(device)) else {
        return false;
    };

    config
        .channels
        .iter()
        .filter(|channel| channel.kind.uses_hardware_slot())
        .any(|channel| {
            let expected_source = channel.source_device.as_deref().unwrap_or(auto_input);
            if expected_source != auto_input {
                return false;
            }
            if channel_uses_persistent_audio_core(channel) {
                return !native_input_target_route_ready(channel, expected_source, source_outputs);
            }
            !managed_modules.iter().any(|module| {
                module.role.as_deref() == Some("input_to_channel")
                    && module.channel_id.as_deref() == Some(channel.id.as_str())
                    && module.source_name.as_deref() == Some(expected_source)
                    && module.sink_name.as_deref() == Some(channel.virtual_sink_name.as_str())
                    && module.route_revision.as_deref()
                        == Some(input_route_revision(&config.settings, channel).as_str())
                    && managed_loopback_has_live_source_output(
                        module,
                        Some("input_to_channel"),
                        Some(channel.id.as_str()),
                        None,
                        Some(expected_source),
                        source_outputs,
                    )
            })
        })
}

fn auto_output_repair_needed(
    config: &MixerConfig,
    auto_output: Option<&str>,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
) -> bool {
    if !config.settings.monitor_follows_default_output
        && !config.device_policy.active_output_fallback
    {
        return false;
    }
    let Some(auto_output) = auto_output.filter(|device| is_restorable_device(device)) else {
        return false;
    };
    let Some(monitor_mix) = config.mixes.iter().find(|mix| mix.id == "monitor") else {
        return false;
    };
    if mix_uses_persistent_audio_core(monitor_mix) {
        return !native_mix_output_target_route_ready(monitor_mix, auto_output, sink_inputs);
    }
    let expected_source = mix_render_source_name(monitor_mix);
    let monitor_routes = managed_modules
        .iter()
        .filter(|module| {
            module.role.as_deref() == Some("mix_monitor")
                && module.mix_id.as_deref() == Some(monitor_mix.id.as_str())
        })
        .collect::<Vec<_>>();

    // Demand-driven route repair creates a missing output when the mix gains
    // an active producer. Auto-device repair only needs to replace an existing
    // route whose selected physical output became stale.
    if monitor_routes.is_empty() {
        return false;
    }

    !monitor_routes.into_iter().any(|module| {
        module.mix_id.as_deref() == Some(monitor_mix.id.as_str())
            && module.source_name.as_deref() == Some(expected_source.as_str())
            && module
                .sink_name
                .as_deref()
                .is_some_and(|sink| audio_endpoint_names_match(sink, auto_output))
            && module.route_revision.as_deref()
                == Some(
                    mix_monitor_route_revision_for_sink(&config.settings, monitor_mix, auto_output)
                        .as_str(),
                )
            && managed_loopback_has_live_source_output(
                module,
                Some("mix_monitor"),
                None,
                Some(monitor_mix.id.as_str()),
                Some(expected_source.as_str()),
                source_outputs,
            )
    })
}

fn default_input_lock_repair_needed(config: &MixerConfig, default_source: Option<&str>) -> bool {
    if !config.settings.lock_default_input {
        return false;
    }
    let Some(expected) = default_input_source(config) else {
        return false;
    };
    default_source.is_none_or(|source| !audio_endpoint_names_match(source, &expected))
}

fn default_output_lock_repair_needed(config: &MixerConfig, default_sink: Option<&str>) -> bool {
    if !config.settings.lock_default_output {
        return false;
    }
    let Some(expected) =
        default_output_channel(config).map(|channel| channel.virtual_sink_name.as_str())
    else {
        return false;
    };
    default_sink.is_none_or(|sink| !audio_endpoint_names_match(sink, expected))
}

fn capture_stream_move_commands_to_locked_default_input(
    config: &MixerConfig,
    source_outputs: &[SourceOutputRoute],
) -> Vec<CommandSpec> {
    if !config.settings.lock_default_input {
        return Vec::new();
    }
    let Some(expected_source) = default_input_source(config) else {
        return Vec::new();
    };

    source_outputs
        .iter()
        .filter(|route| {
            capture_stream_should_move_to_locked_default_input(config, route, &expected_source)
        })
        .map(|route| plan_move_capture_stream_to_source(&route.id, &expected_source))
        .collect()
}

fn capture_stream_move_commands_for_bluetooth_protection(
    source_outputs: &[SourceOutputRoute],
    fallback_source: Option<&str>,
    bluetooth_cards: &[BluetoothAudioCard],
) -> Vec<CommandSpec> {
    let Some(fallback_source) = fallback_source.filter(|source| {
        !source.trim().is_empty() && !bluetooth_input_would_force_hfp(source, bluetooth_cards)
    }) else {
        return Vec::new();
    };

    source_outputs
        .iter()
        .filter(|route| {
            !route.id.trim().is_empty()
                && !route.dont_move
                && !source_output_is_wavelinux_owned(route)
                && route
                    .source_name
                    .as_deref()
                    .is_some_and(|source| bluetooth_input_would_force_hfp(source, bluetooth_cards))
        })
        .map(|route| plan_move_capture_stream_to_source(&route.id, fallback_source))
        .collect()
}

fn capture_stream_should_move_to_locked_default_input(
    config: &MixerConfig,
    route: &SourceOutputRoute,
    expected_source: &str,
) -> bool {
    if route.id.trim().is_empty() || route.dont_move || source_output_is_wavelinux_owned(route) {
        return false;
    }
    let Some(source_name) = route.source_name.as_deref() else {
        return false;
    };
    if capture_stream_uses_user_selected_wavelinux_source(config, route) {
        return false;
    }
    !audio_endpoint_names_match(source_name, expected_source)
}

fn capture_stream_uses_user_selected_wavelinux_source(
    config: &MixerConfig,
    route: &SourceOutputRoute,
) -> bool {
    let selected = [route.source_name.as_deref(), route.target_object.as_deref()];
    selected.into_iter().flatten().any(|source| {
        user_selectable_wavelinux_capture_sources(config)
            .any(|candidate| audio_endpoint_names_match(source, candidate.as_str()))
    })
}

fn user_selectable_wavelinux_capture_sources(
    config: &MixerConfig,
) -> impl Iterator<Item = String> + '_ {
    let mix_sources = config.mixes.iter().flat_map(|mix| {
        [
            mix.virtual_source_name.clone(),
            format!("{}.monitor", mix.virtual_sink_name),
        ]
    });
    let channel_sources = config.channels.iter().flat_map(|channel| {
        [
            format!("{}.monitor", channel.virtual_sink_name),
            channel_mix_source_name(channel),
        ]
    });
    mix_sources.chain(channel_sources)
}

fn mix_render_source_name(mix: &Mix) -> String {
    if mix_uses_persistent_audio_core(mix) {
        mix.virtual_source_name.clone()
    } else {
        format!("{}.monitor", mix.virtual_sink_name)
    }
}

fn capture_move_signature_for_command(
    command: &CommandSpec,
    source_outputs: &[SourceOutputRoute],
) -> String {
    let source_output_id = command.args.get(1).map(String::as_str).unwrap_or("");
    let target_source = command.args.get(2).map(String::as_str).unwrap_or("");
    let current_source = source_outputs
        .iter()
        .find(|route| route.id == source_output_id)
        .and_then(|route| route.source_name.as_deref())
        .unwrap_or("");
    format!("{current_source}->{target_source}")
}

fn capture_move_failure_backoff(attempts: u32) -> Duration {
    let multiplier = 1_u32 << attempts.saturating_sub(1).min(6);
    std::cmp::min(
        CAPTURE_MOVE_FAILURE_BACKOFF
            .checked_mul(multiplier)
            .unwrap_or(CAPTURE_MOVE_FAILURE_MAX_BACKOFF),
        CAPTURE_MOVE_FAILURE_MAX_BACKOFF,
    )
}

fn source_output_is_wavelinux_owned(route: &SourceOutputRoute) -> bool {
    route.managed.as_deref() == Some("1")
        || route.role.is_some()
        || route.channel_id.is_some()
        || route.mix_id.is_some()
        || route_value_contains_wavelinux(route.application_name.as_deref())
        || route_value_contains_wavelinux(route.node_name.as_deref())
        || route_value_contains_wavelinux(route.media_name.as_deref())
        || route_value_is_loopback_node(route.node_name.as_deref())
        || route_value_is_loopback_node(route.media_name.as_deref())
}

fn route_value_contains_wavelinux(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.to_ascii_lowercase().contains("wavelinux"))
}

fn graph_prop(name: &str) -> String {
    format!("{}.{}", graph_property_prefix(), name)
}

fn graph_property_value_from_arg<'a>(properties: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{}.{}=", graph_property_prefix(), name);
    property_value_from_arg(properties, &key)
}

fn command_execution(result: Result<CommandOutput, PwError>) -> CommandExecution {
    match result {
        Ok(output) => CommandExecution {
            command: output.command,
            stdout: output.stdout,
            stderr: output.stderr,
            skipped: output.skipped,
            error: None,
        },
        Err(err) => CommandExecution {
            command: CommandSpec {
                domain: wavelinux_pw::CommandDomain::Diagnostics,
                program: String::new(),
                args: Vec::new(),
                description: "command failed".into(),
            },
            stdout: String::new(),
            stderr: String::new(),
            skipped: false,
            error: Some(err.to_string()),
        },
    }
}

fn command_executions_may_have_mutated_graph(outputs: &[CommandExecution]) -> bool {
    // A failed command can still have applied part of its operation before the
    // client observed the error. Only dry-run/skipped commands are guaranteed
    // to leave the captured host snapshot current.
    outputs.iter().any(|output| !output.skipped)
}

fn command_execution_with_spec(
    command: CommandSpec,
    result: Result<CommandOutput, PwError>,
) -> CommandExecution {
    match result {
        Ok(output) => CommandExecution {
            command: output.command,
            stdout: output.stdout,
            stderr: output.stderr,
            skipped: output.skipped,
            error: None,
        },
        Err(err) => CommandExecution {
            command,
            stdout: String::new(),
            stderr: String::new(),
            skipped: false,
            error: Some(err.to_string()),
        },
    }
}

fn command_execution_with_stale_stream_skip(
    command: CommandSpec,
    result: Result<CommandOutput, PwError>,
) -> CommandExecution {
    let stream_id = command_stream_id(&command).map(str::to_string);
    let output = command_execution_with_spec(command, result);
    if let Some(stream_id) = stream_id {
        ignore_stale_stream_command(output, &stream_id)
    } else {
        output
    }
}

fn stale_process_is_active_effect_child(
    process: &StaleProcess,
    active_effect_pids: &BTreeSet<String>,
    active_effect_config_markers: &BTreeSet<String>,
) -> bool {
    active_effect_pids.contains(&process.pid)
        || active_effect_config_markers
            .iter()
            .any(|marker| process.command.contains(marker))
}

fn ignore_stale_stream_command(mut output: CommandExecution, stream_id: &str) -> CommandExecution {
    if output.error.as_deref().is_some_and(is_stale_stream_error) {
        output.stderr = format!("stream {stream_id} disappeared before the command could apply");
        output.skipped = true;
        output.error = None;
    }
    output
}

fn is_stale_stream_error(error: &str) -> bool {
    error.contains("No such entity") || error.contains("No such process")
}

fn command_stream_id(command: &CommandSpec) -> Option<&str> {
    match command.args.first().map(String::as_str) {
        Some(
            "set-sink-input-volume"
            | "set-sink-input-mute"
            | "set-source-output-volume"
            | "set-source-output-mute"
            | "move-sink-input"
            | "move-source-output",
        ) => command.args.get(1).map(String::as_str),
        _ => None,
    }
}

fn skipped_command(command: CommandSpec) -> CommandExecution {
    CommandExecution {
        command,
        stdout: String::new(),
        stderr: String::new(),
        skipped: true,
        error: None,
    }
}

fn skipped_command_with_stderr(
    command: CommandSpec,
    stderr: impl Into<String>,
) -> CommandExecution {
    CommandExecution {
        command,
        stdout: String::new(),
        stderr: stderr.into(),
        skipped: true,
        error: None,
    }
}

fn cache_expired(checked_at: Option<Instant>, ttl: Duration) -> bool {
    checked_at.is_none_or(|checked_at| checked_at.elapsed() >= ttl)
}

fn split_repair_commands(commands: &[CommandSpec]) -> (Vec<CommandSpec>, Vec<CommandSpec>) {
    let mut graph_commands = Vec::new();
    let mut route_commands = Vec::new();
    for command in commands {
        if command.domain == CommandDomain::Graph {
            graph_commands.push(command.clone());
        } else {
            route_commands.push(command.clone());
        }
    }
    (graph_commands, route_commands)
}

fn default_output_channel(config: &MixerConfig) -> Option<&Channel> {
    config
        .channels
        .iter()
        .find(|channel| channel.kind == ChannelKind::System || channel.id == "system")
        .or_else(|| {
            config
                .channels
                .iter()
                .find(|channel| !channel.kind.uses_hardware_slot())
        })
}

fn route_value_is_loopback_node(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.starts_with("input.loopback-") || value.starts_with("loopback-")
    })
}

fn default_input_source(config: &MixerConfig) -> Option<String> {
    config
        .channels
        .iter()
        .find(|channel| channel.kind.uses_hardware_slot())
        .map(channel_mix_source_name)
}

fn graph_has_wavelinux_nodes(graph: &RuntimeGraph) -> bool {
    graph
        .inputs
        .iter()
        .chain(graph.outputs.iter())
        .any(|device| {
            device.is_virtual
                && (device.id.to_ascii_lowercase().contains("wavelinux")
                    || device.name.to_ascii_lowercase().contains("wavelinux")
                    || device
                        .description
                        .to_ascii_lowercase()
                        .contains("wavelinux"))
        })
}

fn effect_node_has_current_config_revision(device: &DeviceInfo) -> bool {
    device
        .pipewire_properties
        .get(&graph_prop("effect_config_revision"))
        .is_some_and(|revision| {
            revision == EFFECT_CONFIG_REVISION
                || revision == wavelinux_dsp::DSP_CHANNEL_CONFIG_REVISION
        })
}

fn effect_node_matches_current_channel(
    device: &DeviceInfo,
    channel: &Channel,
    expected_name: &str,
    expected_role: &str,
) -> bool {
    device.name == expected_name
        && device
            .pipewire_properties
            .get(&graph_prop("managed"))
            .is_some_and(|managed| managed == "1")
        && device
            .pipewire_properties
            .get(&graph_prop("role"))
            .is_some_and(|role| role == expected_role)
        && device
            .pipewire_properties
            .get(&graph_prop("channel_id"))
            .is_some_and(|channel_id| channel_id == &channel.id)
        && effect_node_has_current_config_revision(device)
}

fn effect_chain_endpoint_readiness_for_graph(
    graph: &RuntimeGraph,
    channel: &Channel,
) -> EffectEndpointReadiness {
    effect_chain_endpoint_readiness_for_devices(&graph.inputs, &graph.outputs, channel)
}

fn effect_chain_endpoint_readiness_for_devices(
    inputs: &[DeviceInfo],
    outputs: &[DeviceInfo],
    channel: &Channel,
) -> EffectEndpointReadiness {
    let source_name = effect_chain_source_name(channel);
    let input_name = effect_chain_input_name(channel);
    let processed_name = effect_chain_filter_output_name(channel);
    let uses_adaptive_bridge = channel_uses_adaptive_latency_bridge(channel);
    EffectEndpointReadiness {
        source_ready: inputs.iter().any(|source| {
            effect_node_matches_current_channel(source, channel, &source_name, "effect_output")
        }),
        input_ready: outputs.iter().any(|sink| {
            effect_node_matches_current_channel(sink, channel, &input_name, "effect_input")
        }),
        processed_ready: !uses_adaptive_bridge
            || inputs.iter().any(|source| {
                effect_node_matches_current_channel(
                    source,
                    channel,
                    &processed_name,
                    "effect_processed",
                )
            }),
    }
}

fn app_routing_graph_ready(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
) -> bool {
    app_routing_graph_ready_with_target_check(
        config,
        graph,
        managed_modules,
        source_outputs,
        sink_inputs,
        true,
    )
}

fn app_routing_graph_ready_without_persistent_targets(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
) -> bool {
    app_routing_graph_ready_with_target_check(
        config,
        graph,
        managed_modules,
        source_outputs,
        sink_inputs,
        false,
    )
}

fn app_routing_graph_ready_with_target_check(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
    check_persistent_targets: bool,
) -> bool {
    let active_app_channel_ids = active_app_channel_ids_for_graph(config, graph);
    let active_mix_ids = active_mix_ids_for_routes(config, graph, source_outputs, sink_inputs);
    let output_names = graph
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();
    let input_names = graph
        .inputs
        .iter()
        .map(|input| input.name.as_str())
        .collect::<BTreeSet<_>>();

    for mix in &config.mixes {
        let sink_missing = !mix_uses_persistent_audio_core(mix)
            && !output_names.contains(mix.virtual_sink_name.as_str());
        if sink_missing || !input_names.contains(mix.virtual_source_name.as_str()) {
            return false;
        }
    }

    for mix in config
        .mixes
        .iter()
        .filter(|mix| active_mix_ids.contains(&mix.id))
    {
        let monitor_source = mix_render_source_name(mix);
        for output in mix.outputs() {
            let route_ready = if mix_uses_persistent_audio_core(mix) {
                if !check_persistent_targets {
                    continue;
                }
                native_mix_output_target_route_ready(mix, &output, sink_inputs)
            } else {
                managed_modules.iter().any(|module| {
                    module.role.as_deref() == Some("mix_monitor")
                        && module.mix_id.as_deref() == Some(mix.id.as_str())
                        && module.source_name.as_deref() == Some(monitor_source.as_str())
                        && module
                            .sink_name
                            .as_deref()
                            .is_some_and(|sink| audio_endpoint_names_match(sink, &output))
                        && module.route_revision.as_deref()
                            == Some(
                                mix_monitor_route_revision_for_sink(&config.settings, mix, &output)
                                    .as_str(),
                            )
                })
            };
            if !route_ready {
                return false;
            }
        }
    }

    if check_persistent_targets {
        for channel in config
            .channels
            .iter()
            .filter(|channel| channel_uses_persistent_audio_core(channel))
        {
            if let Some(source) = channel.source_device.as_deref() {
                if !native_input_target_route_ready(channel, source, source_outputs) {
                    return false;
                }
            }
        }
    }

    config.channels.iter().all(|channel| {
        channel_route_ready(
            channel,
            config,
            &output_names,
            managed_modules,
            effect_chain_endpoint_readiness_for_graph(graph, channel),
            &active_app_channel_ids,
            &active_mix_ids,
        )
    })
}

fn persistent_core_target_routes_need_sync(
    config: &MixerConfig,
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
) -> bool {
    let input_changed = config
        .channels
        .iter()
        .filter(|channel| channel_uses_persistent_audio_core(channel))
        .any(|channel| {
            let actual = source_outputs
                .iter()
                .filter(|route| {
                    route.role.as_deref() == Some("input_target")
                        && route.channel_id.as_deref() == Some(channel.id.as_str())
                })
                .collect::<Vec<_>>();
            match channel.source_device.as_deref() {
                Some(source) => {
                    !native_input_target_route_ready(channel, source, source_outputs)
                        || actual.iter().any(|route| {
                            ![route.source_name.as_deref(), route.target_object.as_deref()]
                                .into_iter()
                                .flatten()
                                .any(|actual| audio_endpoint_names_match(actual, source))
                        })
                }
                None => !actual.is_empty(),
            }
        });
    let output_changed = config
        .mixes
        .iter()
        .filter(|mix| mix_uses_persistent_audio_core(mix))
        .any(|mix| {
            let expected = mix.outputs();
            let actual = sink_inputs
                .iter()
                .filter(|route| {
                    route.role.as_deref() == Some("mix_output_target")
                        && route.mix_id.as_deref() == Some(mix.id.as_str())
                })
                .collect::<Vec<_>>();
            expected
                .iter()
                .any(|output| !native_mix_output_target_route_ready(mix, output, sink_inputs))
                || actual.iter().any(|route| {
                    ![route.sink_name.as_deref(), route.target_object.as_deref()]
                        .into_iter()
                        .flatten()
                        .any(|actual| {
                            expected
                                .iter()
                                .any(|output| audio_endpoint_names_match(actual, output))
                        })
                })
        });
    input_changed || output_changed
}

struct IncrementalMixRouteView<'a> {
    graph: &'a RuntimeGraph,
    managed_modules: &'a [ManagedModule],
    source_outputs: &'a [SourceOutputRoute],
    sink_inputs: &'a [SinkInputRoute],
}

fn route_changes_are_incremental_mix_only(
    config: &MixerConfig,
    view: IncrementalMixRouteView<'_>,
    active_app_channel_ids: &BTreeSet<String>,
    active_mix_ids: &BTreeSet<String>,
    route_health: &[RouteHealthIssue],
) -> bool {
    if route_health
        .iter()
        .any(|issue| !matches!(issue.role.as_str(), "channel_to_mix" | "mix_monitor"))
    {
        return false;
    }

    let needed_commands =
        plan_ensure_graph_for_active_routes(config, active_app_channel_ids, active_mix_ids)
            .commands
            .into_iter()
            .filter(|command| {
                !repair_command_is_satisfied(
                    command,
                    view.graph,
                    view.source_outputs,
                    view.sink_inputs,
                    view.managed_modules,
                )
            })
            .collect::<Vec<_>>();
    needed_commands.iter().all(command_is_incremental_mix_route)
        && (!needed_commands.is_empty() || !route_health.is_empty())
}

fn active_effect_routes_need_repair(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
) -> bool {
    let active_app_channel_ids = active_app_channel_ids_for_graph(config, graph);
    let active_mix_ids = active_mix_ids_for_routes(config, graph, source_outputs, sink_inputs);
    let output_names = graph
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();

    config
        .channels
        .iter()
        .filter(|channel| channel_has_active_effects(channel))
        .any(|channel| {
            !channel_route_ready(
                channel,
                config,
                &output_names,
                managed_modules,
                effect_chain_endpoint_readiness_for_graph(graph, channel),
                &active_app_channel_ids,
                &active_mix_ids,
            )
        })
}

fn stream_route_ready(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
    stream: &AppStream,
) -> bool {
    let Some(channel_id) = stream.routed_channel_id.as_deref() else {
        return true;
    };
    let Some(channel) = config
        .channels
        .iter()
        .find(|channel| channel.id == channel_id)
    else {
        return false;
    };
    let output_names = graph
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();
    let active_mix_ids = active_mix_ids_for_routes(config, graph, source_outputs, sink_inputs);
    channel_route_ready(
        channel,
        config,
        &output_names,
        managed_modules,
        effect_chain_endpoint_readiness_for_graph(graph, channel),
        &BTreeSet::from([channel.id.clone()]),
        &active_mix_ids,
    )
}

fn channel_route_ready(
    channel: &Channel,
    config: &MixerConfig,
    output_names: &BTreeSet<&str>,
    managed_modules: &[ManagedModule],
    effect_readiness: EffectEndpointReadiness,
    active_app_channel_ids: &BTreeSet<String>,
    active_mix_ids: &BTreeSet<String>,
) -> bool {
    if !output_names.contains(channel.virtual_sink_name.as_str()) {
        return false;
    }
    let raw_source_name = format!("{}.monitor", channel.virtual_sink_name);
    let mut source_name = channel_mix_source_name(channel);
    if channel_has_active_effects(channel) {
        let effect_source_name = effect_chain_source_name(channel);
        let effect_input_name = effect_chain_input_name(channel);
        let effect_processed_name = effect_chain_filter_output_name(channel);
        let adaptive_bridge_input_name = effect_chain_adaptive_bridge_input_name(channel);
        if effect_readiness.ready() {
            if !channel_uses_persistent_audio_core(channel) {
                let effect_route_ready = managed_modules.iter().any(|module| {
                    module.role.as_deref() == Some("channel_to_effect")
                        && module.channel_id.as_deref() == Some(channel.id.as_str())
                        && module.source_name.as_deref() == Some(raw_source_name.as_str())
                        && module.sink_name.as_deref() == Some(effect_input_name.as_str())
                        && module.route_revision.as_deref()
                            == Some(effect_route_revision(&config.settings, channel).as_str())
                });
                if !effect_route_ready {
                    return false;
                }
            }
            if channel_uses_adaptive_latency_bridge(channel) {
                let bridge_route_ready = managed_modules.iter().any(|module| {
                    module.role.as_deref() == Some("effect_to_adaptive_bridge")
                        && module.channel_id.as_deref() == Some(channel.id.as_str())
                        && module.source_name.as_deref() == Some(effect_processed_name.as_str())
                        && module.sink_name.as_deref() == Some(adaptive_bridge_input_name.as_str())
                        && module.route_revision.as_deref()
                            == Some(EFFECT_ADAPTIVE_BRIDGE_ROUTE_REVISION)
                });
                if !bridge_route_ready {
                    return false;
                }
            }
            source_name = effect_source_name;
        } else {
            return false;
        }
    }
    if channel_uses_persistent_audio_core(channel) {
        // WaveLinux 6 buses are summed inside the persistent core. Their
        // readiness is represented by the stable mix source endpoint rather
        // than one Pulse module per channel/mix pair.
        return config
            .mixes
            .iter()
            .filter(|mix| {
                channel_mix_route_expected_for_active_routes(
                    channel,
                    mix,
                    &config.settings,
                    active_app_channel_ids,
                    active_mix_ids,
                )
            })
            .all(mix_uses_persistent_audio_core);
    }
    config
        .mixes
        .iter()
        .filter(|mix| {
            channel_mix_route_expected_for_active_routes(
                channel,
                mix,
                &config.settings,
                active_app_channel_ids,
                active_mix_ids,
            )
        })
        .all(|mix| {
            managed_modules.iter().any(|module| {
                module.role.as_deref() == Some("channel_to_mix")
                    && module.channel_id.as_deref() == Some(channel.id.as_str())
                    && module.mix_id.as_deref() == Some(mix.id.as_str())
                    && module.source_name.as_deref() == Some(source_name.as_str())
                    && module.sink_name.as_deref() == Some(mix.virtual_sink_name.as_str())
                    && module.route_revision.as_deref()
                        == Some(channel_mix_route_revision(&config.settings, channel, mix).as_str())
            })
        })
}

fn all_app_channel_ids(config: &MixerConfig) -> BTreeSet<String> {
    config
        .channels
        .iter()
        .filter(|channel| !channel.kind.uses_hardware_slot())
        .map(|channel| channel.id.clone())
        .collect()
}

fn active_app_channel_ids_for_graph(
    config: &MixerConfig,
    graph: &RuntimeGraph,
) -> BTreeSet<String> {
    graph
        .app_streams
        .iter()
        .filter_map(|stream| active_app_channel_id_for_stream(config, stream))
        .collect()
}

fn active_app_channel_id_for_stream(config: &MixerConfig, stream: &AppStream) -> Option<String> {
    if let Some(channel_id) = stream.routed_channel_id.as_deref() {
        if config
            .channels
            .iter()
            .any(|channel| channel.id == channel_id && !channel.kind.uses_hardware_slot())
        {
            return Some(channel_id.to_string());
        }
    }

    route_stream_to_configured_channel(config, stream)
        .filter(|channel| !channel.kind.uses_hardware_slot())
        .map(|channel| channel.id)
}

fn all_mix_ids(config: &MixerConfig) -> BTreeSet<String> {
    config.mixes.iter().map(|mix| mix.id.clone()).collect()
}

fn active_mix_ids_for_routes(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    source_outputs: &[SourceOutputRoute],
    _sink_inputs: &[SinkInputRoute],
) -> BTreeSet<String> {
    // WaveLinux 6 mixes are persistent bridge endpoints. Keeping their output
    // routes alive avoids graph mutation when a recorder starts or an app
    // resumes after being idle.
    if config
        .channels
        .iter()
        .any(channel_uses_persistent_audio_core)
    {
        return all_mix_ids(config);
    }
    let active_app_channel_ids = active_app_channel_ids_for_graph(config, graph);
    config
        .mixes
        .iter()
        .filter(|mix| {
            mix_has_external_capture_consumer(mix, graph, source_outputs)
                || (mix_has_configured_audible_output(mix)
                    && mix_has_active_producer(config, mix, &active_app_channel_ids))
        })
        .map(|mix| mix.id.clone())
        .collect()
}

fn mix_has_configured_audible_output(mix: &Mix) -> bool {
    !mix.muted && !mix.outputs().is_empty()
}

fn mix_has_active_producer(
    config: &MixerConfig,
    mix: &Mix,
    active_app_channel_ids: &BTreeSet<String>,
) -> bool {
    config.channels.iter().any(|channel| {
        channel
            .mix_buses
            .get(&mix.id)
            .is_some_and(|bus| bus.enabled && !bus.muted)
            && if channel.kind.uses_hardware_slot() {
                channel.source_device.is_some()
            } else {
                active_app_channel_ids.contains(&channel.id)
            }
    })
}

fn active_mix_routes_have_custom_levels(
    config: &MixerConfig,
    active_app_channel_ids: &BTreeSet<String>,
    active_mix_ids: &BTreeSet<String>,
) -> bool {
    config.channels.iter().any(|channel| {
        config.mixes.iter().any(|mix| {
            channel_mix_route_expected_for_active_routes(
                channel,
                mix,
                &config.settings,
                active_app_channel_ids,
                active_mix_ids,
            ) && channel
                .mix_buses
                .get(&mix.id)
                .is_some_and(|bus| bus.muted || (bus.volume - 1.0).abs() > 0.001)
        })
    })
}

fn mix_has_external_capture_consumer(
    mix: &Mix,
    graph: &RuntimeGraph,
    source_outputs: &[SourceOutputRoute],
) -> bool {
    source_outputs.iter().any(|output| {
        source_output_uses_source(output, &mix.virtual_source_name, graph)
            && !source_output_is_wavelinux_internal(output, &mix.virtual_source_name)
    })
}

fn source_output_uses_source(
    output: &SourceOutputRoute,
    source_name: &str,
    graph: &RuntimeGraph,
) -> bool {
    if output
        .source_name
        .as_deref()
        .is_some_and(|actual| audio_endpoint_names_match(actual, source_name))
    {
        return true;
    }
    if output
        .target_object
        .as_deref()
        .is_some_and(|actual| audio_endpoint_names_match(actual, source_name))
    {
        return true;
    }
    let Some(source_id) = output.source_id.as_deref() else {
        return false;
    };
    audio_endpoint_names_match(source_id, source_name)
        || graph.inputs.iter().any(|input| {
            input.index.as_deref() == Some(source_id)
                && (audio_endpoint_names_match(&input.id, source_name)
                    || audio_endpoint_names_match(&input.name, source_name))
        })
}

fn source_output_is_wavelinux_internal(output: &SourceOutputRoute, source_name: &str) -> bool {
    if output.managed.as_deref() == Some("1")
        || matches!(
            output.application_name.as_deref(),
            Some("WaveLinux6" | "WaveLinux 6" | "WaveLinux5")
        )
    {
        return true;
    }
    let node_name = output.node_name.as_deref().unwrap_or_default();
    if node_name == format!("input.{source_name}") {
        return true;
    }
    let node_name_lower = node_name.to_ascii_lowercase();
    if node_name_lower.starts_with("input.loopback-")
        || node_name_lower.starts_with("output.loopback-")
        || node_name_lower.starts_with("input.wavelinux")
        || node_name_lower.starts_with("output.wavelinux")
    {
        return true;
    }
    output
        .media_name
        .as_deref()
        .is_some_and(|media_name| media_name.to_ascii_lowercase().starts_with("loopback-"))
}

fn is_restorable_device(device: &str) -> bool {
    !device.to_ascii_lowercase().contains("wavelinux")
}

fn effect_chain_log_mentions_recent(
    path: &Path,
    markers: &[&str],
    channel_id: Option<&str>,
) -> Option<String> {
    let log = fs::read_to_string(path).ok()?;
    let now_nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let window_nanos = FX_LOG_WARNING_WINDOW.as_nanos() as i128;
    let mut found_untimestamped_marker = false;

    for line in log.lines().rev() {
        if !effect_chain_log_line_matches_channel(line, channel_id) {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if !markers.iter().any(|marker| lower.contains(marker)) {
            continue;
        }
        let Some(timestamp) = effect_chain_log_line_timestamp(line) else {
            found_untimestamped_marker = true;
            continue;
        };
        let age_nanos = now_nanos - timestamp.unix_timestamp_nanos();
        if age_nanos <= 0 || age_nanos <= window_nanos {
            return Some(effect_chain_log_line_summary(line));
        }
    }

    if !found_untimestamped_marker {
        return None;
    }

    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return Some("recent untimestamped FX warning".into());
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) if age <= FX_LOG_WARNING_WINDOW => Some("recent untimestamped FX warning".into()),
        Err(_) => Some("recent untimestamped FX warning".into()),
        _ => None,
    }
}

fn effect_chain_log_recent_native_underrun(
    path: &Path,
    channel_id: Option<&str>,
) -> Option<String> {
    let log = fs::read_to_string(path).ok()?;
    let now_nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let window_nanos = FX_LOG_WARNING_WINDOW.as_nanos() as i128;
    let untimestamped_recent = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age <= FX_LOG_WARNING_WINDOW);
    let mut newest: Option<(u64, String)> = None;

    for line in log.lines().rev() {
        if !effect_chain_log_line_matches_channel(line, channel_id) {
            continue;
        }
        let Some(underrun_frames) = effect_chain_native_underrun_frames(line) else {
            continue;
        };
        let line_is_recent =
            effect_chain_log_line_timestamp(line).map_or(untimestamped_recent, |timestamp| {
                let age_nanos = now_nanos - timestamp.unix_timestamp_nanos();
                age_nanos <= 0 || age_nanos <= window_nanos
            });
        if !line_is_recent {
            continue;
        }

        if let Some((newest_underrun_frames, newest_summary)) = newest {
            return (newest_underrun_frames > underrun_frames).then_some(newest_summary);
        }
        newest = Some((underrun_frames, effect_chain_log_line_summary(line)));
    }

    newest.and_then(|(underrun_frames, summary)| (underrun_frames > 0).then_some(summary))
}

fn effect_chain_log_mentions(path: &Path, markers: &[&str], channel_id: Option<&str>) -> bool {
    let Ok(log) = fs::read_to_string(path) else {
        return false;
    };
    log.lines().any(|line| {
        if !effect_chain_log_line_matches_channel(line, channel_id) {
            return false;
        }
        let lower = line.to_ascii_lowercase();
        markers.iter().any(|marker| lower.contains(marker))
    })
}

fn effect_chain_log_failure_summary(path: &Path, channel_id: Option<&str>) -> Option<String> {
    let log = fs::read_to_string(path).ok()?;
    log.lines().rev().find_map(|line| {
        if !effect_chain_log_line_matches_channel(line, channel_id) {
            return None;
        }
        let lower = line.to_ascii_lowercase();
        if !lower.contains("underrun detected")
            && !lower.contains("processing too slow")
            && effect_chain_native_underrun_frames(line).is_none_or(|frames| frames == 0)
        {
            return None;
        }
        Some(effect_chain_log_line_summary(line))
    })
}

fn effect_chain_log_line_matches_channel(line: &str, channel_id: Option<&str>) -> bool {
    let Some(channel_id) = channel_id else {
        return true;
    };
    line.split_whitespace()
        .find_map(|token| token.strip_prefix("channel_id="))
        .is_some_and(|value| {
            value.trim_matches(|character| character == ',' || character == '"') == channel_id
        })
}

fn effect_chain_log_line_summary(line: &str) -> String {
    let summary = line
        .rsplit('|')
        .next()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .unwrap_or(line.trim());
    summary.chars().take(180).collect()
}

fn effect_chain_native_underrun_frames(line: &str) -> Option<u64> {
    let lower = line.to_ascii_lowercase();
    if !lower.contains("native_stats") {
        return None;
    }
    let value = lower
        .split_whitespace()
        .find_map(|token| token.strip_prefix("underrun_frames="))?;
    let digits = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn remove_effect_chain_failure_artifacts(log_path: &Path) {
    for suffix in ["log", "conf", "json"] {
        let _ = fs::remove_file(log_path.with_extension(suffix));
    }
}

fn effect_chain_log_line_timestamp(line: &str) -> Option<OffsetDateTime> {
    let timestamp = line.split_whitespace().next()?;
    OffsetDateTime::parse(timestamp, &Rfc3339).ok()
}

fn effect_chain_failure_log_is_active(path: &Path, modified: SystemTime) -> bool {
    if let Some(timestamp) = effect_chain_failure_log_timestamp(path) {
        let now = OffsetDateTime::now_utc().unix_timestamp_nanos();
        return timestamp >= now || now - timestamp <= FX_LOG_WARNING_WINDOW.as_nanos() as i128;
    }

    match SystemTime::now().duration_since(modified) {
        Ok(age) => age <= FX_LOG_WARNING_WINDOW,
        Err(_) => true,
    }
}

fn effect_chain_failure_log_timestamp(path: &Path) -> Option<i128> {
    let name = path.file_name()?.to_str()?;
    let timestamp = name.split(".failure.").nth(1)?.strip_suffix(".log")?;
    timestamp.parse().ok()
}

fn realtime_fallback_effect(effect_id: &str) -> bool {
    matches!(effect_id, "convolver")
}

fn bypass_realtime_fallback_effects(channel: &mut Channel) -> bool {
    let mut changed = false;
    for effect in &mut channel.effects {
        if !effect.bypassed && realtime_fallback_effect(&effect.effect_id) {
            effect.bypassed = true;
            changed = true;
        }
    }
    changed
}

fn channel_effects_desired_enabled(channel: &Channel) -> bool {
    channel.effects_enabled && channel.effects.iter().any(|effect| !effect.bypassed)
}

fn channel_with_effect_enable_applied(channel: &Channel) -> Channel {
    let mut effective = channel.clone();
    if !effective.effects_enabled {
        for effect in &mut effective.effects {
            effect.bypassed = true;
        }
    }
    effective
}

fn effect_runtime_state_name(state: EffectRuntimeState) -> &'static str {
    match state {
        EffectRuntimeState::Grey => "grey",
        EffectRuntimeState::Red => "red",
        EffectRuntimeState::Green => "green",
    }
}

fn panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).into()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".into()
    }
}

fn safe_file_id(value: &str) -> String {
    let mut safe = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            safe.push(ch);
        } else if !safe.ends_with('-') {
            safe.push('-');
        }
    }
    let safe = safe.trim_matches('-');
    if safe.is_empty() {
        "channel".into()
    } else {
        safe.into()
    }
}

fn safe_hardware_profile_file_id(profile_id: &str) -> String {
    let safe = safe_file_id(profile_id);
    if safe == "channel" {
        "hardware-profile".into()
    } else {
        safe
    }
}

fn clean_profile_id(profile_id: String) -> Result<String, ModelError> {
    let profile_id = profile_id.trim();
    if profile_id.is_empty() {
        return Err(ModelError::InvalidName);
    }
    Ok(profile_id.chars().take(128).collect())
}

fn clean_optional_profile_name(name: String) -> Option<String> {
    let name = name.trim();
    (!name.is_empty()).then(|| name.chars().take(96).collect())
}

fn normalized_profile_latency(mut policy: LatencyPolicy) -> LatencyPolicy {
    policy.stable_msec = policy.stable_msec.map(|value| value.clamp(5, 500));
    policy.low_latency_msec = policy.low_latency_msec.map(|value| value.clamp(5, 500));
    policy.bluetooth_floor_msec = policy.bluetooth_floor_msec.map(|value| value.clamp(5, 500));
    policy
}

fn normalized_profile_routing(policy: RoutingPolicy) -> RoutingPolicy {
    policy
}

fn should_restore_audio_graph_on_launch(graph_namespace: &str, configured: bool) -> bool {
    graph_namespace == "wavelinux6" || configured
}

fn settings_affect_audio_graph(previous: &MixerSettings, next: &MixerSettings) -> bool {
    previous.monitor_follows_default_output != next.monitor_follows_default_output
        || previous.lock_default_input != next.lock_default_input
        || previous.lock_default_output != next.lock_default_output
        || previous.low_latency_mic_monitoring != next.low_latency_mic_monitoring
        || previous.hardware_direct_mic_monitoring != next.hardware_direct_mic_monitoring
        || previous.stream_sync_delay_msec != next.stream_sync_delay_msec
        || previous.monitor_sync_delay_msec != next.monitor_sync_delay_msec
        || previous.optimization_mode != next.optimization_mode
        || previous.runtime_latency_policy != next.runtime_latency_policy
}

fn module_is_stale_for_config(module: &ManagedModule, config: &MixerConfig) -> bool {
    match module.role.as_deref() {
        Some("mix") if config.mixes.iter().all(mix_uses_persistent_audio_core) => true,
        Some("mix") => module.mix_id.as_deref().is_none_or(|mix_id| {
            config
                .mixes
                .iter()
                .find(|mix| mix.id == mix_id)
                .is_none_or(|mix| {
                    module
                        .node_name
                        .as_deref()
                        .is_some_and(|node_name| node_name != mix.virtual_sink_name)
                })
        }),
        Some("mix_source") if config.mixes.iter().all(mix_uses_persistent_audio_core) => true,
        Some("mix_source") => module.mix_id.as_deref().is_none_or(|mix_id| {
            config
                .mixes
                .iter()
                .find(|mix| mix.id == mix_id)
                .is_none_or(|mix| {
                    module
                        .node_name
                        .as_deref()
                        .is_some_and(|node_name| node_name != mix.virtual_source_name)
                })
        }),
        Some("mix_monitor") => module.mix_id.as_deref().is_none_or(|mix_id| {
            config
                .mixes
                .iter()
                .find(|mix| mix.id == mix_id)
                .is_none_or(|mix| {
                    let Some(output) = module.sink_name.as_deref() else {
                        return true;
                    };
                    if !mix
                        .outputs()
                        .iter()
                        .any(|candidate| audio_endpoint_names_match(candidate, output))
                    {
                        return true;
                    }
                    if module.route_revision.as_deref()
                        != Some(
                            mix_monitor_route_revision_for_sink(&config.settings, mix, output)
                                .as_str(),
                        )
                    {
                        return true;
                    }
                    route_endpoint_mismatch(
                        module,
                        Some(&mix_render_source_name(mix)),
                        Some(output),
                    )
                })
        }),
        Some("channel") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    module
                        .node_name
                        .as_deref()
                        .is_some_and(|node_name| node_name != channel.virtual_sink_name)
                })
        }),
        Some("input_to_channel") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    if module.route_revision.as_deref()
                        != Some(input_route_revision(&config.settings, channel).as_str())
                    {
                        return true;
                    }
                    let Some(source) = channel.source_device.as_deref() else {
                        return true;
                    };
                    route_endpoint_mismatch(module, Some(source), Some(&channel.virtual_sink_name))
                })
        }),
        Some("channel_to_effect") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    if channel_uses_persistent_audio_core(channel)
                        || !channel_has_active_effects(channel)
                    {
                        return true;
                    }
                    if module.route_revision.as_deref()
                        != Some(effect_route_revision(&config.settings, channel).as_str())
                    {
                        return true;
                    }
                    route_endpoint_mismatch(
                        module,
                        Some(&format!("{}.monitor", channel.virtual_sink_name)),
                        Some(&effect_chain_input_name(channel)),
                    )
                })
        }),
        Some("effect_to_adaptive_bridge") => {
            let Some(channel_id) = module.channel_id.as_deref() else {
                return true;
            };
            let Some(channel) = config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
            else {
                return true;
            };
            if !channel_uses_adaptive_latency_bridge(channel) {
                return true;
            }
            if module.route_revision.as_deref() != Some(EFFECT_ADAPTIVE_BRIDGE_ROUTE_REVISION) {
                return true;
            }
            route_endpoint_mismatch(
                module,
                Some(&effect_chain_filter_output_name(channel)),
                Some(&effect_chain_adaptive_bridge_input_name(channel)),
            )
        }
        Some("channel_to_mix")
            if config
                .channels
                .iter()
                .all(channel_uses_persistent_audio_core) =>
        {
            true
        }
        Some("channel_to_mix") => {
            let Some(channel_id) = module.channel_id.as_deref() else {
                return true;
            };
            let Some(mix_id) = module.mix_id.as_deref() else {
                return true;
            };
            let Some(channel) = config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
            else {
                return true;
            };
            let Some(mix) = config.mixes.iter().find(|mix| mix.id == mix_id) else {
                return true;
            };
            if channel_mix_route_uses_hardware_direct_monitoring(channel, mix, &config.settings) {
                return true;
            }
            if module.route_revision.as_deref()
                != Some(channel_mix_route_revision(&config.settings, channel, mix).as_str())
            {
                return true;
            }
            !channel.mix_buses.get(mix_id).is_some_and(|bus| bus.enabled)
                || route_endpoint_mismatch(
                    module,
                    Some(&channel_mix_source_name(channel)),
                    Some(&mix.virtual_sink_name),
                )
        }
        Some("effect_chain") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    if !channel_has_active_effects(channel) {
                        return true;
                    }
                    let expected = effect_chain_node_name(channel);
                    module.node_name.as_deref() != Some(expected.as_str())
                })
        }),
        Some("effect_input") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    if !channel_has_active_effects(channel) {
                        return true;
                    }
                    let expected = effect_chain_input_name(channel);
                    module.node_name.as_deref() != Some(expected.as_str())
                })
        }),
        Some("effect_output") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    if !channel_has_active_effects(channel) {
                        return true;
                    }
                    let expected = effect_chain_source_name(channel);
                    module.node_name.as_deref() != Some(expected.as_str())
                })
        }),
        Some("effect_processed") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    if !channel_uses_adaptive_latency_bridge(channel) {
                        return true;
                    }
                    let expected = effect_chain_filter_output_name(channel);
                    module.node_name.as_deref() != Some(expected.as_str())
                })
        }),
        Some("adaptive_bridge_input") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    if !channel_uses_adaptive_latency_bridge(channel) {
                        return true;
                    }
                    let expected = effect_chain_adaptive_bridge_input_name(channel);
                    module.node_name.as_deref() != Some(expected.as_str())
                })
        }),
        Some("mic_passthrough") => module.channel_id.as_deref().is_none_or(|channel_id| {
            config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .is_none_or(|channel| {
                    if channel_has_active_effects(channel) || channel.id != "hardware_in" {
                        return true;
                    }
                    let expected = effect_chain_source_name(channel);
                    module.node_name.as_deref() != Some(expected.as_str())
                })
        }),
        Some(_) => true,
        None => module
            .node_name
            .as_deref()
            .is_some_and(|node_name| node_name.to_ascii_lowercase().contains("wavelinux")),
    }
}

fn route_endpoint_mismatch(
    module: &ManagedModule,
    expected_source: Option<&str>,
    expected_sink: Option<&str>,
) -> bool {
    module
        .source_name
        .as_deref()
        .zip(expected_source)
        .is_some_and(|(actual, expected)| !audio_endpoint_names_match(actual, expected))
        || module
            .sink_name
            .as_deref()
            .zip(expected_sink)
            .is_some_and(|(actual, expected)| !audio_endpoint_names_match(actual, expected))
}

fn audio_endpoint_names_match(actual: &str, expected: &str) -> bool {
    actual == expected
        || bluetooth_endpoint_key(actual)
            .zip(bluetooth_endpoint_key(expected))
            .is_some_and(|(actual, expected)| actual == expected)
}

fn bluetooth_endpoint_key(endpoint: &str) -> Option<String> {
    bluetooth_endpoint_device_key(endpoint).map(|key| {
        if endpoint.trim().starts_with("bluez_input.") {
            format!("bluez_input.{key}")
        } else {
            format!("bluez_output.{key}")
        }
    })
}

fn bluetooth_endpoint_device_key(endpoint: &str) -> Option<String> {
    let endpoint = endpoint
        .trim()
        .strip_suffix(".monitor")
        .unwrap_or_else(|| endpoint.trim());
    let rest = endpoint
        .strip_prefix("bluez_output.")
        .or_else(|| endpoint.strip_prefix("bluez_input."))?;
    let device_id = normalize_bluetooth_device_key(rest);
    if device_id.matches('_').count() < 5 {
        return None;
    }
    Some(device_id)
}

fn normalize_bluetooth_device_key(value: &str) -> String {
    value
        .trim()
        .split('.')
        .next()
        .unwrap_or_default()
        .replace(':', "_")
        .to_ascii_uppercase()
}

fn module_dedupe_key_for_config(module: &ManagedModule, config: &MixerConfig) -> Option<String> {
    match module.role.as_deref()? {
        "mix" | "mix_source" | "mix_monitor" => {
            let mix_id = module.mix_id.as_deref()?;
            config.mixes.iter().any(|mix| mix.id == mix_id).then(|| {
                format!(
                    "{}:{mix_id}:{}:{}",
                    module.role.as_deref().unwrap_or_default(),
                    module.source_name.as_deref().unwrap_or_default(),
                    module.sink_name.as_deref().unwrap_or_default()
                )
            })
        }
        "channel" | "input_to_channel" | "channel_to_effect" => {
            let channel_id = module.channel_id.as_deref()?;
            config
                .channels
                .iter()
                .any(|channel| channel.id == channel_id)
                .then(|| {
                    format!(
                        "{}:{channel_id}:{}:{}",
                        module.role.as_deref().unwrap_or_default(),
                        module.source_name.as_deref().unwrap_or_default(),
                        module.sink_name.as_deref().unwrap_or_default()
                    )
                })
        }
        "channel_to_mix" => {
            let channel_id = module.channel_id.as_deref()?;
            let mix_id = module.mix_id.as_deref()?;
            let channel_exists = config
                .channels
                .iter()
                .any(|channel| channel.id == channel_id);
            let mix_exists = config.mixes.iter().any(|mix| mix.id == mix_id);
            (channel_exists && mix_exists).then(|| {
                format!(
                    "channel_to_mix:{channel_id}:{mix_id}:{}:{}",
                    module.source_name.as_deref().unwrap_or_default(),
                    module.sink_name.as_deref().unwrap_or_default()
                )
            })
        }
        _ => None,
    }
}

fn module_is_stale_for_active_routes(
    module: &ManagedModule,
    config: &MixerConfig,
    active_app_channel_ids: &BTreeSet<String>,
    active_mix_ids: &BTreeSet<String>,
) -> bool {
    if module.role.as_deref() == Some("mix_monitor") {
        let Some(mix_id) = module.mix_id.as_deref() else {
            return true;
        };
        if !active_mix_ids.contains(mix_id) {
            return true;
        }
    }

    if module.role.as_deref() == Some("channel_to_mix") {
        let Some(channel_id) = module.channel_id.as_deref() else {
            return true;
        };
        let Some(mix_id) = module.mix_id.as_deref() else {
            return true;
        };
        let Some(channel) = config
            .channels
            .iter()
            .find(|channel| channel.id == channel_id)
        else {
            return true;
        };
        let Some(mix) = config.mixes.iter().find(|mix| mix.id == mix_id) else {
            return true;
        };
        if !channel_mix_route_expected_for_active_routes(
            channel,
            mix,
            &config.settings,
            active_app_channel_ids,
            active_mix_ids,
        ) {
            return true;
        }
    }

    module_is_stale_for_config(module, config)
}

#[cfg(test)]
fn route_health_issues(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
) -> Vec<RouteHealthIssue> {
    let active_app_channel_ids = all_app_channel_ids(config);
    route_health_issues_for_active_app_channels(
        config,
        graph,
        managed_modules,
        source_outputs,
        sink_inputs,
        &active_app_channel_ids,
    )
}

fn route_health_issues_for_active_app_channels(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
    active_app_channel_ids: &BTreeSet<String>,
) -> Vec<RouteHealthIssue> {
    let active_mix_ids = all_mix_ids(config);
    route_health_issues_for_active_routes(
        config,
        graph,
        managed_modules,
        source_outputs,
        sink_inputs,
        active_app_channel_ids,
        &active_mix_ids,
    )
}

fn route_health_issues_for_active_routes(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
    active_app_channel_ids: &BTreeSet<String>,
    active_mix_ids: &BTreeSet<String>,
) -> Vec<RouteHealthIssue> {
    if !graph_has_wavelinux_nodes(graph) {
        return Vec::new();
    }

    let mut issues = Vec::new();
    let mut seen_routes = BTreeSet::new();

    for module in managed_modules
        .iter()
        .filter(|module| managed_module_is_loopback_route(module))
    {
        let duplicate = module_dedupe_key_for_config(module, config)
            .is_some_and(|key| !seen_routes.insert(key));
        let source_name = module.source_name.as_deref();
        let sink_name = module.sink_name.as_deref();
        let role = module.role.as_deref();
        let live_source_output = find_managed_loopback_source_output(
            module,
            role,
            module.channel_id.as_deref(),
            module.mix_id.as_deref(),
            source_name,
            source_outputs,
        );
        let live_sink_input = find_managed_loopback_sink_input(
            module,
            role,
            module.channel_id.as_deref(),
            module.mix_id.as_deref(),
            sink_name,
            sink_inputs,
        );
        let reason = if module_is_stale_for_active_routes(
            module,
            config,
            active_app_channel_ids,
            active_mix_ids,
        ) {
            Some(RouteHealthReason::StaleConfig)
        } else if duplicate {
            Some(RouteHealthReason::Duplicate)
        } else if source_name.is_none_or(|source| !route_endpoint_source_available(source, graph)) {
            Some(RouteHealthReason::MissingSource)
        } else if sink_name.is_none_or(|sink| !route_endpoint_sink_available(sink, graph)) {
            Some(RouteHealthReason::MissingSink)
        } else {
            match (live_source_output, live_sink_input) {
                (None, _) => Some(RouteHealthReason::MissingSourceOutput),
                (_, None) => Some(RouteHealthReason::MissingSinkInput),
                (Some(source_output), Some(sink_input)) => {
                    if managed_route_level_mismatch(config, module, source_output, sink_input) {
                        Some(RouteHealthReason::LevelMismatch)
                    } else {
                        None
                    }
                }
            }
        };

        if let Some(reason) = reason {
            issues.push(RouteHealthIssue {
                module_id: Some(module.module_id.clone()),
                role: module.role.clone().unwrap_or_else(|| "unknown".into()),
                channel_id: module.channel_id.clone(),
                mix_id: module.mix_id.clone(),
                source_name: module.source_name.clone(),
                sink_name: module.sink_name.clone(),
                reason,
            });
        }
    }

    issues
}

fn effect_route_health_issues_for_channels(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
    channel_ids: &BTreeSet<String>,
) -> Vec<RouteHealthIssue> {
    route_health_issues_for_active_app_channels(
        config,
        graph,
        managed_modules,
        source_outputs,
        sink_inputs,
        &all_app_channel_ids(config),
    )
    .into_iter()
    .filter(|issue| {
        matches!(issue.role.as_str(), "channel_to_effect" | "channel_to_mix")
            && issue
                .channel_id
                .as_deref()
                .is_some_and(|channel_id| channel_ids.contains(channel_id))
    })
    .collect()
}

fn route_endpoint_source_available(source_name: &str, graph: &RuntimeGraph) -> bool {
    graph.inputs.iter().any(|source| {
        audio_endpoint_names_match(&source.id, source_name)
            || audio_endpoint_names_match(&source.name, source_name)
    })
}

fn route_endpoint_sink_available(sink_name: &str, graph: &RuntimeGraph) -> bool {
    graph.outputs.iter().any(|sink| {
        audio_endpoint_names_match(&sink.id, sink_name)
            || audio_endpoint_names_match(&sink.name, sink_name)
    })
}

fn route_health_diagnostics(issues: &[RouteHealthIssue]) -> Vec<Diagnostic> {
    issues
        .iter()
        .map(|issue| {
            let module = issue.module_id.as_deref().unwrap_or("unknown");
            let route = route_health_route_label(issue);
            let reason = route_health_reason_label(&issue.reason);
            Diagnostic {
                code: format!("route.health.{}.{}", issue.role, module),
                severity: DiagnosticSeverity::Warning,
                message: format!("{route} is not healthy: {reason}"),
                action: Some(
                    "WaveLinux will repair stale managed routes automatically; run Repair if this remains visible"
                        .into(),
                ),
            }
        })
        .collect()
}

fn route_health_route_label(issue: &RouteHealthIssue) -> String {
    let mut label = issue.role.clone();
    if let Some(channel_id) = issue.channel_id.as_deref() {
        label.push_str(" channel=");
        label.push_str(channel_id);
    }
    if let Some(mix_id) = issue.mix_id.as_deref() {
        label.push_str(" mix=");
        label.push_str(mix_id);
    }
    label
}

fn route_health_reason_label(reason: &RouteHealthReason) -> &'static str {
    match reason {
        RouteHealthReason::MissingSource => "source endpoint is missing",
        RouteHealthReason::MissingSink => "sink endpoint is missing",
        RouteHealthReason::MissingSourceOutput => "source-output side is missing",
        RouteHealthReason::MissingSinkInput => "sink-input side is missing",
        RouteHealthReason::StaleConfig => "route no longer matches the current config",
        RouteHealthReason::Duplicate => "duplicate managed route",
        RouteHealthReason::LevelMismatch => "route mute or volume drifted from the mixer config",
    }
}

fn auto_device_slot_matches(left: &ResolvedAutoDevice, right: &ResolvedAutoDevice) -> bool {
    left.kind == right.kind && left.channel_id == right.channel_id && left.mix_id == right.mix_id
}

fn stabilize_auto_device_reasons(previous: &[ResolvedAutoDevice], next: &mut [ResolvedAutoDevice]) {
    for device in next {
        let Some(prior) = previous.iter().find(|prior| {
            auto_device_slot_matches(prior, device) && prior.device_id == device.device_id
        }) else {
            continue;
        };
        device.reason = prior.reason;
    }
}

fn auto_device_reason_label(reason: &AutoDeviceReason) -> &'static str {
    match reason {
        AutoDeviceReason::Priority => "priority",
        AutoDeviceReason::SystemDefault => "system_default",
        AutoDeviceReason::ActiveOutput => "active_output",
        AutoDeviceReason::Unavailable => "unavailable",
    }
}

fn route_health_signature(issues: &[RouteHealthIssue]) -> String {
    let mut parts = issues
        .iter()
        .map(|issue| {
            format!(
                "{}|{}|{}|{}|{}|{}|{:?}",
                issue.module_id.as_deref().unwrap_or_default(),
                issue.role,
                issue.channel_id.as_deref().unwrap_or_default(),
                issue.mix_id.as_deref().unwrap_or_default(),
                issue.source_name.as_deref().unwrap_or_default(),
                issue.sink_name.as_deref().unwrap_or_default(),
                issue.reason
            )
        })
        .collect::<Vec<_>>();
    parts.sort();
    parts.join(";")
}

fn route_health_summary(issues: &[RouteHealthIssue]) -> String {
    let mut parts = issues
        .iter()
        .take(6)
        .map(|issue| {
            format!(
                "{}:{}:{}:{}",
                issue.role,
                issue.channel_id.as_deref().unwrap_or("-"),
                issue.mix_id.as_deref().unwrap_or("-"),
                route_health_reason_label(&issue.reason)
            )
        })
        .collect::<Vec<_>>();
    if issues.len() > parts.len() {
        parts.push(format!("+{} more", issues.len() - parts.len()));
    }
    parts.join(", ")
}

fn repair_command_is_satisfied(
    command: &CommandSpec,
    graph: &RuntimeGraph,
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
    managed_modules: &[ManagedModule],
) -> bool {
    if command.program != "pactl" || command.args.first().map(String::as_str) != Some("load-module")
    {
        return false;
    }

    match command.args.get(1).map(String::as_str) {
        Some("module-null-sink") => command_arg_value(&command.args, "sink_name=")
            .is_some_and(|sink_name| graph.outputs.iter().any(|sink| sink.name == sink_name)),
        Some("module-remap-source") => command_arg_value(&command.args, "source_name=")
            .is_some_and(|source_name| {
                let source_exists = graph.inputs.iter().any(|source| source.name == source_name);
                if !source_exists {
                    return false;
                }
                let Some(properties) = command_arg_value(&command.args, "source_properties=")
                else {
                    return true;
                };
                let role = graph_property_value_from_arg(properties, "role");
                let channel_id = graph_property_value_from_arg(properties, "channel_id");
                let mix_id = graph_property_value_from_arg(properties, "mix_id");
                if role.is_none() && channel_id.is_none() && mix_id.is_none() {
                    return true;
                }
                managed_modules.iter().any(|module| {
                    module.role.as_deref() == role
                        && module.channel_id.as_deref() == channel_id
                        && module.mix_id.as_deref() == mix_id
                        && module
                            .node_name
                            .as_deref()
                            .is_some_and(|actual| audio_endpoint_names_match(actual, source_name))
                })
            }),
        Some("module-loopback") => {
            let Some(properties) = command_arg_value(&command.args, "source_output_properties=")
            else {
                return false;
            };
            let role = graph_property_value_from_arg(properties, "role");
            let channel_id = graph_property_value_from_arg(properties, "channel_id");
            let mix_id = graph_property_value_from_arg(properties, "mix_id");
            let route_revision = graph_property_value_from_arg(properties, "route_revision");
            let source_name = command_arg_value(&command.args, "source=");
            let sink_name = command_arg_value(&command.args, "sink=");
            if !route_command_endpoints_available(command, graph) {
                return false;
            }
            if let Some(module) = managed_modules.iter().find(|module| {
                module.role.as_deref() == role
                    && module.channel_id.as_deref() == channel_id
                    && module.mix_id.as_deref() == mix_id
                    && module.route_revision.as_deref() == route_revision
                    && source_name.is_none_or(|source| {
                        module
                            .source_name
                            .as_deref()
                            .is_some_and(|actual| audio_endpoint_names_match(actual, source))
                    })
                    && sink_name.is_none_or(|sink| {
                        module
                            .sink_name
                            .as_deref()
                            .is_some_and(|actual| audio_endpoint_names_match(actual, sink))
                    })
            }) {
                return managed_loopback_has_live_source_output(
                    module,
                    role,
                    channel_id,
                    mix_id,
                    source_name,
                    source_outputs,
                ) && managed_loopback_has_live_sink_input(
                    module,
                    role,
                    channel_id,
                    mix_id,
                    sink_name,
                    sink_inputs,
                );
            }

            false
        }
        _ => false,
    }
}

fn command_is_mix_monitor_route(command: &CommandSpec) -> bool {
    command.program == "pactl"
        && command.args.first().map(String::as_str) == Some("load-module")
        && command.args.get(1).map(String::as_str) == Some("module-loopback")
        && command_arg_value(&command.args, "source_output_properties=")
            .and_then(|properties| graph_property_value_from_arg(properties, "role"))
            == Some("mix_monitor")
}

fn command_is_incremental_mix_route(command: &CommandSpec) -> bool {
    command.program == "pactl"
        && command.args.first().map(String::as_str) == Some("load-module")
        && command.args.get(1).map(String::as_str) == Some("module-loopback")
        && command_arg_value(&command.args, "source_output_properties=")
            .and_then(|properties| graph_property_value_from_arg(properties, "role"))
            .is_some_and(|role| matches!(role, "channel_to_mix" | "mix_monitor"))
}

fn command_is_auto_device_route(command: &CommandSpec) -> bool {
    command.program == "pactl"
        && command.args.first().map(String::as_str) == Some("load-module")
        && command.args.get(1).map(String::as_str) == Some("module-loopback")
        && command_arg_value(&command.args, "source_output_properties=")
            .and_then(|properties| graph_property_value_from_arg(properties, "role"))
            .is_some_and(|role| matches!(role, "input_to_channel" | "mix_monitor"))
}

fn command_routes_active_effect_channel(
    command: &CommandSpec,
    active_effect_channel_ids: &BTreeSet<String>,
) -> bool {
    if active_effect_channel_ids.is_empty()
        || command.program != "pactl"
        || command.args.first().map(String::as_str) != Some("load-module")
        || command.args.get(1).map(String::as_str) != Some("module-loopback")
    {
        return false;
    }

    let Some(properties) = command_arg_value(&command.args, "source_output_properties=") else {
        return false;
    };
    let role = graph_property_value_from_arg(properties, "role");
    let channel_id = graph_property_value_from_arg(properties, "channel_id");

    matches!(role, Some("channel_to_effect") | Some("channel_to_mix"))
        && channel_id.is_some_and(|id| active_effect_channel_ids.contains(id))
}

fn monitor_route_endpoints_available(command: &CommandSpec, graph: &RuntimeGraph) -> bool {
    route_command_endpoints_available(command, graph)
}

fn route_command_endpoints_available(command: &CommandSpec, graph: &RuntimeGraph) -> bool {
    let Some(source_name) = command_arg_value(&command.args, "source=") else {
        return false;
    };
    let Some(sink_name) = command_arg_value(&command.args, "sink=") else {
        return false;
    };

    route_endpoint_source_available(source_name, graph)
        && route_endpoint_sink_available(sink_name, graph)
}

fn command_targets_bluetooth_sink(command: &CommandSpec) -> bool {
    command_arg_value(&command.args, "sink=")
        .map(|sink| {
            sink.trim()
                .to_ascii_lowercase()
                .starts_with("bluez_output.")
        })
        .unwrap_or(false)
}

fn command_arg_value<'a>(args: &'a [String], prefix: &str) -> Option<&'a str> {
    args.iter()
        .find_map(|arg| arg.strip_prefix(prefix))
        .filter(|value| !value.is_empty())
}

fn property_value_from_arg<'a>(properties: &'a str, key: &str) -> Option<&'a str> {
    properties
        .split_whitespace()
        .find_map(|part| part.strip_prefix(key))
        .filter(|value| !value.is_empty())
}

fn load_config(paths: &EnginePaths) -> Result<MixerConfig, EngineError> {
    let path = paths.config_file();
    if path.exists() {
        match read_json(&path) {
            Ok(config) => Ok(config),
            Err(_) => {
                backup_invalid_config(&path);
                Ok(MixerConfig::default())
            }
        }
    } else {
        Ok(import_wavelinux5_config_for_wavelinux6(paths)?.unwrap_or_default())
    }
}

fn effect_chain_file_name(channel_id: &str, suffix: &str) -> String {
    format!(
        "{}-chain-{}.{}",
        graph_prefix(),
        safe_file_id(channel_id),
        suffix
    )
}

fn dsp_channel_config(channel: &Channel) -> wavelinux_dsp::DspChannelConfig {
    let mut effects = channel.effects.clone();
    if !channel.effects_enabled {
        for effect in &mut effects {
            effect.bypassed = true;
        }
    }
    let mut config = wavelinux_dsp::DspChannelConfig::new(
        channel.id.clone(),
        channel.name.clone(),
        graph_prefix(),
        graph_property_prefix(),
        app_display_name(),
        effect_chain_input_name(channel),
        effect_chain_source_name(channel),
        effects,
    );
    config.input_target_node_name = channel.source_device.clone();
    if channel.kind.uses_hardware_slot() {
        config.input_target_capable = true;
    }
    config.input_mode = match channel.input_mode {
        ChannelInputMode::Stereo => wavelinux_dsp::DspInputMode::Stereo,
        ChannelInputMode::MonoLeft => wavelinux_dsp::DspInputMode::MonoLeft,
        ChannelInputMode::MonoRight => wavelinux_dsp::DspInputMode::MonoRight,
        ChannelInputMode::SumMono => wavelinux_dsp::DspInputMode::SumMono,
        ChannelInputMode::SwapLr => wavelinux_dsp::DspInputMode::SwapLr,
    };
    config.input_channels = if matches!(
        channel.input_mode,
        ChannelInputMode::Stereo | ChannelInputMode::SwapLr
    ) {
        2
    } else {
        1
    };
    config
}

fn dsp_adaptive_bridge_config(
    channel: &Channel,
    config: &MixerConfig,
    runtime_root: &Path,
) -> wavelinux_dsp::DspChannelConfig {
    let mut bridge = wavelinux_dsp::DspChannelConfig::new(
        channel.id.clone(),
        channel.name.clone(),
        graph_prefix(),
        graph_property_prefix(),
        app_display_name(),
        effect_chain_adaptive_bridge_input_name(channel),
        effect_chain_source_name(channel),
        Vec::new(),
    );
    bridge.input_role = Some("adaptive_bridge_input".into());
    bridge.output_role = Some("effect_output".into());
    bridge.adaptive_latency = dsp_adaptive_latency_config(&config.settings.adaptive_latency);
    bridge.control_socket_path = Some(
        wavelinux_dsp::channel_control_socket(runtime_root, &graph_prefix(), &channel.id)
            .to_string_lossy()
            .into_owned(),
    );
    bridge
}

fn dsp_adaptive_latency_config(
    settings: &wavelinux_model::AdaptiveLatencySettings,
) -> wavelinux_dsp::DspAdaptiveLatencyConfig {
    wavelinux_dsp::DspAdaptiveLatencyConfig {
        enabled: settings.enabled,
        min_msec: settings.min_msec,
        max_msec: settings.max_msec,
        levels_msec: settings.levels_msec.clone(),
    }
}

fn dsp_mix_config(mix: &Mix, config: &MixerConfig) -> wavelinux_dsp::DspMixConfig {
    let latency_msec = config
        .channels
        .iter()
        .filter(|channel| {
            channel
                .mix_buses
                .get(&mix.id)
                .is_some_and(|bus| bus.enabled)
        })
        .map(|channel| channel_mix_latency_msec(channel, mix, &config.settings))
        .max()
        .unwrap_or(config.settings.adaptive_latency.min_msec)
        .clamp(5, 500);
    let latency_frames = ((u64::from(config.audio.sample_rate_hz) * u64::from(latency_msec)) / 1000)
        .max(1)
        .min(u64::from(u32::MAX)) as u32;
    wavelinux_dsp::DspMixConfig {
        mix_id: mix.id.clone(),
        mix_name: mix.name.clone(),
        graph_prefix: graph_prefix(),
        property_prefix: graph_property_prefix(),
        app_name: app_display_name(),
        output_node_name: mix.virtual_source_name.clone(),
        output_target_node_names: mix.outputs(),
        sample_rate_hz: config.audio.sample_rate_hz,
        latency_frames,
        pipewire_quantum_frames: 0,
        adaptive_latency: dsp_adaptive_latency_config(&config.settings.adaptive_latency),
        volume: mix.volume,
        muted: mix.muted,
        buses: config
            .channels
            .iter()
            .filter_map(|channel| {
                channel
                    .mix_buses
                    .get(&mix.id)
                    .map(|bus| wavelinux_dsp::DspMixBusConfig {
                        channel_id: channel.id.clone(),
                        volume: bus.volume,
                        muted: bus.muted,
                        enabled: bus.enabled,
                    })
            })
            .collect(),
    }
}

fn apply_audio_core_discontinuity_deltas(
    statuses: &mut [AudioCoreChannelStatus],
    counters: &Mutex<BTreeMap<String, u64>>,
) {
    let Ok(mut previous) = counters.lock() else {
        return;
    };
    let active_ids = statuses
        .iter()
        .map(|status| status.channel_id.clone())
        .collect::<BTreeSet<_>>();
    previous.retain(|channel_id, _| active_ids.contains(channel_id));
    for status in statuses {
        if !status.online {
            continue;
        }
        let discontinuity_frames = status.underrun_frames.saturating_add(status.dropped_frames);
        status.underrun_delta = previous
            .insert(status.channel_id.clone(), discontinuity_frames)
            .map_or(0, |prior| discontinuity_frames.saturating_sub(prior));
    }
}

fn normalized_adaptive_levels(settings: &wavelinux_model::AdaptiveLatencySettings) -> Vec<u16> {
    let mut levels = settings
        .levels_msec
        .iter()
        .copied()
        .map(|level| level.clamp(settings.min_msec, settings.max_msec))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if levels.is_empty() {
        levels.push(settings.min_msec);
    }
    if !levels.contains(&settings.min_msec) {
        levels.push(settings.min_msec);
    }
    if !levels.contains(&settings.max_msec) {
        levels.push(settings.max_msec);
    }
    levels.sort_unstable();
    levels.dedup();
    levels
}

fn learned_quantum_floor_for_mix(mix: &Mix, floors: &BTreeMap<String, u32>) -> u32 {
    if mix.id != "monitor" {
        return 0;
    }
    floors.get(&mix.outputs().join("|")).copied().unwrap_or(0)
}

fn adaptive_monitor_output_signature(audio_core: &[AudioCoreChannelStatus]) -> String {
    audio_core
        .iter()
        .find(|status| status.channel_id == "mix:monitor")
        .map(|status| status.output_target_node_names.join("|"))
        .filter(|signature| !signature.is_empty())
        .unwrap_or_else(|| "<no-monitor-output>".into())
}

fn adaptive_latency_signal(
    settings: &wavelinux_model::AdaptiveLatencySettings,
    audio_core: &[AudioCoreChannelStatus],
    cpu_pressure: f32,
    pipewire_warning_delta: u64,
    owned_pipewire_warning_delta: u64,
) -> (AdaptiveLatencySignal, f32, u64, u64) {
    let audio_triggers_enabled = matches!(
        settings.trigger_mode,
        wavelinux_model::AdaptiveLatencyTriggerMode::AudioOnly
            | wavelinux_model::AdaptiveLatencyTriggerMode::Hybrid
    );
    let cpu_triggers_enabled = matches!(
        settings.trigger_mode,
        wavelinux_model::AdaptiveLatencyTriggerMode::CpuOnly
            | wavelinux_model::AdaptiveLatencyTriggerMode::Hybrid
    );
    let pipewire_warning_delta = u64::from(audio_triggers_enabled) * pipewire_warning_delta;
    let owned_pipewire_warning_delta =
        u64::from(audio_triggers_enabled) * owned_pipewire_warning_delta;
    let core_discontinuity_delta = if audio_triggers_enabled {
        audio_core
            .iter()
            .filter(|status| status.online)
            .map(|status| status.underrun_delta)
            .sum()
    } else {
        0
    };
    let underrun_delta = core_discontinuity_delta;
    if underrun_delta > 0 {
        return (
            AdaptiveLatencySignal::AudioTrouble,
            cpu_pressure,
            pipewire_warning_delta,
            underrun_delta,
        );
    }
    if owned_pipewire_warning_delta > 0 {
        return (
            AdaptiveLatencySignal::PipeWireTrouble,
            cpu_pressure,
            pipewire_warning_delta,
            underrun_delta,
        );
    }
    if cpu_triggers_enabled && cpu_pressure >= 0.88 {
        return (
            AdaptiveLatencySignal::CpuPressure,
            cpu_pressure,
            pipewire_warning_delta,
            underrun_delta,
        );
    }
    (
        AdaptiveLatencySignal::Clean,
        cpu_pressure,
        pipewire_warning_delta,
        underrun_delta,
    )
}

#[cfg(unix)]
fn send_adaptive_latency_target(
    socket_path: &Path,
    route_id: &str,
    target_msec: u16,
    pipewire_quantum_frames: u32,
    reason: &str,
) {
    let payload = serde_json::json!({
        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
        "command": "set_target_latency",
        "route_id": route_id,
        "target_msec": target_msec,
        "pipewire_quantum_frames": pipewire_quantum_frames,
        "reason": reason,
    });
    let _ = send_core_control_request(socket_path, &payload);
}

#[cfg(not(unix))]
fn send_adaptive_latency_target(
    _socket_path: &Path,
    _route_id: &str,
    _target_msec: u16,
    _pipewire_quantum_frames: u32,
    _reason: &str,
) {
}

#[cfg(unix)]
fn send_audio_core_input_target(
    socket_path: &Path,
    route_id: &str,
    target_node_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    let before = query_audio_core_diagnostics(socket_path, route_id)?;
    let requested_generation = next_route_generation(&before)?;
    let request_id = Uuid::new_v4().to_string();
    let command = if target_node_name.is_some() {
        "set_input_target"
    } else {
        "clear_input_target"
    };
    let payload = serde_json::json!({
        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
        "command": command,
        "request_id": request_id,
        "route_id": route_id,
        "target_node_name": target_node_name,
        "route_generation": requested_generation,
    });
    let queued = send_core_control_request(socket_path, &payload)?;
    validate_control_request_id(&queued, &request_id)?;
    let queued_generation = queued_route_generation(&queued)?;
    let applied = wait_for_route_generation_ack(
        socket_path,
        route_id,
        queued_generation,
        EFFECT_CORE_ACK_TIMEOUT,
    )?;
    if applied.input_target_node_name.as_deref() != target_node_name {
        return Err(format!(
            "input target generation {queued_generation} applied {}, expected {}",
            applied
                .input_target_node_name
                .as_deref()
                .unwrap_or("<none>"),
            target_node_name.unwrap_or("<none>"),
        ));
    }
    Ok(serde_json::json!({
        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
        "ok": true,
        "request_id": request_id,
        "route_id": route_id,
        "route_generation": queued_generation,
        "applied_route_generation": applied.applied_route_generation,
        "target_node_name": applied.input_target_node_name,
        "operation": if target_node_name.is_some() {
            "input_target_applied"
        } else {
            "input_target_cleared"
        },
    }))
}

#[cfg(not(unix))]
fn send_audio_core_input_target(
    _socket_path: &Path,
    _route_id: &str,
    _target_node_name: Option<&str>,
) -> Result<serde_json::Value, String> {
    Err("WaveLinux audio-core routing requires Unix sockets".into())
}

#[cfg(unix)]
fn send_audio_core_output_targets(
    socket_path: &Path,
    mix_id: &str,
    target_node_names: &[String],
) -> Result<serde_json::Value, String> {
    let before = query_audio_core_diagnostics(socket_path, mix_id)?;
    let requested_generation = next_route_generation(&before)?;
    let request_id = Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
        "command": "set_output_targets",
        "request_id": request_id,
        "mix_id": mix_id,
        "target_node_names": target_node_names,
        "route_generation": requested_generation,
    });
    let queued = send_core_control_request(socket_path, &payload)?;
    validate_control_request_id(&queued, &request_id)?;
    let queued_generation = queued_route_generation(&queued)?;
    let applied = wait_for_route_generation_ack(
        socket_path,
        mix_id,
        queued_generation,
        EFFECT_CORE_ACK_TIMEOUT,
    )?;
    if applied.output_target_node_names != target_node_names {
        return Err(format!(
            "output target generation {queued_generation} applied {:?}, expected {:?}",
            applied.output_target_node_names, target_node_names,
        ));
    }
    Ok(serde_json::json!({
        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
        "ok": true,
        "request_id": request_id,
        "route_id": mix_id,
        "route_generation": queued_generation,
        "applied_route_generation": applied.applied_route_generation,
        "target_node_names": applied.output_target_node_names,
        "operation": "output_targets_applied",
    }))
}

#[cfg(not(unix))]
fn send_audio_core_output_targets(
    _socket_path: &Path,
    _mix_id: &str,
    _target_node_names: &[String],
) -> Result<serde_json::Value, String> {
    Err("WaveLinux audio-core routing requires Unix sockets".into())
}

#[cfg(unix)]
fn send_effect_chain_swap(
    socket_path: &Path,
    route_id: &str,
    config_path: &Path,
    config_revision: &str,
    desired_generation: u64,
) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({
        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
        "command": "swap_chain",
        "request_id": Uuid::new_v4().to_string(),
        "route_id": route_id,
        "config_path": config_path,
        "config_revision": config_revision,
        "desired_generation": desired_generation,
    });
    send_core_control_request(socket_path, &payload)
}

#[cfg(not(unix))]
fn send_effect_chain_swap(
    _socket_path: &Path,
    _route_id: &str,
    _config_path: &Path,
    _config_revision: &str,
    _desired_generation: u64,
) -> Result<serde_json::Value, String> {
    Err("WaveLinux audio-core control requires Unix sockets".into())
}

#[cfg(unix)]
fn query_audio_core_diagnostics(
    socket_path: &Path,
    route_id: &str,
) -> Result<AudioCoreDiagnosticsResponse, String> {
    let payload = serde_json::json!({
        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
        "command": "get_diagnostics",
        "request_id": Uuid::new_v4().to_string(),
        "route_id": route_id,
    });
    let response = send_core_control_request(socket_path, &payload)?;
    let diagnostics: AudioCoreDiagnosticsResponse =
        serde_json::from_value(response).map_err(|err| format!("invalid diagnostics: {err}"))?;
    if diagnostics.protocol_version != wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION {
        return Err(format!(
            "audio core protocol {} does not match expected {}",
            diagnostics.protocol_version,
            wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION
        ));
    }
    if diagnostics.route_id != route_id {
        return Err(format!(
            "audio core returned route {} for {route_id}",
            diagnostics.route_id
        ));
    }
    Ok(diagnostics)
}

#[cfg(unix)]
fn request_audio_core_shutdown(
    socket_path: &Path,
    route_id: &str,
    timeout: Duration,
) -> Result<(), String> {
    let payload = serde_json::json!({
        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
        "command": "shutdown",
        "request_id": Uuid::new_v4().to_string(),
        "route_id": route_id,
    });
    send_core_control_request(socket_path, &payload)?;
    let started = Instant::now();
    while started.elapsed() < timeout {
        if query_audio_core_diagnostics(socket_path, route_id).is_err() {
            return Ok(());
        }
        thread::sleep(EFFECT_CORE_RETRY_MIN);
    }
    Err(format!(
        "audio core did not stop within {} ms after shutdown acknowledgement",
        timeout.as_millis()
    ))
}

#[cfg(not(unix))]
fn request_audio_core_shutdown(
    _socket_path: &Path,
    _route_id: &str,
    _timeout: Duration,
) -> Result<(), String> {
    Err("WaveLinux audio-core shutdown requires Unix sockets".into())
}

#[cfg(unix)]
fn wait_for_audio_core_ready(
    socket_path: &Path,
    route_id: &str,
    timeout: Duration,
) -> Result<AudioCoreDiagnosticsResponse, String> {
    let started = Instant::now();
    let mut delay = EFFECT_CORE_RETRY_MIN;
    let mut last_error = format!("control socket is not ready: {}", socket_path.display());
    while started.elapsed() < timeout {
        match query_audio_core_diagnostics(socket_path, route_id) {
            Ok(response) => return Ok(response),
            Err(error) => last_error = error,
        }
        thread::sleep(delay);
        delay = (delay * 2).min(EFFECT_CORE_RETRY_MAX);
    }
    Err(format!(
        "audio core readiness timed out after {} ms for route_id={} resolved_control_socket={}: {}",
        timeout.as_millis(),
        route_id,
        socket_path.display(),
        last_error
    ))
}

#[cfg(not(unix))]
fn wait_for_audio_core_ready(
    _socket_path: &Path,
    _route_id: &str,
    _timeout: Duration,
) -> Result<AudioCoreDiagnosticsResponse, String> {
    Err("WaveLinux audio-core readiness requires Unix sockets".into())
}

fn next_route_generation(response: &AudioCoreDiagnosticsResponse) -> Result<u64, String> {
    response
        .submitted_route_generation
        .max(response.applied_route_generation)
        .checked_add(1)
        .ok_or_else(|| "audio core route generation exhausted".to_string())
}

fn validate_control_request_id(
    response: &serde_json::Value,
    expected_request_id: &str,
) -> Result<(), String> {
    let response_request_id = response
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "audio core response omitted request_id".to_string())?;
    if response_request_id != expected_request_id {
        return Err(format!(
            "audio core returned request_id {response_request_id}, expected {expected_request_id}"
        ));
    }
    Ok(())
}

fn queued_route_generation(response: &serde_json::Value) -> Result<u64, String> {
    response
        .get("route_generation")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "audio core response omitted route_generation".to_string())
}

fn wait_for_route_generation_ack(
    socket_path: &Path,
    route_id: &str,
    desired_generation: u64,
    timeout: Duration,
) -> Result<AudioCoreDiagnosticsResponse, String> {
    let started = Instant::now();
    let mut delay = EFFECT_CORE_RETRY_MIN;
    let mut last_applied = 0;
    let mut last_submitted = 0;
    let mut last_error = None;
    while started.elapsed() < timeout {
        match query_audio_core_diagnostics(socket_path, route_id) {
            Ok(response) => {
                last_applied = response.applied_route_generation;
                last_submitted = response.submitted_route_generation;
                if response.applied_route_generation >= desired_generation {
                    return Ok(response);
                }
                if response.submitted_route_generation >= desired_generation {
                    if let Some(error) = response.route_target_error.as_deref() {
                        return Err(format!(
                            "audio core rejected route generation {desired_generation} for {route_id}: {error}"
                        ));
                    }
                }
                last_error = None;
            }
            Err(error) => last_error = Some(error),
        }
        thread::sleep(delay);
        delay = (delay * 2).min(EFFECT_CORE_RETRY_MAX);
    }
    Err(format!(
        "audio core route acknowledgement timed out after {} ms for route_id={} desired_generation={} submitted_route_generation={} applied_route_generation={} resolved_control_socket={} last_error={}",
        timeout.as_millis(),
        route_id,
        desired_generation,
        last_submitted,
        last_applied,
        socket_path.display(),
        last_error.as_deref().unwrap_or("none"),
    ))
}

fn wait_for_effect_generation_ack(
    socket_path: &Path,
    route_id: &str,
    desired_generation: u64,
    timeout: Duration,
) -> Result<AudioCoreDiagnosticsResponse, String> {
    let started = Instant::now();
    let mut delay = EFFECT_CORE_RETRY_MIN;
    let mut last_acknowledged = 0;
    let mut last_submitted = 0;
    let mut last_error = None;
    while started.elapsed() < timeout {
        match query_audio_core_diagnostics(socket_path, route_id) {
            Ok(response) => {
                last_acknowledged = response.acknowledged_generation;
                last_submitted = response.submitted_generation;
                last_error = None;
                if response.acknowledged_generation == desired_generation {
                    return Ok(response);
                }
                if response.acknowledged_generation > desired_generation {
                    return Err(format!(
                        "generation {desired_generation} was superseded by acknowledged generation {}",
                        response.acknowledged_generation
                    ));
                }
            }
            Err(error) => last_error = Some(error),
        }
        thread::sleep(delay);
        delay = (delay * 2).min(EFFECT_CORE_RETRY_MAX);
    }
    Err(format!(
        "audio core acknowledgement timed out after {} ms for route_id={} desired_generation={} submitted_generation={} acknowledged_generation={} resolved_control_socket={} last_error={}",
        timeout.as_millis(),
        route_id,
        desired_generation,
        last_submitted,
        last_acknowledged,
        socket_path.display(),
        last_error.as_deref().unwrap_or("none"),
    ))
}

#[cfg(not(unix))]
fn query_audio_core_diagnostics(
    _socket_path: &Path,
    _route_id: &str,
) -> Result<AudioCoreDiagnosticsResponse, String> {
    Err("WaveLinux audio-core diagnostics require Unix sockets".into())
}

#[cfg(unix)]
fn send_core_control_request(
    socket_path: &Path,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    if !socket_path.exists() {
        return Err(format!(
            "control socket is missing: {}",
            socket_path.display()
        ));
    }
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|err| format!("could not connect to {}: {err}", socket_path.display()))?;
    let timeout = Some(Duration::from_secs(1));
    stream
        .set_read_timeout(timeout)
        .map_err(|err| err.to_string())?;
    stream
        .set_write_timeout(timeout)
        .map_err(|err| err.to_string())?;
    stream
        .write_all(payload.to_string().as_bytes())
        .map_err(|err| err.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|err| err.to_string())?;
    let mut response = String::new();
    stream
        .take(1024 * 1024)
        .read_to_string(&mut response)
        .map_err(|err| err.to_string())?;
    let response: serde_json::Value =
        serde_json::from_str(&response).map_err(|err| format!("invalid core response: {err}"))?;
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(response
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("audio core rejected the request")
            .to_string());
    }
    Ok(response)
}

fn content_revision(content: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in content.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn audio_core_channel_processing_revision(config: &wavelinux_dsp::DspChannelConfig) -> String {
    let mut processing = config.clone();
    processing.input_target_node_name = None;
    content_revision(&serde_json::to_string(&processing).unwrap_or_default())
}

fn audio_core_channel_revision_from_path(path: &Path) -> Result<String, String> {
    let config: wavelinux_dsp::DspChannelConfig =
        read_json(path).map_err(|error| error.to_string())?;
    Ok(audio_core_channel_processing_revision(&config))
}

fn audio_core_topology_revision(manifest_path: &Path) -> Result<String, String> {
    let raw = fs::read_to_string(manifest_path).map_err(|err| err.to_string())?;
    let manifest: wavelinux_dsp::DspCoreManifest =
        serde_json::from_str(&raw).map_err(|err| err.to_string())?;
    Ok(wavelinux_dsp::core_topology_revision(&manifest))
}

fn effect_chain_launch_command(
    channel: &Channel,
    config_path: &Path,
    runtime: wavelinux_dsp::AudioRuntimeMode,
    dsp_bridge_allowed: bool,
) -> (String, Vec<String>) {
    let config = config_path.to_string_lossy().to_string();
    if dsp_bridge_allowed
        && (matches!(
            runtime,
            wavelinux_dsp::AudioRuntimeMode::DspCpu
                | wavelinux_dsp::AudioRuntimeMode::DspAuto
                | wavelinux_dsp::AudioRuntimeMode::DspAccelerated
        ) || channel_uses_adaptive_latency_bridge(channel))
    {
        if runtime == wavelinux_dsp::AudioRuntimeMode::DspCpu
            && !channel_uses_adaptive_latency_bridge(channel)
            && channel
                .effects
                .iter()
                .filter(|effect| !effect.bypassed)
                .all(|effect| wavelinux_dsp::native_dsp_effect_supported(&effect.effect_id))
        {
            return (
                dsp_helper_program(),
                vec![
                    "--run-native".into(),
                    "--config".into(),
                    config_path
                        .with_extension("json")
                        .to_string_lossy()
                        .to_string(),
                ],
            );
        }
        let mut args = vec![
            "--run-filter-chain".into(),
            "--channel-id".into(),
            channel.id.clone(),
            "--config".into(),
            config,
        ];
        if channel_uses_adaptive_latency_bridge(channel) {
            args.push("--adaptive-bridge-config".into());
            args.push(
                config_path
                    .with_extension("bridge.json")
                    .to_string_lossy()
                    .to_string(),
            );
        }
        return (dsp_helper_program(), args);
    }
    ("pipewire".into(), vec!["-c".into(), config])
}

fn dsp_helper_program() -> String {
    std::env::var(DSP_HELPER_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "wavelinux6-audio-core".into())
}

fn import_wavelinux5_config_for_wavelinux6(
    paths: &EnginePaths,
) -> Result<Option<MixerConfig>, EngineError> {
    if std::env::var("WAVELINUX_XDG_APP_NAME").ok().as_deref() != Some("WaveLinux6") {
        return Ok(None);
    }
    let Some(config_dir) = wavelinux5_config_dir() else {
        return Ok(None);
    };
    let source = config_dir.join("config.json");
    if source == paths.config_file() || !source.is_file() {
        return Ok(None);
    }
    let mut config: MixerConfig = read_json(&source)?;
    apply_graph_namespace(&mut config);
    config = config.normalized()?;
    write_json(&paths.config_file(), &config)?;
    fs::write(
        paths.wavelinux5_migration_marker(),
        source.to_string_lossy().as_bytes(),
    )?;
    Ok(Some(config))
}

fn wavelinux5_config_dir() -> Option<PathBuf> {
    ProjectDirs::from("io.github", "DuskyProjects", "WaveLinux5")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

fn backup_invalid_config(path: &Path) {
    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let backup = path.with_file_name(format!("config.invalid.{timestamp}.json"));
    let _ = fs::rename(path, backup);
}

fn load_adaptive_quantum_floors(path: &Path) -> Result<BTreeMap<String, u32>, EngineError> {
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let cache: AdaptiveQuantumFloorCache = read_json(path)?;
    if cache.version != ADAPTIVE_QUANTUM_FLOORS_VERSION {
        return Err(EngineError::Json(format!(
            "unsupported adaptive quantum cache version {}",
            cache.version
        )));
    }
    Ok(cache
        .floors
        .into_iter()
        .filter(|(signature, floor)| {
            !signature.trim().is_empty()
                && signature != "<no-monitor-output>"
                && *floor >= 64
                && *floor <= 8192
                && floor.is_power_of_two()
        })
        .collect())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, EngineError> {
    let data = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&data)?)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), EngineError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(value)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let tmp_path = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<(), EngineError> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp_path, path)?;
        if let Some(parent) = path.parent() {
            if let Ok(directory) = fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    write_result?;
    Ok(())
}

fn render_autostart_desktop_entry() -> String {
    let app_name = app_display_name();
    let icon = graph_prefix();
    let startup_wm_class = if graph_prefix() == "wavelinux6" {
        "io.github.duskyprojects.WaveLinux6"
    } else {
        "io.github.duskyprojects.WaveLinux"
    };
    format!(
        "[Desktop Entry]\nType=Application\nName={app_name}\nComment=Linux creator audio mixer\nExec={}\nIcon={icon}\nTerminal=false\nCategories=Audio;AudioVideo;Mixer;\nStartupWMClass={startup_wm_class}\nX-GNOME-Autostart-enabled=true\n",
        desktop_quote(&installed_binary_path()),
    )
}

fn installed_binary_path() -> PathBuf {
    let binary_name = graph_prefix();
    if let Some(bin_home) = std::env::var_os("XDG_BIN_HOME") {
        return PathBuf::from(bin_home).join(&binary_name);
    }
    if let Some(base_dirs) = BaseDirs::new() {
        return base_dirs.home_dir().join(".local/bin").join(&binary_name);
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from(binary_name))
}

fn desktop_quote(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if raw
        .chars()
        .any(|ch| ch.is_whitespace() || ch == '"' || ch == '\\')
    {
        format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        raw.into_owned()
    }
}

pub fn route_stream_to_configured_channel(
    config: &MixerConfig,
    stream: &AppStream,
) -> Option<Channel> {
    if app_stream_is_transient_event(stream) {
        return None;
    }
    let stream_matchers = stream_matchers_for_config(config, stream);
    if let Some(matched) = config
        .app_routes
        .iter()
        .filter(|route| {
            stream_matchers
                .iter()
                .any(|matcher| app_matcher_matches_matcher(&route.matcher, matcher))
                || app_matcher_matches_stream(&route.matcher, stream)
        })
        .max_by_key(|route| app_matcher_specificity(&route.matcher))
    {
        return config
            .channels
            .iter()
            .find(|channel| channel.id == matched.channel_id)
            .cloned();
    }

    let implicit_channel_id = implicit_channel_id_for_stream(stream)?;
    config
        .channels
        .iter()
        .find(|channel| channel.id == implicit_channel_id)
        .cloned()
}

fn fast_routable_streams_for_graph(config: &MixerConfig, graph: &RuntimeGraph) -> Vec<AppStream> {
    let output_names = graph
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();
    graph
        .app_streams
        .iter()
        .filter(|stream| {
            route_stream_to_configured_channel(config, stream)
                .is_some_and(|channel| output_names.contains(channel.virtual_sink_name.as_str()))
        })
        .cloned()
        .collect()
}

fn runtime_route_resnapshot_needed(
    fast_routed_streams: bool,
    rescued_streams: bool,
    routed_streams: bool,
    updated_volumes: bool,
    moved_capture_streams: bool,
) -> bool {
    fast_routed_streams
        || rescued_streams
        || routed_streams
        || updated_volumes
        || moved_capture_streams
}

fn implicit_channel_id_for_stream(stream: &AppStream) -> Option<&'static str> {
    if stream_identity_contains_any(stream, SYSTEM_APP_TOKENS) {
        return Some("system");
    }
    if stream_identity_contains_any(stream, CHAT_APP_TOKENS) {
        return Some("chat");
    }
    if stream_identity_contains_any(stream, GAME_APP_TOKENS) {
        return Some("game");
    }
    if stream_identity_contains_any(stream, MUSIC_APP_TOKENS) {
        return Some("music");
    }
    if stream_identity_contains_any(stream, BROWSER_APP_TOKENS) {
        return Some("browser");
    }
    if stream_identity_contains_any(stream, WEB_WRAPPER_APP_TOKENS) {
        return Some("browser");
    }
    None
}

const SYSTEM_APP_TOKENS: &[&str] = &["plasmashell", "systemsettings", "wireplumber", "pipewire"];

const CHAT_APP_TOKENS: &[&str] = &[
    "discord",
    "vesktop",
    "webcord",
    "slack",
    "teams",
    "zoom",
    "skype",
    "telegram",
    "signal",
    "element",
    "mattermost",
    "whatsapp",
    "messenger",
    "revolt",
    "guilded",
    "ferdium",
    "franz",
    "rambox",
];

const GAME_APP_TOKENS: &[&str] = &[
    "steam",
    "steam_app",
    "proton",
    "wine",
    "wine64",
    "lutris",
    "heroic",
    "bottles",
    "gamescope",
    "minecraft",
    "warframe",
];

const MUSIC_APP_TOKENS: &[&str] = &[
    "spotify",
    "spotifyd",
    "vlc",
    "audacious",
    "rhythmbox",
    "clementine",
    "strawberry",
    "quodlibet",
    "deadbeef",
    "foobar",
    "tidal",
    "plexamp",
    "mpv",
    "celluloid",
];

const BROWSER_APP_TOKENS: &[&str] = &[
    "brave",
    "firefox",
    "librewolf",
    "floorp",
    "waterfox",
    "zen-browser",
    "chromium",
    "chrome",
    "google-chrome",
    "vivaldi",
    "opera",
    "microsoft-edge",
    "msedge",
    "qutebrowser",
    "thorium",
];

const WEB_WRAPPER_APP_TOKENS: &[&str] = &["electron", "webapp", "web-app"];

fn stream_identity_contains_any(stream: &AppStream, tokens: &[&str]) -> bool {
    stream_identity_values(stream).any(|value| {
        let value = value.to_ascii_lowercase();
        tokens.iter().any(|token| value.contains(token))
    })
}

fn app_stream_is_transient_event(stream: &AppStream) -> bool {
    stream_identity_values(stream).any(|value| value.to_ascii_lowercase().contains("wavelinux"))
        || stream_identity_contains_any(
            stream,
            &["libcanberra", "canberra-gtk-play", "desktop event sound"],
        )
}

fn stream_identity_values(stream: &AppStream) -> impl Iterator<Item = &str> {
    [
        stream.app_id.as_deref(),
        stream.binary.as_deref(),
        stream.process_name.as_deref(),
        stream.window_class.as_deref(),
        Some(stream.display_name.as_str()),
        stream.media_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.trim().is_empty())
}

fn configured_volume_for_stream(config: &MixerConfig, stream: &AppStream) -> Option<f32> {
    if app_stream_is_transient_event(stream) {
        return None;
    }
    let stream_matchers = stream_matchers_for_config(config, stream);
    config
        .app_volume_presets
        .iter()
        .filter(|preset| {
            stream_matchers
                .iter()
                .any(|matcher| app_matcher_matches_matcher(&preset.matcher, matcher))
                || app_matcher_matches_stream(&preset.matcher, stream)
        })
        .max_by_key(|preset| app_matcher_specificity(&preset.matcher))
        .map(|preset| preset.volume)
}

fn configured_volume_update_for_stream(config: &MixerConfig, stream: &AppStream) -> Option<f32> {
    let volume = configured_volume_for_stream(config, stream)?;
    ((stream.volume - volume).abs() > 0.01).then_some(volume)
}

fn stream_matchers_for_config(config: &MixerConfig, stream: &AppStream) -> Vec<AppMatcher> {
    let Some(raw) = AppMatcher::from_stream(stream) else {
        return Vec::new();
    };
    let resolved = config.resolve_app_matcher(&raw);
    if resolved == raw {
        vec![raw]
    } else {
        vec![raw, resolved]
    }
}

fn app_matcher_matches_matcher(pattern: &AppMatcher, candidate: &AppMatcher) -> bool {
    matcher_field_matches(&pattern.app_id, candidate.app_id.as_deref())
        && matcher_field_matches(&pattern.process_name, candidate.process_name.as_deref())
        && matcher_field_matches(
            &pattern.binary,
            candidate
                .binary
                .as_deref()
                .or(candidate.process_name.as_deref()),
        )
        && matcher_field_matches(&pattern.window_class, candidate.window_class.as_deref())
        && matcher_media_name_matches(pattern, candidate.media_name.as_deref())
}

fn app_matcher_matches_stream(matcher: &AppMatcher, stream: &AppStream) -> bool {
    matcher_field_matches(&matcher.app_id, stream.app_id.as_deref())
        && matcher_field_matches(&matcher.process_name, stream.process_name.as_deref())
        && matcher_field_matches(
            &matcher.binary,
            stream.binary.as_deref().or(stream.process_name.as_deref()),
        )
        && matcher_field_matches(&matcher.window_class, stream.window_class.as_deref())
        && matcher_media_name_matches(matcher, stream.media_name.as_deref())
}

fn app_matcher_specificity(matcher: &AppMatcher) -> usize {
    [
        matcher.app_id.as_deref(),
        matcher.process_name.as_deref(),
        matcher.binary.as_deref(),
        matcher.window_class.as_deref(),
        matcher.media_name.as_deref(),
    ]
    .into_iter()
    .filter(|value| value.is_some_and(|value| !value.trim().is_empty()))
    .count()
}

fn matcher_field_matches(matcher: &Option<String>, value: Option<&str>) -> bool {
    let Some(matcher) = matcher.as_deref() else {
        return true;
    };
    if matcher.trim().is_empty() {
        return true;
    }
    let Some(value) = value else {
        return false;
    };
    matcher.eq_ignore_ascii_case(value)
}

fn matcher_media_name_matches(matcher: &AppMatcher, value: Option<&str>) -> bool {
    if !matcher_requires_media_name(matcher) {
        return true;
    }
    matcher_field_matches(&matcher.media_name, value)
}

fn matcher_requires_media_name(matcher: &AppMatcher) -> bool {
    let Some(media_name) = matcher.media_name.as_deref() else {
        return false;
    };
    if media_name.trim().is_empty() {
        return false;
    }

    let identity_values = [
        matcher.app_id.as_deref(),
        matcher.binary.as_deref(),
        matcher.process_name.as_deref(),
        matcher.window_class.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.trim().is_empty())
    .map(str::to_ascii_lowercase)
    .collect::<Vec<_>>();

    if identity_values.is_empty() {
        return true;
    }

    identity_values.iter().any(|value| {
        [
            "ferdium", "electron", "chromium", "chrome", "brave", "vivaldi", "webapp", "web-app",
        ]
        .iter()
        .any(|needle| value.contains(needle))
    })
}

fn graph_diagnostics(config: &MixerConfig, graph: &RuntimeGraph) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    if !graph_has_wavelinux_nodes(graph) {
        diagnostics.push(Diagnostic {
            code: "graph.stopped".into(),
            severity: DiagnosticSeverity::Info,
            message: "WaveLinux audio graph is stopped".into(),
            action: Some("Quit and reopen WaveLinux to recreate virtual devices".into()),
        });
        return diagnostics;
    }

    if !meter_sampling_enabled() {
        diagnostics.push(Diagnostic {
            code: "meters.unavailable".into(),
            severity: DiagnosticSeverity::Info,
            message: "PipeWire VU meter supervisor is unavailable".into(),
            action: Some(
                "Install PipeWire host tools or unset WAVELINUX_DISABLE_METERS to show live fader meters"
                    .into(),
            ),
        });
    }

    for mix in &config.mixes {
        if !mix_uses_persistent_audio_core(mix)
            && !graph
                .outputs
                .iter()
                .any(|output| output.name == mix.virtual_sink_name)
        {
            diagnostics.push(Diagnostic {
                code: format!("graph.mix_sink.{}", mix.id),
                severity: DiagnosticSeverity::Error,
                message: format!("{} mix sink is missing", mix.name),
                action: Some("Run Repair to recreate the virtual mix sink".into()),
            });
        }
        if !graph
            .inputs
            .iter()
            .any(|input| input.name == mix.virtual_source_name)
        {
            diagnostics.push(Diagnostic {
                code: format!("graph.mix_source.{}", mix.id),
                severity: DiagnosticSeverity::Error,
                message: format!("{} virtual source is missing", mix.name),
                action: Some("Run Repair so apps can select this mix as an input".into()),
            });
        }
        if meter_sampling_enabled() && !graph.meters.iter().any(|meter| meter.node_id == mix.id) {
            diagnostics.push(Diagnostic {
                code: format!("graph.mix_meter.{}", mix.id),
                severity: DiagnosticSeverity::Warning,
                message: format!("{} has no live meter sample yet", mix.name),
                action: Some("Play audio through the mix or run Repair if it stays silent".into()),
            });
        }
    }

    for channel in &config.channels {
        let channel_sink = graph
            .outputs
            .iter()
            .find(|output| output.name == channel.virtual_sink_name);
        if let Some(channel_sink) = channel_sink {
            let current_revision = if channel_uses_persistent_audio_core(channel) {
                effect_node_has_current_config_revision(channel_sink)
            } else {
                channel_sink
                    .pipewire_properties
                    .get(&graph_prop("channel_config_revision"))
                    .map(String::as_str)
                    == Some(CHANNEL_CONFIG_REVISION)
            };
            if !current_revision {
                diagnostics.push(Diagnostic {
                    code: format!("graph.channel_revision.{}", channel.id),
                    severity: DiagnosticSeverity::Error,
                    message: format!(
                        "{} channel sink was created by an older WaveLinux graph config",
                        channel.name
                    ),
                    action: Some("Run Repair to recreate the virtual channel sink".into()),
                });
            }
        } else {
            diagnostics.push(Diagnostic {
                code: format!("graph.channel_sink.{}", channel.id),
                severity: DiagnosticSeverity::Error,
                message: format!("{} channel sink is missing", channel.name),
                action: Some("Run Repair to recreate the virtual channel sink".into()),
            });
        }
        if channel_has_active_effects(channel) {
            let effect_source_name = effect_chain_source_name(channel);
            let effect_source = graph
                .inputs
                .iter()
                .find(|input| input.name == effect_source_name);
            if let Some(effect_source) = effect_source {
                if !effect_node_has_current_config_revision(effect_source) {
                    diagnostics.push(Diagnostic {
                        code: format!("graph.effect_source_revision.{}", channel.id),
                        severity: DiagnosticSeverity::Warning,
                        message: format!(
                            "{} FX output was created by an older WaveLinux effect config",
                            channel.name
                        ),
                        action: Some("Run Repair to restart the channel effect chain".into()),
                    });
                }
            } else {
                diagnostics.push(Diagnostic {
                    code: format!("graph.effect_source.{}", channel.id),
                    severity: DiagnosticSeverity::Warning,
                    message: format!("{} FX output is not visible yet", channel.name),
                    action: Some("Run Repair to restart the channel effect chain".into()),
                });
            }

            let effect_input_name = effect_chain_input_name(channel);
            let effect_input = graph
                .outputs
                .iter()
                .find(|output| output.name == effect_input_name);
            if let Some(effect_input) = effect_input {
                if !effect_node_has_current_config_revision(effect_input) {
                    diagnostics.push(Diagnostic {
                        code: format!("graph.effect_input_revision.{}", channel.id),
                        severity: DiagnosticSeverity::Warning,
                        message: format!(
                            "{} FX input was created by an older WaveLinux effect config",
                            channel.name
                        ),
                        action: Some("Run Repair to restart the channel effect chain".into()),
                    });
                }
            } else {
                diagnostics.push(Diagnostic {
                    code: format!("graph.effect_input.{}", channel.id),
                    severity: DiagnosticSeverity::Warning,
                    message: format!("{} FX input is not visible yet", channel.name),
                    action: Some("Run Repair to restart the channel effect chain".into()),
                });
            }
        }
    }

    diagnostics.extend(latency_diagnostics(config));

    diagnostics
}

fn route_diagnostics(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    if !graph_has_wavelinux_nodes(graph) {
        return diagnostics;
    }

    let output_names = graph
        .outputs
        .iter()
        .map(|output| output.name.as_str())
        .collect::<BTreeSet<_>>();

    for channel in &config.channels {
        if !output_names.contains(channel.virtual_sink_name.as_str()) {
            continue;
        }

        let raw_source_name = format!("{}.monitor", channel.virtual_sink_name);
        let mut mix_source_name = channel_mix_source_name(channel);

        if channel_has_active_effects(channel) {
            let effect_source_name = effect_chain_source_name(channel);
            let effect_input_name = effect_chain_input_name(channel);

            if effect_chain_endpoint_readiness_for_graph(graph, channel).ready() {
                if !channel_uses_persistent_audio_core(channel)
                    && !managed_modules.iter().any(|module| {
                        managed_module_matches_route(
                            module,
                            "channel_to_effect",
                            Some(&channel.id),
                            None,
                            &raw_source_name,
                            &effect_input_name,
                            &effect_route_revision(&config.settings, channel),
                        )
                    })
                {
                    diagnostics.push(Diagnostic {
                        code: format!("graph.route_effect.{}", channel.id),
                        severity: DiagnosticSeverity::Warning,
                        message: format!("{} FX input route is missing", channel.name),
                        action: Some(
                            "Run Repair to reconnect the channel into its FX chain".into(),
                        ),
                    });
                }
                if channel_uses_adaptive_latency_bridge(channel)
                    && !managed_modules.iter().any(|module| {
                        managed_module_matches_route(
                            module,
                            "effect_to_adaptive_bridge",
                            Some(&channel.id),
                            None,
                            &effect_chain_filter_output_name(channel),
                            &effect_chain_adaptive_bridge_input_name(channel),
                            EFFECT_ADAPTIVE_BRIDGE_ROUTE_REVISION,
                        )
                    })
                {
                    diagnostics.push(Diagnostic {
                        code: format!("graph.route_adaptive_bridge.{}", channel.id),
                        severity: DiagnosticSeverity::Warning,
                        message: format!("{} adaptive mic bridge route is missing", channel.name),
                        action: Some(
                            "Run Repair to reconnect the processed mic into the adaptive bridge"
                                .into(),
                        ),
                    });
                }
                mix_source_name = effect_source_name;
            } else {
                mix_source_name = raw_source_name;
            }
        }

        for mix in config.mixes.iter().filter(|mix| {
            channel
                .mix_buses
                .get(&mix.id)
                .is_some_and(|bus| bus.enabled)
                && !channel_mix_route_uses_hardware_direct_monitoring(
                    channel,
                    mix,
                    &config.settings,
                )
        }) {
            if !output_names.contains(mix.virtual_sink_name.as_str()) {
                continue;
            }
            if managed_modules.iter().any(|module| {
                managed_module_matches_route(
                    module,
                    "channel_to_mix",
                    Some(&channel.id),
                    Some(&mix.id),
                    &mix_source_name,
                    &mix.virtual_sink_name,
                    &channel_mix_route_revision(&config.settings, channel, mix),
                )
            }) {
                continue;
            }

            diagnostics.push(Diagnostic {
                code: format!("graph.route_mix.{}.{}", channel.id, mix.id),
                severity: DiagnosticSeverity::Warning,
                message: format!("{} is not routed into the {} mix", channel.name, mix.name),
                action: Some("Run Repair to restore the missing audio route".into()),
            });
        }
    }

    diagnostics
}

fn managed_module_matches_route(
    module: &ManagedModule,
    role: &str,
    channel_id: Option<&str>,
    mix_id: Option<&str>,
    source_name: &str,
    sink_name: &str,
    route_revision: &str,
) -> bool {
    module.role.as_deref() == Some(role)
        && module.channel_id.as_deref() == channel_id
        && module.mix_id.as_deref() == mix_id
        && module.route_revision.as_deref() == Some(route_revision)
        && module
            .source_name
            .as_deref()
            .is_some_and(|source| audio_endpoint_names_match(source, source_name))
        && module
            .sink_name
            .as_deref()
            .is_some_and(|sink| audio_endpoint_names_match(sink, sink_name))
}

fn latency_diagnostics(config: &MixerConfig) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let heavy_effects = config
        .channels
        .iter()
        .flat_map(|channel| {
            channel
                .effects
                .iter()
                .filter(|effect| !effect.bypassed)
                .map(move |effect| (channel, effect.effect_id.as_str()))
        })
        .filter(|(_, effect_id)| matches!(*effect_id, "rnnoise" | "convolver"))
        .collect::<Vec<_>>();

    if let Ok(latency) = std::env::var("PIPEWIRE_LATENCY") {
        let latency = latency.trim();
        if !latency.is_empty() {
            diagnostics.push(Diagnostic {
                code: "latency.pipewire_env".into(),
                severity: DiagnosticSeverity::Info,
                message: format!("PIPEWIRE_LATENCY is set to {latency}"),
                action: Some(
                    "Use this with your PipeWire quantum/buffer settings when lining up OBS sync"
                        .into(),
                ),
            });
        }
    }

    diagnostics.push(Diagnostic {
        code: "latency.graph_target".into(),
        severity: DiagnosticSeverity::Info,
        message: "WaveLinux graph loopbacks target 10 ms per hop".into(),
        action: Some(
            "Typical mic-to-mix paths are roughly 20-30 ms before host/device buffering and heavy FX"
                .into(),
        ),
    });

    if !heavy_effects.is_empty() {
        let channels = heavy_effects
            .iter()
            .map(|(channel, effect_id)| format!("{}:{effect_id}", channel.name))
            .collect::<Vec<_>>()
            .join(", ");
        diagnostics.push(Diagnostic {
            code: "latency.heavy_effects".into(),
            severity: DiagnosticSeverity::Warning,
            message: "Heavy noise suppression can add monitoring latency".into(),
            action: Some(format!(
                "Review these active FX before low-latency monitoring: {channels}"
            )),
        });
    }

    diagnostics
}

fn pipewire_audio_health_diagnostics(health: &PipeWireAudioHealthStatus) -> Vec<Diagnostic> {
    if health.warning_events == 0 {
        return Vec::new();
    }

    let code = if health.owned_events > 0 {
        "pipewire.audio_health.wavelinux_owned_buffer_resync"
    } else {
        "pipewire.audio_health.recent_buffer_resync"
    };

    vec![Diagnostic {
        code: code.into(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "PipeWire reported {} audio warning events this session ({} direct profiler error, {} WaveLinux-owned direct error, {} out-of-buffer, {} resync, {} link activation failure, {} xrun, {} WaveLinux-owned event)",
            health.warning_events,
            health.direct_errors,
            health.owned_direct_errors,
            health.out_of_buffers,
            health.resyncs,
            health.link_failures,
            health.xruns,
            health.owned_events,
        ),
        action: Some(
            "Let WaveLinux repair stale routes first; if this continues during normal playback, reconnect the affected Bluetooth device or use a stable hardware profile"
                .into(),
        ),
    }]
}

fn audio_core_integrity_diagnostics(statuses: &[AudioCoreChannelStatus]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for status in statuses {
        if !status.online {
            if let Some(error) = status.error.as_deref() {
                diagnostics.push(Diagnostic {
                    code: format!("audio_core.offline.{}", status.channel_id),
                    severity: DiagnosticSeverity::Warning,
                    message: format!("Audio core endpoint {} is offline", status.channel_id),
                    action: Some(format!(
                        "Repair the audio graph if the endpoint does not recover: {error}"
                    )),
                });
            }
            continue;
        }

        if status.non_finite_blocks > 0
            || status.non_finite_samples > 0
            || status.chain_recoveries > 0
        {
            diagnostics.push(Diagnostic {
                code: format!("audio_core.non_finite.{}", status.channel_id),
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "Audio core endpoint {} contained invalid DSP output (blocks={}, samples={}, effect_mask=0x{:x}, recoveries={})",
                    status.channel_id,
                    status.non_finite_blocks,
                    status.non_finite_samples,
                    status.non_finite_effect_mask,
                    status.chain_recoveries,
                ),
                action: Some(
                    "The dry signal was preserved automatically. Disable the affected effect and inspect the Audio Core counters if they continue increasing."
                        .into(),
                ),
            });
        }

        if status.retired_chain_overflows > 0 {
            diagnostics.push(Diagnostic {
                code: format!("audio_core.retired_chain_overflow.{}", status.channel_id),
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "Audio core endpoint {} exhausted its retired-chain queue {} times",
                    status.channel_id, status.retired_chain_overflows
                ),
                action: Some(
                    "Pause rapid effect topology changes and inspect Audio Core processing latency."
                        .into(),
                ),
            });
        }

        if !status.accelerator_startup_failures.is_empty()
            || status.accelerator_fallback_blocks > 0
            || status.accelerator_disabled_states > 0
        {
            diagnostics.push(Diagnostic {
                code: format!("audio_core.accelerator_fallback.{}", status.channel_id),
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "Audio core endpoint {} used the exact CPU neural fallback (provider={}, startup_failures={}, fallback_blocks={}, deadline_misses={}, invalid_results={}, disabled_states={})",
                    status.channel_id,
                    status.accelerator_provider.as_deref().unwrap_or("unknown"),
                    status.accelerator_startup_failures.len(),
                    status.accelerator_fallback_blocks,
                    status.accelerator_deadline_misses,
                    status.accelerator_invalid_results,
                    status.accelerator_disabled_states,
                ),
                action: Some(
                    "Audio continuity was preserved. Requalify the provider pack on this machine or select the CPU provider if fallback counters continue increasing."
                        .into(),
                ),
            });
        }

        if let Some(error) = status.route_target_error.as_deref() {
            diagnostics.push(Diagnostic {
                code: format!("audio_core.route_target.{}", status.channel_id),
                severity: DiagnosticSeverity::Warning,
                message: format!(
                    "Audio core endpoint {} could not apply its latest hardware target",
                    status.channel_id
                ),
                action: Some(format!(
                    "The previous endpoint remains active when possible. Check device availability: {error}"
                )),
            });
        }
    }
    diagnostics
}

fn dsp_runtime_diagnostics() -> Vec<Diagnostic> {
    let dsp_requested = std::env::var_os(wavelinux_dsp::AUDIO_RUNTIME_ENV).is_some()
        || std::env::var_os(wavelinux_dsp::DSP_PROVIDER_ENV).is_some()
        || graph_prefix() == "wavelinux6";
    if !dsp_requested {
        return Vec::new();
    }

    let status = wavelinux_dsp::probe_backend_from_env();
    let selected_provider = status
        .selected_provider
        .map(|provider| provider.as_str())
        .unwrap_or("pipewire_filter_chain");
    let mut diagnostics = Vec::new();
    diagnostics.push(Diagnostic {
        code: "dsp.runtime".into(),
        severity: if status.fallback_active {
            DiagnosticSeverity::Warning
        } else {
            DiagnosticSeverity::Info
        },
        message: format!(
            "DSP requested_runtime={} effective_runtime={} requested_provider={} selected_provider={} accelerated={} fallback_count={}",
            status.runtime.as_str(),
            status.effective_runtime.as_str(),
            status.requested_provider.as_str(),
            selected_provider,
            status.accelerated,
            status.fallback_count
        ),
        action: status.fallback_active.then(|| {
            "Use WAVELINUX_DSP_PROVIDER=cpu, or install a qualified WaveLinux accelerator provider pack when one is available.".into()
        }),
    });

    if let Some(reason) = &status.runtime_fallback_reason {
        diagnostics.push(Diagnostic {
            code: "dsp.runtime_fallback".into(),
            severity: DiagnosticSeverity::Warning,
            message: reason.clone(),
            action: Some(
                "Use WAVELINUX_AUDIO_RUNTIME=dsp_cpu for the native CPU path or pipewire_filter_chain for compatibility rollback."
                    .into(),
            ),
        });
    }

    if !status.provider_probe_failures.is_empty()
        && status.runtime != wavelinux_dsp::AudioRuntimeMode::PipewireFilterChain
    {
        diagnostics.push(Diagnostic {
            code: "dsp.provider_probe".into(),
            severity: DiagnosticSeverity::Info,
            message: format!(
                "DSP provider probes: {}",
                status.provider_probe_failures.join("; ")
            ),
            action: Some(
                "Use WAVELINUX_DSP_PROVIDER=cpu unless an accelerator provider pack has passed workload qualification."
                    .into(),
            ),
        });
    }

    diagnostics
}

fn host_command(program: &str) -> Command {
    let mut command = Command::new(program);
    sanitize_host_command_env(&mut command);
    command
}

fn spawn_pipewire_profiler_monitor() -> io::Result<(Child, ChildStdout)> {
    let mut child = host_command("pw-top")
        .arg("--batch-mode")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(io::Error::other(
            "pw-top profiler did not provide standard output",
        ));
    };
    Ok((child, stdout))
}

fn parse_audio_subscription_event(line: &str) -> Option<AudioSubscriptionEvent> {
    let line = line.trim().to_ascii_lowercase();
    if !line.starts_with("event '") {
        return None;
    }
    if line.contains(" on sink-input ") {
        return Some(AudioSubscriptionEvent::PlaybackStream);
    }
    // Source-output changes describe recording clients, including WaveLinux's
    // display-only meter streams. They do not change hardware, defaults, or
    // desired routes and must not trigger an expensive device/profile refresh.
    if line.contains(" on source-output ") {
        return None;
    }
    [" on sink ", " on source ", " on card ", " on server "]
        .iter()
        .any(|object| line.contains(object))
        .then_some(AudioSubscriptionEvent::Device)
}

#[cfg(test)]
fn audio_subscription_event_relevant(line: &str) -> bool {
    parse_audio_subscription_event(line).is_some()
}

fn coalesce_audio_subscription_events(
    initial: AudioSubscriptionEvent,
    events: &mpsc::Receiver<AudioSubscriptionEvent>,
) -> AudioSubscriptionEvent {
    events.try_iter().fold(initial, std::cmp::max)
}

fn terminate_effect_chain_child(
    program: &str,
    child: &mut Child,
    grace: Duration,
) -> io::Result<std::process::ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }

    if !is_dsp_helper_program(program) {
        child.kill()?;
        return child.wait();
    }

    terminate_child_pid(child, false)?;
    let start = Instant::now();
    while start.elapsed() < grace {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(25));
    }

    terminate_process_group_or_child(child)?;
    child.wait()
}

fn is_dsp_helper_program(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("wavelinux6-audio-core")
}

#[cfg(unix)]
fn terminate_child_pid(child: &mut Child, force: bool) -> io::Result<()> {
    let pid = child.id() as libc::pid_t;
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    if force {
        child.kill()
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn terminate_child_pid(child: &mut Child, force: bool) -> io::Result<()> {
    let _ = force;
    child.kill()
}

#[cfg(unix)]
fn terminate_process_group_or_child(child: &mut Child) -> io::Result<()> {
    let pid = child.id() as libc::pid_t;
    if unsafe { libc::kill(-pid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    child.kill()
}

#[cfg(not(unix))]
fn terminate_process_group_or_child(child: &mut Child) -> io::Result<()> {
    child.kill()
}

fn sanitize_host_command_env(command: &mut Command) {
    for key in HOST_COMMAND_ENV_REMOVE {
        command.env_remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;
    use tempfile::tempdir;
    use wavelinux_model::{percent_to_unit, AppMatcher, DeviceInfo};

    fn test_engine() -> Arc<WaveLinuxEngine> {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let engine = WaveLinuxEngine::new(
            paths,
            EngineOptions {
                dry_run: true,
                auto_repair_on_start: false,
                poll_interval: Duration::from_millis(50),
            },
        )
        .unwrap();
        std::mem::forget(root);
        engine
    }

    fn hardware_channel_with_effects() -> Channel {
        let mut channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        channel.effects = vec![EffectInstance::new("limiter")];
        channel.effects_enabled = true;
        channel
    }

    #[cfg(unix)]
    #[test]
    fn input_route_control_waits_for_set_and_clear_acknowledgements() {
        let root = tempdir().unwrap();
        let socket_path = root.path().join("audio-core.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = thread::spawn(move || {
            let mut generation = 1_u64;
            let mut target = Some("alsa_input.old".to_string());
            let mut route_commands = 0_usize;
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut payload = String::new();
                stream.read_to_string(&mut payload).unwrap();
                let command: serde_json::Value = serde_json::from_str(&payload).unwrap();
                let request_id = command.get("request_id").cloned().unwrap_or_default();
                let response = match command["command"].as_str().unwrap() {
                    "get_diagnostics" => serde_json::json!({
                        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
                        "ok": true,
                        "request_id": request_id,
                        "route_id": "hardware_in",
                        "submitted_route_generation": generation,
                        "applied_route_generation": generation,
                        "input_target_node_name": target,
                    }),
                    "set_input_target" | "clear_input_target" => {
                        generation = command["route_generation"].as_u64().unwrap();
                        target = command
                            .get("target_node_name")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                        route_commands += 1;
                        serde_json::json!({
                            "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
                            "ok": true,
                            "request_id": request_id,
                            "route_id": "hardware_in",
                            "route_generation": generation,
                            "target_node_name": target,
                        })
                    }
                    command => panic!("unexpected command {command}"),
                };
                stream.write_all(response.to_string().as_bytes()).unwrap();
            }
            assert_eq!(route_commands, 2);
            assert_eq!(generation, 3);
            assert!(target.is_none());
        });

        let applied =
            send_audio_core_input_target(&socket_path, "hardware_in", Some("alsa_input.usb"))
                .unwrap();
        assert_eq!(applied["route_generation"], 2);
        assert_eq!(applied["operation"], "input_target_applied");

        let cleared = send_audio_core_input_target(&socket_path, "hardware_in", None).unwrap();
        assert_eq!(cleared["route_generation"], 3);
        assert_eq!(cleared["operation"], "input_target_cleared");
        server.join().unwrap();
    }

    #[test]
    fn repair_snapshot_is_reused_only_after_skipped_preplan_commands() {
        let command = CommandSpec::new(
            CommandDomain::Graph,
            "pactl",
            ["load-module", "module-loopback"],
            "test graph mutation",
        );
        let skipped = CommandExecution {
            command: command.clone(),
            stdout: String::new(),
            stderr: String::new(),
            skipped: true,
            error: None,
        };
        let completed = CommandExecution {
            skipped: false,
            ..skipped.clone()
        };
        let failed = CommandExecution {
            command,
            skipped: false,
            error: Some("server disconnected after accepting command".into()),
            ..skipped.clone()
        };

        assert!(!command_executions_may_have_mutated_graph(&[]));
        assert!(!command_executions_may_have_mutated_graph(&[skipped]));
        assert!(command_executions_may_have_mutated_graph(&[completed]));
        assert!(command_executions_may_have_mutated_graph(&[failed]));
    }

    #[test]
    fn effect_update_state_coalesces_one_hundred_requests_and_ignores_stale_completion() {
        let mut state = EffectUpdateSlot::new(
            hardware_channel_with_effects(),
            Path::new("/run/user/1000/wavelinux6/control/wavelinux6-chain-hardware_in.sock"),
        )
        .state
        .into_inner()
        .unwrap();

        let mut first_attempt = None;
        for index in 0..100 {
            let mut channel = hardware_channel_with_effects();
            channel.effects_enabled = index % 2 == 1;
            channel.effects[0]
                .params
                .insert("strength".into(), index as f32);
            if index % 3 == 0 {
                channel.effects.push(EffectInstance::new("highpass"));
            }
            let decision = state.enqueue(channel).unwrap();
            if index == 0 {
                assert!(decision.start_worker);
                first_attempt = Some(state.begin_latest());
            } else {
                assert!(!decision.start_worker);
                assert_eq!(state.in_flight_generation, Some(2));
            }
        }

        assert_eq!(state.desired.generation, 101);
        assert_eq!(state.coalesced_requests, 99);
        assert_eq!(state.status.coalesced_requests, 99);
        assert!(state.desired.channel.effects_enabled);
        assert_eq!(state.desired.channel.effects[0].params["strength"], 99.0);

        let first_attempt = first_attempt.unwrap();
        let stale = state.finish_attempt(
            first_attempt.generation,
            &Ok(EffectApplyAcknowledgement {
                generation: first_attempt.generation,
                config_revision: "stale".into(),
                chain_swaps: 1,
            }),
        );
        assert!(stale.superseded);
        assert!(state.worker_running);
        assert_eq!(state.status.applied_generation, 0);
        assert_eq!(state.status.state, EffectRuntimeState::Red);

        let latest = state.begin_latest();
        let completed = state.finish_attempt(
            latest.generation,
            &Ok(EffectApplyAcknowledgement {
                generation: latest.generation,
                config_revision: "latest".into(),
                chain_swaps: 2,
            }),
        );
        assert!(!completed.superseded);
        assert!(!state.worker_running);
        assert_eq!(state.status.applied_generation, 101);
        assert_eq!(state.status.desired_generation, 101);
        assert_eq!(state.status.state, EffectRuntimeState::Green);
    }

    #[test]
    fn unavailable_core_finishes_red_without_discarding_selected_effects() {
        let channel = hardware_channel_with_effects();
        let mut state = EffectUpdateSlot::new(
            channel.clone(),
            Path::new("/run/user/1000/wavelinux6/control/wavelinux6-chain-hardware_in.sock"),
        )
        .state
        .into_inner()
        .unwrap();
        state.enqueue(channel).unwrap();
        let desired = state.begin_latest();

        let completed = state.finish_attempt(
            desired.generation,
            &Err("control socket is unavailable".into()),
        );

        assert!(!completed.superseded);
        assert_eq!(state.status.state, EffectRuntimeState::Red);
        assert!(!state.status.core_healthy);
        assert_eq!(state.desired.channel.effects.len(), 1);
        assert_eq!(
            state.status.last_error.as_deref(),
            Some("control socket is unavailable")
        );

        assert!(!state.reserve_recovery_worker(Instant::now()));
        state.status.core_healthy = true;
        state.status.pending = true;
        state.status.resolve_state();
        assert!(state.reserve_recovery_worker(
            Instant::now() + EFFECT_RECOVERY_RETRY_INTERVAL + Duration::from_millis(1)
        ));
        let recovered = state.begin_latest();
        assert_eq!(recovered.generation, desired.generation);
        let completed = state.finish_attempt(
            recovered.generation,
            &Ok(EffectApplyAcknowledgement {
                generation: recovered.generation,
                config_revision: "recovered".into(),
                chain_swaps: 1,
            }),
        );
        assert!(!completed.superseded);
        assert_eq!(state.status.state, EffectRuntimeState::Green);
        assert_eq!(state.status.applied_generation, desired.generation);
        assert_eq!(state.desired.channel.effects.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_microphone_control_protocol_advances_acknowledged_generation() {
        use std::os::unix::net::UnixListener;

        let root = tempdir().unwrap();
        let runtime_root = root.path().join("runtime/wavelinux6");
        let socket_path =
            wavelinux_dsp::channel_control_socket(&runtime_root, "wavelinux6", "hardware_in");
        fs::create_dir_all(socket_path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let config_path = root
            .path()
            .join("effects/wavelinux6-chain-hardware_in.json");
        let channel = hardware_channel_with_effects();
        let mut config = wavelinux_dsp::DspChannelConfig::new(
            channel.id.clone(),
            channel.name.clone(),
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_hardware_in",
            "wavelinux6-mic",
            channel.effects.clone(),
        );
        config.generation = 2;
        config.control_socket_path = Some(socket_path.to_string_lossy().into_owned());
        write_json(&config_path, &config).unwrap();
        let server_config_path = config_path.clone();

        let server = thread::spawn(move || {
            let mut submitted = 1_u64;
            let mut acknowledged = 1_u64;
            let mut chain_swaps = 0_u64;
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                stream.read_to_string(&mut request).unwrap();
                let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                let response = match request["command"].as_str().unwrap() {
                    "get_diagnostics" => serde_json::json!({
                        "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
                        "ok": true,
                        "route_id": "hardware_in",
                        "sample_rate_hz": 48_000,
                        "chain_swaps": chain_swaps,
                        "submitted_generation": submitted,
                        "acknowledged_generation": acknowledged,
                        "rate_correction": 1.0,
                    }),
                    "swap_chain" => {
                        assert_eq!(
                            request["config_path"],
                            server_config_path.to_string_lossy().as_ref()
                        );
                        let loaded: wavelinux_dsp::DspChannelConfig =
                            read_json(&server_config_path).unwrap();
                        assert_eq!(loaded.output_node_name, "wavelinux6-mic");
                        submitted = request["desired_generation"].as_u64().unwrap();
                        acknowledged = submitted;
                        chain_swaps += 1;
                        serde_json::json!({
                            "protocol_version": wavelinux_dsp::CORE_CONTROL_PROTOCOL_VERSION,
                            "ok": true,
                            "route_id": "hardware_in",
                            "graph_revision": submitted,
                        })
                    }
                    command => panic!("unexpected command {command}"),
                };
                stream.write_all(response.to_string().as_bytes()).unwrap();
                stream.write_all(b"\n").unwrap();
            }
        });

        let ready =
            wait_for_audio_core_ready(&socket_path, "hardware_in", Duration::from_millis(250))
                .unwrap();
        assert_eq!(ready.acknowledged_generation, 1);
        let queued = send_effect_chain_swap(
            &socket_path,
            "hardware_in",
            &config_path,
            "test-revision",
            2,
        )
        .unwrap();
        assert_eq!(queued["graph_revision"], 2);
        let applied = wait_for_effect_generation_ack(
            &socket_path,
            "hardware_in",
            2,
            Duration::from_millis(250),
        )
        .unwrap();
        assert_eq!(applied.acknowledged_generation, 2);
        assert_eq!(applied.chain_swaps, 1);
        server.join().unwrap();

        let obsolete_data_socket = root
            .path()
            .join("data/effects/wavelinux6-chain-hardware_in.sock");
        assert!(!obsolete_data_socket.exists());
        assert!(socket_path.starts_with(&runtime_root));
    }

    #[cfg(unix)]
    #[test]
    fn healthy_runtime_reapplies_latest_generation_without_graph_repair() {
        let engine = test_engine();
        engine.write_runtime().unwrap().status.audio_graph_running = true;
        let channel = hardware_channel_with_effects();
        let slot = engine.effect_update_slot(&channel).unwrap();
        let generation = {
            let mut state = slot.state.lock().unwrap();
            state.enqueue(channel).unwrap();
            let desired = state.begin_latest();
            state.finish_attempt(
                desired.generation,
                &Err("control socket is unavailable".into()),
            );
            state.status.core_healthy = true;
            state.status.pending = true;
            state.recovery_not_before = Some(Instant::now() - Duration::from_millis(1));
            state.status.resolve_state();
            desired.generation
        };

        engine.recover_effect_updates_if_ready();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = slot.state.lock().unwrap();
            if !state.worker_running && state.status.applied_generation == generation {
                assert_eq!(state.status.state, EffectRuntimeState::Green);
                assert_eq!(state.desired.generation, generation);
                break;
            }
            assert!(Instant::now() < deadline, "effect recovery did not finish");
            drop(state);
            thread::sleep(Duration::from_millis(10));
        }

        let log = fs::read_to_string(engine.paths.log_file()).unwrap();
        assert!(log.contains("request_recovery=true"));
        assert!(log.contains(&format!("request_acknowledged={generation}")));
        assert!(!log.contains("[repair."));
    }

    #[cfg(unix)]
    #[test]
    fn core_readiness_failure_is_bounded_and_names_the_canonical_socket() {
        let root = tempdir().unwrap();
        let socket_path = wavelinux_dsp::channel_control_socket(
            &root.path().join("runtime/wavelinux6"),
            "wavelinux6",
            "hardware_in",
        );
        let started = Instant::now();
        let error =
            wait_for_audio_core_ready(&socket_path, "hardware_in", Duration::from_millis(60))
                .unwrap_err();

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(error.contains("readiness timed out"));
        assert!(error.contains(socket_path.to_string_lossy().as_ref()));
    }

    #[cfg(unix)]
    #[test]
    fn engine_runtime_control_directory_is_private_and_ephemeral() {
        let engine = test_engine();
        let control_dir = engine.paths.control_sockets_dir();

        assert!(control_dir.starts_with(&engine.paths.runtime_dir));
        assert!(!control_dir.starts_with(&engine.paths.data_dir));
        assert_eq!(
            fs::metadata(&engine.paths.runtime_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(control_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    fn wavelinux5_config_with_effects(effects: Vec<EffectInstance>) -> MixerConfig {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux5");
        config.set_effect_chain("hardware_in", effects).unwrap();
        config
    }

    fn assert_wavelinux6_stop_script_scope(script: &str) {
        assert!(script.contains("wavelinux6-audio-core"));
        assert!(script.contains("$2 == \"module-loopback\""));
        assert!(!script.contains("(^|[/ ])wavelinux([ ]|$)"));
        assert!(!script.contains("WaveLinux_[^ ]*_amd64"));
        assert!(!script.contains(r"\/wavelinux\/effects\/wavelinux-chain-"));
    }

    #[test]
    fn install_script_process_matching_never_targets_stable_wavelinux() {
        let script = include_str!("../../../scripts/install-local.sh");
        let process_matcher = include_str!("../../../scripts/wavelinux-processes.sh");
        assert!(script.contains("stop_previous_wavelinux_processes"));
        assert!(script.contains("cleanup_previous_wavelinux_audio_modules"));
        assert!(script.contains("source \"$ROOT_DIR/scripts/wavelinux-processes.sh\""));
        assert!(process_matcher.contains("app:wavelinux6|app:WaveLinux6_*_amd64.AppImage"));
        assert!(process_matcher.contains("app-runtime:wavelinux6"));
        assert!(process_matcher
            .contains("legacy-app:wavelinux5|legacy-app:WaveLinux5_*_amd64.AppImage"));
        assert!(process_matcher.contains("wavelinux_collect_owned_filter_chain_pids"));
        assert!(process_matcher
            .contains("wavelinux_collect_owned_filter_chain_pids wavelinux6 wavelinux6-chain-"));
        assert!(process_matcher
            .contains("wavelinux_collect_owned_filter_chain_pids wavelinux5 wavelinux5-chain-"));
        assert!(!process_matcher.contains("app:wavelinux|"));
        assert!(!process_matcher.contains("*/wavelinux/effects/wavelinux-chain-*"));
        assert!(!script.contains("wavelinux(5|6)"));
        assert!(!script.contains("WaveLinux(5|6)"));
        assert!(!script.contains("wavelinux6-audio-core|wavelinux5-dsp-helper"));
        assert_wavelinux6_stop_script_scope(script);
    }

    #[test]
    fn uninstall_script_process_matching_never_targets_stable_wavelinux() {
        let script = include_str!("../../../scripts/uninstall-local.sh");
        assert!(script.contains("stop_wavelinux6_processes"));
        assert!(script.contains("cleanup_wavelinux6_audio_modules"));
        assert!(script.contains("WaveLinux6_[^ ]*_amd64"));
        assert!(script.contains(r"\/wavelinux6\/effects\/wavelinux6-chain-"));
        assert_wavelinux6_stop_script_scope(script);
    }

    #[test]
    fn dsp_auto_reports_native_cpu_when_accelerator_is_not_qualified() {
        let inputs = wavelinux_dsp::ProviderProbeInputs {
            cuda_available: false,
            cuda_detail: "provider pack unavailable".into(),
            openvino_available: false,
            openvino_detail: "provider pack unavailable".into(),
            migraphx_available: false,
            migraphx_detail: "provider pack unavailable".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let effective = wavelinux_dsp::select_provider(
            wavelinux_dsp::AudioRuntimeMode::DspAuto,
            wavelinux_dsp::DspProviderPreference::Auto,
            &inputs,
        );

        assert_eq!(effective.runtime, wavelinux_dsp::AudioRuntimeMode::DspAuto);
        assert_eq!(
            effective.effective_runtime,
            wavelinux_dsp::AudioRuntimeMode::DspCpu
        );
        assert!(!effective.fallback_active);
        assert!(effective.runtime_fallback_reason.is_none());
        assert!(!effective.accelerated);
    }

    #[test]
    fn dsp_cpu_runtime_uses_native_helper_without_runtime_fallback() {
        let inputs = wavelinux_dsp::ProviderProbeInputs {
            cuda_available: false,
            cuda_detail: "no cuda".into(),
            openvino_available: false,
            openvino_detail: "no openvino".into(),
            migraphx_available: false,
            migraphx_detail: "no migraphx".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let effective = wavelinux_dsp::select_provider(
            wavelinux_dsp::AudioRuntimeMode::DspCpu,
            wavelinux_dsp::DspProviderPreference::Auto,
            &inputs,
        );

        assert_eq!(effective.runtime, wavelinux_dsp::AudioRuntimeMode::DspCpu);
        assert_eq!(
            effective.effective_runtime,
            wavelinux_dsp::AudioRuntimeMode::DspCpu
        );
        assert!(!effective.fallback_active);
        assert!(effective.runtime_fallback_reason.is_none());
    }

    #[test]
    fn effect_chain_launcher_keeps_pipewire_for_default_runtime() {
        let channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        let (program, args) = effect_chain_launch_command(
            &channel,
            Path::new("/tmp/wavelinux5-chain-hardware_in.conf"),
            wavelinux_dsp::AudioRuntimeMode::PipewireFilterChain,
            true,
        );

        assert_eq!(program, "pipewire");
        assert_eq!(args, vec!["-c", "/tmp/wavelinux5-chain-hardware_in.conf"]);
    }

    #[test]
    fn effect_chain_launcher_uses_adaptive_bridge_for_wavelinux5_default_runtime() {
        let config = wavelinux5_config_with_effects(vec![EffectInstance::new("rnnoise")]);
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        let (program, args) = effect_chain_launch_command(
            channel,
            Path::new("/tmp/wavelinux5-chain-hardware_in.conf"),
            wavelinux_dsp::AudioRuntimeMode::PipewireFilterChain,
            true,
        );

        assert_eq!(program, "wavelinux6-audio-core");
        assert_eq!(
            args,
            vec![
                "--run-filter-chain",
                "--channel-id",
                "hardware_in",
                "--config",
                "/tmp/wavelinux5-chain-hardware_in.conf",
                "--adaptive-bridge-config",
                "/tmp/wavelinux5-chain-hardware_in.bridge.json"
            ]
        );
    }

    #[test]
    fn effect_chain_launcher_uses_wavelinux5_helper_for_dsp_runtime() {
        let channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        let (program, args) = effect_chain_launch_command(
            &channel,
            Path::new("/tmp/wavelinux5-chain-hardware_in.conf"),
            wavelinux_dsp::AudioRuntimeMode::DspAuto,
            true,
        );

        assert_eq!(program, "wavelinux6-audio-core");
        assert_eq!(
            args,
            vec![
                "--run-filter-chain",
                "--channel-id",
                "hardware_in",
                "--config",
                "/tmp/wavelinux5-chain-hardware_in.conf"
            ]
        );
    }

    #[test]
    fn effect_chain_launcher_uses_native_helper_for_dsp_cpu_supported_chain() {
        let mut channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        channel.effects = vec![
            EffectInstance::new("highpass"),
            EffectInstance::new("limiter"),
        ];

        let (program, args) = effect_chain_launch_command(
            &channel,
            Path::new("/tmp/wavelinux5-chain-hardware_in.conf"),
            wavelinux_dsp::AudioRuntimeMode::DspCpu,
            true,
        );

        assert_eq!(program, "wavelinux6-audio-core");
        assert_eq!(
            args,
            vec![
                "--run-native",
                "--config",
                "/tmp/wavelinux5-chain-hardware_in.json"
            ]
        );
    }

    #[test]
    fn effect_chain_launcher_uses_filter_bridge_for_unsupported_native_effect() {
        let mut channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        channel.effects = vec![EffectInstance::new("convolver")];

        let (program, args) = effect_chain_launch_command(
            &channel,
            Path::new("/tmp/wavelinux5-chain-hardware_in.conf"),
            wavelinux_dsp::AudioRuntimeMode::DspCpu,
            true,
        );

        assert_eq!(program, "wavelinux6-audio-core");
        assert_eq!(
            args,
            vec![
                "--run-filter-chain",
                "--channel-id",
                "hardware_in",
                "--config",
                "/tmp/wavelinux5-chain-hardware_in.conf"
            ]
        );
    }

    #[test]
    fn effect_chain_launcher_keeps_stable_on_pipewire_even_with_dsp_env() {
        let channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        let (program, args) = effect_chain_launch_command(
            &channel,
            Path::new("/tmp/wavelinux-chain-hardware_in.conf"),
            wavelinux_dsp::AudioRuntimeMode::DspAuto,
            false,
        );

        assert_eq!(program, "pipewire");
        assert_eq!(args, vec!["-c", "/tmp/wavelinux-chain-hardware_in.conf"]);
    }

    struct LiveGraphCleanup(Arc<WaveLinuxEngine>);

    impl Drop for LiveGraphCleanup {
        fn drop(&mut self) {
            let _ = self.0.cleanup_audio_graph();
        }
    }

    struct ChildProcessCleanup(std::process::Child);

    impl Drop for ChildProcessCleanup {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn live_test_engine(root: &Path) -> Arc<WaveLinuxEngine> {
        WaveLinuxEngine::new(
            EnginePaths::for_tests(root),
            EngineOptions {
                dry_run: false,
                auto_repair_on_start: false,
                poll_interval: Duration::from_millis(100),
            },
        )
        .unwrap()
    }

    #[test]
    fn hardware_profiles_expose_generic_default_as_profile_entry() {
        let engine = test_engine();

        let profiles = engine.list_hardware_profiles().unwrap();
        let default_profile = profiles
            .profiles
            .iter()
            .find(|profile| profile.id == "default.generic-audio")
            .unwrap();

        assert_eq!(default_profile.source, "default");
        assert_eq!(default_profile.name, "Default Generic Audio");
        assert_eq!(default_profile.latency_policy.stable_msec, Some(80));
        assert_eq!(default_profile.routing_policy.output_priority, Some(30));
    }

    #[test]
    fn get_state_reuses_recent_runtime_refresh() {
        let engine = test_engine();
        engine.refresh_runtime().unwrap();
        let first_refresh = engine.read_runtime().unwrap().refreshed_at.unwrap();

        let _ = engine.get_state().unwrap();
        let second_refresh = engine.read_runtime().unwrap().refreshed_at.unwrap();

        assert_eq!(first_refresh, second_refresh);
    }

    #[test]
    fn persistent_wavelinux6_core_starts_even_when_legacy_restore_is_disabled() {
        assert!(should_restore_audio_graph_on_launch("wavelinux6", false));
        assert!(should_restore_audio_graph_on_launch("wavelinux6", true));
        assert!(!should_restore_audio_graph_on_launch("wavelinux5", false));
    }

    #[test]
    fn startup_graph_repair_is_deferred_to_the_background_worker() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(&paths.config_dir).unwrap();
        let mut config = MixerConfig::default();
        config.settings.restore_audio_graph_on_launch = true;
        write_json(&paths.config_file(), &config).unwrap();

        let engine = WaveLinuxEngine::new(
            paths,
            EngineOptions {
                dry_run: true,
                auto_repair_on_start: true,
                poll_interval: Duration::from_millis(50),
            },
        )
        .unwrap();

        assert!(engine.startup_repair_pending.load(Ordering::Acquire));
        assert!(engine
            .startup_initialization_in_progress
            .load(Ordering::Acquire));
        let started = Instant::now();
        let state = engine.get_state().unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(state.engine.message, "Starting audio engine");
        assert!(!state.engine.audio_graph_running);
    }

    #[test]
    fn stale_runtime_refresh_uses_cached_state_when_refresh_busy() {
        let engine = test_engine();
        let _runtime_refresh = engine.runtime_refresh.lock().unwrap();
        let started = Instant::now();

        engine
            .refresh_runtime_if_stale(Duration::from_millis(0))
            .unwrap();

        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn slow_refresh_log_decision_throttles_routine_refreshes() {
        let mut state = SlowRefreshLogState::default();
        let now = Instant::now();

        assert_eq!(
            slow_refresh_log_decision(&mut state, now, Duration::from_millis(450), false, false),
            Some(SlowRefreshLogDecision {
                suppressed_refreshes: 0
            })
        );
        assert_eq!(
            slow_refresh_log_decision(
                &mut state,
                now + Duration::from_secs(10),
                Duration::from_millis(500),
                false,
                false
            ),
            None
        );
        assert_eq!(state.suppressed_refreshes, 1);
        assert_eq!(
            slow_refresh_log_decision(
                &mut state,
                now + ROUTINE_SLOW_REFRESH_LOG_INTERVAL + Duration::from_secs(1),
                Duration::from_millis(475),
                false,
                false
            ),
            Some(SlowRefreshLogDecision {
                suppressed_refreshes: 1
            })
        );
        assert_eq!(state.suppressed_refreshes, 0);
    }

    #[test]
    fn slow_refresh_log_decision_logs_urgent_refreshes_without_throttle() {
        let mut state = SlowRefreshLogState::default();
        let now = Instant::now();

        assert!(slow_refresh_log_decision(
            &mut state,
            now,
            Duration::from_millis(450),
            false,
            false
        )
        .is_some());
        assert_eq!(
            slow_refresh_log_decision(
                &mut state,
                now + Duration::from_secs(5),
                Duration::from_millis(450),
                true,
                false
            ),
            Some(SlowRefreshLogDecision {
                suppressed_refreshes: 0
            })
        );
        assert_eq!(
            slow_refresh_log_decision(
                &mut state,
                now + Duration::from_secs(6),
                Duration::from_millis(450),
                false,
                true
            ),
            Some(SlowRefreshLogDecision {
                suppressed_refreshes: 0
            })
        );
    }

    #[test]
    fn editing_profile_policy_writes_safe_local_override() {
        let engine = test_engine();
        let latency_policy = LatencyPolicy {
            stable_msec: Some(80),
            low_latency_msec: Some(45),
            bluetooth_floor_msec: Some(160),
        };
        let routing_policy = RoutingPolicy {
            input_priority: Some(64),
            output_priority: Some(44),
            allow_auto_select_input: true,
            allow_auto_select_output: true,
            prefer_non_bluetooth_input: true,
        };

        let profiles = engine
            .set_hardware_profile_policy(
                "realtek.alc3254-hda".into(),
                Some("Tuned Realtek ALC3254".into()),
                latency_policy,
                routing_policy,
            )
            .unwrap();
        let profile = profiles
            .profiles
            .iter()
            .find(|profile| profile.id == "realtek.alc3254-hda")
            .unwrap();

        assert_eq!(profile.source, "local");
        assert_eq!(profile.name, "Tuned Realtek ALC3254");
        assert_eq!(profile.latency_policy.stable_msec, Some(80));
        assert!(engine
            .paths
            .local_hardware_profiles_dir()
            .join("wavelinux-user-overrides")
            .join("realtek-alc3254-hda.json")
            .exists());
    }

    #[test]
    fn prewarm_match_count_includes_installed_catalog_profiles() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        let catalog = load_hardware_profile_catalog(&paths);
        let mut input = device(
            "alsa_input.usb-TTGK_Technology_Co._Ltd_CM01-00.mono-fallback",
            "CM01 Mono",
            false,
        );
        input.bus = Some(wavelinux_model::DeviceBus::Usb);
        input.vendor_id = Some("3302".into());
        input.product_id = Some("33a0".into());
        let mut output = device(
            "alsa_output.usb-TTGK_Technology_Co._Ltd_CM01-00.analog-stereo",
            "CM01 Analog Stereo",
            false,
        );
        output.bus = Some(wavelinux_model::DeviceBus::Usb);
        output.vendor_id = Some("3302".into());
        output.product_id = Some("33a0".into());

        assert_eq!(
            count_catalog_hardware_profile_matches(&[input, output], &catalog),
            2
        );
    }

    fn device_mentions_wavelinux(device: &DeviceInfo) -> bool {
        [&device.id, &device.name, &device.description]
            .iter()
            .any(|value| value.to_ascii_lowercase().contains("wavelinux"))
    }

    fn device_uses_sanitized_wavelinux_names(device: &DeviceInfo) -> bool {
        if device.name.contains(' ') {
            return false;
        }
        if !device.description.contains(' ') {
            return true;
        }
        device
            .description
            .strip_prefix("Monitor of ")
            .is_some_and(|target| !target.contains(' '))
    }

    fn state_has_wavelinux_audio_nodes(state: &AppStateSnapshot) -> bool {
        state.graph.inputs.iter().any(device_mentions_wavelinux)
            || state.graph.outputs.iter().any(device_mentions_wavelinux)
    }

    fn device(id: &str, description: &str, is_default: bool) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            index: None,
            name: id.into(),
            description: description.into(),
            is_available: true,
            active_port: None,
            ports: Vec::new(),
            is_default,
            is_virtual: false,
            bus: None,
            vendor_id: None,
            product_id: None,
            alsa_card: None,
            alsa_device: None,
            driver: None,
            bluetooth_modalias: None,
            active_profile: None,
            active_codec: None,
            pipewire_properties: BTreeMap::new(),
            matched_profile_id: None,
            matched_profile_source: None,
            profile_confidence: None,
            active_latency_policy: None,
            active_routing_policy: None,
            active_bluetooth_mic_policy: None,
        }
    }

    fn unavailable_alsa_headset_mono(id: &str) -> DeviceInfo {
        let mut input = device(id, "Headset Mono Microphone", false);
        input.is_available = false;
        input.bus = Some(wavelinux_model::DeviceBus::Pci);
        input.alsa_card = Some("2".into());
        input
            .pipewire_properties
            .insert("device.api".into(), "alsa".into());
        input
            .pipewire_properties
            .insert("device.icon_name".into(), "audio-headset".into());
        input.pipewire_properties.insert(
            "device.profile.description".into(),
            "Headset Mono Microphone".into(),
        );
        input
            .pipewire_properties
            .insert("node.nick".into(), "Headset Mono Microphone".into());
        input.active_port = Some("[In] Headset".into());
        input.ports = vec![wavelinux_model::DevicePortInfo {
            name: "[In] Headset".into(),
            description: "Headset Mono Microphone".into(),
            availability: "not available".into(),
            direction: Some("input".into()),
            port_type: Some("Headset".into()),
        }];
        input
    }

    fn routing_policy_with_input_priority(priority: u8) -> RoutingPolicy {
        RoutingPolicy {
            input_priority: Some(priority),
            output_priority: None,
            allow_auto_select_input: true,
            allow_auto_select_output: true,
            prefer_non_bluetooth_input: true,
        }
    }

    fn effect_endpoint_device(
        id: &str,
        description: &str,
        channel: &Channel,
        role: &str,
    ) -> DeviceInfo {
        let mut device = device(id, description, false);
        device.is_virtual = true;
        device
            .pipewire_properties
            .insert(graph_prop("managed"), "1".into());
        device
            .pipewire_properties
            .insert(graph_prop("role"), role.into());
        device.pipewire_properties.insert(
            graph_prop("effect_config_revision"),
            EFFECT_CONFIG_REVISION.into(),
        );
        device
            .pipewire_properties
            .insert(graph_prop("channel_id"), channel.id.clone());
        device
    }

    fn plan_has_channel_to_mix_route(plan: &PlannedGraph, channel_id: &str, mix_id: &str) -> bool {
        plan.commands.iter().any(|command| {
            command.args.iter().any(|arg| {
                arg.contains("wavelinux.role=channel_to_mix")
                    && arg.contains(&format!("wavelinux.channel_id={channel_id}"))
                    && arg.contains(&format!("wavelinux.mix_id={mix_id}"))
            })
        })
    }

    fn graph_for_config(config: &MixerConfig) -> RuntimeGraph {
        let inputs = config
            .mixes
            .iter()
            .map(|mix| device(&mix.virtual_source_name, &mix.name, false))
            .chain(config.mixes.iter().map(|mix| {
                device(
                    &format!("{}.monitor", mix.virtual_sink_name),
                    &format!("{} monitor", mix.name),
                    false,
                )
            }))
            .chain(config.channels.iter().map(|channel| {
                device(
                    &format!("{}.monitor", channel.virtual_sink_name),
                    &format!("{} monitor", channel.name),
                    false,
                )
            }))
            .chain(
                config
                    .channels
                    .iter()
                    .filter(|channel| channel_has_active_effects(channel))
                    .map(|channel| {
                        effect_endpoint_device(
                            &effect_chain_source_name(channel),
                            &channel.name,
                            channel,
                            "effect_output",
                        )
                    }),
            )
            .chain(
                config
                    .channels
                    .iter()
                    .filter(|channel| channel_uses_adaptive_latency_bridge(channel))
                    .map(|channel| {
                        effect_endpoint_device(
                            &effect_chain_filter_output_name(channel),
                            &channel.name,
                            channel,
                            "effect_processed",
                        )
                    }),
            )
            .chain(
                config
                    .channels
                    .iter()
                    .filter(|channel| wavelinux_pw::channel_uses_passthrough_mic_source(channel))
                    .map(|channel| {
                        effect_endpoint_device(
                            &effect_chain_source_name(channel),
                            &channel.name,
                            channel,
                            "mic_passthrough",
                        )
                    }),
            )
            .collect();
        let outputs = config
            .mixes
            .iter()
            .map(|mix| device(&mix.virtual_sink_name, &mix.name, false))
            .chain(
                config
                    .channels
                    .iter()
                    .map(|channel| device(&channel.virtual_sink_name, &channel.name, false)),
            )
            .chain(
                config
                    .channels
                    .iter()
                    .filter(|channel| channel_has_active_effects(channel))
                    .map(|channel| {
                        effect_endpoint_device(
                            &effect_chain_input_name(channel),
                            &channel.name,
                            channel,
                            "effect_input",
                        )
                    }),
            )
            .chain(
                config
                    .channels
                    .iter()
                    .filter(|channel| channel_uses_adaptive_latency_bridge(channel))
                    .map(|channel| {
                        effect_endpoint_device(
                            &effect_chain_adaptive_bridge_input_name(channel),
                            &channel.name,
                            channel,
                            "adaptive_bridge_input",
                        )
                    }),
            )
            .collect();
        RuntimeGraph {
            inputs,
            outputs,
            app_streams: Vec::new(),
            meters: Vec::new(),
            auto_devices: Vec::new(),
            effect_availability: Vec::new(),
        }
    }

    fn running_graph_for_config(config: &MixerConfig) -> RuntimeGraph {
        let mut graph = graph_for_config(config);
        for device in graph.inputs.iter_mut().chain(graph.outputs.iter_mut()) {
            device.is_virtual = true;
        }
        graph
    }

    fn routing_modules_for_config(config: &MixerConfig) -> Vec<ManagedModule> {
        let mut modules = Vec::new();
        for mix in &config.mixes {
            for output in mix.outputs() {
                modules.push(ManagedModule {
                    module_id: format!("monitor-{}-{}", mix.id, safe_file_id(&output)),
                    role: Some("mix_monitor".into()),
                    channel_id: None,
                    mix_id: Some(mix.id.clone()),
                    route_revision: Some(mix_monitor_route_revision_for_sink(
                        &config.settings,
                        mix,
                        &output,
                    )),
                    node_name: None,
                    source_name: Some(format!("{}.monitor", mix.virtual_sink_name)),
                    sink_name: Some(output),
                });
            }
        }
        for channel in &config.channels {
            let source_name = channel_mix_source_name(channel);
            if channel_has_active_effects(channel) {
                modules.push(ManagedModule {
                    module_id: format!("{}-fx-input", channel.id),
                    role: Some("channel_to_effect".into()),
                    channel_id: Some(channel.id.clone()),
                    mix_id: None,
                    route_revision: Some(effect_route_revision(&config.settings, channel)),
                    node_name: None,
                    source_name: Some(format!("{}.monitor", channel.virtual_sink_name)),
                    sink_name: Some(effect_chain_input_name(channel)),
                });
                if channel_uses_adaptive_latency_bridge(channel) {
                    modules.push(ManagedModule {
                        module_id: format!("{}-fx-adaptive-bridge", channel.id),
                        role: Some("effect_to_adaptive_bridge".into()),
                        channel_id: Some(channel.id.clone()),
                        mix_id: None,
                        route_revision: Some(EFFECT_ADAPTIVE_BRIDGE_ROUTE_REVISION.into()),
                        node_name: None,
                        source_name: Some(effect_chain_filter_output_name(channel)),
                        sink_name: Some(effect_chain_adaptive_bridge_input_name(channel)),
                    });
                }
            }
            for mix in config.mixes.iter().filter(|mix| {
                channel
                    .mix_buses
                    .get(&mix.id)
                    .is_some_and(|bus| bus.enabled)
            }) {
                modules.push(ManagedModule {
                    module_id: format!("{}-{}", channel.id, mix.id),
                    role: Some("channel_to_mix".into()),
                    channel_id: Some(channel.id.clone()),
                    mix_id: Some(mix.id.clone()),
                    route_revision: Some(channel_mix_route_revision(
                        &config.settings,
                        channel,
                        mix,
                    )),
                    node_name: None,
                    source_name: Some(source_name.clone()),
                    sink_name: Some(mix.virtual_sink_name.clone()),
                });
            }
        }
        modules
    }

    fn source_output_for_module(module: &ManagedModule) -> SourceOutputRoute {
        SourceOutputRoute {
            id: format!("source-output-{}", module.module_id),
            module_id: Some(module.module_id.clone()),
            role: module.role.clone(),
            channel_id: module.channel_id.clone(),
            mix_id: module.mix_id.clone(),
            muted: Some(false),
            volume_percent: Some(100),
            source_id: None,
            source_name: module.source_name.clone(),
            target_object: module.source_name.clone(),
            application_name: None,
            node_name: None,
            media_name: None,
            managed: None,
            dont_move: false,
        }
    }

    fn sink_input_for_module(module: &ManagedModule) -> SinkInputRoute {
        SinkInputRoute {
            id: format!("sink-input-{}", module.module_id),
            module_id: Some(module.module_id.clone()),
            role: module.role.clone(),
            channel_id: module.channel_id.clone(),
            mix_id: module.mix_id.clone(),
            muted: Some(false),
            volume_percent: Some(100),
            sink: None,
            sink_name: module.sink_name.clone(),
            target_object: module.sink_name.clone(),
        }
    }

    fn native_input_target_route(channel_id: &str, source_name: &str) -> SourceOutputRoute {
        SourceOutputRoute {
            id: format!("native-input-{channel_id}"),
            module_id: None,
            role: Some("input_target".into()),
            channel_id: Some(channel_id.into()),
            mix_id: None,
            muted: Some(false),
            volume_percent: Some(100),
            source_id: None,
            source_name: Some(source_name.into()),
            target_object: Some(source_name.into()),
            application_name: Some("WaveLinux 6".into()),
            node_name: Some(format!("wavelinux6-input-target-{channel_id}")),
            media_name: None,
            managed: Some("1".into()),
            dont_move: true,
        }
    }

    fn native_mix_output_target_route(mix_id: &str, sink_name: &str) -> SinkInputRoute {
        SinkInputRoute {
            id: format!("native-output-{mix_id}"),
            module_id: None,
            role: Some("mix_output_target".into()),
            channel_id: None,
            mix_id: Some(mix_id.into()),
            muted: Some(false),
            volume_percent: Some(100),
            sink: None,
            sink_name: Some(sink_name.into()),
            target_object: Some(sink_name.into()),
        }
    }

    fn refresh_until(
        engine: &WaveLinuxEngine,
        timeout: Duration,
        mut predicate: impl FnMut(&AppStateSnapshot) -> bool,
    ) -> AppStateSnapshot {
        let started = Instant::now();
        loop {
            engine.refresh_runtime().unwrap();
            let state = engine.get_state().unwrap();
            if predicate(&state) || started.elapsed() >= timeout {
                return state;
            }
            thread::sleep(Duration::from_millis(150));
        }
    }

    fn spawn_silent_route_test_stream(app_id: &str) -> Option<ChildProcessCleanup> {
        let paplay_available = host_command("paplay")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if !paplay_available {
            eprintln!("skipping live route stream: paplay is not available");
            return None;
        }

        let child = host_command("paplay")
            .args([
                "--raw",
                "--rate=48000",
                "--format=s16le",
                "--channels=2",
                "--client-name=WaveLinuxRouteTest",
                "--stream-name=WaveLinuxRouteTestStream",
                "--property=application.name=WaveLinux Route Test",
                &format!("--property=application.id={app_id}"),
                "--property=application.process.binary=wavelinux-route-test",
                "--property=application.process.name=wavelinux-route-test",
                "--property=window.x11.class=WaveLinuxRouteTest",
                "/dev/zero",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        Some(ChildProcessCleanup(child))
    }

    fn spawn_tone_route_test_stream(root: &Path, app_id: &str) -> Option<ChildProcessCleanup> {
        let paplay_available = host_command("paplay")
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        let ffmpeg_available = host_command("ffmpeg")
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if !paplay_available || !ffmpeg_available {
            eprintln!("skipping live tone stream: paplay or ffmpeg is not available");
            return None;
        }

        let tone_path = root.join("wavelinux-tone.raw");
        let ffmpeg_status = host_command("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=880:duration=4",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ac",
                "2",
            ])
            .arg(&tone_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()?;
        if !ffmpeg_status.success() {
            eprintln!("skipping live tone stream: ffmpeg failed to generate tone");
            return None;
        }

        let child = host_command("paplay")
            .args([
                "--raw",
                "--rate=48000",
                "--format=s16le",
                "--channels=2",
                "--client-name=Spotify",
                "--stream-name=Spotify Tone Test",
                "--property=application.name=Spotify",
                &format!("--property=application.id={app_id}"),
                "--property=application.process.binary=spotify",
                "--property=application.process.name=spotify",
                "--property=media.name=Spotify Tone Test",
            ])
            .arg(tone_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        Some(ChildProcessCleanup(child))
    }

    #[test]
    fn creates_and_persists_mix() {
        let engine = test_engine();
        let mix = engine.create_mix("MicrophoneFX".into()).unwrap();
        assert_eq!(mix.name, "MicrophoneFX");
        let state = engine.get_state().unwrap();
        assert!(state.config.mixes.iter().any(|item| item.id == mix.id));
    }

    #[test]
    fn repair_reports_dry_run_commands() {
        let engine = test_engine();
        let report = engine.repair_audio_graph().unwrap();
        assert!(report.dry_run);
        assert!(report.outputs.iter().all(|output| output.skipped));
        assert!(report
            .planned
            .commands
            .iter()
            .any(|command| command.description.contains("create channel sink")));
    }

    #[test]
    fn graph_debug_report_exposes_plan_and_runtime_metadata() {
        let engine = test_engine();
        let report = engine.get_graph_debug_report().unwrap();

        assert!(report.dry_run);
        assert!(!report.audio_graph_running);
        assert!(report
            .planned
            .commands
            .iter()
            .any(|command| command.description.contains("create virtual mix sink")));
        assert!(report.debug_log_path.ends_with("wavelinux-engine.log"));
    }

    #[test]
    fn app_routing_guard_rejects_stale_channel_paths() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let mut graph = graph_for_config(&config);
        let mut modules = routing_modules_for_config(&config);

        let stream = AppStream {
            id: "spotify-stream".into(),
            app_id: Some("spotify".into()),
            binary: Some("spotify".into()),
            process_name: Some("spotify".into()),
            window_class: None,
            display_name: "Spotify".into(),
            media_name: Some("Spotify".into()),
            routed_channel_id: Some("music".into()),
            volume: 1.0,
            muted: false,
        };
        graph.app_streams = vec![stream.clone()];
        assert!(app_routing_graph_ready(&config, &graph, &modules, &[], &[]));

        modules.retain(|module| {
            !(module.role.as_deref() == Some("channel_to_mix")
                && module.channel_id.as_deref() == Some("music")
                && module.mix_id.as_deref() == Some("monitor"))
        });
        assert!(!app_routing_graph_ready(
            &config,
            &graph,
            &modules,
            &[],
            &[]
        ));
        assert!(!stream_route_ready(
            &config,
            &graph,
            &modules,
            &[],
            &[],
            &stream
        ));
    }

    #[test]
    fn app_routing_guard_accepts_ready_stream_paths_and_rescues_stale_ones() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let mut graph = graph_for_config(&config);
        let modules = routing_modules_for_config(&config);
        let stream = AppStream {
            id: "spotify-stream".into(),
            app_id: Some("spotify".into()),
            binary: Some("spotify".into()),
            process_name: Some("spotify".into()),
            window_class: None,
            display_name: "Spotify".into(),
            media_name: Some("Spotify".into()),
            routed_channel_id: Some("music".into()),
            volume: 1.0,
            muted: false,
        };
        graph.app_streams = vec![stream.clone()];

        assert!(stream_route_ready(
            &config,
            &graph,
            &modules,
            &[],
            &[],
            &stream
        ));
        assert!(!engine
            .move_unready_routed_streams_to_default(&config, &graph, &modules, &[], &[])
            .unwrap());

        let mut stale_modules = modules.clone();
        stale_modules.retain(|module| {
            !(module.role.as_deref() == Some("channel_to_mix")
                && module.channel_id.as_deref() == Some("music")
                && module.mix_id.as_deref() == Some("monitor"))
        });
        assert!(!stream_route_ready(
            &config,
            &graph,
            &stale_modules,
            &[],
            &[],
            &stream
        ));
        assert!(engine
            .move_unready_routed_streams_to_default(&config, &graph, &stale_modules, &[], &[])
            .unwrap());
    }

    #[test]
    fn wavelinux6_graph_is_ready_without_pulse_mix_sinks_or_bus_loopbacks() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        let mut graph = running_graph_for_config(&config);
        graph.outputs.retain(|output| {
            config
                .mixes
                .iter()
                .all(|mix| output.name != mix.virtual_sink_name)
        });
        graph.inputs.retain(|input| {
            config
                .mixes
                .iter()
                .all(|mix| input.name != format!("{}.monitor", mix.virtual_sink_name))
        });
        graph.app_streams.push(AppStream {
            id: "browser-stream".into(),
            app_id: Some("brave-browser".into()),
            binary: Some("brave-browser".into()),
            process_name: Some("brave".into()),
            window_class: None,
            display_name: "Brave".into(),
            media_name: Some("YouTube".into()),
            routed_channel_id: Some("browser".into()),
            volume: 1.0,
            muted: false,
        });

        assert!(config.mixes.iter().all(|mix| graph
            .inputs
            .iter()
            .any(|input| input.name == mix.virtual_source_name)));
        assert!(app_routing_graph_ready(&config, &graph, &[], &[], &[]));
    }

    #[test]
    fn wavelinux6_graph_requires_matching_native_device_target_streams() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_channel_input("hardware_in", Some("alsa_input.usb_cm01".into()))
            .unwrap();
        config
            .set_mix_outputs(
                "monitor",
                vec![
                    "alsa_output.usb_cm01".into(),
                    "bluez_output.headphones".into(),
                ],
            )
            .unwrap();
        let mut graph = running_graph_for_config(&config);
        graph.outputs.retain(|output| {
            config
                .mixes
                .iter()
                .all(|mix| output.name != mix.virtual_sink_name)
        });
        graph.inputs.retain(|input| {
            config
                .mixes
                .iter()
                .all(|mix| input.name != format!("{}.monitor", mix.virtual_sink_name))
        });
        let input_route = native_input_target_route("hardware_in", "alsa_input.usb_cm01");
        let output_routes = vec![
            native_mix_output_target_route("monitor", "alsa_output.usb_cm01"),
            native_mix_output_target_route("monitor", "bluez_output.headphones"),
        ];

        assert!(app_routing_graph_ready(
            &config,
            &graph,
            &[],
            std::slice::from_ref(&input_route),
            &output_routes,
        ));
        assert!(!app_routing_graph_ready(
            &config,
            &graph,
            &[],
            &[],
            &output_routes,
        ));
        assert!(!app_routing_graph_ready(
            &config,
            &graph,
            &[],
            std::slice::from_ref(&input_route),
            &output_routes[..1],
        ));

        let wrong_input = native_input_target_route("hardware_in", "alsa_input.internal_mic");
        assert!(!app_routing_graph_ready(
            &config,
            &graph,
            &[],
            std::slice::from_ref(&wrong_input),
            &output_routes,
        ));
    }

    #[test]
    fn app_routing_guard_requires_effect_source_readiness() {
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("music", vec![EffectInstance::new("limiter")])
            .unwrap();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let mut graph = graph_for_config(&config);
        let modules = routing_modules_for_config(&config);
        graph.app_streams = vec![AppStream {
            id: "spotify-stream".into(),
            app_id: Some("spotify".into()),
            binary: Some("spotify".into()),
            process_name: Some("spotify".into()),
            window_class: None,
            display_name: "Spotify".into(),
            media_name: Some("Spotify".into()),
            routed_channel_id: Some("music".into()),
            volume: 1.0,
            muted: false,
        }];

        assert!(app_routing_graph_ready(&config, &graph, &modules, &[], &[]));

        let mut missing_fx_graph = graph.clone();
        missing_fx_graph
            .inputs
            .retain(|input| input.name != "wavelinux_fx_music_source");
        assert!(!app_routing_graph_ready(
            &config,
            &missing_fx_graph,
            &modules,
            &[],
            &[]
        ));
        assert!(active_effect_routes_need_repair(
            &config,
            &missing_fx_graph,
            &modules,
            &[],
            &[]
        ));

        let mut stale_fx_graph = graph.clone();
        stale_fx_graph
            .inputs
            .iter_mut()
            .find(|input| input.name == "wavelinux_fx_music_source")
            .unwrap()
            .pipewire_properties
            .insert(graph_prop("effect_config_revision"), "stale".into());
        assert!(!app_routing_graph_ready(
            &config,
            &stale_fx_graph,
            &modules,
            &[],
            &[]
        ));
        assert!(active_effect_routes_need_repair(
            &config,
            &stale_fx_graph,
            &modules,
            &[],
            &[]
        ));

        let mut wrong_channel_graph = graph.clone();
        wrong_channel_graph
            .outputs
            .iter_mut()
            .find(|output| output.name == "wavelinux_fx_music_input")
            .unwrap()
            .pipewire_properties
            .insert(graph_prop("channel_id"), "chat".into());
        assert!(!app_routing_graph_ready(
            &config,
            &wrong_channel_graph,
            &modules,
            &[],
            &[]
        ));
        assert!(active_effect_routes_need_repair(
            &config,
            &wrong_channel_graph,
            &modules,
            &[],
            &[]
        ));

        let mut raw_fallback_modules = modules.clone();
        for module in raw_fallback_modules.iter_mut().filter(|module| {
            module.role.as_deref() == Some("channel_to_mix")
                && module.channel_id.as_deref() == Some("music")
        }) {
            module.source_name = Some("wavelinux_channel_music.monitor".into());
        }
        assert!(!app_routing_graph_ready(
            &config,
            &missing_fx_graph,
            &raw_fallback_modules,
            &[],
            &[]
        ));
        assert!(active_effect_routes_need_repair(
            &config,
            &missing_fx_graph,
            &raw_fallback_modules,
            &[],
            &[]
        ));
    }

    #[test]
    fn bluetooth_profile_rotation_does_not_make_routes_look_stale() {
        assert!(audio_endpoint_names_match(
            "bluez_output.AA_BB_CC_DD_EE_FF.1",
            "bluez_output.AA_BB_CC_DD_EE_FF.2"
        ));
        assert!(audio_endpoint_names_match(
            "bluez_input.AA_BB_CC_DD_EE_FF.headset-head-unit",
            "bluez_input.AA_BB_CC_DD_EE_FF.handsfree-head-unit"
        ));
        assert!(audio_endpoint_names_match(
            "bluez_input.AA:BB:CC:DD:EE:FF",
            "bluez_input.AA_BB_CC_DD_EE_FF.headset-head-unit"
        ));
        assert!(!audio_endpoint_names_match(
            "bluez_output.AA_BB_CC_DD_EE_FF.1",
            "bluez_output.11_22_33_44_55_66.1"
        ));

        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("bluez_output.AA_BB_CC_DD_EE_FF.1".into()))
            .unwrap();
        let mut module = ManagedModule {
            module_id: "1".into(),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            route_revision: Some(mix_monitor_route_revision_for_sink(
                &config.settings,
                config.mixes.iter().find(|mix| mix.id == "monitor").unwrap(),
                "bluez_output.AA_BB_CC_DD_EE_FF.1",
            )),
            node_name: None,
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            sink_name: Some("bluez_output.AA_BB_CC_DD_EE_FF.2".into()),
        };

        assert!(!module_is_stale_for_config(&module, &config));
        module.sink_name = Some("bluez_output.11_22_33_44_55_66.2".into());
        assert!(module_is_stale_for_config(&module, &config));
    }

    #[test]
    fn meter_supervisor_does_not_spawn_in_dry_run() {
        let mut supervisor = MeterSupervisor::new(true);
        let revision = MeterTargetRevision::new(EngineRevisions::default(), true);
        let update = supervisor.reconcile(
            vec![MeterTarget {
                node_id: "stream".into(),
                source_name: "wavelinux_mix_stream.monitor".into(),
                gain: 1.0,
                muted: false,
            }],
            true,
            revision,
        );

        assert!(update.meters.is_empty());
        assert!(supervisor.process.is_none());
        assert!(supervisor.snapshot_for_revision(revision, true).is_some());
        assert!(supervisor
            .snapshot_for_revision(
                MeterTargetRevision {
                    graph: revision.graph + 1,
                    ..revision
                },
                true,
            )
            .is_none());
    }

    #[test]
    fn native_meter_supervisor_never_spawns_a_pipewire_reader() {
        let mut supervisor = MeterSupervisor::new(false);
        let revision = MeterTargetRevision::new(EngineRevisions::default(), true);
        let target = MeterTarget {
            node_id: "music".into(),
            source_name: "wavelinux6_fx_music_source".into(),
            gain: 1.0,
            muted: false,
        };
        let meter = LevelMeter {
            node_id: "music".into(),
            peak_left: 0.4,
            peak_right: 0.2,
        };

        let update = supervisor.reconcile_native(vec![target], vec![meter.clone()], true, revision);

        assert!(supervisor.native_backend);
        assert!(supervisor.process.is_none());
        assert_eq!(update.meters, vec![meter]);
        assert_eq!(supervisor.snapshot(), update.meters);
    }

    #[test]
    fn native_meter_response_maps_channels_buses_and_mixes_once() {
        let targets = vec![
            MeterTarget {
                node_id: "music".into(),
                source_name: "wavelinux6_fx_music_source".into(),
                gain: 0.5,
                muted: false,
            },
            MeterTarget {
                node_id: "channel:music:mix:stream".into(),
                source_name: "wavelinux6_fx_music_source".into(),
                gain: 0.25,
                muted: false,
            },
            MeterTarget {
                node_id: "stream".into(),
                source_name: "wavelinux6_mix_stream_source".into(),
                gain: 0.2,
                muted: false,
            },
        ];
        let response = NativeCoreMetersResponse {
            channels: vec![NativeCoreMeterReading {
                id: "music".into(),
                peak_left: 0.4,
                peak_right: 0.2,
            }],
            mixes: vec![NativeCoreMeterReading {
                id: "stream".into(),
                peak_left: 0.3,
                peak_right: 0.1,
            }],
        };

        let meters = level_meters_from_native_response(&targets, response);
        assert_eq!(meters.len(), 3);
        assert_eq!(meters[0].node_id, "music");
        assert_eq!(
            meters[0].peak_left,
            meter_output_level(0.4, targets[0].gain)
        );
        assert_eq!(meters[1].node_id, "channel:music:mix:stream");
        assert_eq!(
            meters[1].peak_left,
            meter_output_level(0.4, targets[1].gain)
        );
        assert_eq!(meters[2].node_id, "stream");
        assert_eq!(meters[2].peak_left, meter_output_level(0.3, 1.0));
        assert_ne!(meters[2].peak_left, meter_output_level(0.3, 0.2));
    }

    #[test]
    fn meter_sample_reader_tracks_real_rms_frames() {
        let sample = Arc::new(AtomicMeterSample::default());
        let mut pending = Vec::new();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0.25_f32.to_le_bytes());
        bytes.extend_from_slice(&(-0.5_f32).to_le_bytes());
        bytes.extend_from_slice(&0.1_f32.to_le_bytes());
        bytes.extend_from_slice(&0.2_f32.to_le_bytes());

        consume_meter_bytes(&bytes[..5], &mut pending, &sample);
        assert_eq!(sample.frames.load(Ordering::Relaxed), 0);
        consume_meter_bytes(&bytes[5..], &mut pending, &sample);

        assert_eq!(sample.frames.load(Ordering::Relaxed), 2);
        let snapshot = sample.snapshot();
        assert!(snapshot.updated_at.is_some());
        let expected_left = ((0.25_f32.powi(2) + 0.1_f32.powi(2)) / 2.0).sqrt();
        let expected_right = ((0.5_f32.powi(2) + 0.2_f32.powi(2)) / 2.0).sqrt();
        assert!((snapshot.peak_left - expected_left).abs() < 0.000_001);
        assert!((snapshot.peak_right - expected_right).abs() < 0.000_001);
    }

    #[test]
    fn meter_sample_tracks_current_rms_without_backend_peak_hold() {
        let sample = Arc::new(AtomicMeterSample::default());
        let mut pending = Vec::new();
        let mut hit = Vec::new();
        hit.extend_from_slice(&0.5_f32.to_le_bytes());
        hit.extend_from_slice(&(-0.75_f32).to_le_bytes());
        consume_meter_bytes(&hit, &mut pending, &sample);

        let hit_sample = sample.snapshot();
        assert!((hit_sample.peak_left - 0.5).abs() < f32::EPSILON);
        assert!((hit_sample.peak_right - 0.75).abs() < f32::EPSILON);

        let mut silence = Vec::new();
        silence.extend_from_slice(&0.0_f32.to_le_bytes());
        silence.extend_from_slice(&0.0_f32.to_le_bytes());
        consume_meter_bytes(&silence, &mut pending, &sample);
        let silent_sample = sample.snapshot();
        assert_eq!(silent_sample.peak_left, 0.0);
        assert_eq!(silent_sample.peak_right, 0.0);
    }

    #[test]
    fn meter_sample_ignores_floor_noise() {
        let sample = Arc::new(AtomicMeterSample::default());
        let mut pending = Vec::new();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(METER_NOISE_FLOOR * 0.5).to_le_bytes());
        bytes.extend_from_slice(&(-METER_NOISE_FLOOR * 0.5).to_le_bytes());
        consume_meter_bytes(&bytes, &mut pending, &sample);

        let sample = sample.snapshot();
        assert_eq!(sample.peak_left, 0.0);
        assert_eq!(sample.peak_right, 0.0);
    }

    #[test]
    fn meter_output_level_uses_mixer_display_curve() {
        assert_eq!(meter_output_level(METER_NOISE_FLOOR * 0.5, 1.0), 0.0);
        assert_eq!(meter_output_level(0.5, 0.0), 0.0);
        assert!((0.55..0.65).contains(&meter_output_level(0.1, 1.0)));
        assert!((0.7..0.8).contains(&meter_output_level(0.25, 1.0)));
        assert!((0.85..0.95).contains(&meter_output_level(0.5, 1.0)));
        assert_eq!(meter_output_level(1.0, 1.0), 1.0);
    }

    #[test]
    fn stale_meter_samples_decay_without_new_audio_frames() {
        let now = Instant::now();
        assert_eq!(stale_adjusted_meter_peak(0.7, None, now), 0.0);
        assert_eq!(
            stale_adjusted_meter_peak(0.7, Some(now - Duration::from_millis(60)), now),
            0.7
        );
        let decayed = stale_adjusted_meter_peak(0.7, Some(now - Duration::from_millis(900)), now);
        assert!(decayed < 0.25, "decayed={decayed}");
        assert_eq!(
            stale_adjusted_meter_peak(0.7, Some(now - Duration::from_secs(4)), now),
            0.0
        );
    }

    #[test]
    fn meter_endpoint_targets_sink_monitor_without_default_fallback() {
        let endpoint = MeterEndpoint::from_source_name("wavelinux_channel_music.monitor");
        assert_eq!(endpoint.target_object, "wavelinux_channel_music");
        assert!(endpoint.capture_sink_monitor);
        assert!(endpoint.dont_reconnect);
        assert!(endpoint.dont_remix);

        let source_endpoint = MeterEndpoint::from_source_name("wavelinux_mix_stream_source");
        assert_eq!(source_endpoint.target_object, "wavelinux_mix_stream_source");
        assert!(!source_endpoint.capture_sink_monitor);
        assert!(source_endpoint.dont_reconnect);
        assert!(!source_endpoint.dont_remix);
    }

    #[test]
    fn meter_restore_ids_are_isolated_by_target_source() {
        let microphone = MeterEndpoint::from_source_name("wavelinux6-mic");
        let browser = MeterEndpoint::from_source_name("wavelinux6_fx_browser_source");

        assert_ne!(
            meter_stream_restore_id(&microphone),
            meter_stream_restore_id(&browser)
        );
        assert!(meter_stream_restore_id(&microphone).contains("wavelinux6-mic"));
    }

    #[test]
    fn channel_bus_volume_uses_one_gain_stage_when_both_loopback_sides_exist() {
        let commands = plan_channel_bus_volume_commands(Some("73"), Some("91"), 0.5);

        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].args, vec!["set-sink-input-volume", "73", "50%"]);
        assert_eq!(
            commands[1].args,
            vec!["set-source-output-volume", "91", "100%"]
        );

        let source_only = plan_channel_bus_volume_commands(None, Some("91"), 0.5);
        assert_eq!(
            source_only[0].args,
            vec!["set-source-output-volume", "91", "50%"]
        );
    }

    #[test]
    fn channel_bus_mute_targets_both_loopback_sides_when_available() {
        let commands = plan_channel_bus_mute_commands(Some("73"), Some("91"), true);
        let args = commands
            .iter()
            .map(|command| command.args.clone())
            .collect::<Vec<_>>();

        assert_eq!(commands.len(), 2);
        assert!(args.contains(&vec!["set-sink-input-mute".into(), "73".into(), "1".into()]));
        assert!(args.contains(&vec![
            "set-source-output-mute".into(),
            "91".into(),
            "1".into()
        ]));
    }

    #[test]
    fn timed_cache_expiry_respects_ttl() {
        assert!(cache_expired(None, Duration::from_secs(30)));
        assert!(cache_expired(
            Some(Instant::now() - Duration::from_secs(31)),
            Duration::from_secs(30),
        ));
        assert!(!cache_expired(
            Some(Instant::now() - Duration::from_secs(5)),
            Duration::from_secs(30),
        ));
    }

    #[test]
    fn engine_app_identity_commands_persist_canonical_routes() {
        let engine = test_engine();
        let raw = AppMatcher::from_process_name("Discord");
        let canonical = AppMatcher::from_app_id("com.discordapp.Discord");

        engine
            .assign_app_to_channel("chat".into(), raw.clone())
            .expect("route raw app");
        engine
            .pin_app_identity(raw.clone(), "Voice Chat".into())
            .expect("pin identity");
        engine
            .merge_app_identity(raw.clone(), canonical.clone())
            .expect("merge identity");

        let state = engine.get_state().unwrap();
        assert!(state
            .config
            .app_identity_overrides
            .iter()
            .any(|item| item.source == raw && item.target == canonical));
        assert!(state
            .config
            .app_routes
            .iter()
            .any(|route| route.matcher == canonical && route.channel_id == "chat"));
    }

    #[test]
    fn repair_writes_debug_log() {
        let engine = test_engine();
        engine.repair_audio_graph().unwrap();

        let log = fs::read_to_string(engine.paths.log_file()).unwrap();
        assert!(log.contains("[repair.start]"));
        assert!(log.contains("[repair.plan]"));
        assert!(log.contains("[repair.end]"));
    }

    #[test]
    fn log_maintenance_rotates_logs_on_app_version_change() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(paths.log_version_file(), "4.2.0\n").unwrap();
        fs::write(paths.log_file(), "old engine log\n").unwrap();
        fs::write(paths.legacy_app_log_file(), "old app log\n").unwrap();
        let chain_log = paths.config_dir.join("wavelinux-chain-hardware_in.log");
        fs::write(&chain_log, "old chain log\n").unwrap();

        maintain_logs_for_paths(&paths, "4.3.0").unwrap();

        assert!(!paths.log_file().exists());
        assert_eq!(
            fs::read_to_string(rotated_log_path(&paths.log_file(), 1)).unwrap(),
            "old engine log\n"
        );
        assert_eq!(
            fs::read_to_string(rotated_log_path(&paths.legacy_app_log_file(), 1)).unwrap(),
            "old app log\n"
        );
        assert_eq!(
            fs::read_to_string(rotated_log_path(&chain_log, 1)).unwrap(),
            "old chain log\n"
        );
        assert_eq!(
            fs::read_to_string(paths.log_version_file()).unwrap(),
            "4.3.0\n"
        );
    }

    #[test]
    fn log_maintenance_uses_size_rotation_when_version_is_current() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(paths.log_version_file(), "4.3.0\n").unwrap();
        fs::write(
            paths.log_file(),
            vec![b'x'; (DEBUG_LOG_MAX_BYTES + 1) as usize],
        )
        .unwrap();
        fs::write(paths.legacy_app_log_file(), "small legacy log\n").unwrap();

        maintain_logs_for_paths(&paths, "4.3.0").unwrap();

        assert!(!paths.log_file().exists());
        assert!(rotated_log_path(&paths.log_file(), 1).exists());
        assert_eq!(
            fs::read_to_string(paths.legacy_app_log_file()).unwrap(),
            "small legacy log\n"
        );
        assert!(!rotated_log_path(&paths.legacy_app_log_file(), 1).exists());
    }

    #[test]
    fn log_rotation_keeps_only_bounded_history() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(paths.log_file(), "newest\n").unwrap();
        for index in 1..=(DEBUG_LOG_ROTATED_FILES + 2) {
            fs::write(
                rotated_log_path(&paths.log_file(), index),
                format!("old {index}\n"),
            )
            .unwrap();
        }

        rotate_log(&paths.log_file()).unwrap();
        trim_rotated_logs(&paths.log_file()).unwrap();

        assert_eq!(
            fs::read_to_string(rotated_log_path(&paths.log_file(), 1)).unwrap(),
            "newest\n"
        );
        assert_eq!(
            fs::read_to_string(rotated_log_path(&paths.log_file(), DEBUG_LOG_ROTATED_FILES))
                .unwrap(),
            format!("old {}\n", DEBUG_LOG_ROTATED_FILES - 1)
        );
        assert!(!rotated_log_path(&paths.log_file(), DEBUG_LOG_ROTATED_FILES + 1).exists());
        assert!(!rotated_log_path(&paths.log_file(), DEBUG_LOG_ROTATED_FILES + 2).exists());
    }

    #[test]
    fn invalid_saved_config_is_backed_up_and_replaced() {
        let root = tempdir().unwrap();
        let paths = EnginePaths::for_tests(root.path());
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(
            paths.config_file(),
            r#"{"version":1,"mixes":[],"channels":["Music"]}"#,
        )
        .unwrap();

        let engine = WaveLinuxEngine::new(
            paths.clone(),
            EngineOptions {
                dry_run: true,
                auto_repair_on_start: false,
                poll_interval: Duration::from_millis(50),
            },
        )
        .unwrap();

        let state = engine.get_state().unwrap();
        assert!(state
            .config
            .channels
            .iter()
            .any(|channel| channel.id == "music"));
        assert!(paths.config_file().exists());
        assert!(fs::read_dir(paths.config_dir)
            .unwrap()
            .flatten()
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("config.invalid.")));
    }

    #[test]
    fn stale_cleanup_keeps_current_modules_and_flags_old_untagged_modules() {
        let config = MixerConfig::default();
        let current_channel = ManagedModule {
            module_id: "1".into(),
            role: Some("channel_to_mix".into()),
            channel_id: Some("game".into()),
            mix_id: Some("stream".into()),
            route_revision: Some(channel_mix_route_revision(
                &config.settings,
                config
                    .channels
                    .iter()
                    .find(|channel| channel.id == "game")
                    .unwrap(),
                config.mixes.iter().find(|mix| mix.id == "stream").unwrap(),
            )),
            node_name: Some("wavelinux_channel_game.monitor".into()),
            source_name: Some("wavelinux_channel_game.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };
        let old_untagged = ManagedModule {
            module_id: "2".into(),
            role: None,
            channel_id: None,
            mix_id: None,
            route_revision: None,
            node_name: Some("wavelinux_system.monitor".into()),
            source_name: Some("wavelinux_system.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };
        let removed_channel = ManagedModule {
            module_id: "3".into(),
            role: Some("channel_to_mix".into()),
            channel_id: Some("voice_chat".into()),
            mix_id: Some("stream".into()),
            route_revision: None,
            node_name: Some("wavelinux_voice_chat.monitor".into()),
            source_name: Some("wavelinux_voice_chat.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };

        assert!(!module_is_stale_for_config(&current_channel, &config));
        assert!(module_is_stale_for_config(&old_untagged, &config));
        assert!(module_is_stale_for_config(&removed_channel, &config));
    }

    #[test]
    fn wavelinux6_marks_replaced_pulse_mix_modules_and_routes_stale() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.test".into()))
            .unwrap();
        let monitor = config.mixes.iter().find(|mix| mix.id == "monitor").unwrap();

        let replaced = [
            ManagedModule {
                module_id: "legacy-mix".into(),
                role: Some("mix".into()),
                channel_id: None,
                mix_id: Some(monitor.id.clone()),
                route_revision: None,
                node_name: Some(monitor.virtual_sink_name.clone()),
                source_name: None,
                sink_name: None,
            },
            ManagedModule {
                module_id: "legacy-mix-source".into(),
                role: Some("mix_source".into()),
                channel_id: None,
                mix_id: Some(monitor.id.clone()),
                route_revision: None,
                node_name: Some(monitor.virtual_source_name.clone()),
                source_name: None,
                sink_name: None,
            },
            ManagedModule {
                module_id: "legacy-channel-route".into(),
                role: Some("channel_to_mix".into()),
                channel_id: Some("browser".into()),
                mix_id: Some(monitor.id.clone()),
                route_revision: None,
                node_name: None,
                source_name: Some("wavelinux6_fx_browser_source".into()),
                sink_name: Some(monitor.virtual_sink_name.clone()),
            },
            ManagedModule {
                module_id: "legacy-monitor-route".into(),
                role: Some("mix_monitor".into()),
                channel_id: None,
                mix_id: Some(monitor.id.clone()),
                route_revision: Some(mix_monitor_route_revision_for_sink(
                    &config.settings,
                    monitor,
                    "alsa_output.test",
                )),
                node_name: None,
                source_name: Some(format!("{}.monitor", monitor.virtual_sink_name)),
                sink_name: Some("alsa_output.test".into()),
            },
        ];

        assert!(replaced
            .iter()
            .all(|module| module_is_stale_for_config(module, &config)));
    }

    #[test]
    fn stale_cleanup_keeps_current_effect_chain_nodes() {
        let mut config = MixerConfig::default();
        let channel = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        channel.effects = vec![EffectInstance::new("limiter")];

        let effect_input = ManagedModule {
            module_id: "effect-input".into(),
            role: Some("effect_input".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: None,
            node_name: Some(effect_chain_input_name(channel)),
            source_name: None,
            sink_name: None,
        };
        let effect_output = ManagedModule {
            module_id: "effect-output".into(),
            role: Some("effect_output".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: None,
            node_name: Some(effect_chain_source_name(channel)),
            source_name: None,
            sink_name: None,
        };
        let stale_effect_output = ManagedModule {
            module_id: "stale-effect-output".into(),
            role: Some("effect_output".into()),
            channel_id: Some("music".into()),
            mix_id: None,
            route_revision: None,
            node_name: Some(effect_chain_source_name(channel)),
            source_name: None,
            sink_name: None,
        };

        assert!(!module_is_stale_for_config(&effect_input, &config));
        assert!(!module_is_stale_for_config(&effect_output, &config));
        assert!(module_is_stale_for_config(&stale_effect_output, &config));
    }

    #[test]
    fn stale_cleanup_keeps_wavelinux5_adaptive_effect_bridge_nodes() {
        let config = wavelinux5_config_with_effects(vec![EffectInstance::new("rnnoise")]);
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        let effect_processed = ManagedModule {
            module_id: "effect-processed".into(),
            role: Some("effect_processed".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: None,
            node_name: Some(effect_chain_filter_output_name(channel)),
            source_name: None,
            sink_name: None,
        };
        let adaptive_input = ManagedModule {
            module_id: "adaptive-input".into(),
            role: Some("adaptive_bridge_input".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: None,
            node_name: Some(effect_chain_adaptive_bridge_input_name(channel)),
            source_name: None,
            sink_name: None,
        };
        let bridge_route = ManagedModule {
            module_id: "adaptive-route".into(),
            role: Some("effect_to_adaptive_bridge".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: Some(EFFECT_ADAPTIVE_BRIDGE_ROUTE_REVISION.into()),
            node_name: None,
            source_name: Some(effect_chain_filter_output_name(channel)),
            sink_name: Some(effect_chain_adaptive_bridge_input_name(channel)),
        };
        let stale_bridge_route = ManagedModule {
            module_id: "stale-adaptive-route".into(),
            route_revision: Some("old".into()),
            ..bridge_route.clone()
        };

        assert!(!module_is_stale_for_config(&effect_processed, &config));
        assert!(!module_is_stale_for_config(&adaptive_input, &config));
        assert!(!module_is_stale_for_config(&bridge_route, &config));
        assert!(module_is_stale_for_config(&stale_bridge_route, &config));
    }

    #[test]
    fn stale_cleanup_excludes_supervised_filter_chain_child_for_active_helper() {
        let active_helper = StaleProcess {
            pid: "100".into(),
            command: "/home/dusky/.local/bin/wavelinux6-audio-core --run-filter-chain".into(),
        };
        let supervised_pipewire_child = StaleProcess {
            pid: "200".into(),
            command: "/usr/bin/pipewire -c /home/dusky/.local/share/wavelinux5/effects/wavelinux5-chain-hardware_in.conf".into(),
        };
        let unrelated_old_chain = StaleProcess {
            pid: "300".into(),
            command: "/usr/bin/pipewire -c /tmp/wavelinux5-chain-chat.conf".into(),
        };
        let active_pids = BTreeSet::from(["100".to_string()]);
        let active_config_markers = BTreeSet::from(["wavelinux5-chain-hardware_in.conf".into()]);

        assert!(stale_process_is_active_effect_child(
            &active_helper,
            &active_pids,
            &active_config_markers
        ));
        assert!(stale_process_is_active_effect_child(
            &supervised_pipewire_child,
            &active_pids,
            &active_config_markers
        ));
        assert!(!stale_process_is_active_effect_child(
            &unrelated_old_chain,
            &active_pids,
            &active_config_markers
        ));
    }

    #[test]
    fn repair_requires_loopback_endpoint_match() {
        let config = MixerConfig::default();
        let command = plan_ensure_graph(&config)
            .commands
            .into_iter()
            .find(|command| {
                command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains("wavelinux.channel_id=game")
                        && arg.contains("wavelinux.mix_id=stream")
                }) && command
                    .args
                    .iter()
                    .any(|arg| arg == "sink=wavelinux_mix_stream")
            })
            .unwrap();
        let wrong_endpoint = ManagedModule {
            module_id: "1".into(),
            role: Some("channel_to_mix".into()),
            channel_id: Some("game".into()),
            mix_id: Some("stream".into()),
            route_revision: Some(channel_mix_route_revision(
                &config.settings,
                config
                    .channels
                    .iter()
                    .find(|channel| channel.id == "game")
                    .unwrap(),
                config.mixes.iter().find(|mix| mix.id == "stream").unwrap(),
            )),
            node_name: Some("wavelinux_channel_game.monitor".into()),
            source_name: Some("wavelinux_channel_game.monitor".into()),
            sink_name: Some("wavelinux_mix_monitor".into()),
        };
        let hydrated_route = wavelinux_pw::SourceOutputRoute {
            id: "91".into(),
            module_id: Some("1".into()),
            role: Some("channel_to_mix".into()),
            channel_id: Some("game".into()),
            mix_id: Some("stream".into()),
            muted: Some(false),
            volume_percent: Some(100),
            source_id: Some("55".into()),
            source_name: Some("wavelinux_channel_game.monitor".into()),
            target_object: Some("wavelinux_channel_game".into()),
            application_name: None,
            node_name: None,
            media_name: None,
            managed: None,
            dont_move: false,
        };
        let wrong_sink_input = sink_input_for_module(&wrong_endpoint);

        assert!(!repair_command_is_satisfied(
            &command,
            &running_graph_for_config(&config),
            std::slice::from_ref(&hydrated_route),
            std::slice::from_ref(&wrong_sink_input),
            std::slice::from_ref(&wrong_endpoint)
        ));
    }

    #[test]
    fn repair_accepts_matching_loopback_endpoint() {
        let config = MixerConfig::default();
        let command = plan_ensure_graph(&config)
            .commands
            .into_iter()
            .find(|command| {
                command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains("wavelinux.channel_id=game")
                        && arg.contains("wavelinux.mix_id=stream")
                }) && command
                    .args
                    .iter()
                    .any(|arg| arg == "sink=wavelinux_mix_stream")
            })
            .unwrap();
        let matching_endpoint = ManagedModule {
            module_id: "1".into(),
            role: Some("channel_to_mix".into()),
            channel_id: Some("game".into()),
            mix_id: Some("stream".into()),
            route_revision: Some(channel_mix_route_revision(
                &config.settings,
                config
                    .channels
                    .iter()
                    .find(|channel| channel.id == "game")
                    .unwrap(),
                config.mixes.iter().find(|mix| mix.id == "stream").unwrap(),
            )),
            node_name: Some("wavelinux_channel_game.monitor".into()),
            source_name: Some("wavelinux_channel_game.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };

        let graph = running_graph_for_config(&config);
        let source_output = source_output_for_module(&matching_endpoint);
        let sink_input = sink_input_for_module(&matching_endpoint);

        assert!(repair_command_is_satisfied(
            &command,
            &graph,
            std::slice::from_ref(&source_output),
            std::slice::from_ref(&sink_input),
            std::slice::from_ref(&matching_endpoint)
        ));
    }

    #[test]
    fn repair_requires_matching_role_for_public_mic_passthrough_source() {
        let config = MixerConfig::default();
        let command = plan_ensure_graph(&config)
            .commands
            .into_iter()
            .find(|command| {
                command
                    .args
                    .iter()
                    .any(|arg| arg == "source_name=wavelinux-mic")
                    && command
                        .args
                        .iter()
                        .any(|arg| arg.contains("wavelinux.role=mic_passthrough"))
            })
            .unwrap();
        let stale_fx_source = ManagedModule {
            module_id: "old-fx".into(),
            role: Some("effect_output".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: None,
            node_name: Some("wavelinux-mic".into()),
            source_name: None,
            sink_name: None,
        };
        let matching_passthrough = ManagedModule {
            module_id: "passthrough".into(),
            role: Some("mic_passthrough".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: None,
            node_name: Some("wavelinux-mic".into()),
            source_name: None,
            sink_name: None,
        };
        let graph = running_graph_for_config(&config);

        assert!(!repair_command_is_satisfied(
            &command,
            &graph,
            &[],
            &[],
            std::slice::from_ref(&stale_fx_source),
        ));
        assert!(repair_command_is_satisfied(
            &command,
            &graph,
            &[],
            &[],
            std::slice::from_ref(&matching_passthrough),
        ));
    }

    #[test]
    fn repair_requires_both_loopback_halves() {
        let config = MixerConfig::default();
        let command = plan_ensure_graph(&config)
            .commands
            .into_iter()
            .find(|command| {
                command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains("wavelinux.channel_id=game")
                        && arg.contains("wavelinux.mix_id=stream")
                }) && command
                    .args
                    .iter()
                    .any(|arg| arg == "sink=wavelinux_mix_stream")
            })
            .unwrap();
        let module = routing_modules_for_config(&config)
            .into_iter()
            .find(|module| {
                module.role.as_deref() == Some("channel_to_mix")
                    && module.channel_id.as_deref() == Some("game")
                    && module.mix_id.as_deref() == Some("stream")
            })
            .unwrap();
        let graph = running_graph_for_config(&config);
        let source_output = source_output_for_module(&module);
        let sink_input = sink_input_for_module(&module);
        let unrelated_source_output = SourceOutputRoute {
            module_id: Some("unrelated".into()),
            role: Some("channel_to_mix".into()),
            channel_id: Some("music".into()),
            mix_id: Some("stream".into()),
            ..source_output.clone()
        };
        let unrelated_sink_input = SinkInputRoute {
            module_id: Some("unrelated".into()),
            role: Some("channel_to_mix".into()),
            channel_id: Some("music".into()),
            mix_id: Some("stream".into()),
            ..sink_input.clone()
        };

        assert!(!repair_command_is_satisfied(
            &command,
            &graph,
            std::slice::from_ref(&source_output),
            std::slice::from_ref(&unrelated_sink_input),
            std::slice::from_ref(&module),
        ));
        assert!(!repair_command_is_satisfied(
            &command,
            &graph,
            std::slice::from_ref(&unrelated_source_output),
            std::slice::from_ref(&sink_input),
            std::slice::from_ref(&module),
        ));
        assert!(repair_command_is_satisfied(
            &command,
            &graph,
            std::slice::from_ref(&source_output),
            std::slice::from_ref(&sink_input),
            std::slice::from_ref(&module),
        ));
    }

    #[test]
    fn duplicate_modules_share_dedupe_key() {
        let config = MixerConfig::default();
        let first = ManagedModule {
            module_id: "1".into(),
            role: Some("channel_to_mix".into()),
            channel_id: Some("game".into()),
            mix_id: Some("stream".into()),
            route_revision: None,
            node_name: Some("wavelinux_channel_game.monitor".into()),
            source_name: Some("wavelinux_channel_game.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };
        let second = ManagedModule {
            module_id: "2".into(),
            ..first.clone()
        };

        assert_eq!(
            module_dedupe_key_for_config(&first, &config),
            module_dedupe_key_for_config(&second, &config)
        );
    }

    #[test]
    fn route_health_reports_duplicate_channel_mix_route() {
        let config = MixerConfig::default();
        let graph = running_graph_for_config(&config);
        let route = routing_modules_for_config(&config)
            .into_iter()
            .find(|module| {
                module.role.as_deref() == Some("channel_to_mix")
                    && module.channel_id.as_deref() == Some("game")
                    && module.mix_id.as_deref() == Some("stream")
            })
            .unwrap();
        let duplicate = ManagedModule {
            module_id: "duplicate".into(),
            ..route.clone()
        };
        let source_output = source_output_for_module(&route);
        let sink_input = sink_input_for_module(&route);

        let issues = route_health_issues(
            &config,
            &graph,
            &[route, duplicate],
            std::slice::from_ref(&source_output),
            std::slice::from_ref(&sink_input),
        );

        assert!(issues.iter().any(|issue| {
            issue.module_id.as_deref() == Some("duplicate")
                && issue.role == "channel_to_mix"
                && issue.reason == RouteHealthReason::Duplicate
        }));
    }

    #[test]
    fn route_health_reports_missing_sink_input() {
        let config = MixerConfig::default();
        let graph = running_graph_for_config(&config);
        let route = routing_modules_for_config(&config)
            .into_iter()
            .find(|module| {
                module.role.as_deref() == Some("channel_to_mix")
                    && module.channel_id.as_deref() == Some("game")
                    && module.mix_id.as_deref() == Some("stream")
            })
            .unwrap();
        let source_output = source_output_for_module(&route);
        let unrelated_sink_input = SinkInputRoute {
            module_id: Some("unrelated".into()),
            role: Some("channel_to_mix".into()),
            channel_id: Some("music".into()),
            mix_id: Some("stream".into()),
            ..sink_input_for_module(&route)
        };

        let issues = route_health_issues(
            &config,
            &graph,
            std::slice::from_ref(&route),
            std::slice::from_ref(&source_output),
            std::slice::from_ref(&unrelated_sink_input),
        );

        assert_eq!(issues.len(), 1, "issues={issues:?}");
        assert_eq!(issues[0].reason, RouteHealthReason::MissingSinkInput);
        assert_eq!(issues[0].sink_name.as_deref(), Some("wavelinux_mix_stream"));
    }

    #[test]
    fn incremental_mix_sync_replaces_broken_route_without_touching_fx() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let mut graph = running_graph_for_config(&config);
        graph
            .outputs
            .push(device("alsa_output.speakers", "Speakers", false));
        let modules = routing_modules_for_config(&config);
        let browser_route = modules
            .iter()
            .find(|module| {
                module.role.as_deref() == Some("channel_to_mix")
                    && module.channel_id.as_deref() == Some("browser")
                    && module.mix_id.as_deref() == Some("monitor")
            })
            .unwrap();
        let source_outputs = modules
            .iter()
            .filter(|module| module.module_id != browser_route.module_id)
            .map(source_output_for_module)
            .collect::<Vec<_>>();
        let sink_inputs = modules
            .iter()
            .map(sink_input_for_module)
            .collect::<Vec<_>>();
        let issue = RouteHealthIssue {
            module_id: Some(browser_route.module_id.clone()),
            role: "channel_to_mix".into(),
            channel_id: Some("browser".into()),
            mix_id: Some("monitor".into()),
            source_name: browser_route.source_name.clone(),
            sink_name: browser_route.sink_name.clone(),
            reason: RouteHealthReason::MissingSourceOutput,
        };

        let outputs = engine
            .sync_active_mix_routes_unlocked(
                &config,
                IncrementalMixRouteView {
                    graph: &graph,
                    managed_modules: &modules,
                    source_outputs: &source_outputs,
                    sink_inputs: &sink_inputs,
                },
                &BTreeSet::from(["browser".to_string()]),
                &BTreeSet::from(["monitor".to_string()]),
                &[issue],
            )
            .unwrap();
        let descriptions = outputs
            .iter()
            .map(|output| output.command.description.as_str())
            .collect::<Vec<_>>();

        assert!(descriptions
            .iter()
            .any(|description| description.contains("unload managed channel_to_mix")));
        assert!(descriptions.contains(&"route 'Browser' to 'Monitor'"));
        assert!(descriptions
            .iter()
            .all(|description| !description.contains("effect chain")));
    }

    #[test]
    fn effect_route_health_filter_targets_only_synced_effect_channels() {
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        let graph = running_graph_for_config(&config);
        let modules = routing_modules_for_config(&config);
        let mut source_outputs = modules
            .iter()
            .map(source_output_for_module)
            .collect::<Vec<_>>();
        let sink_inputs = modules
            .iter()
            .map(sink_input_for_module)
            .collect::<Vec<_>>();
        source_outputs.retain(|route| {
            !(route.role.as_deref() == Some("channel_to_mix")
                && ((route.channel_id.as_deref() == Some("hardware_in")
                    && route.mix_id.as_deref() == Some("stream"))
                    || (route.channel_id.as_deref() == Some("music")
                        && route.mix_id.as_deref() == Some("monitor"))))
        });

        let issues = effect_route_health_issues_for_channels(
            &config,
            &graph,
            &modules,
            &source_outputs,
            &sink_inputs,
            &BTreeSet::from(["hardware_in".to_string()]),
        );

        assert_eq!(issues.len(), 1, "issues={issues:?}");
        assert_eq!(issues[0].role, "channel_to_mix");
        assert_eq!(issues[0].channel_id.as_deref(), Some("hardware_in"));
        assert_eq!(issues[0].mix_id.as_deref(), Some("stream"));
        assert_eq!(issues[0].reason, RouteHealthReason::MissingSourceOutput);
    }

    #[test]
    fn route_health_reports_stale_non_auto_channel_mix_route() {
        let config = MixerConfig::default();
        let graph = running_graph_for_config(&config);
        let mut route = routing_modules_for_config(&config)
            .into_iter()
            .find(|module| {
                module.role.as_deref() == Some("channel_to_mix")
                    && module.channel_id.as_deref() == Some("game")
                    && module.mix_id.as_deref() == Some("stream")
            })
            .unwrap();
        route.route_revision = Some("old-revision".into());
        let source_output = source_output_for_module(&route);
        let sink_input = sink_input_for_module(&route);

        let issues = route_health_issues(
            &config,
            &graph,
            std::slice::from_ref(&route),
            std::slice::from_ref(&source_output),
            std::slice::from_ref(&sink_input),
        );

        assert_eq!(issues.len(), 1, "issues={issues:?}");
        assert_eq!(issues[0].reason, RouteHealthReason::StaleConfig);
    }

    #[test]
    fn route_health_marks_inactive_app_mix_routes_stale_only_until_stream_is_active() {
        let config = MixerConfig::default();
        let graph = running_graph_for_config(&config);
        let modules = routing_modules_for_config(&config);
        let source_outputs = modules
            .iter()
            .map(source_output_for_module)
            .collect::<Vec<_>>();
        let sink_inputs = modules
            .iter()
            .map(sink_input_for_module)
            .collect::<Vec<_>>();

        let inactive_issues = route_health_issues_for_active_app_channels(
            &config,
            &graph,
            &modules,
            &source_outputs,
            &sink_inputs,
            &BTreeSet::new(),
        );
        assert!(inactive_issues.iter().any(|issue| {
            issue.role == "channel_to_mix"
                && issue.channel_id.as_deref() == Some("browser")
                && issue.reason == RouteHealthReason::StaleConfig
        }));
        assert!(!inactive_issues.iter().any(|issue| {
            issue.role == "channel_to_mix"
                && issue.channel_id.as_deref() == Some("hardware_in")
                && issue.reason == RouteHealthReason::StaleConfig
        }));

        let active_browser = BTreeSet::from(["browser".to_string()]);
        let active_issues = route_health_issues_for_active_app_channels(
            &config,
            &graph,
            &modules,
            &source_outputs,
            &sink_inputs,
            &active_browser,
        );
        assert!(!active_issues.iter().any(|issue| {
            issue.role == "channel_to_mix"
                && issue.channel_id.as_deref() == Some("browser")
                && issue.reason == RouteHealthReason::StaleConfig
        }));
        assert!(active_issues.iter().any(|issue| {
            issue.role == "channel_to_mix"
                && issue.channel_id.as_deref() == Some("music")
                && issue.reason == RouteHealthReason::StaleConfig
        }));
    }

    #[test]
    fn route_health_marks_muted_bus_and_inactive_mix_sends_stale() {
        let mut config = MixerConfig::default();
        let graph = running_graph_for_config(&config);
        let modules = routing_modules_for_config(&config);
        let source_outputs = modules
            .iter()
            .map(source_output_for_module)
            .collect::<Vec<_>>();
        let sink_inputs = modules
            .iter()
            .map(sink_input_for_module)
            .collect::<Vec<_>>();

        let monitor_only = BTreeSet::from(["monitor".to_string()]);
        let inactive_stream_issues = route_health_issues_for_active_routes(
            &config,
            &graph,
            &modules,
            &source_outputs,
            &sink_inputs,
            &BTreeSet::new(),
            &monitor_only,
        );
        assert!(inactive_stream_issues.iter().any(|issue| {
            issue.role == "channel_to_mix"
                && issue.channel_id.as_deref() == Some("hardware_in")
                && issue.mix_id.as_deref() == Some("stream")
                && issue.reason == RouteHealthReason::StaleConfig
        }));

        config.channels[0]
            .mix_buses
            .get_mut("monitor")
            .unwrap()
            .muted = true;
        let active_mixes = BTreeSet::from(["monitor".to_string(), "stream".to_string()]);
        let muted_bus_issues = route_health_issues_for_active_routes(
            &config,
            &graph,
            &modules,
            &source_outputs,
            &sink_inputs,
            &BTreeSet::new(),
            &active_mixes,
        );
        assert!(muted_bus_issues.iter().any(|issue| {
            issue.role == "channel_to_mix"
                && issue.channel_id.as_deref() == Some("hardware_in")
                && issue.mix_id.as_deref() == Some("monitor")
                && issue.reason == RouteHealthReason::StaleConfig
        }));
    }

    #[test]
    fn inactive_monitor_output_is_stale_until_an_app_feeds_the_mix() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let mut graph = running_graph_for_config(&config);
        graph
            .outputs
            .push(device("alsa_output.speakers", "Speakers", false));
        let modules = routing_modules_for_config(&config);
        let source_outputs = modules
            .iter()
            .map(source_output_for_module)
            .collect::<Vec<_>>();
        let sink_inputs = modules
            .iter()
            .map(sink_input_for_module)
            .collect::<Vec<_>>();

        let idle_mixes = active_mix_ids_for_routes(&config, &graph, &source_outputs, &sink_inputs);
        assert!(!idle_mixes.contains("monitor"));
        let idle_issues = route_health_issues_for_active_routes(
            &config,
            &graph,
            &modules,
            &source_outputs,
            &sink_inputs,
            &BTreeSet::new(),
            &idle_mixes,
        );
        assert!(idle_issues.iter().any(|issue| {
            issue.role == "mix_monitor"
                && issue.mix_id.as_deref() == Some("monitor")
                && issue.reason == RouteHealthReason::StaleConfig
        }));

        graph.app_streams.push(AppStream {
            id: "browser-stream".into(),
            app_id: Some("browser".into()),
            binary: Some("browser".into()),
            process_name: Some("browser".into()),
            window_class: None,
            display_name: "Browser".into(),
            media_name: Some("Browser audio".into()),
            routed_channel_id: Some("browser".into()),
            volume: 1.0,
            muted: false,
        });
        let active_mixes =
            active_mix_ids_for_routes(&config, &graph, &source_outputs, &sink_inputs);
        assert!(active_mixes.contains("monitor"));
    }

    #[test]
    fn wavelinux6_keeps_all_mix_bridges_active_without_consumers() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");

        let active_mixes = active_mix_ids_for_routes(&config, &RuntimeGraph::default(), &[], &[]);

        assert_eq!(
            active_mixes,
            BTreeSet::from(["monitor".into(), "stream".into()])
        );
    }

    #[test]
    fn newly_active_browser_routes_use_incremental_mix_sync() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let mut graph = running_graph_for_config(&config);
        graph
            .outputs
            .push(device("alsa_output.speakers", "Speakers", false));
        graph.app_streams.push(AppStream {
            id: "brave-stream".into(),
            app_id: Some("brave-browser".into()),
            binary: Some("brave-browser".into()),
            process_name: Some("brave".into()),
            window_class: None,
            display_name: "Brave".into(),
            media_name: Some("YouTube".into()),
            routed_channel_id: Some("browser".into()),
            volume: 1.0,
            muted: false,
        });
        let active_channels = active_app_channel_ids_for_graph(&config, &graph);
        let active_mixes = BTreeSet::from(["monitor".to_string()]);
        let mut modules = routing_modules_for_config(&config)
            .into_iter()
            .filter(|module| {
                !(module.role.as_deref() == Some("mix_monitor")
                    && module.mix_id.as_deref() == Some("monitor")
                    || module.role.as_deref() == Some("channel_to_mix")
                        && module.channel_id.as_deref() == Some("browser")
                        && module.mix_id.as_deref() == Some("monitor"))
            })
            .collect::<Vec<_>>();
        modules.extend(config.mixes.iter().map(|mix| ManagedModule {
            module_id: format!("{}-source", mix.id),
            role: Some("mix_source".into()),
            channel_id: None,
            mix_id: Some(mix.id.clone()),
            route_revision: None,
            node_name: Some(mix.virtual_source_name.clone()),
            source_name: None,
            sink_name: None,
        }));
        let hardware_channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        modules.push(ManagedModule {
            module_id: "hardware-mic-source".into(),
            role: Some("mic_passthrough".into()),
            channel_id: Some(hardware_channel.id.clone()),
            mix_id: None,
            route_revision: None,
            node_name: Some(effect_chain_source_name(hardware_channel)),
            source_name: None,
            sink_name: None,
        });
        let source_outputs = modules
            .iter()
            .map(source_output_for_module)
            .collect::<Vec<_>>();
        let sink_inputs = modules
            .iter()
            .map(sink_input_for_module)
            .collect::<Vec<_>>();

        let needed = plan_ensure_graph_for_active_routes(&config, &active_channels, &active_mixes)
            .commands
            .into_iter()
            .filter(|command| {
                !repair_command_is_satisfied(
                    command,
                    &graph,
                    &source_outputs,
                    &sink_inputs,
                    &modules,
                )
            })
            .map(|command| command.description)
            .collect::<Vec<_>>();
        assert!(
            route_changes_are_incremental_mix_only(
                &config,
                IncrementalMixRouteView {
                    graph: &graph,
                    managed_modules: &modules,
                    source_outputs: &source_outputs,
                    sink_inputs: &sink_inputs,
                },
                &active_channels,
                &active_mixes,
                &[],
            ),
            "needed={needed:?}"
        );

        graph
            .outputs
            .retain(|output| output.name != "wavelinux_mix_monitor");
        assert!(!route_changes_are_incremental_mix_only(
            &config,
            IncrementalMixRouteView {
                graph: &graph,
                managed_modules: &modules,
                source_outputs: &source_outputs,
                sink_inputs: &sink_inputs,
            },
            &active_channels,
            &active_mixes,
            &[],
        ));
    }

    #[test]
    fn pulse_subscription_wakes_for_stream_and_device_events_only() {
        assert!(audio_subscription_event_relevant(
            "Event 'new' on sink-input #42"
        ));
        assert!(audio_subscription_event_relevant(
            "Event 'change' on sink-input #42"
        ));
        assert!(audio_subscription_event_relevant(
            "Event 'change' on card #7"
        ));
        assert!(!audio_subscription_event_relevant(
            "Event 'change' on client #12"
        ));
        assert!(!audio_subscription_event_relevant("garbled output"));
        assert_eq!(
            parse_audio_subscription_event("Event 'new' on sink-input #42"),
            Some(AudioSubscriptionEvent::PlaybackStream)
        );
        assert_eq!(
            parse_audio_subscription_event("Event 'change' on source-output #5"),
            None
        );
        assert_eq!(
            parse_audio_subscription_event("Event 'change' on card #7"),
            Some(AudioSubscriptionEvent::Device)
        );
    }

    #[test]
    fn pulse_subscription_coalescing_preserves_the_broadest_refresh() {
        let (tx, rx) = mpsc::sync_channel(4);
        tx.try_send(AudioSubscriptionEvent::PlaybackStream).unwrap();
        tx.try_send(AudioSubscriptionEvent::Device).unwrap();
        assert_eq!(
            coalesce_audio_subscription_events(AudioSubscriptionEvent::PlaybackStream, &rx),
            AudioSubscriptionEvent::Device
        );
    }

    #[test]
    fn route_health_reports_muted_mix_monitor_route() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let mut graph = running_graph_for_config(&config);
        graph
            .outputs
            .push(device("alsa_output.speakers", "Speakers", false));
        let route = routing_modules_for_config(&config)
            .into_iter()
            .find(|module| {
                module.role.as_deref() == Some("mix_monitor")
                    && module.mix_id.as_deref() == Some("monitor")
            })
            .unwrap();
        let mut source_output = source_output_for_module(&route);
        let mut sink_input = sink_input_for_module(&route);
        source_output.muted = Some(true);
        sink_input.muted = Some(true);

        let issues = route_health_issues(
            &config,
            &graph,
            std::slice::from_ref(&route),
            std::slice::from_ref(&source_output),
            std::slice::from_ref(&sink_input),
        );

        assert_eq!(issues.len(), 1, "issues={issues:?}");
        assert_eq!(issues[0].reason, RouteHealthReason::LevelMismatch);
        assert_eq!(issues[0].role, "mix_monitor");
    }

    #[test]
    fn route_health_repair_is_suppressed_during_effect_sync() {
        let engine = test_engine();
        let issue = RouteHealthIssue {
            module_id: Some("route".into()),
            role: "channel_to_mix".into(),
            channel_id: Some("hardware_in".into()),
            mix_id: Some("stream".into()),
            source_name: Some("wavelinux-mic".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
            reason: RouteHealthReason::MissingSource,
        };

        let guard = engine.mark_effect_sync_active();
        assert!(!engine.route_health_repair_allowed(std::slice::from_ref(&issue)));
        drop(guard);

        assert!(engine.route_health_repair_allowed(&[issue]));
    }

    #[test]
    fn managed_route_level_commands_unmute_restored_monitor_route() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let route = routing_modules_for_config(&config)
            .into_iter()
            .find(|module| {
                module.role.as_deref() == Some("mix_monitor")
                    && module.mix_id.as_deref() == Some("monitor")
            })
            .unwrap();
        let mut source_output = source_output_for_module(&route);
        let mut sink_input = sink_input_for_module(&route);
        source_output.muted = Some(true);
        source_output.volume_percent = Some(82);
        sink_input.muted = Some(true);
        sink_input.volume_percent = Some(0);

        let commands = managed_route_level_commands(
            &config,
            std::slice::from_ref(&source_output),
            std::slice::from_ref(&sink_input),
        );
        let args = commands
            .iter()
            .map(|command| command.args.clone())
            .collect::<Vec<_>>();

        assert!(args.contains(&vec![
            "set-sink-input-mute".into(),
            sink_input.id.clone(),
            "0".into()
        ]));
        assert!(args.contains(&vec![
            "set-sink-input-volume".into(),
            sink_input.id.clone(),
            "100%".into()
        ]));
        assert!(args.contains(&vec![
            "set-source-output-mute".into(),
            source_output.id.clone(),
            "0".into()
        ]));
        assert!(args.contains(&vec![
            "set-source-output-volume".into(),
            source_output.id.clone(),
            "100%".into()
        ]));
    }

    #[test]
    fn graph_sink_level_commands_skip_converged_sinks() {
        let config = MixerConfig::default();
        let mut levels = BTreeMap::new();
        for mix in &config.mixes {
            levels.insert(
                mix.virtual_sink_name.clone(),
                SinkLevelState {
                    volume_percent: Some((mix.volume.clamp(0.0, 1.0) * 100.0).round() as u8),
                    muted: mix.muted,
                },
            );
        }
        for channel in &config.channels {
            levels.insert(
                channel.virtual_sink_name.clone(),
                SinkLevelState {
                    volume_percent: Some(100),
                    muted: false,
                },
            );
        }

        assert!(graph_sink_level_commands(&config, &levels).is_empty());

        let mix = &config.mixes[0];
        let level = levels.get_mut(&mix.virtual_sink_name).unwrap();
        level.volume_percent = Some(12);
        level.muted = !mix.muted;
        let commands = graph_sink_level_commands(&config, &levels);
        assert_eq!(commands.len(), 2);
        assert!(commands
            .iter()
            .all(|command| command.args.get(1) == Some(&mix.virtual_sink_name)));
    }

    #[test]
    fn stale_managed_route_stream_level_command_is_skipped() {
        let command = plan_set_route_sink_input_volume("gone-stream", 1.0);
        assert_eq!(command_stream_id(&command), Some("gone-stream"));
        let output = command_execution_with_spec(
            command.clone(),
            Err(PwError::CommandFailed {
                program: "pactl".into(),
                args: command.args.clone(),
                stderr: "Failure: No such entity".into(),
            }),
        );
        let output = ignore_stale_stream_command(output, "gone-stream");

        assert!(output.skipped);
        assert_eq!(output.error, None);
        assert_eq!(
            output.stderr,
            "stream gone-stream disappeared before the command could apply"
        );
    }

    #[test]
    fn stale_capture_stream_move_is_skipped_without_failure_backoff() {
        let engine = test_engine();
        let command = plan_move_capture_stream_to_source("gone-capture", "wavelinux-mic");
        assert_eq!(command_stream_id(&command), Some("gone-capture"));
        let output = command_execution_with_stale_stream_skip(
            command.clone(),
            Err(PwError::CommandFailed {
                program: "pactl".into(),
                args: command.args.clone(),
                stderr: "Failure: No such entity".into(),
            }),
        );

        engine
            .remember_failed_capture_moves(&[(
                "gone-capture".into(),
                "alsa_input.usb_mic->wavelinux-mic".into(),
                output.clone(),
            )])
            .unwrap();

        assert!(output.skipped);
        assert_eq!(output.error, None);
        assert!(!engine
            .capture_move_recently_failed("gone-capture", "alsa_input.usb_mic->wavelinux-mic"));
    }

    fn pulse_capture_route() -> SourceOutputRoute {
        SourceOutputRoute {
            id: "99".into(),
            module_id: None,
            role: None,
            channel_id: None,
            mix_id: None,
            muted: Some(false),
            volume_percent: Some(100),
            source_id: Some("55".into()),
            source_name: Some("alsa_input.usb_mic".into()),
            target_object: None,
            application_name: Some("Discord".into()),
            node_name: Some("Discord input".into()),
            media_name: Some("RecordStream".into()),
            managed: None,
            dont_move: false,
        }
    }

    fn register_pulse_capture_stream(engine: &WaveLinuxEngine) {
        engine.pipewire_registry.mark_connected(false);
        engine.pipewire_registry.apply_batch(vec![
            serde_json::json!({
                "id": 8,
                "type": "PipeWire:Interface:Client",
                "info": {"props": {"client.api": "pipewire-pulse"}}
            }),
            serde_json::json!({
                "id": 21,
                "type": "PipeWire:Interface:Node",
                "info": {"props": {
                    "media.class": "Audio/Source",
                    "node.name": "wavelinux-mic",
                    "object.serial": 501
                }}
            }),
            serde_json::json!({
                "id": 31,
                "type": "PipeWire:Interface:Node",
                "info": {"props": {
                    "media.class": "Stream/Input/Audio",
                    "node.name": "discord-input",
                    "client.id": 8,
                    "object.serial": 99
                }}
            }),
        ]);
    }

    #[test]
    fn capture_stream_removed_from_registry_is_skipped_before_pactl() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config.settings.lock_default_input = true;
        let route = pulse_capture_route();
        register_pulse_capture_stream(&engine);
        assert_eq!(
            engine
                .pipewire_registry
                .capture_route_backend("99", "wavelinux-mic"),
            Some(StreamRouteBackend::PulseCompatibility)
        );

        engine
            .pipewire_registry
            .apply_batch(vec![serde_json::json!({"id": 31, "info": null})]);
        assert_eq!(
            engine
                .pipewire_registry
                .capture_route_backend("99", "wavelinux-mic"),
            None
        );

        let outputs = engine
            .execute_capture_stream_moves_unlocked_with_devices(
                &config,
                std::slice::from_ref(&route),
                &[],
                &[],
            )
            .unwrap();

        assert!(outputs.is_empty());
        assert!(!engine.capture_move_recently_failed("99", "alsa_input.usb_mic->wavelinux-mic"));
        assert!(fs::read_to_string(engine.paths.log_file())
            .unwrap()
            .contains(
                "stream=99 skipped because it is no longer present in the PipeWire registry"
            ));
    }

    #[test]
    fn pulse_compatible_capture_stream_move_still_uses_pactl() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config.settings.lock_default_input = true;
        let route = pulse_capture_route();
        register_pulse_capture_stream(&engine);

        let outputs = engine
            .execute_capture_stream_moves_unlocked_with_devices(
                &config,
                std::slice::from_ref(&route),
                &[],
                &[],
            )
            .unwrap();

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].command.program, "pactl");
        assert_eq!(
            outputs[0].command.args,
            ["move-source-output", "99", "wavelinux-mic"]
        );
        assert!(outputs[0].skipped);
        assert_eq!(outputs[0].error, None);
    }

    #[test]
    fn default_locks_choose_system_and_hardware_input_nodes() {
        let mut config = MixerConfig::default();
        assert_eq!(
            default_output_channel(&config).map(|channel| channel.virtual_sink_name.as_str()),
            Some("wavelinux_channel_system")
        );
        assert_eq!(
            default_input_source(&config).as_deref(),
            Some("wavelinux-mic")
        );

        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        assert_eq!(
            default_input_source(&config).as_deref(),
            Some("wavelinux-mic")
        );
    }

    #[test]
    fn default_input_keeps_public_mic_when_fx_nodes_are_missing() {
        let mut config = MixerConfig::default();
        config.settings.lock_default_input = true;
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();

        let graph = RuntimeGraph {
            inputs: vec![device(
                "wavelinux_channel_hardware_in.monitor",
                "Monitor of wavelinux-input",
                false,
            )],
            outputs: vec![device("wavelinux_channel_hardware_in", "Input", false)],
            app_streams: Vec::new(),
            meters: Vec::new(),
            auto_devices: Vec::new(),
            effect_availability: Vec::new(),
        };
        let effective = config_with_unavailable_effects_bypassed(&config, &graph);

        assert_eq!(
            default_input_source(&config).as_deref(),
            Some("wavelinux-mic")
        );
        assert_eq!(
            default_input_source(&effective).as_deref(),
            Some("wavelinux-mic")
        );
        assert!(default_input_lock_repair_needed(
            &effective,
            Some("wavelinux_channel_hardware_in.monitor")
        ));

        let route = SourceOutputRoute {
            id: "99".into(),
            module_id: None,
            role: None,
            channel_id: None,
            mix_id: None,
            muted: Some(false),
            volume_percent: Some(100),
            source_id: Some("55".into()),
            source_name: Some("alsa_input.usb_mic".into()),
            target_object: None,
            application_name: Some("Discord".into()),
            node_name: Some("Discord input".into()),
            media_name: Some("RecordStream".into()),
            managed: None,
            dont_move: false,
        };
        let commands = capture_stream_move_commands_to_locked_default_input(
            &effective,
            std::slice::from_ref(&route),
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].args,
            ["move-source-output", "99", "wavelinux-mic"]
        );
    }

    #[test]
    fn default_input_keeps_fx_source_when_fx_nodes_are_visible() {
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        let graph = graph_for_config(&config);
        let effective = config_with_unavailable_effects_bypassed(&config, &graph);

        assert_eq!(
            default_input_source(&effective).as_deref(),
            Some("wavelinux-mic")
        );
    }

    #[test]
    fn default_input_lock_repairs_when_system_default_mic_drifts() {
        let mut config = MixerConfig::default();
        assert!(!default_input_lock_repair_needed(
            &config,
            Some("alsa_input.usb_mic")
        ));

        config.settings.lock_default_input = true;
        assert!(default_input_lock_repair_needed(
            &config,
            Some("alsa_input.usb_mic")
        ));
        assert!(!default_input_lock_repair_needed(
            &config,
            Some("wavelinux-mic")
        ));

        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        assert!(default_input_lock_repair_needed(
            &config,
            Some("wavelinux_mix_stream_source")
        ));
        assert!(!default_input_lock_repair_needed(
            &config,
            Some("wavelinux-mic")
        ));
    }

    #[test]
    fn default_device_lock_drift_is_separate_from_route_repair() {
        let mut config = MixerConfig::default();
        config.settings.lock_default_input = true;
        let route_repair = auto_device_route_repair_needed(&config, None, None, &[], &[], &[]);
        let lock_repair =
            default_device_lock_repair_needed(&config, Some("alsa_input.usb_mic"), None);

        assert!(!route_repair);
        assert!(lock_repair);
    }

    #[test]
    fn default_input_lock_moves_live_capture_streams_to_wavelinux_mic() {
        let mut config = MixerConfig::default();
        let route = SourceOutputRoute {
            id: "99".into(),
            module_id: None,
            role: None,
            channel_id: None,
            mix_id: None,
            muted: Some(false),
            volume_percent: Some(100),
            source_id: Some("55".into()),
            source_name: Some("alsa_input.usb_mic".into()),
            target_object: None,
            application_name: Some("Discord".into()),
            node_name: Some("Discord input".into()),
            media_name: Some("RecordStream".into()),
            managed: None,
            dont_move: false,
        };

        assert!(capture_stream_move_commands_to_locked_default_input(
            &config,
            std::slice::from_ref(&route)
        )
        .is_empty());

        config.settings.lock_default_input = true;
        let commands = capture_stream_move_commands_to_locked_default_input(
            &config,
            std::slice::from_ref(&route),
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].args,
            ["move-source-output", "99", "wavelinux-mic"]
        );

        let already_routed = SourceOutputRoute {
            source_name: Some("wavelinux-mic".into()),
            ..route.clone()
        };
        assert!(
            capture_stream_move_commands_to_locked_default_input(&config, &[already_routed])
                .is_empty()
        );

        let selected_stream_mix = SourceOutputRoute {
            source_name: Some("wavelinux_mix_stream_source".into()),
            target_object: Some("wavelinux_mix_stream_source".into()),
            ..route.clone()
        };
        assert!(capture_stream_move_commands_to_locked_default_input(
            &config,
            &[selected_stream_mix]
        )
        .is_empty());

        let selected_stream_monitor = SourceOutputRoute {
            source_name: Some("wavelinux_mix_stream.monitor".into()),
            target_object: Some("wavelinux_mix_stream.monitor".into()),
            ..route.clone()
        };
        assert!(capture_stream_move_commands_to_locked_default_input(
            &config,
            &[selected_stream_monitor]
        )
        .is_empty());

        let selected_browser_channel = SourceOutputRoute {
            source_name: Some("wavelinux_channel_browser.monitor".into()),
            target_object: Some("wavelinux_channel_browser.monitor".into()),
            ..route.clone()
        };
        assert!(capture_stream_move_commands_to_locked_default_input(
            &config,
            &[selected_browser_channel]
        )
        .is_empty());

        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        let commands = capture_stream_move_commands_to_locked_default_input(
            &config,
            std::slice::from_ref(&route),
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].args,
            ["move-source-output", "99", "wavelinux-mic"]
        );

        let wavelinux_owned = SourceOutputRoute {
            source_name: Some("alsa_input.usb_mic".into()),
            application_name: Some("WaveLinux filter-chain".into()),
            ..route.clone()
        };
        assert!(
            capture_stream_move_commands_to_locked_default_input(&config, &[wavelinux_owned])
                .is_empty()
        );

        let loopback_route = SourceOutputRoute {
            node_name: Some("input.loopback-2169-33".into()),
            media_name: Some("loopback-2169-33 input".into()),
            ..route.clone()
        };
        assert!(
            capture_stream_move_commands_to_locked_default_input(&config, &[loopback_route])
                .is_empty()
        );

        let audio_core_capture = SourceOutputRoute {
            source_name: Some("alsa_input.usb_mic".into()),
            application_name: Some("WaveLinux 6".into()),
            node_name: Some("wavelinux6-input-target-hardware_in".into()),
            media_name: Some("WaveLinux 6 Input hardware input".into()),
            managed: Some("1".into()),
            dont_move: true,
            ..route.clone()
        };
        assert!(capture_stream_move_commands_to_locked_default_input(
            &config,
            &[audio_core_capture]
        )
        .is_empty());

        let native_meter = SourceOutputRoute {
            dont_move: true,
            application_name: None,
            node_name: Some("pipewire-native-meter".into()),
            media_name: Some("Capture".into()),
            ..route
        };
        assert!(
            capture_stream_move_commands_to_locked_default_input(&config, &[native_meter])
                .is_empty()
        );
    }

    #[test]
    fn default_output_lock_repairs_when_system_default_sink_drifts() {
        let mut config = MixerConfig::default();
        config.settings.lock_default_output = true;

        assert!(default_output_lock_repair_needed(
            &config,
            Some("alsa_output.speaker")
        ));
        assert!(!default_output_lock_repair_needed(
            &config,
            Some("wavelinux_channel_system")
        ));
    }

    #[test]
    fn default_output_guard_respects_unlocked_output_defaults() {
        let mut config = MixerConfig::default();
        config.settings.lock_default_output = false;
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.usb_cm01".into()))
            .unwrap();

        assert!(!default_output_lock_repair_needed(
            &config,
            Some("alsa_output.usb_cm01")
        ));
        assert!(!default_output_lock_repair_needed(
            &config,
            Some("wavelinux_mix_monitor")
        ));
        assert!(!default_output_lock_repair_needed(
            &config,
            Some("wavelinux_channel_system")
        ));
        assert!(!default_output_lock_repair_needed(
            &config,
            Some("wavelinux_channel_game")
        ));
    }

    #[test]
    fn default_device_restore_ignores_wavelinux_nodes() {
        assert!(is_restorable_device("alsa_output.speaker"));
        assert!(!is_restorable_device("wavelinux_channel_system"));
        assert!(!is_restorable_device("WAVELINUX_mix_stream_source"));
    }

    #[test]
    fn auto_output_overrides_saved_monitor_output_for_graph() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.old".into()))
            .unwrap();
        config.settings.monitor_follows_default_output = true;

        let effective = effective_config_with_auto_devices(
            &config,
            &[],
            &[],
            None,
            Some("bluez_output.sony".into()),
            &[],
        );

        let monitor = effective
            .mixes
            .iter()
            .find(|mix| mix.id == "monitor")
            .unwrap();
        assert_eq!(monitor.monitor_output.as_deref(), Some("bluez_output.sony"));
        assert_eq!(
            effective.device_policy.preferred_output.as_deref(),
            Some("bluez_output.sony")
        );
    }

    #[test]
    fn followed_monitor_output_persists_auto_selected_real_output() {
        let engine = test_engine();
        let mut saved = MixerConfig::default();
        saved
            .set_mix_monitor_output("monitor", Some("bluez_output.dead".into()))
            .unwrap();
        saved.settings.monitor_follows_default_output = true;
        saved.device_policy.preferred_output = Some("bluez_output.dead".into());
        saved.device_policy.active_output_fallback = true;

        {
            let mut config = engine.write_config().unwrap();
            *config = saved.clone();
        }
        engine.persist_config().unwrap();

        let mut effective = saved.clone();
        effective
            .set_mix_monitor_output("monitor", Some("alsa_output.speaker".into()))
            .unwrap();
        effective.device_policy.preferred_output = Some("alsa_output.speaker".into());
        effective.device_policy.active_output_fallback = false;

        engine
            .persist_followed_monitor_output_selection(&saved, &effective)
            .unwrap();

        let config = engine.read_config().unwrap();
        let monitor = config.mixes.iter().find(|mix| mix.id == "monitor").unwrap();
        assert_eq!(
            monitor.monitor_output.as_deref(),
            Some("alsa_output.speaker")
        );
        assert_eq!(
            config.device_policy.preferred_output.as_deref(),
            Some("alsa_output.speaker")
        );
        assert!(!config.device_policy.active_output_fallback);
    }

    #[test]
    fn profiled_devices_raise_runtime_route_latency_floor() {
        let mut config = MixerConfig::default();
        config.settings.low_latency_mic_monitoring = true;
        let realtek_policy = LatencyPolicy {
            stable_msec: Some(60),
            low_latency_msec: Some(35),
            bluetooth_floor_msec: None,
        };
        let mut input = device(
            "alsa_input.realtek",
            "Realtek ALC3254 Digital Microphone",
            false,
        );
        input.active_latency_policy = Some(realtek_policy.clone());
        let mut output = device("alsa_output.realtek", "Realtek ALC3254 Speaker", false);
        output.active_latency_policy = Some(realtek_policy);
        let inputs = vec![input];
        let outputs = vec![output];

        let effective = effective_config_with_profiled_devices(
            &config,
            &inputs,
            &outputs,
            &[],
            None,
            None,
            Some("alsa_output.realtek"),
        );
        let plan = plan_ensure_graph(&effective);

        let runtime_latency = effective
            .settings
            .runtime_latency_policy
            .as_ref()
            .expect("profile latency policy should be resolved for graph planning");
        assert_eq!(runtime_latency.stable_msec, Some(60));
        assert_eq!(runtime_latency.low_latency_msec, Some(35));
        assert_eq!(runtime_latency.bluetooth_floor_msec, Some(240));
        assert!(plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=35".into())
                && command
                    .args
                    .iter()
                    .any(|arg| arg.contains("wavelinux.role=mix_monitor"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=60".into())
                && command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains("wavelinux.channel_id=music")
                })
        }));

        let stale_low_latency_route = ManagedModule {
            module_id: "1".into(),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            route_revision: Some("1-latency-20".into()),
            node_name: None,
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            sink_name: Some("alsa_output.realtek".into()),
        };
        assert!(auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &inputs,
                outputs: &outputs,
                bluetooth_cards: &[],
                default_source: None,
                default_sink: None,
                active_sink: Some("alsa_output.realtek"),
                managed_modules: &[stale_low_latency_route],
                source_outputs: &[],
                sink_inputs: &[],
            }
        ));

        let hardware_channel = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        let monitor_mix = effective
            .mixes
            .iter()
            .find(|mix| mix.id == "monitor")
            .unwrap();
        let current_input_route = ManagedModule {
            module_id: "2".into(),
            role: Some("input_to_channel".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: Some(input_route_revision(&effective.settings, hardware_channel)),
            node_name: None,
            source_name: Some("alsa_input.realtek".into()),
            sink_name: Some(hardware_channel.virtual_sink_name.clone()),
        };
        let current_profile_latency_route = ManagedModule {
            module_id: "3".into(),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            route_revision: Some(mix_monitor_route_revision_for_sink(
                &effective.settings,
                monitor_mix,
                "alsa_output.realtek",
            )),
            node_name: None,
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            sink_name: Some("alsa_output.realtek".into()),
        };
        let source_outputs = vec![
            source_output_for_module(&current_input_route),
            source_output_for_module(&current_profile_latency_route),
        ];
        assert!(!auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &inputs,
                outputs: &outputs,
                bluetooth_cards: &[],
                default_source: None,
                default_sink: None,
                active_sink: Some("alsa_output.realtek"),
                managed_modules: &[current_input_route, current_profile_latency_route],
                source_outputs: &source_outputs,
                sink_inputs: &[],
            }
        ));
    }

    #[test]
    fn bluetooth_output_latency_policy_does_not_inherit_input_profile_floors() {
        let mut config = MixerConfig::default();
        config.settings.low_latency_mic_monitoring = true;
        let input_policy = LatencyPolicy {
            stable_msec: Some(160),
            low_latency_msec: Some(120),
            bluetooth_floor_msec: None,
        };
        let output_policy = LatencyPolicy {
            stable_msec: Some(45),
            low_latency_msec: Some(25),
            bluetooth_floor_msec: Some(80),
        };
        let mut input = device(
            "alsa_input.realtek",
            "Realtek ALC3254 Digital Microphone",
            true,
        );
        input.active_latency_policy = Some(input_policy);
        let mut output = device("bluez_output.sony", "WH-1000XM4 Bluetooth", true);
        output.active_latency_policy = Some(output_policy);
        let inputs = vec![input];
        let outputs = vec![output];

        let effective = effective_config_with_profiled_devices(
            &config,
            &inputs,
            &outputs,
            &[],
            Some("alsa_input.realtek"),
            Some("bluez_output.sony"),
            Some("bluez_output.sony"),
        );
        let plan = plan_ensure_graph(&effective);
        let runtime_latency = effective
            .settings
            .runtime_latency_policy
            .as_ref()
            .expect("output profile latency policy should be used for playback planning");

        assert_eq!(runtime_latency.stable_msec, Some(45));
        assert_eq!(runtime_latency.low_latency_msec, Some(25));
        assert_eq!(runtime_latency.bluetooth_floor_msec, Some(80));
        assert!(plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=25".into())
                && command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains("wavelinux.channel_id=browser")
                        && arg.contains("wavelinux.mix_id=monitor")
                })
        }));
        assert!(plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=80".into())
                && command
                    .args
                    .iter()
                    .any(|arg| arg.contains("wavelinux.role=mix_monitor"))
        }));
        assert!(!plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=160".into())
                && command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains("wavelinux.mix_id=monitor")
                })
        }));
    }

    #[test]
    fn hardware_direct_monitoring_disables_when_wave_xlr_is_not_available() {
        let mut config = MixerConfig::default();
        config.settings.hardware_direct_mic_monitoring = true;
        config
            .set_channel_input("hardware_in", Some("alsa_input.usb_mic".into()))
            .unwrap();
        let inputs = vec![device("alsa_input.usb_mic", "USB Microphone", true)];

        let effective =
            effective_config_with_profiled_devices(&config, &inputs, &[], &[], None, None, None);
        let plan = plan_ensure_graph(&effective);

        assert!(!effective.settings.hardware_direct_mic_monitoring);
        assert!(plan_has_channel_to_mix_route(
            &plan,
            "hardware_in",
            "monitor"
        ));
    }

    #[test]
    fn hardware_direct_monitoring_skips_monitor_route_for_available_wave_xlr() {
        let wave_xlr_source = "alsa_input.usb-Elgato_Wave_XLR.analog-stereo";
        let mut config = MixerConfig::default();
        config.settings.hardware_direct_mic_monitoring = true;
        config
            .set_channel_input("hardware_in", Some(wave_xlr_source.into()))
            .unwrap();
        let mut input = device(wave_xlr_source, "Elgato Wave XLR", true);
        input.matched_profile_id = Some("elgato.wave-xlr".into());

        let effective = effective_config_with_profiled_devices(
            &config,
            &[input],
            &[],
            &[],
            Some(wave_xlr_source),
            None,
            None,
        );
        let plan = plan_ensure_graph(&effective);

        assert!(effective.settings.hardware_direct_mic_monitoring);
        assert!(!plan_has_channel_to_mix_route(
            &plan,
            "hardware_in",
            "monitor"
        ));
        assert!(plan_has_channel_to_mix_route(
            &plan,
            "hardware_in",
            "stream"
        ));
    }

    #[test]
    fn hardware_direct_monitoring_disables_when_saved_wave_xlr_is_missing() {
        let wave_xlr_source = "alsa_input.usb-Elgato_Wave_XLR.analog-stereo";
        let mut config = MixerConfig::default();
        config.settings.hardware_direct_mic_monitoring = true;
        config
            .set_channel_input("hardware_in", Some(wave_xlr_source.into()))
            .unwrap();
        let inputs = vec![device("alsa_input.usb_mic", "USB Microphone", true)];

        let effective =
            effective_config_with_profiled_devices(&config, &inputs, &[], &[], None, None, None);
        let plan = plan_ensure_graph(&effective);

        assert_eq!(
            effective
                .channels
                .iter()
                .find(|channel| channel.id == "hardware_in")
                .unwrap()
                .source_device
                .as_deref(),
            Some("alsa_input.usb_mic")
        );
        assert!(!effective.settings.hardware_direct_mic_monitoring);
        assert!(plan_has_channel_to_mix_route(
            &plan,
            "hardware_in",
            "monitor"
        ));
    }

    #[test]
    fn auto_output_requests_repair_when_monitor_loopback_targets_old_sink() {
        let config = MixerConfig::default();
        let old_route = ManagedModule {
            module_id: "1".into(),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            route_revision: Some(mix_monitor_route_revision_for_sink(
                &config.settings,
                config.mixes.iter().find(|mix| mix.id == "monitor").unwrap(),
                "alsa_output.old",
            )),
            node_name: None,
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            sink_name: Some("alsa_output.old".into()),
        };
        let current_route = ManagedModule {
            module_id: "2".into(),
            sink_name: Some("bluez_output.sony".into()),
            route_revision: Some(mix_monitor_route_revision_for_sink(
                &config.settings,
                config.mixes.iter().find(|mix| mix.id == "monitor").unwrap(),
                "bluez_output.sony",
            )),
            ..old_route.clone()
        };
        let live_current_route = source_output_for_module(&current_route);

        assert!(auto_output_repair_needed(
            &config,
            Some("bluez_output.sony"),
            &[old_route],
            &[],
            &[]
        ));
        assert!(!auto_output_repair_needed(
            &config,
            Some("bluez_output.sony"),
            std::slice::from_ref(&current_route),
            std::slice::from_ref(&live_current_route),
            &[],
        ));
    }

    #[test]
    fn auto_output_repairs_when_monitor_loopback_module_has_no_live_source_output() {
        let config = MixerConfig::default();
        let current_route = ManagedModule {
            module_id: "2".into(),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            route_revision: Some(mix_monitor_route_revision_for_sink(
                &config.settings,
                config.mixes.iter().find(|mix| mix.id == "monitor").unwrap(),
                "bluez_output.sony",
            )),
            node_name: None,
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            sink_name: Some("bluez_output.sony".into()),
        };
        let live_route = SourceOutputRoute {
            id: "91".into(),
            module_id: Some("2".into()),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            muted: Some(false),
            volume_percent: Some(100),
            source_id: Some("55".into()),
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            target_object: Some("wavelinux_mix_monitor".into()),
            application_name: None,
            node_name: None,
            media_name: None,
            managed: None,
            dont_move: false,
        };
        let unrelated_live_route = SourceOutputRoute {
            id: "92".into(),
            module_id: Some("unrelated".into()),
            role: Some("channel_to_mix".into()),
            channel_id: Some("music".into()),
            mix_id: Some("monitor".into()),
            muted: Some(false),
            volume_percent: Some(100),
            source_id: Some("56".into()),
            source_name: Some("wavelinux_channel_music.monitor".into()),
            target_object: Some("wavelinux_mix_monitor".into()),
            application_name: None,
            node_name: None,
            media_name: None,
            managed: None,
            dont_move: false,
        };

        assert!(auto_output_repair_needed(
            &config,
            Some("bluez_output.sony"),
            std::slice::from_ref(&current_route),
            std::slice::from_ref(&unrelated_live_route),
            &[],
        ));
        assert!(!auto_output_repair_needed(
            &config,
            Some("bluez_output.sony"),
            std::slice::from_ref(&current_route),
            std::slice::from_ref(&live_route),
            &[],
        ));
    }

    #[test]
    fn bluetooth_monitor_route_refreshes_when_output_identity_changes() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("bluez_output.sony".into()))
            .unwrap();
        let mut output = device("bluez_output.sony", "WH-1000XM4", false);
        output
            .pipewire_properties
            .insert("object.serial".into(), "new-serial".into());
        output.active_profile = Some("a2dp-sink".into());
        output.active_codec = Some("aac".into());
        let monitor_mix = config.mixes.iter().find(|mix| mix.id == "monitor").unwrap();
        let route = ManagedModule {
            module_id: "1".into(),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            route_revision: Some(mix_monitor_route_revision_for_sink(
                &config.settings,
                monitor_mix,
                "bluez_output.sony",
            )),
            node_name: None,
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            sink_name: Some("bluez_output.sony".into()),
        };
        let runtime = RuntimeCache {
            bluetooth_monitor_routes: BTreeMap::from([(
                "monitor".into(),
                BluetoothMonitorRouteSignature {
                    output: "bluez_output.sony".into(),
                    serial: Some("old-serial".into()),
                    profile: Some("a2dp-sink".into()),
                    codec: Some("aac".into()),
                },
            )]),
            ..RuntimeCache::new(false)
        };

        assert!(bluetooth_monitor_route_refresh_needed(
            &runtime,
            &config,
            &[output.clone()],
            std::slice::from_ref(&route),
        ));

        let runtime = RuntimeCache {
            bluetooth_monitor_routes: bluetooth_monitor_route_signatures(
                &config,
                std::slice::from_ref(&output),
            ),
            ..RuntimeCache::new(false)
        };
        assert!(!bluetooth_monitor_route_refresh_needed(
            &runtime,
            &config,
            &[output],
            &[route],
        ));
    }

    #[test]
    fn bluetooth_monitor_route_refreshes_duplicate_final_routes() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("bluez_output.sony".into()))
            .unwrap();
        let mut output = device("bluez_output.sony", "WH-1000XM4", false);
        output
            .pipewire_properties
            .insert("object.serial".into(), "serial".into());
        let runtime = RuntimeCache {
            bluetooth_monitor_routes: bluetooth_monitor_route_signatures(
                &config,
                std::slice::from_ref(&output),
            ),
            ..RuntimeCache::new(false)
        };
        let monitor_mix = config.mixes.iter().find(|mix| mix.id == "monitor").unwrap();
        let route = ManagedModule {
            module_id: "1".into(),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            route_revision: Some(mix_monitor_route_revision_for_sink(
                &config.settings,
                monitor_mix,
                "bluez_output.sony",
            )),
            node_name: None,
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            sink_name: Some("bluez_output.sony".into()),
        };
        let duplicate = ManagedModule {
            module_id: "2".into(),
            ..route.clone()
        };

        assert!(bluetooth_monitor_route_refresh_needed(
            &runtime,
            &config,
            &[output],
            &[route, duplicate],
        ));
    }

    #[test]
    fn auto_device_repair_ignores_non_device_route_staleness() {
        let config = MixerConfig::default();
        let outputs = vec![device("alsa_output.speaker", "Built-in Speaker", true)];
        let effective = effective_config_with_profiled_devices(
            &config,
            &[],
            &outputs,
            &[],
            None,
            Some("alsa_output.speaker"),
            Some("alsa_output.speaker"),
        );
        let monitor_mix = effective
            .mixes
            .iter()
            .find(|mix| mix.id == "monitor")
            .unwrap();
        let current_monitor_route = ManagedModule {
            module_id: "monitor".into(),
            role: Some("mix_monitor".into()),
            channel_id: None,
            mix_id: Some("monitor".into()),
            route_revision: Some(mix_monitor_route_revision_for_sink(
                &effective.settings,
                monitor_mix,
                "alsa_output.speaker",
            )),
            node_name: None,
            source_name: Some("wavelinux_mix_monitor.monitor".into()),
            sink_name: Some("alsa_output.speaker".into()),
        };
        let stale_music_route = ManagedModule {
            module_id: "music-monitor".into(),
            role: Some("channel_to_mix".into()),
            channel_id: Some("music".into()),
            mix_id: Some("monitor".into()),
            route_revision: Some("1-latency-1".into()),
            node_name: None,
            source_name: Some("wavelinux_channel_music.monitor".into()),
            sink_name: Some("wavelinux_mix_monitor".into()),
        };
        let source_outputs = vec![source_output_for_module(&current_monitor_route)];

        assert!(!auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &[],
                outputs: &outputs,
                bluetooth_cards: &[],
                default_source: None,
                default_sink: Some("alsa_output.speaker"),
                active_sink: Some("alsa_output.speaker"),
                managed_modules: &[current_monitor_route, stale_music_route],
                source_outputs: &source_outputs,
                sink_inputs: &[],
            }
        ));
    }

    #[test]
    fn auto_output_prefers_usb_then_bluetooth_then_jack_then_speaker() {
        let outputs = vec![
            device("alsa_output.speaker", "Built-in Speakers", false),
            device("alsa_output.pci_headphones", "Headphones", false),
            device("alsa_output.usb_dac", "USB Audio DAC", false),
            device("bluez_output.sony", "WH-1000XM4 Bluetooth", false),
        ];

        assert_eq!(
            best_monitor_output(&outputs).as_deref(),
            Some("alsa_output.usb_dac")
        );
        assert_eq!(
            best_monitor_output(&outputs[..3]).as_deref(),
            Some("alsa_output.usb_dac")
        );
        assert_eq!(
            best_monitor_output(&outputs[..2]).as_deref(),
            Some("alsa_output.pci_headphones")
        );
        assert_eq!(
            best_monitor_output(&outputs[..1]).as_deref(),
            Some("alsa_output.speaker")
        );
        assert_eq!(
            preferred_monitor_output(&outputs, Some("alsa_output.pci_headphones"), None).as_deref(),
            Some("alsa_output.usb_dac")
        );
        assert_eq!(
            preferred_monitor_output(&outputs, Some("wavelinux_channel_system"), None).as_deref(),
            Some("alsa_output.usb_dac")
        );
        assert_eq!(
            preferred_monitor_output(
                &outputs,
                Some("alsa_output.speaker"),
                Some("bluez_output.sony")
            )
            .as_deref(),
            Some("alsa_output.usb_dac")
        );
        let rotated_bluetooth = [device(
            "bluez_output.AC_80_0A_72_BD_10.a2dp-sink",
            "WH-1000XM4 Bluetooth",
            false,
        )];
        assert_eq!(
            preferred_monitor_output(
                &rotated_bluetooth,
                Some("bluez_output.AC:80:0A:72:BD:10.headset-head-unit"),
                None,
            )
            .as_deref(),
            Some("bluez_output.AC_80_0A_72_BD_10.a2dp-sink")
        );
    }

    #[test]
    fn stale_saved_input_falls_back_to_best_available_hardware() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.source_device = Some("alsa_input.dead".into());
        config.device_policy.preferred_input = Some("alsa_input.dead".into());
        let inputs = vec![
            device("alsa_input.pci_mic", "Built-in Microphone", true),
            device("alsa_input.usb_interface", "USB Audio Interface", false),
        ];

        let effective =
            effective_config_with_profiled_devices(&config, &inputs, &[], &[], None, None, None);
        let hardware = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        assert_eq!(
            hardware.source_device.as_deref(),
            Some("alsa_input.usb_interface")
        );
        assert_eq!(
            effective.device_policy.restorable_input.as_deref(),
            Some("alsa_input.dead")
        );
        assert!(effective.device_policy.active_input_fallback);
        assert!(auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &inputs,
                outputs: &[],
                bluetooth_cards: &[],
                default_source: None,
                default_sink: None,
                active_sink: None,
                managed_modules: &[],
                source_outputs: &[],
                sink_inputs: &[],
            }
        ));
    }

    #[test]
    fn available_manual_input_is_preserved_over_auto_candidate() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.source_device = Some("alsa_input.pci_mic".into());
        let inputs = vec![
            device("alsa_input.pci_mic", "Built-in Microphone", true),
            device("alsa_input.usb_interface", "USB Audio Interface", false),
        ];

        let effective =
            effective_config_with_profiled_devices(&config, &inputs, &[], &[], None, None, None);
        let hardware = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        assert_eq!(
            hardware.source_device.as_deref(),
            Some("alsa_input.pci_mic")
        );
        assert!(!effective.device_policy.active_input_fallback);
    }

    #[test]
    fn unavailable_alsa_headset_mono_manual_input_falls_back_temporarily() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.source_device = Some("alsa_input.pci_headset".into());
        let inputs = vec![
            unavailable_alsa_headset_mono("alsa_input.pci_headset"),
            device("alsa_input.pci_mic", "Digital Microphone", true),
        ];

        let effective =
            effective_config_with_profiled_devices(&config, &inputs, &[], &[], None, None, None);
        let hardware = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        assert_eq!(
            hardware.source_device.as_deref(),
            Some("alsa_input.pci_mic")
        );
        assert_eq!(
            effective.device_policy.restorable_input.as_deref(),
            Some("alsa_input.pci_headset")
        );
        assert!(effective.device_policy.active_input_fallback);
    }

    #[test]
    fn unavailable_usb_profiled_input_can_use_safe_availability_override() {
        let mut usb = device("alsa_input.usb_cm01", "CM01 Mono", false);
        usb.is_available = false;
        usb.bus = Some(wavelinux_model::DeviceBus::Usb);
        usb.vendor_id = Some("1234".into());
        usb.matched_profile_id = Some("ttgk.cm01".into());
        usb.profile_confidence = Some(wavelinux_model::ProfileConfidence::High);
        usb.active_routing_policy = Some(routing_policy_with_input_priority(80));

        assert_eq!(
            best_hardware_input(
                &[unavailable_alsa_headset_mono("alsa_input.pci_headset"), usb],
                &[],
            )
            .as_deref(),
            Some("alsa_input.usb_cm01")
        );
    }

    #[test]
    fn adaptive_latency_controller_uses_hysteresis() {
        let settings = wavelinux_model::AdaptiveLatencySettings::default();
        let mut controller = AdaptiveLatencyController::default();
        let start = controller.last_change + Duration::from_secs(2);

        let status = controller.update(
            &settings,
            AdaptiveLatencySignal::CpuPressure,
            0.92,
            0,
            0,
            start,
        );
        assert_eq!(status.target_msec, 40);
        assert_eq!(status.last_reason, "cpu_pressure");
        assert_eq!(status.cpu_pressure, 0.92);

        let status = controller.update(
            &settings,
            AdaptiveLatencySignal::AudioTrouble,
            0.30,
            2,
            128,
            start + Duration::from_secs(1),
        );
        assert_eq!(status.target_msec, 80);
        assert_eq!(status.last_reason, "audio_trouble");
        assert_eq!(status.pipewire_warning_delta, 2);

        let status = controller.update(
            &settings,
            AdaptiveLatencySignal::Clean,
            0.20,
            0,
            0,
            start + Duration::from_secs(10),
        );
        assert_eq!(status.target_msec, 80);

        let status = controller.update(
            &settings,
            AdaptiveLatencySignal::Clean,
            0.10,
            0,
            0,
            start + Duration::from_secs(45),
        );
        assert_eq!(status.target_msec, 60);
        assert_eq!(status.last_reason, "clean_recovery");
    }

    #[test]
    fn adaptive_quantum_learns_failed_recovery_floor_per_output() {
        let mut controller = AdaptiveQuantumController::default();
        let start = Instant::now();

        assert_eq!(
            controller.update(1024, 0, "alsa_output.usb", start),
            (1024, 0, false)
        );
        assert_eq!(
            controller.update(512, 0, "alsa_output.usb", start + Duration::from_secs(15)),
            (512, 0, false)
        );
        assert_eq!(
            controller.update(0, 0, "alsa_output.usb", start + Duration::from_secs(30)),
            (0, 0, false)
        );
        assert_eq!(
            controller.update(1024, 2, "alsa_output.usb", start + Duration::from_secs(31)),
            (1024, 512, true)
        );
        assert_eq!(
            controller.update(0, 0, "alsa_output.usb", start + Duration::from_secs(60)),
            (512, 512, false)
        );

        assert_eq!(
            controller.update(
                0,
                0,
                "alsa_output.bluetooth",
                start + Duration::from_secs(61)
            ),
            (0, 0, false)
        );
    }

    #[test]
    fn adaptive_quantum_floor_cache_round_trips_and_filters_invalid_entries() {
        let temp = tempfile::tempdir().expect("temporary engine root");
        let path = temp.path().join(ADAPTIVE_QUANTUM_FLOORS_FILE);
        let cache = AdaptiveQuantumFloorCache {
            version: ADAPTIVE_QUANTUM_FLOORS_VERSION,
            floors: BTreeMap::from([
                ("alsa_output.usb".into(), 512),
                ("invalid".into(), 300),
                ("<no-monitor-output>".into(), 1024),
            ]),
        };
        write_json(&path, &cache).expect("write learned floors");

        assert_eq!(
            load_adaptive_quantum_floors(&path).expect("load learned floors"),
            BTreeMap::from([("alsa_output.usb".into(), 512)])
        );
    }

    #[test]
    fn adaptive_latency_controller_raises_slowly_for_pipewire_trouble() {
        let settings = wavelinux_model::AdaptiveLatencySettings::default();
        let mut controller = AdaptiveLatencyController::default();
        let start = controller.last_change + Duration::from_secs(2);

        let status = controller.update(
            &settings,
            AdaptiveLatencySignal::PipeWireTrouble,
            0.20,
            1,
            0,
            start,
        );
        assert_eq!(status.target_msec, 28);
        assert_eq!(status.last_reason, "initial");

        let status = controller.update(
            &settings,
            AdaptiveLatencySignal::PipeWireTrouble,
            0.20,
            1,
            0,
            start + Duration::from_secs(3),
        );
        assert_eq!(status.target_msec, 40);
        assert_eq!(status.last_reason, "pipewire_trouble");
    }

    #[test]
    fn adaptive_latency_signal_ignores_uncorrelated_pipewire_warning() {
        let settings = wavelinux_model::AdaptiveLatencySettings {
            trigger_mode: wavelinux_model::AdaptiveLatencyTriggerMode::AudioOnly,
            ..Default::default()
        };
        let (signal, _cpu_pressure, pipewire_warning_delta, underrun_delta) =
            adaptive_latency_signal(&settings, &[], 0.20, 1, 0);

        assert_eq!(signal, AdaptiveLatencySignal::Clean);
        assert_eq!(pipewire_warning_delta, 1);
        assert_eq!(underrun_delta, 0);
    }

    #[test]
    fn adaptive_latency_signal_uses_correlated_pipewire_warning() {
        let settings = wavelinux_model::AdaptiveLatencySettings {
            trigger_mode: wavelinux_model::AdaptiveLatencyTriggerMode::AudioOnly,
            ..Default::default()
        };
        let (signal, _cpu_pressure, pipewire_warning_delta, underrun_delta) =
            adaptive_latency_signal(&settings, &[], 0.20, 1, 1);

        assert_eq!(signal, AdaptiveLatencySignal::PipeWireTrouble);
        assert_eq!(pipewire_warning_delta, 1);
        assert_eq!(underrun_delta, 0);
    }

    #[test]
    fn adaptive_latency_signal_uses_live_core_discontinuities() {
        let settings = wavelinux_model::AdaptiveLatencySettings {
            trigger_mode: wavelinux_model::AdaptiveLatencyTriggerMode::AudioOnly,
            ..Default::default()
        };
        let audio_core = vec![AudioCoreChannelStatus {
            channel_id: "hardware_in".into(),
            online: true,
            underrun_delta: 256,
            ..AudioCoreChannelStatus::default()
        }];

        let (signal, _cpu_pressure, pipewire_warning_delta, underrun_delta) =
            adaptive_latency_signal(&settings, &audio_core, 0.20, 0, 0);

        assert_eq!(signal, AdaptiveLatencySignal::AudioTrouble);
        assert_eq!(pipewire_warning_delta, 0);
        assert_eq!(underrun_delta, 256);
    }

    #[test]
    fn audio_core_integrity_diagnostics_report_contained_invalid_output() {
        let statuses = vec![AudioCoreChannelStatus {
            channel_id: "hardware_in".into(),
            online: true,
            non_finite_blocks: 2,
            non_finite_samples: 128,
            non_finite_effect_mask: 1,
            chain_recoveries: 1,
            ..AudioCoreChannelStatus::default()
        }];

        let diagnostics = audio_core_integrity_diagnostics(&statuses);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "audio_core.non_finite.hardware_in");
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Warning);
        assert!(diagnostics[0].message.contains("effect_mask=0x1"));
        assert!(diagnostics[0].message.contains("recoveries=1"));
    }

    #[test]
    fn audio_core_integrity_diagnostics_ignore_clean_online_endpoint() {
        let statuses = vec![AudioCoreChannelStatus {
            channel_id: "mix:stream".into(),
            online: true,
            ..AudioCoreChannelStatus::default()
        }];

        assert!(audio_core_integrity_diagnostics(&statuses).is_empty());
    }

    #[test]
    fn audio_core_integrity_diagnostics_report_offline_endpoint_error() {
        let statuses = vec![AudioCoreChannelStatus {
            channel_id: "mix:monitor".into(),
            error: Some("connection refused".into()),
            ..AudioCoreChannelStatus::default()
        }];

        let diagnostics = audio_core_integrity_diagnostics(&statuses);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "audio_core.offline.mix:monitor");
        assert!(diagnostics[0]
            .action
            .as_deref()
            .is_some_and(|action| action.contains("connection refused")));
    }

    #[test]
    fn pipewire_health_deltas_do_not_repeat_old_warnings() {
        let previous = PipeWireAudioHealthStatus {
            warning_events: 4,
            owned_events: 2,
            ..Default::default()
        };
        let current = PipeWireAudioHealthStatus {
            warning_events: 5,
            owned_events: 3,
            ..Default::default()
        };
        assert_eq!(pipewire_health_deltas(&previous, &current), (1, 1));
        assert_eq!(pipewire_health_deltas(&current, &current), (0, 0));
    }

    #[test]
    fn pipewire_health_tracker_counts_relevant_and_owned_events() {
        let tracker = PipeWireAudioHealthTracker::default();
        assert!(!tracker.observe_line("ordinary PipeWire status", "wavelinux6"));
        assert!(tracker.observe_line("wavelinux6-mic: out of buffers; resync", "wavelinux6"));
        assert!(tracker.observe_line("link failed to activate", "wavelinux6"));
        let status = tracker.snapshot();
        assert_eq!(status.warning_events, 2);
        assert_eq!(status.out_of_buffers, 1);
        assert_eq!(status.resyncs, 1);
        assert_eq!(status.link_failures, 1);
        assert_eq!(status.owned_events, 1);
        assert!(status.last_event_unix.is_some());
    }

    #[test]
    fn pipewire_profiler_uses_first_sample_as_baseline_and_counts_deltas() {
        let tracker = PipeWireAudioHealthTracker::default();
        assert!(!tracker.observe_profiler_line(
            "S   ID  QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "wavelinux6"
        ));
        assert!(!tracker.observe_profiler_line(
            "C 130 0 0 --- --- --- --- 0 wavelinux6-input-target",
            "wavelinux6"
        ));
        assert!(!tracker.observe_profiler_line(
            "S   ID  QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "wavelinux6"
        ));
        assert!(!tracker.observe_profiler_line(
            "R 130 256 48000 10us 20us 0.0 0.0 4 F32LE 2 48000 wavelinux6-input-target",
            "wavelinux6"
        ));
        assert!(!tracker.observe_profiler_line(
            "S   ID  QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "wavelinux6"
        ));
        assert!(!tracker.observe_profiler_line(
            "R 130 256 48000 10us 20us 0.0 0.0 4 F32LE 2 48000 wavelinux6-input-target",
            "wavelinux6"
        ));
        assert!(tracker.observe_profiler_line(
            "R 130 256 48000 10us 20us 0.0 0.0 6 F32LE 2 48000 wavelinux6-input-target",
            "wavelinux6"
        ));

        let status = tracker.snapshot();
        assert_eq!(status.profiler_samples, 3);
        assert_eq!(status.direct_errors, 2);
        assert_eq!(status.owned_direct_errors, 2);
        assert_eq!(status.warning_events, 2);
        assert_eq!(status.xruns, 2);
        assert_eq!(status.owned_events, 2);
    }

    #[test]
    fn pipewire_profiler_ignores_non_audio_nodes_with_error_counters() {
        let tracker = PipeWireAudioHealthTracker::default();
        for line in [
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
        ] {
            assert!(!tracker.observe_profiler_line(line, "wavelinux6"));
        }
        assert!(!tracker.observe_profiler_line(
            "R 210 0 0 131.5us 15.9us 0.05 0.01 18 BGRA 2100x1400 plasmashell",
            "wavelinux6"
        ));
        assert_eq!(tracker.snapshot().direct_errors, 0);
    }

    #[test]
    fn pipewire_profiler_uses_idle_error_changes_as_a_running_baseline() {
        let tracker = PipeWireAudioHealthTracker::default();
        for line in [
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
        ] {
            assert!(!tracker.observe_profiler_line(line, "wavelinux6"));
        }
        assert!(!tracker.observe_profiler_line(
            "I 61 512 48000 4us 5us 0.0 0.0 0 S32LE 2 48000 built-in-speaker",
            "wavelinux6"
        ));
        assert!(!tracker.observe_profiler_line(
            "I 61 512 48000 4us 5us 0.0 0.0 4 S32LE 2 48000 built-in-speaker",
            "wavelinux6"
        ));
        assert!(!tracker.observe_profiler_line(
            "R 61 512 48000 4us 5us 0.0 0.0 4 S32LE 2 48000 built-in-speaker",
            "wavelinux6"
        ));
        assert!(tracker.observe_profiler_line(
            "R 61 512 48000 4us 5us 0.0 0.0 5 S32LE 2 48000 built-in-speaker",
            "wavelinux6"
        ));

        let status = tracker.snapshot();
        assert_eq!(status.direct_errors, 1);
        assert_eq!(status.warning_events, 1);
    }

    #[test]
    fn pipewire_profiler_resets_baseline_when_a_node_id_is_reused() {
        let tracker = PipeWireAudioHealthTracker::default();
        for line in [
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
            "S ID QUANT RATE WAIT BUSY W/Q B/Q ERR FORMAT NAME",
        ] {
            assert!(!tracker.observe_profiler_line(line, "wavelinux6"));
        }
        assert!(!tracker.observe_profiler_line(
            "R 130 256 48000 10us 20us 0.0 0.0 9 F32LE 2 48000 old-node",
            "wavelinux6"
        ));
        assert!(!tracker.observe_profiler_line(
            "R 130 256 48000 10us 20us 0.0 0.0 1 F32LE 2 48000 wavelinux6-new-node",
            "wavelinux6"
        ));
        assert!(tracker.observe_profiler_line(
            "R 130 256 48000 10us 20us 0.0 0.0 2 F32LE 2 48000 wavelinux6-new-node",
            "wavelinux6"
        ));
        let status = tracker.snapshot();
        assert_eq!(status.direct_errors, 1);
        assert_eq!(status.owned_direct_errors, 1);
    }

    #[test]
    fn proc_stat_cpu_pressure_uses_busy_time_delta() {
        let previous = parse_proc_stat_cpu("cpu  100 0 50 800 50 0 0 0 0 0\n").unwrap();
        let current = parse_proc_stat_cpu("cpu  130 0 70 840 60 0 0 0 0 0\n").unwrap();
        let pressure = cpu_pressure_between(previous, current).unwrap();
        assert!((pressure - 0.5).abs() < 0.001);
    }

    #[test]
    fn proc_scheduler_pressure_uses_stalled_time_delta() {
        let total = parse_proc_pressure_total(
            "some avg10=12.50 avg60=4.00 avg300=1.00 total=1900000\n\
             full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
        )
        .unwrap();
        assert_eq!(total, 1_900_000);
        let pressure = stall_pressure_between(1_000_000, total, Duration::from_secs(1)).unwrap();
        assert!((pressure - 0.9).abs() < 0.001);
    }

    #[test]
    fn proc_load_pressure_normalizes_by_available_cpus() {
        let pressure = parse_proc_load_pressure("18.00 12.00 6.00 8/2400 1\n", 24).unwrap();
        assert!((pressure - 0.75).abs() < 0.001);
        assert_eq!(
            parse_proc_load_pressure("48.00 1.00 1.00 1/1 1\n", 24),
            Some(1.0)
        );
    }

    #[test]
    fn adaptive_latency_target_changes_do_not_change_route_revisions() {
        let mut config = MixerConfig::default();
        config.settings.low_latency_mic_monitoring = true;
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        let before = input_route_revision(&config.settings, channel);

        let settings = config.settings.adaptive_latency.clone();
        let mut controller = AdaptiveLatencyController::default();
        let _ = controller.update(
            &settings,
            AdaptiveLatencySignal::AudioTrouble,
            0.35,
            1,
            64,
            controller.last_change + Duration::from_secs(2),
        );
        let after = input_route_revision(&config.settings, channel);

        assert_eq!(before, after);
    }

    #[test]
    fn stale_saved_manual_output_falls_back_to_default_sink() {
        let mut config = MixerConfig::default();
        config.settings.monitor_follows_default_output = false;
        config
            .set_mix_monitor_output("monitor", Some("bluez_output.dead".into()))
            .unwrap();
        config.device_policy.preferred_output = Some("bluez_output.dead".into());
        let outputs = vec![device("alsa_output.speaker", "Built-in Speakers", true)];

        let effective = effective_config_with_profiled_devices(
            &config,
            &[],
            &outputs,
            &[],
            None,
            Some("alsa_output.speaker"),
            None,
        );
        let monitor = effective
            .mixes
            .iter()
            .find(|mix| mix.id == "monitor")
            .unwrap();

        assert_eq!(
            monitor.monitor_output.as_deref(),
            Some("alsa_output.speaker")
        );
        assert_eq!(
            effective.device_policy.restorable_output.as_deref(),
            Some("bluez_output.dead")
        );
        assert!(effective.device_policy.active_output_fallback);
        assert!(!auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &[],
                outputs: &outputs,
                bluetooth_cards: &[],
                default_source: None,
                default_sink: Some("alsa_output.speaker"),
                active_sink: None,
                managed_modules: &[],
                source_outputs: &[],
                sink_inputs: &[],
            }
        ));
    }

    #[test]
    fn available_manual_output_is_preserved_over_auto_candidate() {
        let mut config = MixerConfig::default();
        config.settings.monitor_follows_default_output = false;
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speaker".into()))
            .unwrap();
        let outputs = vec![
            device("alsa_output.speaker", "Built-in Speakers", true),
            device("bluez_output.sony", "WH-1000XM4 Bluetooth", false),
        ];

        let effective = effective_config_with_profiled_devices(
            &config,
            &[],
            &outputs,
            &[],
            None,
            Some("bluez_output.sony"),
            None,
        );
        let monitor = effective
            .mixes
            .iter()
            .find(|mix| mix.id == "monitor")
            .unwrap();

        assert_eq!(
            monitor.monitor_output.as_deref(),
            Some("alsa_output.speaker")
        );
        assert!(!effective.device_policy.active_output_fallback);
    }

    #[test]
    fn monitor_preroute_requires_available_source_and_output() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("bluez_output.sony".into()))
            .unwrap();
        let command = plan_ensure_graph(&config)
            .commands
            .into_iter()
            .find(command_is_mix_monitor_route)
            .unwrap();
        let mut graph = RuntimeGraph::default();
        graph.inputs.push(device(
            "wavelinux_mix_monitor.monitor",
            "Monitor of wavelinux-monitor",
            false,
        ));
        graph
            .outputs
            .push(device("bluez_output.sony", "WH-1000XM4", false));

        assert!(monitor_route_endpoints_available(&command, &graph));

        graph.outputs.clear();
        assert!(!monitor_route_endpoints_available(&command, &graph));
    }

    #[test]
    fn active_effect_repair_forces_effect_loopback_reroutes() {
        let route = CommandSpec::new(
            CommandDomain::Route,
            "pactl",
            [
                "load-module",
                "module-loopback",
                "source=wavelinux_channel_hardware_in.monitor",
                "sink=wavelinux_fx_hardware_in_input",
                "source_output_properties=wavelinux.managed=1 wavelinux.role=channel_to_effect wavelinux.channel_id=hardware_in wavelinux.route_revision=1-latency-20",
            ],
            "route input through FX",
        );
        let unrelated_route = CommandSpec::new(
            CommandDomain::Route,
            "pactl",
            [
                "load-module",
                "module-loopback",
                "source=wavelinux_channel_music.monitor",
                "sink=wavelinux_mix_monitor",
                "source_output_properties=wavelinux.managed=1 wavelinux.role=channel_to_mix wavelinux.channel_id=music wavelinux.mix_id=monitor wavelinux.route_revision=1-latency-20",
            ],
            "route music to monitor",
        );
        let active_effect_channels = BTreeSet::from(["hardware_in".to_string()]);

        assert!(command_routes_active_effect_channel(
            &route,
            &active_effect_channels
        ));
        assert!(!command_routes_active_effect_channel(
            &unrelated_route,
            &active_effect_channels
        ));
        assert!(!command_routes_active_effect_channel(
            &route,
            &BTreeSet::new()
        ));
    }

    #[test]
    fn auto_input_ignores_monitor_sources_and_repairs_hotplugged_hardware() {
        let mut config = MixerConfig::default();
        let hardware_in = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware_in.source_device = None;

        let inputs = vec![
            device("alsa_output.speaker.monitor", "Monitor of Speakers", false),
            device("bluez_input.headset", "Bluetooth Headset Microphone", true),
            device("alsa_input.pci_mic", "Built-in Microphone", true),
            device("alsa_input.pci_jack", "Front Mic Jack", false),
            device("alsa_input.usb_interface", "USB Audio Interface", false),
        ];
        let best = best_hardware_input(&inputs, &[]);
        assert_eq!(best.as_deref(), Some("alsa_input.usb_interface"));
        assert_eq!(
            best_hardware_input(&inputs[..4], &[]).as_deref(),
            Some("alsa_input.pci_jack")
        );
        assert_eq!(
            best_hardware_input(&inputs[..3], &[]).as_deref(),
            Some("alsa_input.pci_mic")
        );
        assert_eq!(
            best_hardware_input(&inputs[..2], &[]).as_deref(),
            Some("bluez_input.headset")
        );
        let mut unavailable_headset =
            device("alsa_input.pci_headset", "Headset Mono Microphone", true);
        unavailable_headset.is_available = false;
        assert_eq!(
            best_hardware_input(
                &[
                    unavailable_headset,
                    device("alsa_input.pci_mic", "Digital Microphone", false)
                ],
                &[],
            )
            .as_deref(),
            Some("alsa_input.pci_mic")
        );

        assert_eq!(
            best_hardware_input(
                &[
                    unavailable_alsa_headset_mono("alsa_input.pci_headset"),
                    device("alsa_input.pci_mic", "Digital Microphone", false),
                ],
                &[],
            )
            .as_deref(),
            Some("alsa_input.pci_mic")
        );

        let mut profiled_headset = unavailable_alsa_headset_mono("alsa_input.pci_headset");
        profiled_headset.active_routing_policy = Some(routing_policy_with_input_priority(95));
        let mut profiled_digital_mic = device("alsa_input.pci_mic", "Digital Microphone", false);
        profiled_digital_mic.active_routing_policy = Some(routing_policy_with_input_priority(58));
        let mut cm01 = device("alsa_input.usb_cm01", "CM01 Mono", true);
        cm01.bus = Some(wavelinux_model::DeviceBus::Usb);
        cm01.active_routing_policy = Some(routing_policy_with_input_priority(80));
        assert_eq!(
            best_hardware_input(&[profiled_headset, profiled_digital_mic, cm01], &[]).as_deref(),
            Some("alsa_input.usb_cm01")
        );

        let old_route = ManagedModule {
            module_id: "1".into(),
            role: Some("input_to_channel".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: Some(input_route_revision(
                &config.settings,
                config
                    .channels
                    .iter()
                    .find(|channel| channel.id == "hardware_in")
                    .unwrap(),
            )),
            node_name: None,
            source_name: Some("alsa_input.pci_mic".into()),
            sink_name: Some("wavelinux_channel_hardware_in".into()),
        };
        let current_route = ManagedModule {
            module_id: "2".into(),
            source_name: Some("alsa_input.usb_interface".into()),
            ..old_route.clone()
        };
        let live_current_route = source_output_for_module(&current_route);

        assert!(auto_input_repair_needed(
            &config,
            Some("alsa_input.usb_interface"),
            &[old_route],
            &[]
        ));
        assert!(!auto_input_repair_needed(
            &config,
            Some("alsa_input.usb_interface"),
            std::slice::from_ref(&current_route),
            std::slice::from_ref(&live_current_route),
        ));
    }

    #[test]
    fn auto_input_uses_priority_before_system_default_microphone() {
        let config = MixerConfig::default();
        let inputs = vec![
            device("alsa_input.pci_mic", "Built-in Microphone", true),
            device("alsa_input.usb_interface", "USB Audio Interface", false),
        ];

        let effective = effective_config_with_profiled_devices(
            &config,
            &inputs,
            &[],
            &[],
            Some("alsa_input.pci_mic"),
            None,
            None,
        );
        let hardware = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        assert_eq!(
            hardware.source_device.as_deref(),
            Some("alsa_input.usb_interface")
        );
    }

    #[test]
    fn auto_input_ignores_wavelinux_default_source_and_uses_hardware_ranking() {
        let config = MixerConfig::default();
        let inputs = vec![
            device("alsa_input.pci_mic", "Built-in Microphone", true),
            device("alsa_input.usb_interface", "USB Audio Interface", false),
        ];

        let effective = effective_config_with_profiled_devices(
            &config,
            &inputs,
            &[],
            &[],
            Some("wavelinux_mix_stream_source"),
            None,
            None,
        );
        let hardware = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        assert_eq!(
            hardware.source_device.as_deref(),
            Some("alsa_input.usb_interface")
        );
    }

    #[test]
    fn resolved_auto_input_reports_priority_selection() {
        let config = MixerConfig::default();
        let inputs = vec![
            device("alsa_input.pci_mic", "Built-in Microphone", true),
            device("alsa_input.usb_interface", "USB Audio Interface", false),
        ];

        let auto_devices = resolved_auto_devices_for_config(
            &config,
            &inputs,
            &[],
            &[],
            Some("wavelinux-mic"),
            None,
            None,
        );
        let input = auto_devices
            .iter()
            .find(|device| device.kind == AutoDeviceKind::Input)
            .unwrap();

        assert_eq!(input.channel_id.as_deref(), Some("hardware_in"));
        assert_eq!(input.device_id.as_deref(), Some("alsa_input.usb_interface"));
        assert_eq!(
            input.device_description.as_deref(),
            Some("USB Audio Interface")
        );
        assert_eq!(input.priority, Some(80));
        assert_eq!(input.reason, AutoDeviceReason::Priority);
    }

    #[test]
    fn unchanged_auto_output_keeps_stable_reason_metadata() {
        let config = MixerConfig::default();
        let outputs = vec![device(
            "alsa_output.usb_interface",
            "USB Audio Interface",
            true,
        )];
        let previous = resolved_auto_devices_for_config(
            &config,
            &[],
            &outputs,
            &[],
            None,
            Some("alsa_output.usb_interface"),
            None,
        );
        let mut next = resolved_auto_devices_for_config(
            &config,
            &[],
            &outputs,
            &[],
            None,
            None,
            Some("alsa_output.usb_interface"),
        );
        let before = next
            .iter()
            .find(|device| device.kind == AutoDeviceKind::Output)
            .unwrap();
        assert_eq!(before.reason, AutoDeviceReason::ActiveOutput);

        stabilize_auto_device_reasons(&previous, &mut next);

        let after = next
            .iter()
            .find(|device| device.kind == AutoDeviceKind::Output)
            .unwrap();
        assert_eq!(after.reason, AutoDeviceReason::SystemDefault);
    }

    #[test]
    fn auto_input_repair_triggers_when_higher_priority_device_appears() {
        let config = MixerConfig::default();
        let inputs = vec![
            device("alsa_input.pci_mic", "Built-in Microphone", true),
            device("alsa_input.usb_interface", "USB Audio Interface", false),
        ];
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        let stale_module = ManagedModule {
            module_id: "input-route".into(),
            role: Some("input_to_channel".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: Some(input_route_revision(&config.settings, channel)),
            node_name: None,
            source_name: Some("alsa_input.pci_mic".into()),
            sink_name: Some(channel.virtual_sink_name.clone()),
        };

        assert!(auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &inputs,
                outputs: &[],
                bluetooth_cards: &[],
                default_source: Some("wavelinux-mic"),
                default_sink: None,
                active_sink: None,
                managed_modules: std::slice::from_ref(&stale_module),
                source_outputs: &[source_output_for_module(&stale_module)],
                sink_inputs: &[],
            }
        ));
    }

    #[test]
    fn auto_input_repair_ignores_lower_priority_device_appearing() {
        let config = MixerConfig::default();
        let inputs = vec![
            device("alsa_input.usb_interface", "USB Audio Interface", true),
            device("alsa_input.pci_mic", "Built-in Microphone", false),
        ];
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        let current_module = ManagedModule {
            module_id: "input-route".into(),
            role: Some("input_to_channel".into()),
            channel_id: Some("hardware_in".into()),
            mix_id: None,
            route_revision: Some(input_route_revision(&config.settings, channel)),
            node_name: None,
            source_name: Some("alsa_input.usb_interface".into()),
            sink_name: Some(channel.virtual_sink_name.clone()),
        };

        assert!(!auto_device_route_repair_needed_for_profiled_devices(
            &config,
            ProfiledDeviceRepairView {
                inputs: &inputs,
                outputs: &[],
                bluetooth_cards: &[],
                default_source: Some("wavelinux-mic"),
                default_sink: None,
                active_sink: None,
                managed_modules: std::slice::from_ref(&current_module),
                source_outputs: &[source_output_for_module(&current_module)],
                sink_inputs: &[],
            }
        ));
    }

    #[test]
    fn bluetooth_headset_input_is_not_auto_selected_when_a2dp_is_available() {
        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("headset-head-unit".into()),
            preferred_a2dp_profile: Some("a2dp-sink".into()),
            profiles: Vec::new(),
        }];
        let inputs = vec![device(
            "bluez_input.AC:80:0A:72:BD:10",
            "WH-1000XM4 Bluetooth Headset Microphone",
            true,
        )];

        assert_eq!(best_hardware_input(&inputs, &cards), None);
        assert!(bluetooth_input_would_force_hfp(
            "bluez_input.AC:80:0A:72:BD:10",
            &cards
        ));
    }

    #[test]
    fn disconnected_bluetooth_cards_are_reinitialized_on_reconnect() {
        let mut runtime = RuntimeCache::new(true);
        runtime.initialized_bluetooth_cards.insert(
            "bluez_card.AC_80_0A_72_BD_10".into(),
            "a2dp-sink-aac".into(),
        );

        prune_initialized_bluetooth_cards(&mut runtime, &[]);

        assert!(runtime.initialized_bluetooth_cards.is_empty());

        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("a2dp-sink".into()),
            preferred_a2dp_profile: Some("a2dp-sink-aac".into()),
            profiles: Vec::new(),
        }];
        let commands =
            plan_bluetooth_a2dp_profiles(&cards, &runtime.initialized_bluetooth_cards, false);

        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].args,
            [
                "set-card-profile",
                "bluez_card.AC_80_0A_72_BD_10",
                "a2dp-sink-aac"
            ]
        );
    }

    #[test]
    fn bluetooth_protection_moves_capture_streams_off_hfp_source() {
        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("headset-head-unit".into()),
            preferred_a2dp_profile: Some("a2dp-sink".into()),
            profiles: Vec::new(),
        }];
        let route = SourceOutputRoute {
            id: "77".into(),
            module_id: None,
            role: None,
            channel_id: None,
            mix_id: None,
            muted: Some(false),
            volume_percent: Some(100),
            source_id: Some("55".into()),
            source_name: Some("bluez_input.AC:80:0A:72:BD:10".into()),
            target_object: None,
            application_name: Some("Discord".into()),
            node_name: Some("Discord input".into()),
            media_name: Some("RecordStream".into()),
            managed: None,
            dont_move: false,
        };

        let commands = capture_stream_move_commands_for_bluetooth_protection(
            std::slice::from_ref(&route),
            Some("alsa_input.usb_dji"),
            &cards,
        );

        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].args,
            ["move-source-output", "77", "alsa_input.usb_dji"]
        );

        let bluetooth_fallback = capture_stream_move_commands_for_bluetooth_protection(
            std::slice::from_ref(&route),
            Some("bluez_input.AC:80:0A:72:BD:10"),
            &cards,
        );
        assert!(bluetooth_fallback.is_empty());

        let fixed_native_stream = SourceOutputRoute {
            dont_move: true,
            ..route.clone()
        };
        assert!(capture_stream_move_commands_for_bluetooth_protection(
            &[fixed_native_stream],
            Some("alsa_input.usb_dji"),
            &cards,
        )
        .is_empty());

        let wavelinux_owned = SourceOutputRoute {
            application_name: Some("WaveLinux filter-chain".into()),
            ..route
        };
        assert!(capture_stream_move_commands_for_bluetooth_protection(
            &[wavelinux_owned],
            Some("alsa_input.usb_dji"),
            &cards,
        )
        .is_empty());
    }

    #[test]
    fn failed_capture_moves_are_backed_off_by_source_output_id() {
        let engine = test_engine();
        let route = SourceOutputRoute {
            id: "77".into(),
            module_id: None,
            role: None,
            channel_id: None,
            mix_id: None,
            muted: Some(false),
            volume_percent: Some(100),
            source_id: Some("55".into()),
            source_name: Some("alsa_input.usb_mic".into()),
            target_object: None,
            application_name: Some("Browser capture".into()),
            node_name: Some("browser-capture".into()),
            media_name: Some("CaptureStream".into()),
            managed: None,
            dont_move: false,
        };
        let failed_move = CommandExecution {
            command: plan_move_capture_stream_to_source("77", "wavelinux_mix_stream_source"),
            stdout: String::new(),
            stderr: String::new(),
            skipped: false,
            error: Some("Failure: Invalid argument".into()),
        };

        engine
            .remember_failed_capture_moves(&[(
                "77".into(),
                "alsa_input.usb_mic->wavelinux_mix_stream_source".into(),
                failed_move,
            )])
            .unwrap();
        assert!(engine
            .capture_move_recently_failed("77", "alsa_input.usb_mic->wavelinux_mix_stream_source"));
        assert!(!engine.capture_move_recently_failed(
            "77",
            "alsa_input.usb_other->wavelinux_mix_stream_source"
        ));

        let outputs = engine
            .execute_capture_stream_moves_unlocked_with_devices(
                &MixerConfig::default(),
                &[route],
                &[],
                &[],
            )
            .unwrap();

        assert!(outputs.is_empty());
    }

    #[test]
    fn failed_app_stream_moves_are_backed_off_by_stream_id() {
        let engine = test_engine();
        let failed_move = CommandExecution {
            command: plan_move_app_stream(
                "320089",
                engine
                    .read_config()
                    .unwrap()
                    .channels
                    .iter()
                    .find(|channel| channel.id == "game")
                    .unwrap(),
            ),
            stdout: String::new(),
            stderr: String::new(),
            skipped: false,
            error: Some("Failure: Invalid argument".into()),
        };

        engine
            .remember_app_stream_move_result("320089", &failed_move)
            .unwrap();

        assert!(engine.app_stream_move_recently_failed("320089"));

        let ok_move = CommandExecution {
            error: None,
            ..failed_move
        };
        engine
            .remember_app_stream_move_result("320089", &ok_move)
            .unwrap();
        assert!(!engine.app_stream_move_recently_failed("320089"));
    }

    #[test]
    fn effective_config_drops_bluetooth_input_that_would_force_hfp() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.source_device = Some("bluez_input.AC:80:0A:72:BD:10".into());

        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("headset-head-unit".into()),
            preferred_a2dp_profile: Some("a2dp-sink".into()),
            profiles: Vec::new(),
        }];
        let effective = effective_config_with_auto_devices(&config, &[], &[], None, None, &cards);
        let hardware = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        assert_eq!(hardware.source_device, None);
        assert!(effective.device_policy.active_input_fallback);
    }

    #[test]
    fn effect_chain_configs_are_written_and_pruned() {
        let engine = test_engine();
        let mut limiter = EffectInstance::new("limiter");
        limiter.instance_id = "limiter-1".into();

        engine
            .set_effect_chain("hardware_in".into(), vec![limiter.clone()])
            .unwrap();
        let path = engine
            .paths
            .effect_chains_dir()
            .join("wavelinux-chain-hardware_in.conf");
        let dsp_path = engine
            .paths
            .effect_chains_dir()
            .join("wavelinux-chain-hardware_in.json");
        engine.rebuild_effect_chain_configs().unwrap();
        let config = fs::read_to_string(&path).unwrap();
        assert!(config.contains("WaveLinux FX Input"));
        assert!(config.contains("limiter-1"));
        let dsp_config: wavelinux_dsp::DspChannelConfig =
            serde_json::from_str(&fs::read_to_string(&dsp_path).unwrap()).unwrap();
        assert_eq!(dsp_config.channel_id, "hardware_in");
        assert_eq!(dsp_config.input_node_name, "wavelinux_fx_hardware_in_input");
        assert_eq!(dsp_config.output_node_name, "wavelinux-mic");
        assert_eq!(dsp_config.property_prefix, "wavelinux");
        let socket_path = PathBuf::from(dsp_config.control_socket_path.unwrap());
        assert!(socket_path.starts_with(engine.paths.control_sockets_dir()));
        assert!(!socket_path.starts_with(engine.paths.data_dir.clone()));

        engine
            .bypass_effect("hardware_in".into(), limiter.instance_id, true)
            .unwrap();
        engine.rebuild_effect_chain_configs().unwrap();
        assert!(!path.exists());
        assert!(!dsp_path.exists());
    }

    #[test]
    fn native_mix_config_preserves_bus_policy_and_public_source() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_mix_outputs(
                "stream",
                vec![
                    "alsa_output.usb_primary".into(),
                    "bluez_output.secondary".into(),
                ],
            )
            .unwrap();
        config.channels[0]
            .mix_buses
            .get_mut("stream")
            .unwrap()
            .muted = true;
        config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "music")
            .unwrap()
            .mix_buses
            .get_mut("stream")
            .unwrap()
            .enabled = false;
        let stream = config.mixes.iter().find(|mix| mix.id == "stream").unwrap();

        let mix = dsp_mix_config(stream, &config);

        assert_eq!(mix.output_node_name, "wavelinux6_mix_stream_source");
        assert_eq!(
            mix.output_target_node_names,
            vec![
                "alsa_output.usb_primary".to_string(),
                "bluez_output.secondary".to_string(),
            ]
        );
        let expected_latency_msec = config
            .channels
            .iter()
            .filter(|channel| {
                channel
                    .mix_buses
                    .get("stream")
                    .is_some_and(|bus| bus.enabled)
            })
            .map(|channel| channel_mix_latency_msec(channel, stream, &config.settings))
            .max()
            .unwrap();
        assert_eq!(expected_latency_msec, 80);
        assert_eq!(mix.latency_frames, u32::from(expected_latency_msec) * 48);
        assert!(
            mix.buses
                .iter()
                .find(|bus| bus.channel_id == "hardware_in")
                .unwrap()
                .muted
        );
        assert!(
            !mix.buses
                .iter()
                .find(|bus| bus.channel_id == "music")
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn monitor_manifest_uses_persisted_quantum_floor_for_exact_output_set() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_mix_outputs("monitor", vec!["alsa_output.usb_cm01".into()])
            .unwrap();
        let monitor = config.mixes.iter().find(|mix| mix.id == "monitor").unwrap();
        let floors = BTreeMap::from([("alsa_output.usb_cm01".into(), 512)]);

        assert_eq!(learned_quantum_floor_for_mix(monitor, &floors), 512);
        let stream = config.mixes.iter().find(|mix| mix.id == "stream").unwrap();
        assert_eq!(learned_quantum_floor_for_mix(stream, &floors), 0);
    }

    #[test]
    fn audio_core_manifest_preserves_effective_input_target_and_mono_mode() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_channel_input("hardware_in", Some("alsa_input.usb_cm01_mono".into()))
            .unwrap();
        config
            .set_channel_input_mode("hardware_in", ChannelInputMode::SumMono)
            .unwrap();
        config
            .set_mix_outputs(
                "monitor",
                vec!["alsa_output.usb_cm01".into(), "bluez_output.xm4".into()],
            )
            .unwrap();

        engine
            .rebuild_effect_chain_configs_from_config(&config, "wavelinux6")
            .unwrap();
        let manifest: wavelinux_dsp::DspCoreManifest = serde_json::from_str(
            &fs::read_to_string(
                engine
                    .paths
                    .effect_chains_dir()
                    .join(AUDIO_CORE_MANIFEST_FILE),
            )
            .unwrap(),
        )
        .unwrap();
        let hardware = manifest
            .channels
            .iter()
            .find(|channel| channel.channel_id == "hardware_in")
            .unwrap();
        let monitor = manifest
            .mixes
            .iter()
            .find(|mix| mix.mix_id == "monitor")
            .unwrap();

        assert_eq!(
            hardware.input_target_node_name.as_deref(),
            Some("alsa_input.usb_cm01_mono")
        );
        assert!(hardware.input_target_capable);
        assert!(hardware.input_role.is_none());
        assert_eq!(hardware.input_mode, wavelinux_dsp::DspInputMode::SumMono);
        assert_eq!(hardware.input_channels, 1);
        assert_eq!(
            monitor.output_target_node_names,
            vec![
                "alsa_output.usb_cm01".to_string(),
                "bluez_output.xm4".to_string(),
            ]
        );
    }

    #[test]
    fn hardware_target_names_do_not_change_audio_core_topology_revision() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_channel_input("hardware_in", Some("alsa_input.first".into()))
            .unwrap();
        config
            .set_channel_input_mode("hardware_in", ChannelInputMode::SumMono)
            .unwrap();
        config
            .set_mix_outputs("monitor", vec!["alsa_output.first".into()])
            .unwrap();
        engine
            .rebuild_effect_chain_configs_from_config(&config, "wavelinux6")
            .unwrap();
        let manifest = engine
            .paths
            .effect_chains_dir()
            .join(AUDIO_CORE_MANIFEST_FILE);
        let first = audio_core_topology_revision(&manifest).unwrap();

        config
            .set_channel_input("hardware_in", Some("alsa_input.second".into()))
            .unwrap();
        config
            .set_mix_outputs("monitor", vec!["alsa_output.second".into()])
            .unwrap();
        engine
            .rebuild_effect_chain_configs_from_config(&config, "wavelinux6")
            .unwrap();
        let retargeted = audio_core_topology_revision(&manifest).unwrap();
        assert_eq!(retargeted, first);

        let mut latency_adjusted: wavelinux_dsp::DspCoreManifest =
            serde_json::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
        for channel in &mut latency_adjusted.channels {
            channel.latency_frames = channel.latency_frames.saturating_mul(2);
        }
        for mix in &mut latency_adjusted.mixes {
            mix.latency_frames = mix.latency_frames.saturating_mul(2);
        }
        write_json(&manifest, &latency_adjusted).unwrap();
        assert_eq!(audio_core_topology_revision(&manifest).unwrap(), first);

        config.set_channel_input("hardware_in", None).unwrap();
        engine
            .rebuild_effect_chain_configs_from_config(&config, "wavelinux6")
            .unwrap();
        assert_eq!(audio_core_topology_revision(&manifest).unwrap(), first);

        config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap()
            .name = "Renamed hardware input".into();
        engine
            .rebuild_effect_chain_configs_from_config(&config, "wavelinux6")
            .unwrap();
        assert_ne!(audio_core_topology_revision(&manifest).unwrap(), first);
    }

    #[test]
    fn persistent_core_target_sync_detects_target_changes_without_topology_repair() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_channel_input("hardware_in", Some("alsa_input.usb_new".into()))
            .unwrap();
        config
            .set_mix_outputs("monitor", vec!["alsa_output.usb_new".into()])
            .unwrap();
        let old_input = native_input_target_route("hardware_in", "alsa_input.usb_old");
        let old_output = native_mix_output_target_route("monitor", "alsa_output.usb_old");

        assert!(persistent_core_target_routes_need_sync(
            &config,
            std::slice::from_ref(&old_input),
            std::slice::from_ref(&old_output),
        ));

        let new_input = native_input_target_route("hardware_in", "alsa_input.usb_new");
        let new_output = native_mix_output_target_route("monitor", "alsa_output.usb_new");
        assert!(persistent_core_target_routes_need_sync(
            &config,
            &[old_input.clone(), new_input.clone()],
            &[old_output.clone(), new_output.clone()],
        ));
        assert!(!persistent_core_target_routes_need_sync(
            &config,
            std::slice::from_ref(&new_input),
            std::slice::from_ref(&new_output),
        ));

        config.set_channel_input("hardware_in", None).unwrap();
        assert!(persistent_core_target_routes_need_sync(
            &config,
            std::slice::from_ref(&new_input),
            std::slice::from_ref(&new_output),
        ));
        assert!(!persistent_core_target_routes_need_sync(
            &config,
            &[],
            std::slice::from_ref(&new_output),
        ));
    }

    #[test]
    fn wavelinux5_effect_chain_configs_migrate_deepfilternet_to_rnnoise() {
        let engine = test_engine();
        let mut deepfilter = EffectInstance::new("deepfilternet");
        deepfilter.instance_id = "deepfilter".into();
        let mut gate = EffectInstance::new("gate");
        gate.instance_id = "gate".into();

        engine
            .set_effect_chain("hardware_in".into(), vec![deepfilter, gate])
            .unwrap();
        let saved_config = engine.read_config().unwrap().clone();
        let channel = saved_config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            "Underrun detected (RTF: 1.50). Processing too slow!\n",
        )
        .unwrap();

        engine
            .rebuild_effect_chain_configs_for_runtime_prefix("wavelinux5")
            .unwrap();

        let path = engine
            .paths
            .effect_chains_dir()
            .join("wavelinux-chain-hardware_in.conf");
        let rendered = fs::read_to_string(&path).unwrap();
        assert!(rendered.contains("librnnoise_ladspa"));
        assert!(rendered.contains("noise_suppressor_stereo"));
        assert!(!rendered.contains("libdeep_filter_ladspa"));
        assert!(!rendered.contains("deepfilter"));
        assert!(rendered.contains("gate_1410"));

        let dsp_path = engine
            .paths
            .effect_chains_dir()
            .join("wavelinux-chain-hardware_in.json");
        let dsp_config: wavelinux_dsp::DspChannelConfig =
            serde_json::from_str(&fs::read_to_string(&dsp_path).unwrap()).unwrap();
        let bypassed = dsp_config
            .effects
            .iter()
            .map(|effect| (effect.effect_id.as_str(), effect.bypassed))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(bypassed.get("rnnoise"), Some(&false));
        assert_eq!(bypassed.get("gate"), Some(&false));
    }

    #[test]
    fn effect_edits_return_before_deferred_sync_writes_filter_chain() {
        let engine = test_engine();
        let path = engine
            .paths
            .effect_chains_dir()
            .join("wavelinux-chain-hardware_in.conf");

        engine
            .set_effect_chain("hardware_in".into(), vec![EffectInstance::new("limiter")])
            .unwrap();

        assert!(!path.exists());
        engine.rebuild_effect_chain_configs().unwrap();
        assert!(path.exists());
    }

    #[test]
    fn active_missing_effects_are_reported_in_diagnostics() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        let graph = RuntimeGraph {
            effect_availability: vec![wavelinux_model::EffectAvailability {
                effect_id: "limiter".into(),
                available: false,
                detail: "missing limiter plugin".into(),
            }],
            ..RuntimeGraph::default()
        };

        let diagnostics = engine.effect_chain_diagnostics(&config, &graph);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code.starts_with("effects.missing.hardware_in.")
                && diagnostic.severity == DiagnosticSeverity::Warning
                && diagnostic.message.contains("Limiter on Input")
        }));
    }

    #[test]
    fn effect_diagnostics_report_source_visibility() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        engine.rebuild_effect_chain_configs().unwrap();

        let missing_source = RuntimeGraph::default();
        let diagnostics = engine.effect_chain_diagnostics(&config, &missing_source);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "effects.source.hardware_in"
                && diagnostic.severity == DiagnosticSeverity::Warning
                && diagnostic.message.contains("not visible")
        }));

        let visible_source = RuntimeGraph {
            inputs: vec![device("wavelinux-mic", "WaveLinux-mic", false)],
            ..RuntimeGraph::default()
        };
        let diagnostics = engine.effect_chain_diagnostics(&config, &visible_source);
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "effects.source.hardware_in"
                && diagnostic.severity == DiagnosticSeverity::Info
        }));
    }

    #[test]
    fn recent_fx_chain_log_warnings_are_reported_in_diagnostics() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            "Underrun detected (RTF: 1.14). Processing too slow!\nPossible clipping detected (1.000).\n",
        )
        .unwrap();

        let diagnostics = engine.effect_chain_diagnostics(&config, &RuntimeGraph::default());

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.underrun.hardware_in"));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "effects.underrun.hardware_in"
                && diagnostic
                    .message
                    .contains("FX chain is missing realtime deadlines")
        }));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.clipping.hardware_in"));
    }

    #[test]
    fn native_fx_bridge_underrun_delta_is_reported_in_diagnostics() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            concat!(
                "wavelinux6-audio-core native_stats channel_id=hardware_in captured_frames=48000 rendered_frames=48000 dropped_frames=0 underrun_frames=0 process_calls=100 buffered_frames=1344 target_latency_msec=28 reason=initial\n",
                "wavelinux6-audio-core native_stats channel_id=hardware_in captured_frames=96000 rendered_frames=97024 dropped_frames=0 underrun_frames=64 process_calls=200 buffered_frames=64 target_latency_msec=28 reason=initial\n",
            ),
        )
        .unwrap();

        let diagnostics = engine.effect_chain_diagnostics(&config, &RuntimeGraph::default());

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "effects.underrun.hardware_in"
                && diagnostic.message.contains("underrun_frames=64")
        }));
    }

    #[test]
    fn flat_native_fx_bridge_underrun_counter_is_not_current_failure() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            concat!(
                "wavelinux6-audio-core native_stats channel_id=hardware_in captured_frames=48000 rendered_frames=48000 dropped_frames=0 underrun_frames=64 process_calls=100 buffered_frames=1344 target_latency_msec=60 reason=audio_trouble\n",
                "wavelinux6-audio-core native_stats channel_id=hardware_in captured_frames=96000 rendered_frames=96000 dropped_frames=0 underrun_frames=64 process_calls=200 buffered_frames=2688 target_latency_msec=60 reason=audio_trouble\n",
            ),
        )
        .unwrap();

        let diagnostics = engine.effect_chain_diagnostics(&config, &RuntimeGraph::default());

        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.underrun.hardware_in"));
    }

    #[test]
    fn consolidated_core_diagnostics_are_scoped_to_the_failing_channel() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        config
            .set_effect_chain("music", vec![EffectInstance::new("eq")])
            .unwrap();
        let hardware = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(hardware),
            concat!(
                "wavelinux6-audio-core native_stats channel_id=hardware_in underrun_frames=0\n",
                "wavelinux6-audio-core native_stats channel_id=hardware_in underrun_frames=0\n",
                "wavelinux6-audio-core native_stats channel_id=music underrun_frames=0\n",
                "wavelinux6-audio-core native_stats channel_id=music underrun_frames=64\n",
            ),
        )
        .unwrap();

        let diagnostics = engine.effect_chain_diagnostics(&config, &RuntimeGraph::default());

        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.underrun.hardware_in"));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.underrun.music"));
    }

    #[test]
    fn old_timestamped_fx_chain_log_warnings_are_ignored() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        let old_timestamp = (OffsetDateTime::now_utc() - time::Duration::minutes(30))
            .format(&Rfc3339)
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            format!(
                "{old_timestamp} | WARN | rnnoise_ladspa | Underrun detected (RTF: 2.00). Processing too slow!\n"
            ),
        )
        .unwrap();

        let diagnostics = engine.effect_chain_diagnostics(&config, &RuntimeGraph::default());

        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.underrun.hardware_in"));
    }

    #[test]
    fn unhealthy_fx_chain_runtime_keeps_processed_input() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            "Underrun detected (RTF: 2.00). Processing too slow!\n",
        )
        .unwrap();

        let effective = engine.config_with_unhealthy_effects_bypassed(&config);

        assert_eq!(
            default_input_source(&config).as_deref(),
            Some("wavelinux-mic")
        );
        assert_eq!(
            default_input_source(&effective).as_deref(),
            Some("wavelinux-mic")
        );
        assert!(effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .is_some_and(channel_has_active_effects));

        let diagnostics = engine.effect_chain_diagnostics(&effective, &RuntimeGraph::default());
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.underrun.hardware_in"));
    }

    #[test]
    fn realtime_fallback_does_not_bypass_rnnoise_or_standard_effects() {
        let mut channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        channel.effects = vec![
            EffectInstance::new("highpass"),
            EffectInstance::new("eq"),
            EffectInstance::new("compressor"),
            EffectInstance::new("rnnoise"),
            EffectInstance::new("gate"),
            EffectInstance::new("limiter"),
        ];

        assert!(!bypass_realtime_fallback_effects(&mut channel));

        let bypassed = channel
            .effects
            .iter()
            .map(|effect| (effect.effect_id.as_str(), effect.bypassed))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(bypassed.get("rnnoise"), Some(&false));
        assert_eq!(bypassed.get("highpass"), Some(&false));
        assert_eq!(bypassed.get("eq"), Some(&false));
        assert_eq!(bypassed.get("compressor"), Some(&false));
        assert_eq!(bypassed.get("gate"), Some(&false));
        assert_eq!(bypassed.get("limiter"), Some(&false));
    }

    #[test]
    fn realtime_fallback_bypasses_future_heavy_effect_without_inserting_rnnoise() {
        let mut channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        channel.effects = vec![
            EffectInstance::new("highpass"),
            EffectInstance::new("convolver"),
            EffectInstance::new("limiter"),
        ];

        assert!(bypass_realtime_fallback_effects(&mut channel));

        let effects = channel
            .effects
            .iter()
            .map(|effect| (effect.effect_id.as_str(), effect.bypassed))
            .collect::<Vec<_>>();
        assert_eq!(
            effects,
            vec![("highpass", false), ("convolver", true), ("limiter", false),]
        );
        assert!(!channel
            .effects
            .iter()
            .any(|effect| effect.effect_id == "rnnoise"));
    }

    #[test]
    fn stale_matching_fx_failure_artifact_does_not_bypass_current_chain() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        let channel = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        channel.effects = vec![EffectInstance::new("convolver")];
        let failed_config = dsp_channel_config(channel);
        let old_log_path = engine
            .paths
            .config_dir
            .join("wavelinux-chain-hardware_in.log.failure.1.log");
        fs::write(
            &old_log_path,
            "2026-01-01T00:00:00Z | WARN | processing too slow\n",
        )
        .unwrap();
        fs::write(
            old_log_path.with_extension("json"),
            serde_json::to_string_pretty(&failed_config).unwrap(),
        )
        .unwrap();

        let effective =
            engine.config_with_unhealthy_effects_bypassed_for_runtime_prefix(&config, "wavelinux5");
        let channel = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        assert!(channel
            .effects
            .iter()
            .any(|effect| effect.effect_id == "convolver" && !effect.bypassed));
        assert!(engine
            .active_effect_chain_failure_log_path(channel)
            .is_none());
    }

    #[test]
    fn quiet_fx_chain_runtime_keeps_processed_input() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            "filter-chain running\n",
        )
        .unwrap();

        let effective = engine.config_with_unhealthy_effects_bypassed(&config);

        assert_eq!(
            default_input_source(&effective).as_deref(),
            Some("wavelinux-mic")
        );
    }

    #[test]
    fn quiet_fx_chain_log_does_not_report_realtime_or_clipping_warnings() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            "filter-chain running\n",
        )
        .unwrap();

        let diagnostics = engine.effect_chain_diagnostics(&config, &RuntimeGraph::default());

        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.underrun.hardware_in"));
        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.clipping.hardware_in"));
    }

    #[test]
    fn route_diagnostics_accept_complete_effect_routes() {
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        let graph = running_graph_for_config(&config);
        let modules = routing_modules_for_config(&config);

        let diagnostics = route_diagnostics(&config, &graph, &modules);

        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.code.starts_with("graph.route_")),
            "diagnostics={diagnostics:?}"
        );
    }

    #[test]
    fn route_diagnostics_accept_complete_wavelinux5_adaptive_effect_bridge() {
        let config = wavelinux5_config_with_effects(vec![EffectInstance::new("rnnoise")]);
        let graph = running_graph_for_config(&config);
        let modules = routing_modules_for_config(&config);

        let diagnostics = route_diagnostics(&config, &graph, &modules);

        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.code.starts_with("graph.route_")),
            "diagnostics={diagnostics:?}"
        );
    }

    #[test]
    fn route_diagnostics_accept_wavelinux5_native_bridge_revision() {
        let config = wavelinux5_config_with_effects(vec![EffectInstance::new("rnnoise")]);
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        let mut graph = running_graph_for_config(&config);
        let native_bridge_nodes = BTreeSet::from([
            effect_chain_source_name(channel),
            effect_chain_adaptive_bridge_input_name(channel),
        ]);
        for device in graph.inputs.iter_mut().chain(graph.outputs.iter_mut()) {
            if native_bridge_nodes.contains(&device.name) {
                device.pipewire_properties.insert(
                    graph_prop("effect_config_revision"),
                    wavelinux_dsp::DSP_CHANNEL_CONFIG_REVISION.into(),
                );
            }
        }
        let modules = routing_modules_for_config(&config);

        assert!(effect_chain_endpoint_readiness_for_graph(&graph, channel).ready());
        let diagnostics = route_diagnostics(&config, &graph, &modules);

        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.code.starts_with("graph.route_")),
            "diagnostics={diagnostics:?}"
        );
    }

    #[test]
    fn route_diagnostics_report_missing_effect_route() {
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        let graph = running_graph_for_config(&config);
        let mut modules = routing_modules_for_config(&config);
        modules.retain(|module| {
            !(module.role.as_deref() == Some("channel_to_effect")
                && module.channel_id.as_deref() == Some("hardware_in"))
        });

        let diagnostics = route_diagnostics(&config, &graph, &modules);

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "graph.route_effect.hardware_in"
                && diagnostic.severity == DiagnosticSeverity::Warning
        }));
    }

    #[test]
    fn route_diagnostics_report_missing_wavelinux5_adaptive_bridge_route() {
        let config = wavelinux5_config_with_effects(vec![EffectInstance::new("rnnoise")]);
        let graph = running_graph_for_config(&config);
        let mut modules = routing_modules_for_config(&config);
        modules.retain(|module| {
            !(module.role.as_deref() == Some("effect_to_adaptive_bridge")
                && module.channel_id.as_deref() == Some("hardware_in"))
        });

        let diagnostics = route_diagnostics(&config, &graph, &modules);

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "graph.route_adaptive_bridge.hardware_in"
                && diagnostic.severity == DiagnosticSeverity::Warning
        }));
    }

    #[test]
    fn route_diagnostics_report_missing_channel_mix_route() {
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        let graph = running_graph_for_config(&config);
        let mut modules = routing_modules_for_config(&config);
        modules.retain(|module| {
            !(module.role.as_deref() == Some("channel_to_mix")
                && module.channel_id.as_deref() == Some("hardware_in")
                && module.mix_id.as_deref() == Some("stream"))
        });

        let diagnostics = route_diagnostics(&config, &graph, &modules);

        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "graph.route_mix.hardware_in.stream"
                && diagnostic.severity == DiagnosticSeverity::Warning
        }));
    }

    #[test]
    fn route_diagnostics_accept_hardware_direct_monitoring_skip() {
        let mut config = MixerConfig::default();
        config.settings.hardware_direct_mic_monitoring = true;
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();
        let graph = running_graph_for_config(&config);
        let modules = routing_modules_for_config(&config);

        let diagnostics = route_diagnostics(&config, &graph, &modules);

        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.route_mix.hardware_in.monitor"));
    }

    #[test]
    fn repair_starts_base_graph_before_fx() {
        let engine = test_engine();
        engine
            .set_mix_monitor_output("monitor".into(), Some("alsa_output.speakers".into()))
            .unwrap();
        engine
            .set_effect_chain("hardware_in".into(), vec![EffectInstance::new("limiter")])
            .unwrap();

        let report = engine.repair_audio_graph().unwrap();
        let base_graph_index = report
            .outputs
            .iter()
            .position(|output| output.command.description == "create channel sink 'Input'")
            .unwrap();
        let fx_index = report
            .outputs
            .iter()
            .position(|output| output.command.description == "start 'Input' effect chain")
            .unwrap();
        assert!(base_graph_index < fx_index);
    }

    #[test]
    fn targeted_effect_sync_only_rebuilds_affected_channel_routes() {
        let engine = test_engine();
        {
            let mut config = engine.write_config().unwrap();
            config
                .set_effect_chain("music", vec![EffectInstance::new("limiter")])
                .unwrap();
            config
                .set_effect_chain("chat", vec![EffectInstance::new("gate")])
                .unwrap();
        }
        engine.rebuild_effect_chain_configs().unwrap();

        let outputs = engine
            .sync_effect_channels(&BTreeSet::from(["music".to_string()]))
            .unwrap();
        let descriptions = outputs
            .iter()
            .map(|output| output.command.description.as_str())
            .collect::<Vec<_>>();

        assert!(descriptions.contains(&"start 'Music' effect chain"));
        assert!(descriptions
            .iter()
            .any(|description| description.contains("route 'Music' to 'Monitor'")));
        assert!(descriptions
            .iter()
            .all(|description| !description.contains("'Chat'")));
    }

    #[test]
    fn effect_sync_requeues_when_graph_mutation_is_busy() {
        let engine = test_engine();
        let _audio_commands = engine.audio_commands.lock().unwrap();

        let result = engine
            .try_sync_effect_channels(&BTreeSet::from(["music".to_string()]))
            .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn app_matcher_routes_to_channel() {
        let mut config = MixerConfig::default();
        config
            .assign_app_to_channel("chat", AppMatcher::from_app_id("discord"))
            .unwrap();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("discord".into()),
            binary: Some("Discord".into()),
            process_name: Some("Discord".into()),
            window_class: Some("discord".into()),
            display_name: "Discord".into(),
            media_name: None,
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };
        let channel = route_stream_to_configured_channel(&config, &stream).unwrap();
        assert_eq!(channel.id, "chat");

        let mut config = MixerConfig::default();
        config
            .assign_app_to_channel("browser", AppMatcher::from_window_class("DISCORD"))
            .unwrap();
        let channel = route_stream_to_configured_channel(&config, &stream).unwrap();
        assert_eq!(channel.id, "browser");
    }

    #[test]
    fn browser_streams_route_to_browser_channel_by_default() {
        let config = MixerConfig::default();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("chromium".into()),
            binary: Some("chrome".into()),
            process_name: Some("chrome".into()),
            window_class: Some("Chromium".into()),
            display_name: "Chromium".into(),
            media_name: Some("Playback".into()),
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };

        let channel = route_stream_to_configured_channel(&config, &stream).unwrap();

        assert_eq!(channel.id, "browser");
    }

    #[test]
    fn fast_app_routing_requires_the_target_channel_sink() {
        let config = MixerConfig::default();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("brave".into()),
            binary: Some("brave".into()),
            process_name: Some("brave".into()),
            window_class: Some("Brave-browser".into()),
            display_name: "Brave".into(),
            media_name: Some("YouTube".into()),
            routed_channel_id: None,
            volume: 1.0,
            muted: false,
        };
        let mut graph = running_graph_for_config(&config);
        graph.app_streams = vec![stream];

        assert_eq!(fast_routable_streams_for_graph(&config, &graph).len(), 1);

        let browser_sink = config
            .channels
            .iter()
            .find(|channel| channel.id == "browser")
            .unwrap()
            .virtual_sink_name
            .clone();
        graph.outputs.retain(|output| output.name != browser_sink);
        assert!(fast_routable_streams_for_graph(&config, &graph).is_empty());
    }

    #[test]
    fn healthy_noop_routing_does_not_request_another_host_snapshot() {
        assert!(!runtime_route_resnapshot_needed(
            false, false, false, false, false
        ));
        assert!(runtime_route_resnapshot_needed(
            false, false, false, false, true
        ));
        assert!(runtime_route_resnapshot_needed(
            true, false, false, false, false
        ));
    }

    #[test]
    fn transient_desktop_event_streams_do_not_enter_the_mixer() {
        let engine = test_engine();
        let config = MixerConfig::default();
        let stream = AppStream {
            id: "event-sound".into(),
            app_id: Some("org.freedesktop.libcanberra".into()),
            binary: Some("canberra-gtk-play".into()),
            process_name: Some("libcanberra".into()),
            window_class: None,
            display_name: "libcanberra".into(),
            media_name: Some("Desktop event sound".into()),
            routed_channel_id: None,
            volume: 1.0,
            muted: false,
        };

        assert!(route_stream_to_configured_channel(&config, &stream).is_none());
        assert!(active_app_channel_id_for_stream(&config, &stream).is_none());
        assert!(!engine
            .remember_observed_apps(std::slice::from_ref(&stream))
            .unwrap());
        assert!(engine.read_config().unwrap().app_history.is_empty());

        let mut restored_graph = running_graph_for_config(&config);
        restored_graph.app_streams = vec![AppStream {
            routed_channel_id: Some("system".into()),
            ..stream
        }];
        assert!(engine
            .move_unready_routed_streams_to_default(
                &config,
                &restored_graph,
                &routing_modules_for_config(&config),
                &[],
                &[],
            )
            .unwrap());
    }

    #[test]
    fn wavelinux_diagnostic_streams_never_enter_user_routes() {
        let engine = test_engine();
        let config = MixerConfig::default();
        let stream = AppStream {
            id: "continuity-fixture".into(),
            app_id: Some("wavelinux6-stress-tone".into()),
            binary: Some("wavelinux6-stress-tone".into()),
            process_name: Some("paplay".into()),
            window_class: None,
            display_name: "WaveLinux 6 Stress Tone".into(),
            media_name: Some("Continuity Fixture".into()),
            routed_channel_id: None,
            volume: 1.0,
            muted: false,
        };

        assert!(route_stream_to_configured_channel(&config, &stream).is_none());
        assert!(active_app_channel_id_for_stream(&config, &stream).is_none());
        assert!(!engine
            .remember_observed_apps(std::slice::from_ref(&stream))
            .unwrap());
        assert!(engine.read_config().unwrap().app_history.is_empty());
    }

    #[test]
    fn electron_web_playback_routes_to_browser_without_explicit_route() {
        let config = MixerConfig::default();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("electron".into()),
            binary: Some("electron".into()),
            process_name: Some("electron".into()),
            window_class: None,
            display_name: "IPTVNator".into(),
            media_name: Some("Playback".into()),
            routed_channel_id: Some("chat".into()),
            volume: percent_to_unit(80.0),
            muted: false,
        };

        let channel = route_stream_to_configured_channel(&config, &stream).unwrap();

        assert_eq!(channel.id, "browser");
    }

    #[test]
    fn chat_named_electron_stream_routes_to_chat_by_default() {
        let config = MixerConfig::default();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("electron".into()),
            binary: Some("electron".into()),
            process_name: Some("electron".into()),
            window_class: None,
            display_name: "Discord".into(),
            media_name: Some("Playback".into()),
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };

        let channel = route_stream_to_configured_channel(&config, &stream).unwrap();

        assert_eq!(channel.id, "chat");
    }

    #[test]
    fn explicit_app_route_overrides_default_stream_classification() {
        let mut config = MixerConfig::default();
        config
            .assign_app_to_channel("chat", AppMatcher::from_app_id("brave"))
            .unwrap();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("brave".into()),
            binary: Some("brave".into()),
            process_name: Some("brave".into()),
            window_class: Some("Brave-browser".into()),
            display_name: "Brave".into(),
            media_name: Some("Playback".into()),
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };

        let channel = route_stream_to_configured_channel(&config, &stream).unwrap();

        assert_eq!(channel.id, "chat");
    }

    #[test]
    fn wrapper_app_media_matchers_override_broad_routes() {
        let mut config = MixerConfig::default();
        let slack_stream = AppStream {
            id: "1".into(),
            app_id: Some("ferdium".into()),
            binary: Some("ferdium".into()),
            process_name: Some("ferdium".into()),
            window_class: Some("Ferdium".into()),
            display_name: "Ferdium".into(),
            media_name: Some("Slack".into()),
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };
        let discord_stream = AppStream {
            id: "2".into(),
            media_name: Some("Discord".into()),
            ..slack_stream.clone()
        };

        config
            .assign_app_to_channel("chat", AppMatcher::from_app_id("ferdium"))
            .unwrap();
        config
            .assign_app_to_channel("music", AppMatcher::from_stream(&slack_stream).unwrap())
            .unwrap();
        config
            .set_app_volume_preset(AppMatcher::from_app_id("ferdium"), 0.8)
            .unwrap();
        config
            .set_app_volume_preset(AppMatcher::from_stream(&slack_stream).unwrap(), 0.35)
            .unwrap();

        assert_eq!(
            route_stream_to_configured_channel(&config, &slack_stream)
                .unwrap()
                .id,
            "music"
        );
        assert_eq!(
            route_stream_to_configured_channel(&config, &discord_stream)
                .unwrap()
                .id,
            "chat"
        );
        assert_eq!(
            configured_volume_for_stream(&config, &slack_stream),
            Some(0.35)
        );
        assert_eq!(
            configured_volume_for_stream(&config, &discord_stream),
            Some(0.8)
        );
    }

    #[test]
    fn stable_app_identity_survives_changed_media_name_for_non_wrapper_apps() {
        let mut config = MixerConfig::default();
        config
            .assign_app_to_channel(
                "music",
                AppMatcher {
                    app_id: Some("spotify".into()),
                    binary: Some("spotify".into()),
                    process_name: Some("spotify".into()),
                    window_class: None,
                    media_name: Some("audio-src".into()),
                },
            )
            .unwrap();

        let stream = AppStream {
            id: "1".into(),
            app_id: Some("spotify".into()),
            binary: Some("spotify".into()),
            process_name: Some("spotify".into()),
            window_class: Some("spotify".into()),
            display_name: "Spotify".into(),
            media_name: Some("Different Track Title".into()),
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };

        let channel = route_stream_to_configured_channel(&config, &stream).unwrap();
        assert_eq!(channel.id, "music");
    }

    #[test]
    fn media_only_app_matchers_do_not_match_every_stream() {
        let mut config = MixerConfig::default();
        config
            .assign_app_to_channel(
                "music",
                AppMatcher {
                    app_id: None,
                    binary: None,
                    process_name: None,
                    window_class: None,
                    media_name: Some("Spotify".into()),
                },
            )
            .unwrap();

        let spotify_stream = AppStream {
            id: "1".into(),
            app_id: None,
            binary: None,
            process_name: None,
            window_class: None,
            display_name: "Spotify".into(),
            media_name: Some("Spotify".into()),
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };
        let discord_stream = AppStream {
            id: "2".into(),
            display_name: "Discord".into(),
            media_name: Some("Discord".into()),
            ..spotify_stream.clone()
        };

        assert_eq!(
            route_stream_to_configured_channel(&config, &spotify_stream)
                .unwrap()
                .id,
            "music"
        );
        assert_eq!(
            route_stream_to_configured_channel(&config, &discord_stream)
                .unwrap()
                .id,
            "chat"
        );
    }

    #[test]
    fn app_volume_presets_match_stream_identity() {
        let mut config = MixerConfig::default();
        config
            .set_app_volume_preset(AppMatcher::from_app_id("spotify"), 0.42)
            .unwrap();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("spotify".into()),
            binary: Some("spotify".into()),
            process_name: Some("spotify".into()),
            window_class: Some("spotify".into()),
            display_name: "Spotify".into(),
            media_name: None,
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };

        assert_eq!(configured_volume_for_stream(&config, &stream), Some(0.42));
    }

    #[test]
    fn routing_preserves_stream_volume_without_a_changed_preset() {
        let mut config = MixerConfig::default();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("brave".into()),
            binary: Some("brave".into()),
            process_name: Some("brave".into()),
            window_class: Some("Brave-browser".into()),
            display_name: "Brave".into(),
            media_name: Some("Playback".into()),
            routed_channel_id: None,
            volume: 0.37,
            muted: false,
        };

        assert_eq!(configured_volume_update_for_stream(&config, &stream), None);
        config
            .set_app_volume_preset(AppMatcher::from_app_id("brave"), 0.37)
            .unwrap();
        assert_eq!(configured_volume_update_for_stream(&config, &stream), None);
        config
            .set_app_volume_preset(AppMatcher::from_app_id("brave"), 0.42)
            .unwrap();
        assert_eq!(
            configured_volume_update_for_stream(&config, &stream),
            Some(0.42)
        );
    }

    #[test]
    fn app_route_can_be_removed() {
        let engine = test_engine();
        let matcher = AppMatcher::from_app_id("spotify");
        engine
            .assign_app_to_channel("music".into(), matcher.clone())
            .unwrap();

        let removed = engine.remove_app_route(matcher.clone()).unwrap().unwrap();
        assert_eq!(removed.channel_id, "music");
        assert!(engine.remove_app_route(matcher).unwrap().is_none());
        assert!(engine.get_state().unwrap().config.app_routes.is_empty());
    }

    #[test]
    fn remembered_apps_can_be_forgotten() {
        let engine = test_engine();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("spotify".into()),
            binary: Some("spotify".into()),
            process_name: Some("spotify".into()),
            window_class: Some("spotify".into()),
            display_name: "Spotify".into(),
            media_name: Some("Playback".into()),
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };

        assert!(engine.remember_observed_apps(&[stream]).unwrap());
        let matcher = AppMatcher::from_app_id("spotify");
        engine
            .assign_app_to_channel("music".into(), matcher.clone())
            .unwrap();
        engine.set_app_volume_preset(matcher.clone(), 0.55).unwrap();

        let forgotten = engine.forget_app(matcher.clone()).unwrap().unwrap();
        assert!(forgotten.forgotten);
        let state = engine.get_state().unwrap();
        assert!(state.config.app_routes.is_empty());
        assert!(state.config.app_volume_presets.is_empty());
        assert!(state.config.app_history[0].forgotten);
        assert!(!engine.restore_app(matcher).unwrap().unwrap().forgotten);
    }

    #[test]
    #[ignore = "mutates the live user PipeWire graph"]
    fn live_audio_graph_stale_cleanup_repair_and_sound_check() {
        let engine = WaveLinuxEngine::new(
            EnginePaths::from_xdg().unwrap(),
            EngineOptions {
                dry_run: false,
                auto_repair_on_start: false,
                poll_interval: Duration::from_millis(100),
            },
        )
        .unwrap();

        engine.cleanup_stale_audio_graph().unwrap();
        let repair_result = engine.repair_audio_graph();
        let failed_commands = repair_result
            .as_ref()
            .map(|repair| {
                repair
                    .outputs
                    .iter()
                    .filter_map(|output| {
                        output
                            .error
                            .as_ref()
                            .map(|error| format!("{}: {error}", output.command.shell_line()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let refresh_result = if repair_result.is_ok() {
            engine.refresh_runtime()
        } else {
            Ok(())
        };
        let errors = if refresh_result.is_ok() {
            engine
                .run_diagnostics()
                .map(|report| {
                    report
                        .diagnostics
                        .into_iter()
                        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let cleanup_result = engine.cleanup_audio_graph();
        assert!(cleanup_result.is_ok(), "{cleanup_result:#?}");

        repair_result.unwrap();
        refresh_result.unwrap();
        assert!(failed_commands.is_empty(), "{failed_commands:#?}");
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    #[ignore = "mutates the live user PipeWire graph"]
    fn live_audio_graph_level_mutations_and_cleanup_are_stable() {
        let root = tempdir().unwrap();
        let engine = live_test_engine(root.path());
        let _cleanup = LiveGraphCleanup(engine.clone());

        engine.cleanup_audio_graph().unwrap();
        engine.refresh_runtime().unwrap();
        assert!(!state_has_wavelinux_audio_nodes(
            &engine.get_state().unwrap()
        ));

        let repair = engine.repair_audio_graph().unwrap();
        let failed_commands = repair
            .outputs
            .iter()
            .filter_map(|output| {
                output
                    .error
                    .as_ref()
                    .map(|error| format!("{}: {error}", output.command.shell_line()))
            })
            .collect::<Vec<_>>();
        assert!(failed_commands.is_empty(), "{failed_commands:#?}");

        engine.refresh_runtime().unwrap();
        let state = engine.get_state().unwrap();
        assert!(state.engine.audio_graph_running);
        assert!(state
            .graph
            .outputs
            .iter()
            .any(|output| output.name == "wavelinux_mix_monitor"));
        assert!(state
            .graph
            .outputs
            .iter()
            .any(|output| output.name == "wavelinux_mix_stream"));
        assert!(state
            .graph
            .inputs
            .iter()
            .any(|input| input.name == "wavelinux_mix_monitor_source"));
        assert!(state
            .graph
            .inputs
            .iter()
            .any(|input| input.name == "wavelinux_mix_stream_source"));
        assert!(
            state
                .graph
                .inputs
                .iter()
                .chain(state.graph.outputs.iter())
                .filter(|device| device_mentions_wavelinux(device))
                .all(device_uses_sanitized_wavelinux_names),
            "{:?}",
            state
                .graph
                .inputs
                .iter()
                .chain(state.graph.outputs.iter())
                .filter(|device| device_mentions_wavelinux(device))
                .collect::<Vec<_>>()
        );
        if meter_sampling_enabled() {
            let metered = refresh_until(&engine, Duration::from_secs(4), |state| {
                state
                    .graph
                    .meters
                    .iter()
                    .any(|meter| meter.node_id == "stream")
            });
            assert!(
                metered
                    .graph
                    .meters
                    .iter()
                    .any(|meter| meter.node_id == "stream"),
                "meters={:?}",
                metered.graph.meters
            );
            assert!(
                metered
                    .graph
                    .meters
                    .iter()
                    .any(|meter| meter.node_id
                        == wavelinux_pw::channel_bus_meter_id("game", "stream")),
                "meters={:?}",
                metered.graph.meters
            );
            assert!(
                metered.graph.meters.iter().all(|meter| {
                    (0.0..=1.0).contains(&meter.peak_left)
                        && (0.0..=1.0).contains(&meter.peak_right)
                }),
                "meters={:?}",
                metered.graph.meters
            );
        }

        engine.set_mix_volume("stream".into(), 0.42).unwrap();
        engine.set_mix_mute("stream".into(), true).unwrap();
        engine.set_mix_mute("stream".into(), false).unwrap();
        engine
            .set_channel_volume("hardware_in".into(), "stream".into(), 0.35)
            .unwrap();
        engine
            .set_channel_mute("hardware_in".into(), "stream".into(), true)
            .unwrap();
        engine
            .set_channel_mute("hardware_in".into(), "stream".into(), false)
            .unwrap();

        engine.refresh_runtime().unwrap();
        let debug = engine.get_graph_debug_report().unwrap();
        assert!(debug.audio_graph_running);
        assert!(!debug.managed_modules.is_empty());

        let diagnostics = engine.run_diagnostics().unwrap();
        let errors = diagnostics
            .diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "{errors:#?}");

        let cleanup = engine.cleanup_audio_graph().unwrap();
        let cleanup_errors = cleanup
            .iter()
            .filter_map(|output| output.error.as_ref())
            .collect::<Vec<_>>();
        assert!(cleanup_errors.is_empty(), "{cleanup_errors:#?}");
        engine.refresh_runtime().unwrap();
        assert!(!state_has_wavelinux_audio_nodes(
            &engine.get_state().unwrap()
        ));

        let second_cleanup = engine.cleanup_audio_graph().unwrap();
        let second_cleanup_errors = second_cleanup
            .iter()
            .filter_map(|output| output.error.as_ref())
            .collect::<Vec<_>>();
        assert!(
            second_cleanup_errors.is_empty(),
            "{second_cleanup_errors:#?}"
        );
    }

    #[test]
    #[ignore = "mutates the live user PipeWire graph and plays a short test tone"]
    fn live_music_route_meters_only_music_channel() {
        let root = tempdir().unwrap();
        let engine = live_test_engine(root.path());
        let _cleanup = LiveGraphCleanup(engine.clone());

        engine.cleanup_audio_graph().unwrap();
        engine
            .assign_app_to_channel("music".into(), AppMatcher::from_app_id("spotify"))
            .unwrap();
        engine.repair_audio_graph().unwrap();

        let Some(_tone) = spawn_tone_route_test_stream(root.path(), "spotify") else {
            return;
        };

        let state = refresh_until(&engine, Duration::from_secs(6), |state| {
            let music_stream = state
                .graph
                .meters
                .iter()
                .find(|meter| {
                    meter.node_id == wavelinux_pw::channel_bus_meter_id("music", "stream")
                })
                .map(|meter| meter.peak_left.max(meter.peak_right))
                .unwrap_or(0.0);
            let music_monitor = state
                .graph
                .meters
                .iter()
                .find(|meter| {
                    meter.node_id == wavelinux_pw::channel_bus_meter_id("music", "monitor")
                })
                .map(|meter| meter.peak_left.max(meter.peak_right))
                .unwrap_or(0.0);
            music_stream > 0.02 || music_monitor > 0.02
        });

        let music_level = state
            .graph
            .meters
            .iter()
            .filter(|meter| {
                meter.node_id == wavelinux_pw::channel_bus_meter_id("music", "stream")
                    || meter.node_id == wavelinux_pw::channel_bus_meter_id("music", "monitor")
            })
            .map(|meter| meter.peak_left.max(meter.peak_right))
            .fold(0.0_f32, f32::max);
        let other_channel_level = state
            .graph
            .meters
            .iter()
            .filter(|meter| meter.node_id.starts_with("channel:"))
            .filter(|meter| !meter.node_id.starts_with("channel:music:"))
            .map(|meter| meter.peak_left.max(meter.peak_right))
            .fold(0.0_f32, f32::max);

        assert!(
            music_level > 0.02,
            "expected music meter to move, meters={:?}",
            state.graph.meters
        );
        assert!(
            other_channel_level < 0.02,
            "non-music channel meters moved; max_other={other_channel_level}, meters={:?}",
            state.graph.meters
        );
    }

    #[test]
    #[ignore = "mutates the live user PipeWire graph"]
    fn live_audio_graph_effect_chain_starts_routes_and_cleans_up() {
        let root = tempdir().unwrap();
        let engine = live_test_engine(root.path());
        let _cleanup = LiveGraphCleanup(engine.clone());

        engine.cleanup_audio_graph().unwrap();
        engine
            .set_effect_chain("hardware_in".into(), vec![EffectInstance::new("highpass")])
            .unwrap();
        engine.rebuild_effect_chain_configs().unwrap();

        let config_path = engine
            .paths
            .effect_chains_dir()
            .join("wavelinux-chain-hardware_in.conf");
        let effect_log_path = engine
            .paths
            .config_dir
            .join("wavelinux-chain-hardware_in.log");
        let config_text = fs::read_to_string(&config_path).unwrap();
        assert!(config_text.contains("wavelinux-mic"));
        assert!(config_text.contains("WaveLinux-mic"));
        assert!(config_text.contains("bq_highpass"));

        let repair = engine.repair_audio_graph().unwrap();
        let failed_commands = repair
            .outputs
            .iter()
            .filter_map(|output| {
                output
                    .error
                    .as_ref()
                    .map(|error| format!("{}: {error}", output.command.shell_line()))
            })
            .collect::<Vec<_>>();
        assert!(failed_commands.is_empty(), "{failed_commands:#?}");
        assert!(repair.outputs.iter().any(|output| {
            output.command.domain == CommandDomain::Effects
                && output.command.description == "start 'Input' effect chain"
                && !output.skipped
        }));

        let state = refresh_until(&engine, Duration::from_secs(3), |state| {
            state
                .graph
                .inputs
                .iter()
                .any(|input| input.name == "wavelinux-mic")
        });
        assert!(state.engine.audio_graph_running);
        assert!(state
            .graph
            .effect_availability
            .iter()
            .any(|effect| { effect.effect_id == "highpass" && effect.available }));
        assert!(
            state
                .graph
                .inputs
                .iter()
                .any(|input| input.name == "wavelinux-mic"),
            "inputs={:?}\neffect_log={}",
            state.graph.inputs,
            fs::read_to_string(&effect_log_path).unwrap_or_default()
        );

        let debug = engine.get_graph_debug_report().unwrap();
        assert!(debug
            .stale_processes
            .iter()
            .any(|process| process.command.contains("wavelinux-chain-hardware_in.conf")));
        assert!(debug
            .source_output_routes
            .iter()
            .any(|route| route.channel_id.as_deref() == Some("hardware_in")));

        let diagnostics = engine.run_diagnostics().unwrap();
        let errors = diagnostics
            .diagnostics
            .into_iter()
            .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "{errors:#?}");

        engine
            .set_channel_volume("hardware_in".into(), "stream".into(), 0.44)
            .unwrap();
        engine
            .set_channel_mute("hardware_in".into(), "stream".into(), true)
            .unwrap();
        engine
            .set_channel_mute("hardware_in".into(), "stream".into(), false)
            .unwrap();

        let cleanup = engine.cleanup_audio_graph().unwrap();
        let cleanup_errors = cleanup
            .iter()
            .filter_map(|output| output.error.as_ref())
            .collect::<Vec<_>>();
        assert!(cleanup_errors.is_empty(), "{cleanup_errors:#?}");

        let stopped = refresh_until(&engine, Duration::from_secs(2), |state| {
            !state_has_wavelinux_audio_nodes(state)
        });
        assert!(!state_has_wavelinux_audio_nodes(&stopped));
        assert!(engine
            .get_graph_debug_report()
            .unwrap()
            .stale_processes
            .is_empty());
    }

    #[test]
    #[ignore = "mutates the live user PipeWire graph"]
    fn live_audio_graph_complex_voice_chain_uses_fx_source() {
        let required = ["rnnoise", "compressor", "limiter"];
        let availability = probe_effect_availability(&EffectCatalog::default());
        if required.iter().any(|effect_id| {
            !availability
                .iter()
                .any(|effect| effect.effect_id == *effect_id && effect.available)
        }) {
            eprintln!("skipping complex voice chain test; required LADSPA plugins are unavailable");
            return;
        }

        let root = tempdir().unwrap();
        let engine = live_test_engine(root.path());
        let _cleanup = LiveGraphCleanup(engine.clone());

        engine.cleanup_audio_graph().unwrap();
        engine
            .set_effect_chain(
                "hardware_in".into(),
                vec![
                    test_effect("limiter", &[("ceiling_db", -1.0), ("input_gain_db", 0.0)]),
                    test_effect(
                        "gate",
                        &[
                            ("attack_ms", 2.5),
                            ("hold_ms", 80.0),
                            ("release_ms", 160.0),
                            ("threshold_db", -35.0),
                        ],
                    ),
                    test_effect(
                        "eq",
                        &[
                            ("band_63_gain_db", -4.0),
                            ("band_125_gain_db", -2.0),
                            ("band_250_gain_db", -1.0),
                            ("band_500_gain_db", 0.0),
                            ("band_1k_gain_db", 1.0),
                            ("band_2k_gain_db", 2.5),
                            ("band_4k_gain_db", 2.0),
                            ("band_8k_gain_db", 1.0),
                        ],
                    ),
                    test_effect(
                        "compressor",
                        &[
                            ("attack_ms", 3.0),
                            ("makeup_gain_db", 4.0),
                            ("ratio", 6.0),
                            ("release_ms", 80.0),
                            ("threshold_db", -16.0),
                        ],
                    ),
                    test_effect("limiter", &[("ceiling_db", -1.0), ("input_gain_db", 0.0)]),
                    test_effect(
                        "rnnoise",
                        &[
                            ("vad_threshold", 25.0),
                            ("hold_ms", 200.0),
                            ("minimum_voice_level_db", -70.0),
                        ],
                    ),
                ],
            )
            .unwrap();
        engine.rebuild_effect_chain_configs().unwrap();

        let config_path = engine
            .paths
            .effect_chains_dir()
            .join("wavelinux-chain-hardware_in.conf");
        let config_text = fs::read_to_string(&config_path).unwrap();
        assert!(config_text.contains("gate_1410"));
        assert!(config_text.contains("param_eq"));
        assert!(config_text.contains("filters1"));
        assert!(config_text.contains("filters2"));

        let repair = engine.repair_audio_graph().unwrap();
        let failed_commands = repair
            .outputs
            .iter()
            .filter_map(|output| {
                output
                    .error
                    .as_ref()
                    .map(|error| format!("{}: {error}", output.command.shell_line()))
            })
            .collect::<Vec<_>>();
        assert!(failed_commands.is_empty(), "{failed_commands:#?}");

        let state = refresh_until(&engine, Duration::from_secs(6), |state| {
            state
                .graph
                .inputs
                .iter()
                .any(|input| input.name == "wavelinux-mic")
        });
        assert!(
            state
                .graph
                .inputs
                .iter()
                .any(|input| input.name == "wavelinux-mic"),
            "inputs={:?}",
            state.graph.inputs
        );

        let debug = engine.get_graph_debug_report().unwrap();
        assert!(
            debug.source_output_routes.iter().any(|route| {
                route.channel_id.as_deref() == Some("hardware_in")
                    && route.target_object.as_deref() == Some("wavelinux-mic")
            }),
            "source_output_routes={:?}",
            debug.source_output_routes
        );
    }

    #[test]
    #[ignore = "mutates the live user PipeWire graph"]
    fn live_app_routing_identity_and_volume_presets_follow_streams() {
        let root = tempdir().unwrap();
        let engine = live_test_engine(root.path());
        let _cleanup = LiveGraphCleanup(engine.clone());
        const SOURCE_APP_ID: &str = "io.github.wavelinux.RouteTest.Source";
        const CANONICAL_APP_ID: &str = "io.github.wavelinux.RouteTest.Canonical";
        let source = AppMatcher::from_app_id(SOURCE_APP_ID);
        let canonical = AppMatcher::from_app_id(CANONICAL_APP_ID);

        engine.cleanup_audio_graph().unwrap();
        let repair = engine.repair_audio_graph().unwrap();
        let failed_commands = repair
            .outputs
            .iter()
            .filter_map(|output| {
                output
                    .error
                    .as_ref()
                    .map(|error| format!("{}: {error}", output.command.shell_line()))
            })
            .collect::<Vec<_>>();
        assert!(failed_commands.is_empty(), "{failed_commands:#?}");

        engine
            .pin_app_identity(canonical.clone(), "Route Test App".into())
            .unwrap();
        engine
            .merge_app_identity(source.clone(), canonical.clone())
            .unwrap();
        engine
            .assign_app_to_channel("music".into(), canonical.clone())
            .unwrap();
        engine
            .set_app_volume_preset(canonical.clone(), 0.37)
            .unwrap();

        let stream_guard = match spawn_silent_route_test_stream(SOURCE_APP_ID) {
            Some(stream) => stream,
            None => return,
        };

        let state = refresh_until(&engine, Duration::from_secs(8), |state| {
            state.graph.app_streams.iter().any(|stream| {
                stream.app_id.as_deref() == Some(SOURCE_APP_ID)
                    && stream.routed_channel_id.as_deref() == Some("music")
                    && (stream.volume - 0.37).abs() <= 0.04
            })
        });
        let stream = state
            .graph
            .app_streams
            .iter()
            .find(|stream| stream.app_id.as_deref() == Some(SOURCE_APP_ID))
            .unwrap_or_else(|| {
                panic!(
                    "route test stream not visible: {:?}",
                    state.graph.app_streams
                )
            });
        assert_eq!(stream.display_name, "WaveLinux Route Test");
        assert_eq!(stream.binary.as_deref(), Some("wavelinux-route-test"));
        assert_eq!(stream.process_name.as_deref(), Some("wavelinux-route-test"));
        assert_eq!(stream.window_class.as_deref(), Some("WaveLinuxRouteTest"));
        assert_eq!(
            stream.media_name.as_deref(),
            Some("WaveLinuxRouteTestStream")
        );
        assert_eq!(stream.routed_channel_id.as_deref(), Some("music"));
        assert!(
            (stream.volume - 0.37).abs() <= 0.04,
            "stream volume was {}",
            stream.volume
        );

        assert!(state.config.app_history.iter().any(|app| {
            app.matcher == canonical && app.display_name == "Route Test App" && !app.forgotten
        }));
        assert!(engine
            .get_graph_debug_report()
            .unwrap()
            .graph
            .app_streams
            .iter()
            .any(|stream| {
                stream.app_id.as_deref() == Some(SOURCE_APP_ID)
                    && stream.routed_channel_id.as_deref() == Some("music")
            }));

        let stream_id = stream.id.clone();
        let removed_route = engine.remove_app_route(canonical.clone()).unwrap().unwrap();
        assert_eq!(removed_route.channel_id, "music");
        assert!(engine
            .remove_app_volume_preset(canonical.clone())
            .unwrap()
            .is_some());

        let move_default = engine
            .move_app_stream_to_default(stream_id.clone())
            .unwrap();
        assert!(!move_default.skipped);
        assert!(move_default.error.is_none(), "{move_default:#?}");
        let state = refresh_until(&engine, Duration::from_secs(4), |state| {
            state
                .graph
                .app_streams
                .iter()
                .find(|stream| stream.id == stream_id)
                .is_some_and(|stream| stream.routed_channel_id.as_deref() != Some("music"))
        });
        assert!(
            state
                .graph
                .app_streams
                .iter()
                .find(|stream| stream.id == stream_id)
                .is_some_and(|stream| stream.routed_channel_id.as_deref() != Some("music")),
            "stream stayed routed to music: {:?}",
            state.graph.app_streams
        );

        let forgotten = engine.forget_app(canonical.clone()).unwrap().unwrap();
        assert!(forgotten.forgotten);
        let restored = engine.restore_app(canonical.clone()).unwrap().unwrap();
        assert!(!restored.forgotten);
        let reset = engine.reset_app_identity(canonical).unwrap().unwrap();
        assert!(!reset.forgotten);
        let state = engine.get_state().unwrap();
        assert!(state.config.app_routes.is_empty());
        assert!(state.config.app_volume_presets.is_empty());
        assert!(state.config.app_identity_overrides.is_empty());
        assert!(state.config.app_label_overrides.is_empty());

        drop(stream_guard);
        let cleanup = engine.cleanup_audio_graph().unwrap();
        let cleanup_errors = cleanup
            .iter()
            .filter_map(|output| output.error.as_ref())
            .collect::<Vec<_>>();
        assert!(cleanup_errors.is_empty(), "{cleanup_errors:#?}");
        let stopped = refresh_until(&engine, Duration::from_secs(2), |state| {
            !state_has_wavelinux_audio_nodes(state)
        });
        assert!(!state_has_wavelinux_audio_nodes(&stopped));
    }

    #[test]
    fn sound_check_counts_virtual_mixes() {
        let engine = test_engine();
        let report = engine.run_diagnostics().unwrap();
        assert_eq!(report.virtual_mix_count, 2);
    }

    #[test]
    fn stopped_graph_reports_info_not_missing_nodes() {
        let config = MixerConfig::default();
        let diagnostics = graph_diagnostics(&config, &RuntimeGraph::default());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, "graph.stopped");
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Info);
    }

    #[test]
    fn stopped_graph_skips_live_stream_commands() {
        let engine = test_engine();

        let volume = engine.set_app_stream_volume("42".into(), 0.5).unwrap();
        assert!(volume.skipped);
        assert_eq!(
            volume.command.args,
            vec!["set-sink-input-volume", "42", "50%"]
        );

        let move_default = engine.move_app_stream_to_default("42".into()).unwrap();
        assert!(move_default.skipped);
        assert_eq!(
            move_default.command.args,
            vec!["move-sink-input", "42", "@DEFAULT_SINK@"]
        );
    }

    #[test]
    fn settings_are_persisted() {
        let engine = test_engine();
        let mut settings = engine.get_state().unwrap().config.settings;
        settings.lock_default_output = true;
        settings.monitor_follows_default_output = false;
        engine.set_settings(settings).unwrap();
        let settings = engine.get_state().unwrap().config.settings;
        assert!(settings.lock_default_output);
        assert!(!settings.monitor_follows_default_output);
        assert!(settings.keep_running_in_tray);
    }

    #[test]
    fn start_at_login_writes_autostart_entry() {
        let engine = test_engine();
        let mut settings = engine.get_state().unwrap().config.settings;
        settings.start_at_login = true;
        engine.set_settings(settings.clone()).unwrap();

        let autostart_file = engine.paths.autostart_file();
        let entry = fs::read_to_string(&autostart_file).unwrap();
        assert!(entry.contains("X-GNOME-Autostart-enabled=true"));
        assert!(entry.contains("Exec="));

        settings.start_at_login = false;
        engine.set_settings(settings).unwrap();
        assert!(!autostart_file.exists());
    }

    #[test]
    fn channels_can_be_renamed_and_deleted() {
        let engine = test_engine();
        engine
            .rename_channel("game".into(), "Gameplay".into())
            .unwrap();
        assert!(engine
            .get_state()
            .unwrap()
            .config
            .channels
            .iter()
            .any(|channel| channel.name == "Gameplay"));
        engine.delete_channel("game".into()).unwrap();
        assert!(!engine
            .get_state()
            .unwrap()
            .config
            .channels
            .iter()
            .any(|channel| channel.id == "game"));
    }

    #[test]
    fn linked_channel_volume_persists_across_buses() {
        let engine = test_engine();
        engine
            .set_channel_linked("hardware_in".into(), true)
            .unwrap();
        engine
            .set_channel_volume("hardware_in".into(), "stream".into(), 0.35)
            .unwrap();
        let hardware_in = engine
            .get_state()
            .unwrap()
            .config
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        assert!(hardware_in
            .mix_buses
            .values()
            .all(|bus| (bus.volume - 0.35).abs() < f32::EPSILON));
    }

    #[test]
    fn channel_input_is_persisted() {
        let engine = test_engine();
        engine
            .set_channel_input(
                "hardware_in".into(),
                Some("alsa_input.usb_interface".into()),
            )
            .unwrap();
        let hardware_in = engine
            .get_state()
            .unwrap()
            .config
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        assert_eq!(
            hardware_in.source_device.as_deref(),
            Some("alsa_input.usb_interface")
        );

        engine
            .set_channel_input_mode("hardware_in".into(), ChannelInputMode::SumMono)
            .unwrap();
        let hardware_in = engine
            .get_state()
            .unwrap()
            .config
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        assert_eq!(hardware_in.input_mode, ChannelInputMode::SumMono);
    }

    fn test_effect(effect_id: &str, params: &[(&str, f32)]) -> EffectInstance {
        let mut effect = EffectInstance::new(effect_id);
        effect.params = params
            .iter()
            .map(|(key, value)| ((*key).to_string(), *value))
            .collect();
        effect
    }
}
