use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::mem;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, TryLockError};
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
    app_display_name, apply_graph_namespace, graph_prefix, graph_property_prefix, safe_node_id,
    AppMatcher, AppRoute, AppStateSnapshot, AppStream, AppVolumePreset, AutoDeviceKind,
    AutoDeviceReason, Channel, ChannelInputMode, ChannelKind, DeviceInfo, Diagnostic,
    DiagnosticSeverity, EffectAvailability, EffectCatalog, EffectInstance, EngineStatus,
    FallbackHardwareProfile, HardwareProfile, HardwareProfileUiState, KnownApp, LatencyPolicy,
    LevelMeter, Mix, MixerConfig, MixerSettings, ModelError, ResolvedAutoDevice, RoutingPolicy,
    RuntimeGraph, StreamerBindingProfile, StreamerDevicesConfig,
};
use wavelinux_pw::{
    a2dp_codec_rank_with_preferences, channel_bus_route_ids_from_routes,
    channel_has_active_effects, channel_mix_route_revision,
    channel_mix_route_uses_hardware_direct_monitoring, channel_mix_source_name,
    effect_chain_input_name, effect_chain_source_name, effect_route_revision, input_route_revision,
    meter_sampling_enabled, meter_targets_for_config_with_devices,
    mix_monitor_route_revision_for_sink, plan_bluetooth_a2dp_profiles, plan_ensure_graph,
    plan_kill_stale_processes, plan_move_app_stream, plan_move_app_stream_to_default,
    plan_move_capture_stream_to_source, plan_route_channel_to_effect, plan_route_channel_to_mix,
    plan_set_channel_bus_mute, plan_set_channel_bus_source_output_mute,
    plan_set_channel_bus_source_output_volume, plan_set_channel_bus_volume, plan_set_default_sink,
    plan_set_default_source, plan_set_managed_sink_mute, plan_set_managed_sink_volume,
    plan_set_mix_mute as plan_pw_set_mix_mute, plan_set_mix_volume as plan_pw_set_mix_volume,
    plan_set_route_sink_input_mute, plan_set_route_sink_input_volume,
    plan_set_route_source_output_mute, plan_set_route_source_output_volume, plan_set_source_mute,
    plan_set_source_volume, plan_set_stream_mute, plan_set_stream_volume, plan_unload_modules,
    probe_effect_availability, render_filter_chain, BluetoothAudioCard, ChannelBusRouteIds,
    CommandDomain, CommandOutput, CommandSpec, ManagedModule, MeterTarget, PlannedGraph, PwClient,
    PwError, SinkInputRoute, SnapshotCommandTiming, SourceOutputRoute, StaleProcess,
    EFFECT_CONFIG_REVISION,
};

mod hardware_profiles;

use hardware_profiles::{
    apply_profile_policy_to_devices, apply_profile_policy_to_graph, apply_profiles_to_devices,
    hardware_profile_by_id, hardware_profile_diagnostics, hardware_profile_ui_state,
    load_hardware_profile_catalog, remote_profile_sync_needed, sync_remote_profiles_for_devices,
    HardwareProfileCatalog,
};

const DEBUG_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
const DEBUG_LOG_ROTATED_FILES: usize = 4;
const LOG_VERSION_FILE: &str = "log-version";
const ENGINE_LOG_FILE: &str = "wavelinux-engine.log";
const LEGACY_APP_LOG_FILE: &str = "wavelinux.log";
const EFFECT_CHAIN_LOG_SUFFIX: &str = ".log";
const HOST_DIAGNOSTICS_TTL: Duration = Duration::from_secs(30);
const EFFECT_AVAILABILITY_TTL: Duration = Duration::from_secs(30);
const HARDWARE_PROFILE_TTL: Duration = Duration::from_secs(15);
const REMOTE_PROFILE_SYNC_MIN_INTERVAL: Duration = Duration::from_secs(30);
const METER_RESTART_BACKOFF: Duration = Duration::from_secs(5);
const METER_IDLE_STOP_AFTER: Duration = Duration::from_millis(750);
const METER_NOISE_FLOOR: f32 = 0.008;
const METER_STALE_AFTER: Duration = Duration::from_millis(120);
const METER_STALE_RELEASE_PER_SECOND: f32 = 0.08;
const METER_DISPLAY_FLOOR_DB: f32 = -54.0;
const METER_DISPLAY_CEILING_DB: f32 = 0.0;
const METER_DISPLAY_EXPONENT: f32 = 1.15;
const EFFECT_GRAPH_SYNC_DEBOUNCE: Duration = Duration::from_millis(500);
const EFFECT_NODE_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const EFFECT_NODE_CLEAR_TIMEOUT: Duration = Duration::from_secs(2);
const EFFECT_NODE_READY_STABLE_SAMPLES: usize = 2;
const EFFECT_NODE_READY_SETTLE: Duration = Duration::from_millis(100);
const GRAPH_REPAIR_DEBOUNCE: Duration = Duration::from_millis(650);
const ROUTE_HEALTH_REPAIR_BACKOFF: Duration = Duration::from_secs(10);
const UI_STATE_REFRESH_MAX_AGE: Duration = Duration::from_millis(4_000);
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
const PIPEWIRE_HEALTH_LOG_SINCE: &str = "10 minutes ago";
const DSP_LIVE_HELPER_FALLBACK_REASON: &str =
    "WaveLinux5 accelerated native DSP helper graph is still experimental; using helper-supervised PipeWire filter-chain rollback unless WAVELINUX_AUDIO_RUNTIME=dsp_cpu is set";
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
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            autostart_dir: base_dirs.config_dir().join("autostart"),
        })
    }

    pub fn for_tests(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            autostart_dir: root.join("autostart"),
        }
    }

    fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    fn effect_chains_dir(&self) -> PathBuf {
        self.data_dir.join("effects")
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
            poll_interval: Duration::from_millis(2_000),
        }
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
}

impl EffectEndpointReadiness {
    fn ready(self) -> bool {
        self.source_ready && self.input_ready
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
    handles: BTreeMap<String, MeterProcess>,
    targets: BTreeMap<String, MeterTarget>,
    last_attempts: BTreeMap<String, Instant>,
    last_requested_at: Option<Instant>,
}

#[derive(Debug, Default)]
struct MeterSupervisorUpdate {
    meters: Vec<LevelMeter>,
    started: usize,
    stopped: usize,
    failed: Vec<String>,
}

#[derive(Debug)]
struct MeterProcess {
    sample: Arc<Mutex<MeterSample>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct MeterSample {
    peak_left: f32,
    peak_right: f32,
    frames: u64,
    updated_at: Option<Instant>,
}

impl MeterSupervisor {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            handles: BTreeMap::new(),
            targets: BTreeMap::new(),
            last_attempts: BTreeMap::new(),
            last_requested_at: None,
        }
    }

    fn reconcile(
        &mut self,
        targets: Vec<MeterTarget>,
        mark_requested: bool,
    ) -> MeterSupervisorUpdate {
        let mut update = MeterSupervisorUpdate::default();
        if mark_requested {
            self.last_requested_at = Some(Instant::now());
        }
        if self.dry_run || !meter_sampling_enabled() {
            update.stopped += self.handles.len();
            self.stop_all();
            return update;
        }

        let targets = targets
            .into_iter()
            .map(|target| (target.node_id.clone(), target))
            .collect::<BTreeMap<_, _>>();
        self.targets = targets.clone();
        let source_names = targets
            .values()
            .map(|target| target.source_name.clone())
            .collect::<BTreeSet<_>>();
        let now = Instant::now();
        let mut stopped = Vec::new();
        for (source_name, handle) in &mut self.handles {
            let exited = handle.has_exited();
            if !source_names.contains(source_name) || exited {
                stopped.push((source_name.clone(), exited));
            }
        }
        update.stopped += stopped.len();
        for (source_name, exited) in stopped {
            self.handles.remove(&source_name);
            if exited {
                self.last_attempts.insert(source_name, now);
            } else {
                self.last_attempts.remove(&source_name);
            }
        }

        self.last_attempts
            .retain(|source_name, _| source_names.contains(source_name));
        for source_name in source_names {
            if self.handles.contains_key(&source_name) {
                continue;
            }
            if self
                .last_attempts
                .get(&source_name)
                .is_some_and(|attempt| now.duration_since(*attempt) < METER_RESTART_BACKOFF)
            {
                continue;
            }
            self.last_attempts.insert(source_name.clone(), now);
            match MeterProcess::spawn(&source_name) {
                Ok(handle) => {
                    self.last_attempts.remove(&source_name);
                    self.handles.insert(source_name, handle);
                    update.started += 1;
                }
                Err(err) => update.failed.push(format!("{source_name}: {err}")),
            }
        }

        update.meters = self.snapshot();
        update
    }

    fn snapshot_or_stop_idle(&mut self) -> MeterSupervisorUpdate {
        let now = Instant::now();
        if self.requested_recently_at(now) {
            return MeterSupervisorUpdate {
                meters: self.snapshot(),
                ..MeterSupervisorUpdate::default()
            };
        }
        self.reconcile(Vec::new(), false)
    }

    fn requested_recently(&self) -> bool {
        self.requested_recently_at(Instant::now())
    }

    fn requested_recently_at(&self, now: Instant) -> bool {
        self.last_requested_at
            .is_some_and(|requested_at| now.duration_since(requested_at) <= METER_IDLE_STOP_AFTER)
    }

    fn snapshot(&self) -> Vec<LevelMeter> {
        self.targets
            .values()
            .filter_map(|target| {
                self.handles
                    .get(&target.source_name)
                    .map(|handle| handle.level_meter(target))
            })
            .collect()
    }

    fn stop_all(&mut self) {
        self.handles.clear();
        self.targets.clear();
        self.last_attempts.clear();
        self.last_requested_at = None;
    }
}

impl MeterProcess {
    fn spawn(source_name: &str) -> Result<Self, std::io::Error> {
        let endpoint = MeterEndpoint::from_source_name(source_name);
        let endpoint_context = endpoint.describe();
        let sample = Arc::new(Mutex::new(MeterSample::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let reader_sample = Arc::clone(&sample);
        let reader_stop = Arc::clone(&stop);
        let thread_name = format!("{}-meter-{}", graph_prefix(), safe_file_id(source_name));
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run_pipewire_meter_stream(endpoint, reader_sample, reader_stop, ready_tx);
            })
            .map_err(std::io::Error::other)?;

        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(Self {
                sample,
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

    fn level_meter(&self, target: &MeterTarget) -> LevelMeter {
        let sample = self.sample.lock().map(|sample| *sample).unwrap_or_default();
        let now = Instant::now();
        let gain = if target.muted { 0.0 } else { target.gain }.clamp(0.0, 1.5);
        LevelMeter {
            node_id: target.node_id.clone(),
            peak_left: meter_output_level(
                stale_adjusted_meter_peak(sample.peak_left, sample.updated_at, now),
                gain,
            ),
            peak_right: meter_output_level(
                stale_adjusted_meter_peak(sample.peak_right, sample.updated_at, now),
                gain,
            ),
        }
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

fn run_pipewire_meter_stream(
    endpoint: MeterEndpoint,
    sample: Arc<Mutex<MeterSample>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    if let Err(err) = run_pipewire_meter_stream_inner(endpoint, sample, stop, ready.clone()) {
        let _ = ready.send(Err(err));
    }
}

fn run_pipewire_meter_stream_inner(
    endpoint: MeterEndpoint,
    sample: Arc<Mutex<MeterSample>>,
    stop: Arc<AtomicBool>,
    ready: mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();
    let endpoint_context = endpoint.describe();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|err| format!("PipeWire meter mainloop creation failed: {err}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|err| format!("PipeWire meter context creation failed: {err}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|err| format!("PipeWire meter core connection failed: {err}"))?;
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "DSP",
        *pw::keys::MEDIA_NAME => format!("{} VU Meter", app_display_name()),
        *pw::keys::NODE_NAME => format!("{}-meter-{}", graph_prefix(), safe_file_id(&endpoint.source_name)),
        *pw::keys::NODE_DESCRIPTION => format!("{} meter for {}", app_display_name(), endpoint.source_name),
        *pw::keys::NODE_VIRTUAL => "true",
        *pw::keys::NODE_PASSIVE => "true",
        *pw::keys::TARGET_OBJECT => endpoint.target_object.clone(),
    };
    if endpoint.dont_remix {
        props.insert(*pw::keys::STREAM_DONT_REMIX, "true");
    }
    if endpoint.dont_reconnect {
        props.insert(*pw::keys::NODE_DONT_RECONNECT, "true");
    }
    props.insert("application.name", app_display_name());
    props.insert("node.latency", "256/48000");
    props.insert("node.dont-move", "true");
    props.insert("state.restore-props", "false");
    props.insert("state.restore-target", "false");
    props.insert(graph_prop("managed"), "1");
    props.insert(graph_prop("role"), "meter");
    if endpoint.capture_sink_monitor {
        props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    }

    let stream = pw::stream::StreamBox::new(&core, &format!("{}-meter", graph_prefix()), props)
        .map_err(|err| format!("PipeWire meter stream creation failed: {err}"))?;
    let data = PipeWireMeterData {
        format: Default::default(),
        sample,
    };
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .param_changed(|_, user_data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param)
            else {
                return;
            };
            if media_type != spa::param::format::MediaType::Audio
                || media_subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let _ = user_data.format.parse(param);
        })
        .process(|stream, user_data| {
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
            if end > bytes.len() {
                return;
            }
            consume_meter_interleaved_f32le(&bytes[offset..end], channels, &user_data.sample);
        })
        .register()
        .map_err(|err| err.to_string())?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(48_000);
    audio_info.set_channels(2);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|err| err.to_string())?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values)
        .ok_or_else(|| "PipeWire meter format pod was invalid".to_string())?];

    let mut stream_flags =
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS;
    if endpoint.dont_reconnect {
        stream_flags |= pw::stream::StreamFlags::DONT_RECONNECT;
    }
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            stream_flags,
            &mut params,
        )
        .map_err(|err| {
            format!("PipeWire meter stream connect failed: {err}; {endpoint_context}")
        })?;
    let _ = ready.send(Ok(()));

    while !stop.load(Ordering::SeqCst) {
        mainloop.loop_().iterate(Duration::from_millis(5));
    }

    Ok(())
}

struct PipeWireMeterData {
    format: spa::param::audio::AudioInfoRaw,
    sample: Arc<Mutex<MeterSample>>,
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
fn consume_meter_bytes(bytes: &[u8], pending: &mut Vec<u8>, sample: &Arc<Mutex<MeterSample>>) {
    pending.extend_from_slice(bytes);
    let frame_bytes = (pending.len() / 8) * 8;
    if frame_bytes == 0 {
        return;
    }

    consume_meter_interleaved_f32le(&pending[..frame_bytes], 2, sample);
    pending.drain(..frame_bytes);
}

fn consume_meter_interleaved_f32le(
    bytes: &[u8],
    channels: usize,
    sample: &Arc<Mutex<MeterSample>>,
) {
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

    if let Ok(mut sample) = sample.lock() {
        sample.peak_left = gate_meter_peak(incoming_left);
        sample.peak_right = gate_meter_peak(incoming_right);
        sample.frames = sample.frames.saturating_add(frames);
        sample.updated_at = Some(Instant::now());
    }
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
}

#[derive(Debug)]
pub struct WaveLinuxEngine {
    paths: EnginePaths,
    options: EngineOptions,
    pw: PwClient,
    startup_defaults: DefaultDevices,
    config: RwLock<MixerConfig>,
    runtime: RwLock<RuntimeCache>,
    meter_supervisor: Mutex<MeterSupervisor>,
    effect_chain_processes: Mutex<BTreeMap<String, EffectChainProcess>>,
    runtime_refresh: Mutex<()>,
    host_diagnostics: Mutex<TimedCache<Vec<Diagnostic>>>,
    effect_availability: Mutex<TimedCache<Vec<EffectAvailability>>>,
    hardware_profiles: Arc<Mutex<TimedCache<HardwareProfileCatalog>>>,
    remote_profile_sync: Arc<Mutex<RemoteProfileSyncState>>,
    slow_refresh_log: Mutex<SlowRefreshLogState>,
    audio_commands: Mutex<()>,
    capture_move_failures: Mutex<BTreeMap<String, CaptureMoveFailure>>,
    app_stream_move_failures: Mutex<BTreeMap<String, Instant>>,
    deferred_effect_sync: Mutex<DeferredEffectSync>,
    deferred_graph_repair: Mutex<DeferredGraphRepair>,
    route_health_repair: Mutex<RouteHealthRepairState>,
    stop: AtomicBool,
}

#[derive(Debug, Default)]
struct DeferredEffectSync {
    generation: u64,
    channel_ids: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct DeferredGraphRepair {
    generation: u64,
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

impl WaveLinuxEngine {
    pub fn from_xdg() -> Result<Arc<Self>, EngineError> {
        Self::from_xdg_for_app_version(env!("CARGO_PKG_VERSION"))
    }

    pub fn from_xdg_for_app_version(app_version: &str) -> Result<Arc<Self>, EngineError> {
        let paths = EnginePaths::from_xdg()?;
        maintain_logs_for_paths(&paths, app_version)?;
        Self::new(paths, EngineOptions::default())
    }

    pub fn new(paths: EnginePaths, options: EngineOptions) -> Result<Arc<Self>, EngineError> {
        fs::create_dir_all(&paths.config_dir)?;
        fs::create_dir_all(paths.local_hardware_profiles_dir())?;
        let config = load_config(&paths)?.normalized()?;
        let pw = PwClient::new(options.dry_run);
        let startup_defaults = DefaultDevices::capture(&pw);
        let engine = Arc::new(Self {
            pw,
            startup_defaults,
            runtime: RwLock::new(RuntimeCache::new(options.dry_run)),
            config: RwLock::new(config),
            meter_supervisor: Mutex::new(MeterSupervisor::new(options.dry_run)),
            effect_chain_processes: Mutex::new(BTreeMap::new()),
            runtime_refresh: Mutex::new(()),
            host_diagnostics: Mutex::new(TimedCache::default()),
            effect_availability: Mutex::new(TimedCache::default()),
            hardware_profiles: Arc::new(Mutex::new(TimedCache::default())),
            remote_profile_sync: Arc::new(Mutex::new(RemoteProfileSyncState::default())),
            slow_refresh_log: Mutex::new(SlowRefreshLogState::default()),
            audio_commands: Mutex::new(()),
            capture_move_failures: Mutex::new(BTreeMap::new()),
            app_stream_move_failures: Mutex::new(BTreeMap::new()),
            deferred_effect_sync: Mutex::new(DeferredEffectSync::default()),
            deferred_graph_repair: Mutex::new(DeferredGraphRepair::default()),
            route_health_repair: Mutex::new(RouteHealthRepairState::default()),
            paths,
            options,
            stop: AtomicBool::new(false),
        });
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
                    config.settings.restore_audio_graph_on_launch,
                    config.settings.lock_default_input,
                    config.settings.lock_default_output,
                    engine.startup_defaults.sink.as_deref().unwrap_or("<none>"),
                    engine.startup_defaults.source.as_deref().unwrap_or("<none>"),
                    if meter_sampling_enabled() {
                        "pipewire-stream"
                    } else {
                        "disabled"
                    },
                ),
            );
        }
        let restore_on_launch = engine
            .read_config()
            .map(|config| config.settings.restore_audio_graph_on_launch)
            .unwrap_or(false);
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
        let startup_source_levels = engine.reset_startup_hardware_microphone_levels()?;
        if !startup_source_levels.is_empty() {
            engine.log_command_executions("startup.source-levels", &startup_source_levels);
        }
        if engine.options.auto_repair_on_start && restore_on_launch {
            if startup_graph_reusable {
                engine.log_engine_event(
                    "repair.startup",
                    "existing audio graph matched current profiles and routes; skipped startup repair",
                );
                let _ = engine.refresh_runtime();
            } else {
                let _ = engine.repair_audio_graph();
            }
        }
        #[cfg(not(test))]
        engine.schedule_hardware_profile_prewarm();
        Ok(engine)
    }

    pub fn spawn_background(self: &Arc<Self>) -> thread::JoinHandle<()> {
        let engine = Arc::clone(self);
        thread::spawn(move || {
            while !engine.stop.load(Ordering::SeqCst) {
                let _ = engine.refresh_runtime();
                thread::sleep(engine.options.poll_interval);
            }
        })
    }

    pub fn stop_background(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.stop_meter_supervisor();
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
        let _ = self.refresh_runtime_if_stale(UI_STATE_REFRESH_MAX_AGE);
        self.cached_state()
    }

    pub fn observe_state(&self) -> Result<AppStateSnapshot, EngineError> {
        self.refresh_cached_meters()?;
        self.cached_state()
    }

    pub fn observe_meters(&self) -> Result<Vec<LevelMeter>, EngineError> {
        let (graph, audio_graph_running) = {
            let runtime = self.read_runtime()?;
            (
                runtime.graph.clone(),
                runtime.status.audio_graph_running && !self.stop.load(Ordering::SeqCst),
            )
        };
        let config = self.read_config()?.clone();
        let config = effective_config_with_runtime_auto_devices(&config, &graph);
        let config = config_with_unavailable_effects_bypassed(&config, &graph);
        let config = self.config_with_unhealthy_effects_bypassed(&config);
        let meters = self.refresh_meter_supervisor(&config, &graph, audio_graph_running, true)?;
        let mut runtime = self.write_runtime()?;
        if runtime.status.audio_graph_running {
            runtime.graph.meters = meters.clone();
        } else if !runtime.graph.meters.is_empty() {
            runtime.graph.meters.clear();
        }
        Ok(meters)
    }

    fn cached_state(&self) -> Result<AppStateSnapshot, EngineError> {
        let config = self.read_config()?.clone();
        let runtime = self.read_runtime()?;
        Ok(AppStateSnapshot {
            config,
            graph: runtime.graph.clone(),
            diagnostics: runtime.diagnostics.clone(),
            engine: runtime.status.clone(),
            catalog: EffectCatalog::default(),
        })
    }

    pub fn refresh_runtime(&self) -> Result<(), EngineError> {
        let _runtime_refresh = self.lock_runtime_refresh()?;
        self.refresh_runtime_unlocked()
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
        let (mut graph, mut snapshot_command_timings) =
            self.snapshot_for_config_timed(Some(&config))?;
        record_refresh_phase(&mut refresh_phases, &mut phase_started, "snapshot");
        let mut bluetooth_cards = self.bluetooth_audio_cards().unwrap_or_default();
        let mut default_source = self.pw.default_source().ok().flatten();
        let mut default_sink = self.pw.default_sink().ok().flatten();
        let mut active_sink = self.pw.active_playback_sink().ok().flatten();
        let mut managed_modules = self.pw.managed_modules().unwrap_or_default();
        let mut source_outputs = self.pw.source_output_routes().unwrap_or_default();
        let mut sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
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
        let mut audio_graph_running = graph_has_wavelinux_nodes(&graph);
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
                let (next_graph, timings) = self.snapshot_for_config_timed(Some(&config))?;
                graph = next_graph;
                snapshot_command_timings.extend(timings);
                bluetooth_cards = self.bluetooth_audio_cards().unwrap_or_default();
                default_source = self.pw.default_source().ok().flatten();
                default_sink = self.pw.default_sink().ok().flatten();
                active_sink = self.pw.active_playback_sink().ok().flatten();
                managed_modules = self.pw.managed_modules().unwrap_or_default();
                source_outputs = self.pw.source_output_routes().unwrap_or_default();
                sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
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
            },
        );
        let active_effect_route_repair_needed =
            active_effect_routes_need_repair(&desired_audio_config, &graph, &managed_modules);
        let route_health = route_health_issues(
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
        let default_device_lock_repair_needed = default_device_lock_repair_needed(
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
        let mut route_mutations_deferred = false;
        let mut route_mutation_requested = false;
        if audio_graph_running
            && !self.stop.load(Ordering::SeqCst)
            && (auto_device_repair_needed
                || active_effect_route_repair_needed
                || route_health_repair_needed
                || realtime_fallback_repair_needed
                || bluetooth_monitor_route_refresh_needed)
        {
            route_mutation_requested = true;
            let default_lock_only_repair = default_device_lock_repair_needed
                && !auto_device_route_repair_needed
                && !active_effect_route_repair_needed
                && !route_health_repair_needed
                && !realtime_fallback_repair_needed
                && !bluetooth_monitor_route_refresh_needed;
            let reason = if default_lock_only_repair {
                "default audio device selection changed; restoring app-facing default only"
            } else if bluetooth_monitor_route_refresh_needed
                && !auto_device_repair_needed
                && !active_effect_route_repair_needed
                && !route_health_repair_needed
                && !realtime_fallback_repair_needed
            {
                "Bluetooth monitor route changed or duplicated; rebuilding final output route"
            } else if realtime_fallback_repair_needed
                && !auto_device_repair_needed
                && !active_effect_route_repair_needed
                && !route_health_repair_needed
            {
                "realtime FX fallback triggered; rebuilding affected effect chains"
            } else if route_health_repair_needed {
                "managed audio route is stale or detached; repairing audio routes"
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
                        self.rebuild_effect_chain_configs()?;
                        outputs.extend(
                            self.sync_effect_channels_unlocked(&realtime_fallback_channel_ids)?,
                        );
                    }
                    if active_effect_route_repair_needed || route_health_repair_needed {
                        outputs.extend(self.repair_audio_graph_unlocked()?.outputs);
                    } else if !default_lock_only_repair
                        && (auto_device_route_repair_needed || default_device_lock_repair_needed)
                    {
                        outputs.extend(self.repair_auto_device_routes_unlocked()?);
                    }
                    self.log_command_executions("hotplug.device", &outputs);
                    if default_lock_only_repair {
                        default_source = self.pw.default_source().ok().flatten();
                        default_sink = self.pw.default_sink().ok().flatten();
                        active_sink = self.pw.active_playback_sink().ok().flatten();
                    } else {
                        let (next_graph, timings) =
                            self.snapshot_for_config_timed(Some(&config))?;
                        graph = next_graph;
                        snapshot_command_timings.extend(timings);
                        bluetooth_cards = self.bluetooth_audio_cards().unwrap_or_default();
                        default_source = self.pw.default_source().ok().flatten();
                        default_sink = self.pw.default_sink().ok().flatten();
                        active_sink = self.pw.active_playback_sink().ok().flatten();
                        managed_modules = self.pw.managed_modules().unwrap_or_default();
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
            let graph_ready_for_apps =
                app_routing_graph_ready(&audio_config, &graph, &managed_modules);
            let rescued_streams = self.move_unready_routed_streams_to_default(
                &audio_config,
                &graph,
                &managed_modules,
            )?;
            let routed_streams = if graph_ready_for_apps {
                self.route_configured_streams(&audio_config, &graph.app_streams)?
            } else {
                self.log_engine_event(
                    "route.streams",
                    "audio graph is not ready for app routing; leaving apps on real outputs",
                );
                false
            };
            let updated_volumes =
                self.apply_configured_stream_volumes(&config, &graph.app_streams)?;
            source_outputs = self.pw.source_output_routes().unwrap_or_default();
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
            if rescued_streams || routed_streams || updated_volumes || moved_capture_streams {
                let (next_graph, timings) = self.snapshot_for_config_timed(Some(&config))?;
                graph = next_graph;
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
        let auto_devices = resolved_auto_devices_for_config(
            &config,
            &graph.inputs,
            &graph.outputs,
            &bluetooth_cards,
            default_source.as_deref(),
            default_sink.as_deref(),
            active_sink.as_deref(),
        );
        let diagnostics = self.host_diagnostics()?;
        let healthy = diagnostics
            .iter()
            .all(|item| item.severity != DiagnosticSeverity::Error);
        let mut runtime = self.write_runtime()?;
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
        Ok(())
    }

    fn route_health_repair_allowed(&self, issues: &[RouteHealthIssue]) -> bool {
        let signature = route_health_signature(issues);
        let summary = route_health_summary(issues);
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

    pub fn repair_audio_graph(&self) -> Result<RepairReport, EngineError> {
        self.log_engine_event("repair.start", "requested audio graph repair");
        let report = {
            let _audio_commands = self.lock_audio_commands()?;
            self.repair_audio_graph_unlocked()?
        };
        let _ = self.refresh_runtime();
        Ok(report)
    }

    fn repair_audio_graph_unlocked(&self) -> Result<RepairReport, EngineError> {
        let started = Instant::now();
        let mut outputs = self.ensure_bluetooth_a2dp_profiles(true)?;
        self.log_command_executions("repair.bluetooth", &outputs);
        if outputs
            .iter()
            .any(|output| !output.skipped && output.error.is_none())
        {
            thread::sleep(Duration::from_millis(250));
        }
        let config = self.effective_config_for_audio_graph(&self.read_config()?.clone());
        let monitor_preroute_outputs = self.preload_monitor_output_routes_for_config(&config)?;
        let preserve_stale_monitor_routes = monitor_preroute_outputs.iter().any(|output| {
            output.error.is_some() || output.stderr.contains("preserving existing monitor route")
        });
        self.log_command_executions("repair.preroute", &monitor_preroute_outputs);
        outputs.extend(monitor_preroute_outputs);
        let cleanup_outputs =
            self.cleanup_stale_modules_for_config(&config, preserve_stale_monitor_routes)?;
        self.log_command_executions("repair.cleanup", &cleanup_outputs);
        outputs.extend(cleanup_outputs);
        self.rebuild_effect_chain_configs()?;

        let mut planned = plan_ensure_graph(&config);
        let planned_count = planned.commands.len();
        let existing_graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        let managed_modules = self.pw.managed_modules().unwrap_or_default();
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        let sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
        let active_effect_channel_ids = config
            .channels
            .iter()
            .filter(|channel| channel_has_active_effects(channel))
            .map(|channel| channel.id.clone())
            .collect::<BTreeSet<_>>();
        planned.commands.retain(|command| {
            if command_routes_active_effect_channel(command, &active_effect_channel_ids) {
                return true;
            }
            !repair_command_is_satisfied(
                command,
                &existing_graph,
                &source_outputs,
                &sink_inputs,
                &managed_modules,
            )
        });

        let (graph_commands, mut route_commands) = split_repair_commands(&planned.commands);
        self.log_engine_event(
            "repair.plan",
            format!(
                "planned={} retained={} graph_commands={} route_commands={} managed_modules={} source_outputs={} sink_inputs={} inputs={} outputs={}",
                planned_count,
                planned.commands.len(),
                graph_commands.len(),
                route_commands.len(),
                managed_modules.len(),
                source_outputs.len(),
                sink_inputs.len(),
                existing_graph.inputs.len(),
                existing_graph.outputs.len(),
            ),
        );
        outputs.extend(
            self.pw
                .execute_all(graph_commands)
                .into_iter()
                .map(command_execution),
        );

        outputs.extend(self.start_effect_chain_processes(&config)?);
        let mut route_config = config.clone();
        let active_effect_channels = config
            .channels
            .iter()
            .filter(|channel| channel_has_active_effects(channel))
            .collect::<Vec<_>>();
        if !active_effect_channels.is_empty() {
            for channel in &active_effect_channels {
                let _ = self.wait_for_effect_nodes(channel);
            }
            let post_effect_graph = self
                .pw
                .snapshot_for_config_with_effect_availability(None, Vec::new());
            let mut missing_effect_channels = Vec::new();
            for channel in &mut route_config.channels {
                if !channel_has_active_effects(channel) {
                    continue;
                }
                if effect_chain_endpoint_readiness_for_graph(&post_effect_graph, channel).ready() {
                    continue;
                }
                missing_effect_channels.push(channel.name.clone());
                for effect in &mut channel.effects {
                    effect.bypassed = true;
                }
            }
            if !missing_effect_channels.is_empty() {
                self.log_engine_event(
                    "repair.effects",
                    format!(
                        "missing FX sources for {}; routing affected channels from raw monitors",
                        missing_effect_channels.join(", ")
                    ),
                );
                let fallback_plan = plan_ensure_graph(&route_config);
                let (_, fallback_route_commands) = split_repair_commands(&fallback_plan.commands);
                let managed_modules = self.pw.managed_modules().unwrap_or_default();
                let source_outputs = self.pw.source_output_routes().unwrap_or_default();
                let sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
                route_commands = fallback_route_commands
                    .into_iter()
                    .filter(|command| {
                        !repair_command_is_satisfied(
                            command,
                            &post_effect_graph,
                            &source_outputs,
                            &sink_inputs,
                            &managed_modules,
                        )
                    })
                    .collect();
            }
            outputs.extend(self.cleanup_modules(|module| {
                matches!(
                    module.role.as_deref(),
                    Some("channel_to_mix") | Some("channel_to_effect")
                ) && module
                    .channel_id
                    .as_deref()
                    .is_some_and(|channel_id| active_effect_channel_ids.contains(channel_id))
            })?);
        }

        outputs.extend(
            self.pw
                .execute_all(route_commands)
                .into_iter()
                .map(command_execution),
        );
        outputs.extend(self.apply_graph_levels(&route_config)?);
        outputs.extend(self.apply_default_device_locks(&route_config)?);
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        outputs.extend(self.execute_capture_stream_moves_unlocked(&route_config, &source_outputs)?);
        self.log_command_executions("repair.outputs", &outputs);
        self.log_engine_event(
            "repair.end",
            format!(
                "outputs={} failed={} skipped={} elapsed_ms={}",
                outputs.len(),
                outputs
                    .iter()
                    .filter(|output| output.error.is_some())
                    .count(),
                outputs.iter().filter(|output| output.skipped).count(),
                started.elapsed().as_millis(),
            ),
        );
        Ok(RepairReport {
            dry_run: self.options.dry_run,
            planned,
            outputs,
        })
    }

    fn repair_auto_device_routes_unlocked(&self) -> Result<Vec<CommandExecution>, EngineError> {
        let started = Instant::now();
        let config = self.effective_config_for_audio_graph(&self.read_config()?.clone());
        let monitor_preroute_outputs = self.preload_monitor_output_routes_for_config(&config)?;
        let preserve_stale_monitor_routes = monitor_preroute_outputs.iter().any(|output| {
            output.error.is_some() || output.stderr.contains("preserving existing monitor route")
        });
        let mut outputs = monitor_preroute_outputs;
        outputs.extend(self.cleanup_stale_auto_device_modules_for_config(
            &config,
            preserve_stale_monitor_routes,
        )?);

        let mut planned = plan_ensure_graph(&config);
        let planned_count = planned.commands.len();
        let existing_graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        let managed_modules = self.pw.managed_modules().unwrap_or_default();
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        let sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
        planned.commands.retain(|command| {
            command_is_auto_device_route(command)
                && !repair_command_is_satisfied(
                    command,
                    &existing_graph,
                    &source_outputs,
                    &sink_inputs,
                    &managed_modules,
                )
        });
        self.log_engine_event(
            "repair.auto-device",
            format!(
                "planned={} retained={} managed_modules={} source_outputs={} sink_inputs={} inputs={} outputs={}",
                planned_count,
                planned.commands.len(),
                managed_modules.len(),
                source_outputs.len(),
                sink_inputs.len(),
                existing_graph.inputs.len(),
                existing_graph.outputs.len(),
            ),
        );
        outputs.extend(
            self.pw
                .execute_all(planned.commands)
                .into_iter()
                .map(command_execution),
        );
        outputs.extend(self.apply_default_device_locks(&config)?);
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        outputs.extend(self.execute_capture_stream_moves_unlocked(&config, &source_outputs)?);
        self.log_engine_event(
            "repair.auto-device",
            format!(
                "outputs={} failed={} skipped={} elapsed_ms={}",
                outputs.len(),
                outputs
                    .iter()
                    .filter(|output| output.error.is_some())
                    .count(),
                outputs.iter().filter(|output| output.skipped).count(),
                started.elapsed().as_millis(),
            ),
        );
        Ok(outputs)
    }

    fn repair_bluetooth_monitor_routes_unlocked(
        &self,
        config: &MixerConfig,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let plan = plan_ensure_graph(config);
        let monitor_commands = plan
            .commands
            .into_iter()
            .filter(command_is_mix_monitor_route)
            .filter(command_targets_bluetooth_sink)
            .collect::<Vec<_>>();
        if monitor_commands.is_empty() {
            return Ok(Vec::new());
        }

        let desired_routes = monitor_commands
            .iter()
            .filter_map(|command| {
                let properties = command_arg_value(&command.args, "source_output_properties=")?;
                let mix_id = graph_property_value_from_arg(properties, "mix_id")?;
                let sink = command_arg_value(&command.args, "sink=")?;
                Some((mix_id.to_owned(), sink.to_owned()))
            })
            .collect::<Vec<_>>();

        let mut outputs = self.cleanup_modules(|module| {
            module.role.as_deref() == Some("mix_monitor")
                && desired_routes.iter().any(|(mix_id, sink)| {
                    module.mix_id.as_deref() == Some(mix_id.as_str())
                        && module
                            .sink_name
                            .as_deref()
                            .is_some_and(|actual| audio_endpoint_names_match(actual, sink))
                })
        })?;

        if !outputs.is_empty() {
            thread::sleep(CLEANUP_MODULE_SETTLE);
        }

        let mut graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        if monitor_commands
            .iter()
            .any(|command| !monitor_route_endpoints_available(command, &graph))
        {
            for _ in 0..6 {
                thread::sleep(Duration::from_millis(200));
                graph = self
                    .pw
                    .snapshot_for_config_with_effect_availability(None, Vec::new());
                if monitor_commands
                    .iter()
                    .all(|command| monitor_route_endpoints_available(command, &graph))
                {
                    break;
                }
            }
        }

        if monitor_commands
            .iter()
            .any(|command| monitor_route_endpoints_available(command, &graph))
        {
            self.log_engine_event(
                "hotplug.output",
                "Bluetooth monitor route reset; waiting for A2DP transport before reconnecting",
            );
            thread::sleep(BLUETOOTH_MONITOR_ROUTE_SETTLE);
            graph = self
                .pw
                .snapshot_for_config_with_effect_availability(None, Vec::new());
        }

        let commands = monitor_commands
            .into_iter()
            .filter_map(|command| {
                if monitor_route_endpoints_available(&command, &graph) {
                    Some(command)
                } else {
                    outputs.push(skipped_command_with_stderr(
                        command,
                        "Bluetooth monitor output is not visible; keeping route disconnected",
                    ));
                    None
                }
            })
            .collect::<Vec<_>>();
        outputs.extend(
            self.pw
                .execute_all(commands)
                .into_iter()
                .map(command_execution),
        );
        Ok(outputs)
    }

    pub fn run_diagnostics(&self) -> Result<SoundCheckReport, EngineError> {
        let state = self.get_state()?;
        let mut diagnostics = state.diagnostics.clone();
        let config = self.effective_config_for_audio_graph(&state.config);
        let managed_modules = self.pw.managed_modules().unwrap_or_default();
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        let sink_input_routes = self.pw.sink_input_routes().unwrap_or_default();
        let route_health = route_health_issues(
            &config,
            &state.graph,
            &managed_modules,
            &source_outputs,
            &sink_input_routes,
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
        let planned = plan_ensure_graph(&config);
        let mut graph = self.snapshot_for_config(Some(&config))?;
        let audio_graph_running = graph_has_wavelinux_nodes(&graph);
        graph.meters = self.meter_snapshot_or_stop_idle()?;
        let mut diagnostics = self.host_diagnostics()?;
        let managed_modules = self.pw.managed_modules().unwrap_or_default();
        let sink_input_routes = self.pw.sink_input_routes().unwrap_or_default();
        let source_output_routes = self.pw.source_output_routes().unwrap_or_default();
        let route_health = route_health_issues(
            &config,
            &graph,
            &managed_modules,
            &source_output_routes,
            &sink_input_routes,
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

    pub fn create_mix(self: &Arc<Self>, name: String) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.create_mix(name))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn rename_mix(self: &Arc<Self>, mix_id: String, name: String) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.rename_mix(mix_id, name))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn move_mix(&self, mix_id: String, direction: i32) -> Result<Mix, EngineError> {
        self.update_config(|config| config.move_mix(mix_id, direction))?
    }

    pub fn delete_mix(self: &Arc<Self>, mix_id: String) -> Result<Mix, EngineError> {
        let removed = self.update_config(|config| config.delete_mix(mix_id))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(removed)
    }

    pub fn set_mix_volume(&self, mix_id: String, volume: f32) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.set_mix_volume(mix_id, volume))??;
        let graph_running = self.audio_graph_running_cached();
        self.log_engine_event(
            "level.mix",
            format!(
                "mix={} volume={:.3} graph_running={}",
                mix.id, mix.volume, graph_running
            ),
        );
        if graph_running {
            let _audio_commands = self.lock_audio_commands()?;
            let output =
                command_execution(self.pw.execute(plan_pw_set_mix_volume(&mix, mix.volume)));
            self.log_command_executions("level.mix", &[output]);
            self.refresh_meter_targets_after_level_change();
        }
        Ok(mix)
    }

    pub fn set_mix_mute(&self, mix_id: String, muted: bool) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.set_mix_mute(mix_id, muted))??;
        let graph_running = self.audio_graph_running_cached();
        self.log_engine_event(
            "level.mix",
            format!(
                "mix={} muted={} graph_running={}",
                mix.id, mix.muted, graph_running
            ),
        );
        if graph_running {
            let _audio_commands = self.lock_audio_commands()?;
            let output = command_execution(self.pw.execute(plan_pw_set_mix_mute(&mix, mix.muted)));
            self.log_command_executions("level.mix", &[output]);
            self.refresh_meter_targets_after_level_change();
        }
        Ok(mix)
    }

    pub fn set_mix_icon(&self, mix_id: String, icon: Option<String>) -> Result<Mix, EngineError> {
        self.update_config(|config| config.set_mix_icon(mix_id, icon))?
    }

    pub fn set_channel_icon(
        &self,
        channel_id: String,
        icon: Option<String>,
    ) -> Result<Channel, EngineError> {
        self.update_config(|config| config.set_channel_icon(channel_id, icon))?
    }

    pub fn set_mix_monitor_output(
        self: &Arc<Self>,
        mix_id: String,
        output: Option<String>,
    ) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| {
            let mix = config.set_mix_monitor_output(mix_id, output)?;
            if mix.id == "monitor" {
                config.settings.monitor_follows_default_output = false;
            }
            Ok(mix)
        })??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn set_mix_outputs(
        self: &Arc<Self>,
        mix_id: String,
        outputs: Vec<String>,
    ) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.set_mix_outputs(mix_id, outputs))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn create_channel(
        self: &Arc<Self>,
        name: String,
        kind: ChannelKind,
    ) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| config.create_channel(name, kind))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn rename_channel(
        self: &Arc<Self>,
        channel_id: String,
        name: String,
    ) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| config.rename_channel(channel_id, name))??;
        let _ = self.rebuild_effect_chain_configs();
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn move_channel(&self, channel_id: String, direction: i32) -> Result<Channel, EngineError> {
        self.update_config(|config| config.move_channel(channel_id, direction))?
    }

    pub fn delete_channel(self: &Arc<Self>, channel_id: String) -> Result<Channel, EngineError> {
        let removed = self.update_config(|config| config.delete_channel(channel_id))??;
        let _ = self.rebuild_effect_chain_configs();
        let _ = self.repair_audio_graph_if_running();
        Ok(removed)
    }

    pub fn set_channel_linked(
        &self,
        channel_id: String,
        linked: bool,
    ) -> Result<Channel, EngineError> {
        self.update_config(|config| config.set_channel_linked(channel_id, linked))?
    }

    pub fn set_channel_input(
        self: &Arc<Self>,
        channel_id: String,
        source_device: Option<String>,
    ) -> Result<Channel, EngineError> {
        let source_device = self.sanitize_hardware_input_for_bluetooth_a2dp(source_device);
        let channel =
            self.update_config(|config| config.set_channel_input(channel_id, source_device))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn set_hardware_input_device(
        self: &Arc<Self>,
        channel_id: String,
        source_device: Option<String>,
    ) -> Result<Channel, EngineError> {
        self.set_channel_input(channel_id, source_device)
    }

    pub fn set_channel_input_mode(
        self: &Arc<Self>,
        channel_id: String,
        input_mode: ChannelInputMode,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.set_channel_input_mode(channel_id, input_mode))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn restore_device(self: &Arc<Self>, kind: String) -> Result<MixerConfig, EngineError> {
        let normalized_kind = kind.trim().to_ascii_lowercase();
        let config = self.update_config(|config| {
            match normalized_kind.as_str() {
                "input" | "source" => {
                    let source = config.device_policy.restorable_input.clone();
                    if let Some(source) = source {
                        if let Some(channel) = config
                            .channels
                            .iter_mut()
                            .find(|channel| channel.kind.uses_hardware_slot())
                        {
                            channel.source_device = Some(source.clone());
                            config.device_policy.preferred_input = Some(source);
                        }
                    }
                    config.device_policy.restorable_input = None;
                    config.device_policy.active_input_fallback = false;
                }
                "output" | "sink" => {
                    let output = config.device_policy.restorable_output.clone();
                    if let Some(output) = output {
                        let mix_index = config
                            .mixes
                            .iter()
                            .position(|mix| mix.id == "monitor")
                            .or_else(|| (!config.mixes.is_empty()).then_some(0));
                        if let Some(mix_index) = mix_index {
                            let mix = &mut config.mixes[mix_index];
                            mix.set_outputs(vec![output.clone()]);
                            config.device_policy.preferred_output = Some(output);
                        }
                    }
                    config.device_policy.restorable_output = None;
                    config.device_policy.active_output_fallback = false;
                }
                _ => return Err(ModelError::InvalidName),
            }
            Ok(config.clone())
        })??;
        let _ = self.repair_audio_graph_if_running();
        Ok(config)
    }

    pub fn set_settings(
        self: &Arc<Self>,
        settings: MixerSettings,
    ) -> Result<MixerSettings, EngineError> {
        self.apply_start_at_login(settings.start_at_login)?;
        let (settings, audio_graph_needs_repair) = self.update_config(|config| {
            let previous = config.settings.clone();
            let settings = config.set_settings(settings);
            Ok((
                settings.clone(),
                settings_affect_audio_graph(&previous, &settings),
            ))
        })??;
        if audio_graph_needs_repair {
            let _ = self.repair_audio_graph_if_running();
        }
        Ok(settings)
    }

    pub fn list_hardware_profiles(&self) -> Result<HardwareProfileUiState, EngineError> {
        let catalog = self.hardware_profiles()?;
        let config = self.read_config()?.clone();
        Ok(hardware_profile_ui_state(&catalog, &config.device_policy))
    }

    pub fn streamer_devices_config(&self) -> Result<StreamerDevicesConfig, EngineError> {
        Ok(self.read_config()?.streamer_devices.clone())
    }

    pub fn ensure_streamer_binding_profiles(
        &self,
        profiles: Vec<StreamerBindingProfile>,
    ) -> Result<StreamerDevicesConfig, EngineError> {
        self.update_config(|config| Ok(config.ensure_streamer_binding_profiles(profiles)))?
    }

    pub fn set_streamer_device_enabled(
        &self,
        device_id: String,
        enabled: bool,
    ) -> Result<StreamerDevicesConfig, EngineError> {
        self.update_config(|config| config.set_streamer_device_enabled(device_id, enabled))?
    }

    pub fn set_streamer_binding_profile(
        &self,
        profile: StreamerBindingProfile,
    ) -> Result<StreamerBindingProfile, EngineError> {
        self.update_config(|config| config.set_streamer_binding_profile(profile))?
    }

    pub fn set_device_hardware_profile(
        &self,
        device_id: String,
        profile_id: Option<String>,
    ) -> Result<HardwareProfileUiState, EngineError> {
        let device_id = device_id.trim().to_string();
        if device_id.is_empty() {
            return Err(ModelError::InvalidName.into());
        }
        let profile_id = profile_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(profile_id) = profile_id.as_deref() {
            let config = self.read_config()?.clone();
            let catalog = self.hardware_profiles()?;
            if profile_id != config.device_policy.fallback_hardware_profile.id
                && !catalog
                    .profiles
                    .iter()
                    .any(|entry| entry.profile.id == profile_id)
            {
                return Err(ModelError::InvalidConfig(format!(
                    "unknown hardware profile: {profile_id}"
                ))
                .into());
            }
        }

        self.update_config(|config| {
            if let Some(profile_id) = profile_id.clone() {
                config
                    .device_policy
                    .hardware_profile_assignments
                    .insert(device_id.clone(), profile_id);
            } else {
                config
                    .device_policy
                    .hardware_profile_assignments
                    .remove(&device_id);
            }
            Ok(())
        })??;
        self.log_engine_event(
            "hardware.profile.assignment",
            format!(
                "device={} profile={}",
                device_id,
                profile_id.as_deref().unwrap_or("auto")
            ),
        );
        let _ = self.refresh_runtime();
        self.list_hardware_profiles()
    }

    pub fn set_fallback_hardware_profile(
        &self,
        fallback_profile: FallbackHardwareProfile,
    ) -> Result<HardwareProfileUiState, EngineError> {
        let fallback_profile = fallback_profile.normalized();
        let fallback_id = fallback_profile.id.clone();
        self.update_config(|config| {
            let old_id = config.device_policy.fallback_hardware_profile.id.clone();
            config.device_policy.fallback_hardware_profile = fallback_profile.clone();
            if old_id != fallback_id {
                for assigned_profile_id in config
                    .device_policy
                    .hardware_profile_assignments
                    .values_mut()
                {
                    if *assigned_profile_id == old_id {
                        *assigned_profile_id = fallback_id.clone();
                    }
                }
            }
            Ok(())
        })??;
        self.log_engine_event(
            "hardware.profile.fallback",
            format!("profile={} name={}", fallback_id, fallback_profile.name),
        );
        let _ = self.refresh_runtime();
        self.list_hardware_profiles()
    }

    pub fn set_hardware_profile_policy(
        &self,
        profile_id: String,
        name: Option<String>,
        latency_policy: LatencyPolicy,
        routing_policy: RoutingPolicy,
    ) -> Result<HardwareProfileUiState, EngineError> {
        let profile_id = clean_profile_id(profile_id)?;
        let name = name.and_then(clean_optional_profile_name);
        let config = self.read_config()?.clone();
        if profile_id == config.device_policy.fallback_hardware_profile.id {
            let mut fallback_profile = config.device_policy.fallback_hardware_profile.clone();
            if let Some(name) = name {
                fallback_profile.name = name;
            }
            fallback_profile.latency_policy = normalized_profile_latency(latency_policy);
            fallback_profile.routing_policy = normalized_profile_routing(routing_policy);
            return self.set_fallback_hardware_profile(fallback_profile);
        }

        let catalog = self.hardware_profiles()?;
        let mut profile = hardware_profile_by_id(&catalog, &profile_id)
            .cloned()
            .ok_or_else(|| {
                ModelError::InvalidConfig(format!("unknown hardware profile: {profile_id}"))
            })?;
        if let Some(name) = name {
            profile.name = name;
        }
        profile.latency_policy = normalized_profile_latency(latency_policy);
        profile.routing_policy = normalized_profile_routing(routing_policy);
        profile.revision = profile.revision.saturating_add(1).max(1);
        let path = self.write_local_hardware_profile_override(&profile)?;
        self.reload_hardware_profiles_cache()?;
        self.log_engine_event(
            "hardware.profile.override",
            format!("profile={} path={}", profile.id, path.display()),
        );
        let _ = self.refresh_runtime();
        self.list_hardware_profiles()
    }

    pub fn set_channel_volume(
        &self,
        channel_id: String,
        mix_id: String,
        volume: f32,
    ) -> Result<wavelinux_model::MixBus, EngineError> {
        let (bus, channel) = self.update_config(|config| {
            let bus = config.set_channel_volume(channel_id.clone(), mix_id.clone(), volume)?;
            let channel = config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .cloned()
                .ok_or_else(|| ModelError::ChannelNotFound(channel_id.clone()))?;
            Ok((bus, channel))
        })??;

        let graph_running = self.audio_graph_running_cached();
        self.log_engine_event(
            "level.channel",
            format!(
                "channel={} mix={} volume={:.3} linked={} graph_running={}",
                channel.id, mix_id, bus.volume, channel.linked, graph_running
            ),
        );
        if !graph_running {
            return Ok(bus);
        }

        let _audio_commands = self.lock_audio_commands()?;
        let mut outputs = Vec::new();
        if channel.linked {
            for (linked_mix_id, linked_bus) in &channel.mix_buses {
                if !linked_bus.enabled {
                    continue;
                }
                outputs.extend(self.execute_channel_bus_volume_unlocked(
                    &channel.id,
                    linked_mix_id,
                    linked_bus.volume,
                ));
            }
        } else if bus.enabled {
            outputs.extend(self.execute_channel_bus_volume_unlocked(
                &channel.id,
                &mix_id,
                bus.volume,
            ));
        }
        self.log_command_executions("level.channel", &outputs);
        self.refresh_meter_targets_after_level_change();
        Ok(bus)
    }

    pub fn set_channel_mute(
        &self,
        channel_id: String,
        mix_id: String,
        muted: bool,
    ) -> Result<wavelinux_model::MixBus, EngineError> {
        let bus = self.update_config(|config| {
            config.set_channel_mute(channel_id.clone(), mix_id.clone(), muted)
        })??;
        let graph_running = self.audio_graph_running_cached();
        self.log_engine_event(
            "level.channel",
            format!(
                "channel={} mix={} muted={} graph_running={}",
                channel_id, mix_id, bus.muted, graph_running
            ),
        );
        if !graph_running {
            return Ok(bus);
        }

        let _audio_commands = self.lock_audio_commands()?;
        let outputs = if bus.enabled {
            self.execute_channel_bus_mute_unlocked(&channel_id, &mix_id, bus.muted)
        } else {
            Vec::new()
        };
        self.log_command_executions("level.channel", &outputs);
        self.refresh_meter_targets_after_level_change();
        Ok(bus)
    }

    pub fn set_channel_bus_enabled(
        self: &Arc<Self>,
        channel_id: String,
        mix_id: String,
        enabled: bool,
    ) -> Result<wavelinux_model::MixBus, EngineError> {
        let bus = self.update_config(|config| {
            config.set_channel_bus_enabled(channel_id.clone(), mix_id.clone(), enabled)
        })??;
        let _ = self.repair_audio_graph_if_running();
        Ok(bus)
    }

    pub fn assign_app_to_channel(
        &self,
        channel_id: String,
        matcher: AppMatcher,
    ) -> Result<AppRoute, EngineError> {
        self.update_config(|config| config.assign_app_to_channel(channel_id, matcher))?
    }

    pub fn remove_app_route(&self, matcher: AppMatcher) -> Result<Option<AppRoute>, EngineError> {
        self.update_config(|config| Ok(config.remove_app_route(matcher)))?
    }

    pub fn set_app_volume_preset(
        &self,
        matcher: AppMatcher,
        volume: f32,
    ) -> Result<AppVolumePreset, EngineError> {
        self.update_config(|config| config.set_app_volume_preset(matcher, volume))?
    }

    pub fn remove_app_volume_preset(
        &self,
        matcher: AppMatcher,
    ) -> Result<Option<AppVolumePreset>, EngineError> {
        self.update_config(|config| Ok(config.remove_app_volume_preset(matcher)))?
    }

    pub fn forget_app(&self, matcher: AppMatcher) -> Result<Option<KnownApp>, EngineError> {
        self.update_config(|config| Ok(config.forget_app(matcher)))?
    }

    pub fn restore_app(&self, matcher: AppMatcher) -> Result<Option<KnownApp>, EngineError> {
        self.update_config(|config| Ok(config.restore_app(matcher)))?
    }

    pub fn pin_app_identity(
        &self,
        matcher: AppMatcher,
        label: String,
    ) -> Result<KnownApp, EngineError> {
        self.update_config(|config| config.pin_app_identity(matcher, label))?
    }

    pub fn merge_app_identity(
        &self,
        source: AppMatcher,
        target: AppMatcher,
    ) -> Result<KnownApp, EngineError> {
        self.update_config(|config| config.merge_app_identity(source, target))?
    }

    pub fn reset_app_identity(&self, matcher: AppMatcher) -> Result<Option<KnownApp>, EngineError> {
        self.update_config(|config| Ok(config.reset_app_identity(matcher)))?
    }

    pub fn move_app_stream(
        &self,
        stream_id: String,
        channel_id: String,
    ) -> Result<CommandExecution, EngineError> {
        let saved_config = self.read_config()?.clone();
        let route_config = self.effective_config_for_audio_graph(&saved_config);
        let channel = route_config
            .channels
            .iter()
            .find(|channel| channel.id == channel_id)
            .cloned()
            .ok_or_else(|| ModelError::ChannelNotFound(channel_id.clone()))?;
        let command = plan_move_app_stream(&stream_id, &channel);
        if !self.audio_graph_running_cached() {
            return Ok(skipped_command(command));
        }

        let _audio_commands = self.lock_audio_commands()?;
        let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
        let output = ignore_stale_stream_command(output, &stream_id);
        if output.error.is_none() && !output.skipped {
            let level_outputs = self.apply_managed_route_levels(&route_config)?;
            self.log_command_executions("route.levels", &level_outputs);
        }
        Ok(output)
    }

    pub fn move_app_stream_to_default(
        &self,
        stream_id: String,
    ) -> Result<CommandExecution, EngineError> {
        let command = plan_move_app_stream_to_default(&stream_id);
        if !self.audio_graph_running_cached() {
            return Ok(skipped_command(command));
        }

        let _audio_commands = self.lock_audio_commands()?;
        let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
        Ok(ignore_stale_stream_command(output, &stream_id))
    }

    pub fn set_app_stream_volume(
        &self,
        stream_id: String,
        volume: f32,
    ) -> Result<CommandExecution, EngineError> {
        let command = plan_set_stream_volume(&stream_id, volume);
        if !self.audio_graph_running_cached() {
            return Ok(skipped_command(command));
        }

        let _audio_commands = self.lock_audio_commands()?;
        let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
        Ok(ignore_stale_stream_command(output, &stream_id))
    }

    pub fn set_app_stream_mute(
        &self,
        stream_id: String,
        muted: bool,
    ) -> Result<CommandExecution, EngineError> {
        let command = plan_set_stream_mute(&stream_id, muted);
        if !self.audio_graph_running_cached() {
            return Ok(skipped_command(command));
        }

        let _audio_commands = self.lock_audio_commands()?;
        let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
        Ok(ignore_stale_stream_command(output, &stream_id))
    }

    pub fn set_effect_chain(
        self: &Arc<Self>,
        channel_id: String,
        effects: Vec<EffectInstance>,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.set_effect_chain(channel_id, effects))??;
        self.schedule_effect_graph_sync(channel.id.clone());
        Ok(channel)
    }

    pub fn set_effect_param(
        self: &Arc<Self>,
        channel_id: String,
        instance_id: String,
        param_id: String,
        value: f32,
    ) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| {
            config.set_effect_param(channel_id, instance_id, param_id, value)
        })??;
        self.schedule_effect_graph_sync(channel.id.clone());
        Ok(channel)
    }

    pub fn bypass_effect(
        self: &Arc<Self>,
        channel_id: String,
        instance_id: String,
        bypassed: bool,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.bypass_effect(channel_id, instance_id, bypassed))??;
        self.schedule_effect_graph_sync(channel.id.clone());
        Ok(channel)
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
        let _audio_commands = self.lock_audio_commands()?;
        let outputs = self.cleanup_stale_modules_for_config(&config, false)?;
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
        let managed_modules = self.pw.managed_modules().unwrap_or_default();
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        let sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
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

        if !app_routing_graph_ready(&effective_config, &graph, &managed_modules) {
            self.log_engine_event(
                "startup.cleanup",
                "existing graph routes do not match current config; forcing rebuild",
            );
            return Ok(false);
        }

        Ok(
            route_diagnostics(&effective_config, &graph, &managed_modules).is_empty()
                && route_health_issues(
                    &effective_config,
                    &graph,
                    &managed_modules,
                    &source_outputs,
                    &sink_inputs,
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

    fn reset_startup_hardware_microphone_levels(
        &self,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let inputs = match self.pw.list_inputs() {
            Ok(inputs) => inputs,
            Err(err) => {
                self.log_engine_event(
                    "startup.source-levels",
                    format!(
                        "skipped microphone level reset because inputs could not be read: {err}"
                    ),
                );
                return Ok(Vec::new());
            }
        };
        let bluetooth_cards = self.bluetooth_audio_cards().unwrap_or_default();
        let commands = startup_microphone_level_reset_commands(&inputs, &bluetooth_cards);
        if commands.is_empty() {
            self.log_engine_event("startup.source-levels", "no hardware microphones to reset");
            return Ok(Vec::new());
        }

        let _audio_commands = self.lock_audio_commands()?;
        Ok(commands
            .into_iter()
            .map(|command| {
                let stream_id = command_stream_id(&command).map(str::to_string);
                let result = self.pw.execute(command.clone());
                let output = command_execution_with_spec(command, result);
                if let Some(stream_id) = stream_id {
                    ignore_stale_stream_command(output, &stream_id)
                } else {
                    output
                }
            })
            .collect())
    }

    fn route_configured_streams(
        &self,
        config: &MixerConfig,
        streams: &[AppStream],
    ) -> Result<bool, EngineError> {
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
            return Ok(false);
        }

        self.log_engine_event(
            "route.streams",
            format!(
                "routing {} configured app stream(s): {}",
                routes.len(),
                routes
                    .iter()
                    .map(|(stream, channel)| format!("{}->{}", stream.id, channel.id))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        );
        let _audio_commands = self.lock_audio_commands()?;
        for (stream, channel) in routes {
            let command = plan_move_app_stream(&stream.id, &channel);
            let output = command_execution_with_spec(command.clone(), self.pw.execute(command));
            let output = ignore_stale_stream_command(output, &stream.id);
            if output.skipped && output.stderr.contains("disappeared") {
                self.log_engine_event(
                    "route.streams",
                    format!(
                        "stream {} disappeared before configured routing; ignoring stale state",
                        stream.id
                    ),
                );
                continue;
            }

            let move_succeeded = output.error.is_none() && !output.skipped;
            self.remember_app_stream_move_result(&stream.id, &output)?;
            self.log_command_executions("route.streams", std::slice::from_ref(&output));
            if move_succeeded {
                let target_volume = configured_volume_for_stream(config, &stream).unwrap_or(1.0);
                let volume_command = plan_set_stream_volume(&stream.id, target_volume);
                let volume_output = command_execution_with_spec(
                    volume_command.clone(),
                    self.pw.execute(volume_command),
                );
                self.log_command_executions("route.streams", std::slice::from_ref(&volume_output));
            }
        }
        Ok(true)
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
            self.log_command_executions("default.input", std::slice::from_ref(output));
        }
        Ok(!outputs.is_empty())
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
        let outputs = commands
            .into_iter()
            .map(|command| {
                let result = self.pw.execute(command.clone());
                command_execution_with_spec(command, result)
            })
            .collect::<Vec<_>>();
        self.remember_failed_capture_moves(&outputs, source_outputs)?;
        Ok(outputs)
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
        outputs: &[CommandExecution],
        source_outputs: &[SourceOutputRoute],
    ) -> Result<(), EngineError> {
        let mut failures = self
            .capture_move_failures
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        let now = Instant::now();
        for output in outputs {
            let Some(source_output_id) = output.command.args.get(1) else {
                continue;
            };
            if output.error.is_some() {
                let signature = capture_move_signature_for_command(&output.command, source_outputs);
                let attempts = failures
                    .get(source_output_id)
                    .filter(|failure| failure.signature == signature)
                    .map(|failure| failure.attempts.saturating_add(1))
                    .unwrap_or(1);
                failures.insert(
                    source_output_id.clone(),
                    CaptureMoveFailure {
                        failed_at: now,
                        attempts,
                        signature,
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
    ) -> Result<bool, EngineError> {
        let stream_ids = graph
            .app_streams
            .iter()
            .filter(|stream| {
                stream.routed_channel_id.is_some()
                    && !stream_route_ready(config, graph, managed_modules, stream)
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
                let volume = configured_volume_for_stream(config, stream)?;
                ((stream.volume - volume).abs() > 0.01).then(|| (stream.id.clone(), volume))
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
        let mut outputs = Vec::new();
        for mix in &config.mixes {
            outputs.push(command_execution(
                self.pw.execute(plan_pw_set_mix_volume(mix, mix.volume)),
            ));
            outputs.push(command_execution(
                self.pw.execute(plan_pw_set_mix_mute(mix, mix.muted)),
            ));
        }

        for channel in &config.channels {
            outputs.push(command_execution(self.pw.execute(
                plan_set_managed_sink_volume(&channel.virtual_sink_name, 1.0),
            )));
            outputs.push(command_execution(self.pw.execute(
                plan_set_managed_sink_mute(&channel.virtual_sink_name, false),
            )));
            for (mix_id, bus) in &channel.mix_buses {
                if !bus.enabled {
                    continue;
                }
                outputs.extend(self.execute_channel_bus_volume_unlocked(
                    &channel.id,
                    mix_id,
                    bus.volume,
                ));
                outputs.extend(self.execute_channel_bus_mute_unlocked(
                    &channel.id,
                    mix_id,
                    bus.muted,
                ));
            }
        }
        outputs.extend(self.apply_managed_route_levels(config)?);
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
        let commands = {
            let mut runtime = self
                .runtime
                .write()
                .map_err(|_| EngineError::LockPoisoned)?;
            prune_initialized_bluetooth_cards(&mut runtime, &bluetooth_cards);
            let commands = plan_bluetooth_a2dp_profiles(
                &bluetooth_cards,
                &runtime.initialized_bluetooth_cards,
                force_all_a2dp,
            );
            for card in &bluetooth_cards {
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
        let mut cards = self.pw.bluetooth_audio_cards()?;
        if let Ok(catalog) = self.hardware_profiles() {
            let inputs = self.pw.list_inputs().unwrap_or_default();
            let outputs = self.pw.list_outputs().unwrap_or_default();

            let mut profiled_inputs = inputs.clone();
            let mut profiled_outputs = outputs.clone();
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
        let dir = self.paths.effect_chains_dir();
        fs::create_dir_all(&dir)?;

        let catalog = EffectCatalog::default();
        let mut written = Vec::new();
        let mut desired = BTreeSet::new();
        for channel in config
            .channels
            .iter()
            .filter(|channel| channel.effects.iter().any(|effect| !effect.bypassed))
        {
            let file_name = effect_chain_file_name(&channel.id, "conf");
            desired.insert(file_name.clone());
            let path = dir.join(&file_name);
            let tmp_path = dir.join(format!(".{}.{}.tmp", file_name, Uuid::new_v4().simple()));
            fs::write(&tmp_path, render_filter_chain(channel, &catalog))?;
            fs::rename(&tmp_path, &path)?;
            written.push(path);

            let file_name = effect_chain_file_name(&channel.id, "json");
            desired.insert(file_name.clone());
            let path = dir.join(&file_name);
            let tmp_path = dir.join(format!(".{}.{}.tmp", file_name, Uuid::new_v4().simple()));
            let dsp_config = dsp_channel_config(channel);
            fs::write(&tmp_path, serde_json::to_string_pretty(&dsp_config)?)?;
            fs::rename(&tmp_path, &path)?;
            written.push(path);
        }

        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with(&format!("{}-chain-", graph_prefix()))
                && (name.ends_with(".conf") || name.ends_with(".json"))
                && !desired.contains(name)
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
        let mut outputs = Vec::new();
        for channel in config
            .channels
            .iter()
            .filter(|channel| channel.effects.iter().any(|effect| !effect.bypassed))
        {
            outputs.push(self.start_effect_chain_process(channel));
        }
        Ok(outputs)
    }

    fn start_effect_chain_process(&self, channel: &Channel) -> CommandExecution {
        let path = self
            .paths
            .effect_chains_dir()
            .join(effect_chain_file_name(&channel.id, "conf"));
        let (program, args) = effect_chain_launch_command(
            channel,
            &path,
            wavelinux_dsp::AudioRuntimeMode::from_env(),
            graph_prefix() == "wavelinux5",
        );
        let command = CommandSpec::new(
            CommandDomain::Effects,
            program.clone(),
            args.clone(),
            format!("start '{}' effect chain", channel.name),
        );
        let log_path = self.effect_chain_log_path(channel);

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
            Ok(CommandOutput {
                command: command.clone(),
                stdout: String::new(),
                stderr: "effect helper is already running".into(),
                skipped: true,
            })
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

    fn effect_chain_log_path(&self, channel: &Channel) -> PathBuf {
        self.paths
            .config_dir
            .join(effect_chain_file_name(&channel.id, "log"))
    }

    fn effect_chain_diagnostics(
        &self,
        config: &MixerConfig,
        graph: &RuntimeGraph,
    ) -> Vec<Diagnostic> {
        let availability = graph
            .effect_availability
            .iter()
            .map(|effect| (effect.effect_id.as_str(), effect))
            .collect::<BTreeMap<_, _>>();
        let catalog = EffectCatalog::default();
        let mut diagnostics = Vec::new();

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

            if self.effect_chain_log_mentions_realtime_underrun(channel) {
                diagnostics.push(Diagnostic {
                    code: format!("effects.underrun.{}", channel.id),
                    severity: DiagnosticSeverity::Warning,
                    message: format!("{} FX chain is missing realtime deadlines", channel.name),
                    action: Some(
                        "Bypass duplicate/heavy noise suppression or switch to the light voice preset"
                            .into(),
                    ),
                });
            }
            if self.effect_chain_log_mentions_clipping(channel) {
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
        effect_chain_log_mentions_recent(
            &self.effect_chain_log_path(channel),
            &["underrun detected", "processing too slow"],
        )
    }

    fn effect_chain_log_mentions_clipping(&self, channel: &Channel) -> bool {
        effect_chain_log_mentions_recent(
            &self.effect_chain_log_path(channel),
            &["clipping detected"],
        )
    }

    fn config_with_unhealthy_effects_bypassed(&self, config: &MixerConfig) -> MixerConfig {
        self.config_with_unhealthy_effects_bypassed_for_runtime_prefix(config, &graph_prefix())
    }

    fn config_with_unhealthy_effects_bypassed_for_runtime_prefix(
        &self,
        config: &MixerConfig,
        runtime_prefix: &str,
    ) -> MixerConfig {
        if runtime_prefix != "wavelinux5" {
            // Stable WaveLinux keeps the existing behavior: runtime FX warnings
            // remain diagnostic-only so user-selected processing is preserved.
            return config.clone();
        }

        let mut effective = config.clone();
        for channel in &mut effective.channels {
            if !self.effect_chain_log_mentions_realtime_underrun(channel) {
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
        if runtime_prefix != "wavelinux5" {
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
        let modules = self
            .pw
            .managed_modules()?
            .into_iter()
            .filter(|module| should_unload(module))
            .collect::<Vec<_>>();
        Ok(self
            .pw
            .execute_all(plan_unload_modules(&modules))
            .into_iter()
            .map(command_execution)
            .collect())
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

    fn effect_chain_process_is_tracked(&self, channel_id: &str) -> bool {
        self.reap_effect_chain_processes();
        self.effect_chain_processes
            .lock()
            .map(|processes| processes.contains_key(channel_id))
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
        Ok(self
            .pw
            .stale_processes()?
            .into_iter()
            .filter(|process| !active_effect_pids.contains(&process.pid))
            .collect())
    }

    fn cleanup_stale_modules_for_config(
        &self,
        config: &MixerConfig,
        preserve_stale_monitor_routes: bool,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let mut outputs = self.cleanup_stale_processes()?;
        let mut seen = BTreeSet::new();
        outputs.extend(self.cleanup_modules(|module| {
            if preserve_stale_monitor_routes && module.role.as_deref() == Some("mix_monitor") {
                return false;
            }
            if module_is_stale_for_config(module, config) {
                return true;
            }

            module_dedupe_key_for_config(module, config).is_some_and(|key| !seen.insert(key))
        })?);
        Ok(outputs)
    }

    fn cleanup_stale_auto_device_modules_for_config(
        &self,
        config: &MixerConfig,
        preserve_stale_monitor_routes: bool,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let mut seen = BTreeSet::new();
        self.cleanup_modules(|module| {
            if !module_is_auto_device_route(module) {
                return false;
            }
            if preserve_stale_monitor_routes && module.role.as_deref() == Some("mix_monitor") {
                return false;
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
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let plan = plan_ensure_graph(config);
        let mut existing_graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        let monitor_commands = plan
            .commands
            .into_iter()
            .filter(command_is_mix_monitor_route)
            .collect::<Vec<_>>();
        if monitor_commands.iter().any(|command| {
            command_targets_bluetooth_sink(command)
                && !monitor_route_endpoints_available(command, &existing_graph)
        }) {
            for _ in 0..6 {
                thread::sleep(Duration::from_millis(200));
                existing_graph = self
                    .pw
                    .snapshot_for_config_with_effect_availability(None, Vec::new());
                if monitor_commands.iter().all(|command| {
                    !command_targets_bluetooth_sink(command)
                        || monitor_route_endpoints_available(command, &existing_graph)
                }) {
                    break;
                }
            }
        }
        let mut managed_modules = self.pw.managed_modules().unwrap_or_default();
        let mut source_outputs = self.pw.source_output_routes().unwrap_or_default();
        let mut sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
        if monitor_commands.iter().any(|command| {
            command_targets_bluetooth_sink(command)
                && monitor_route_endpoints_available(command, &existing_graph)
                && !repair_command_is_satisfied(
                    command,
                    &existing_graph,
                    &source_outputs,
                    &sink_inputs,
                    &managed_modules,
                )
        }) {
            self.log_engine_event(
                "hotplug.output",
                "Bluetooth monitor output is visible; waiting for A2DP transport to settle",
            );
            thread::sleep(BLUETOOTH_MONITOR_ROUTE_SETTLE);
            existing_graph = self
                .pw
                .snapshot_for_config_with_effect_availability(None, Vec::new());
            managed_modules = self.pw.managed_modules().unwrap_or_default();
            source_outputs = self.pw.source_output_routes().unwrap_or_default();
            sink_inputs = self.pw.sink_input_routes().unwrap_or_default();
        }
        let mut skipped = Vec::new();
        let commands = monitor_commands
            .into_iter()
            .filter_map(|command| {
                if !monitor_route_endpoints_available(&command, &existing_graph) {
                    skipped.push(skipped_command_with_stderr(
                        command,
                        "monitor output is not visible yet; preserving existing monitor route",
                    ));
                    return None;
                }
                (!repair_command_is_satisfied(
                    &command,
                    &existing_graph,
                    &source_outputs,
                    &sink_inputs,
                    &managed_modules,
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
        write_json(&self.paths.config_file(), &config)
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
        let (mut graph, timings) = self.pw.snapshot_for_config_with_effect_availability_timed(
            config,
            self.effect_availability()?,
        );
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

    fn host_diagnostics(&self) -> Result<Vec<Diagnostic>, EngineError> {
        let mut cache = self
            .host_diagnostics
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        if cache_expired(cache.checked_at, HOST_DIAGNOSTICS_TTL) {
            let mut diagnostics = self.pw.diagnostics();
            diagnostics.extend(pipewire_audio_health_diagnostics());
            diagnostics.extend(dsp_runtime_diagnostics());
            cache.value = diagnostics;
            cache.checked_at = Some(Instant::now());
        }
        Ok(cache.value.clone())
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
        let targets = if audio_graph_running {
            meter_targets_for_config_with_devices(config, &graph.inputs)
        } else {
            Vec::new()
        };
        let update = {
            let mut supervisor = self
                .meter_supervisor
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            if mark_requested || supervisor.requested_recently() {
                supervisor.reconcile(targets, mark_requested)
            } else {
                supervisor.snapshot_or_stop_idle()
            }
        };

        if update.started > 0 || update.stopped > 0 || !update.failed.is_empty() {
            self.log_engine_event(
                "meters.supervisor",
                format!(
                    "started={} stopped={} failed={} active={}",
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

        Ok(update.meters)
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

    fn refresh_meter_targets_after_level_change(&self) {
        let result = (|| -> Result<(), EngineError> {
            let config = self.read_config()?.clone();
            let (graph, audio_graph_running) = {
                let runtime = self.read_runtime()?;
                (
                    runtime.graph.clone(),
                    runtime.status.audio_graph_running && !self.stop.load(Ordering::SeqCst),
                )
            };
            let meters =
                self.refresh_meter_supervisor(&config, &graph, audio_graph_running, false)?;
            let mut runtime = self.write_runtime()?;
            if runtime.status.audio_graph_running {
                runtime.graph.meters = meters;
            }
            Ok(())
        })();
        if let Err(err) = result {
            self.log_engine_event(
                "meters.supervisor",
                format!("level-change meter target refresh failed: {err}"),
            );
        }
    }

    fn refresh_meter_targets_from_live_graph(&self, area: &str) {
        let result = (|| -> Result<(), EngineError> {
            let config = self.effective_config_for_audio_graph(&self.read_config()?.clone());
            let graph = self
                .pw
                .snapshot_for_config_with_effect_availability(None, Vec::new());
            let audio_graph_running =
                graph_has_wavelinux_nodes(&graph) && !self.stop.load(Ordering::SeqCst);
            let meters =
                self.refresh_meter_supervisor(&config, &graph, audio_graph_running, false)?;
            let mut runtime = self.write_runtime()?;
            if runtime.status.audio_graph_running {
                runtime.graph.meters = meters;
            }
            Ok(())
        })();
        if let Err(err) = result {
            self.log_engine_event(
                area,
                format!("meter target refresh after effect sync failed: {err}"),
            );
        }
    }

    fn stop_meter_supervisor(&self) {
        if let Ok(mut supervisor) = self.meter_supervisor.lock() {
            let stopped = supervisor.handles.len();
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
        let config = self.effective_config_for_audio_graph(&self.read_config()?.clone());
        let channels = config
            .channels
            .iter()
            .filter(|channel| channel_ids.contains(&channel.id))
            .collect::<Vec<_>>();
        if channels.is_empty() {
            return Ok(Vec::new());
        }

        let mut outputs = self.cleanup_modules(|module| {
            matches!(
                module.role.as_deref(),
                Some("channel_to_mix") | Some("channel_to_effect")
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
            for channel in channels
                .iter()
                .copied()
                .filter(|channel| channel_has_active_effects(channel))
            {
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

        for channel in channels {
            let mut route_channel = (*channel).clone();
            if channel_has_active_effects(channel) {
                let start_output = self.start_effect_chain_process(channel);
                let start_failed = start_output.error.is_some();
                outputs.push(start_output);
                if uncleared_effect_channels.contains(&channel.id)
                    || start_failed
                    || !self.wait_for_effect_nodes(channel)
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
                outputs.extend(
                    self.pw
                        .execute_all(plan_route_channel_to_effect(
                            &route_channel,
                            &config.settings,
                        ))
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

        Ok(outputs)
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
        self.schedule_effect_graph_sync_many(BTreeSet::from([channel_id]));
    }

    fn schedule_effect_graph_sync_many(self: &Arc<Self>, channel_ids: BTreeSet<String>) {
        if channel_ids.is_empty() {
            return;
        }
        let generation = match self.deferred_effect_sync.lock() {
            Ok(mut sync) => {
                sync.generation = sync.generation.saturating_add(1);
                sync.channel_ids.extend(channel_ids);
                sync.generation
            }
            Err(_) => {
                self.log_engine_event("effects.sync", "failed to schedule effect graph sync");
                return;
            }
        };
        let engine = Arc::clone(self);
        let _ = thread::Builder::new()
            .name("wavelinux-effects-sync".into())
            .spawn(move || {
                thread::sleep(EFFECT_GRAPH_SYNC_DEBOUNCE);
                if engine.stop.load(Ordering::SeqCst) {
                    return;
                }
                let channel_ids = match engine.deferred_effect_sync.lock() {
                    Ok(mut sync) => {
                        if sync.generation != generation {
                            return;
                        }
                        mem::take(&mut sync.channel_ids)
                    }
                    Err(_) => return,
                };
                if channel_ids.is_empty() {
                    return;
                }
                if let Err(err) = engine.rebuild_effect_chain_configs() {
                    engine.log_engine_event(
                        "effects.sync",
                        format!("failed to write effect chain configs: {err}"),
                    );
                    return;
                }
                if engine.audio_graph_running_cached() {
                    engine.log_engine_event(
                        "effects.sync",
                        format!(
                            "effect chain changed; syncing affected channels: {}",
                            channel_ids.iter().cloned().collect::<Vec<_>>().join(", ")
                        ),
                    );
                    match engine.try_sync_effect_channels(&channel_ids) {
                        Ok(Some(outputs)) => {
                            engine.log_command_executions("effects.sync", &outputs);
                            engine.refresh_meter_targets_from_live_graph("effects.sync");
                        }
                        Ok(None) => {
                            // Preserve the accumulated channel set. The next
                            // debounce pass will retry after the active graph
                            // mutation has had time to finish.
                            engine.log_engine_event(
                                "effects.sync",
                                "effect route sync requeued; graph mutation is still running",
                            );
                            engine.schedule_effect_graph_sync_many(channel_ids);
                        }
                        Err(err) => {
                            engine.log_engine_event("effects.sync", format!("sync failed: {err}"));
                        }
                    }
                } else {
                    engine.log_engine_event(
                        "effects.sync",
                        "effect chain changed while audio graph was stopped; repair skipped",
                    );
                }
            });
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
        && input.is_available
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
    if let Some(default_input) = default_source.and_then(|source| {
        inputs.iter().find(|input| {
            audio_endpoint_names_match(&input.id, source)
                && input_device_can_auto_select(input, bluetooth_cards)
        })
    }) {
        return Some(AutoDeviceChoice {
            device_id: default_input.id.clone(),
            priority: hardware_input_priority(default_input),
            reason: AutoDeviceReason::SystemDefault,
        });
    }
    best_hardware_input_choice(inputs, bluetooth_cards)
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
        && input.is_available
        && is_restorable_device(&input.id)
        && !looks_like_monitor_source(input)
        && !bluetooth_input_would_force_hfp(&input.id, bluetooth_cards)
        && input
            .active_routing_policy
            .as_ref()
            .is_none_or(|policy| policy.allow_auto_select_input)
}

fn startup_microphone_level_reset_commands(
    inputs: &[DeviceInfo],
    bluetooth_cards: &[BluetoothAudioCard],
) -> Vec<CommandSpec> {
    inputs
        .iter()
        .filter(|input| {
            input.is_available
                && !input.is_virtual
                && is_restorable_device(&input.id)
                && !looks_like_monitor_source(input)
                && input.bus != Some(wavelinux_model::DeviceBus::Bluetooth)
                && !input.id.trim().starts_with("bluez_input.")
                && !bluetooth_input_would_force_hfp(&input.id, bluetooth_cards)
        })
        .flat_map(|input| {
            [
                plan_set_source_volume(&input.id, safe_startup_input_volume(input)),
                plan_set_source_mute(&input.id, false),
            ]
        })
        .collect()
}

fn safe_startup_input_volume(input: &DeviceInfo) -> f32 {
    let text = format!("{} {}", input.id, input.description).to_lowercase();
    if input.bus == Some(wavelinux_model::DeviceBus::Pci)
        || text.contains("digital microphone")
        || text.contains("built-in")
        || text.contains("internal")
    {
        0.46
    } else {
        1.0
    }
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
    if let Some(priority) = input
        .active_routing_policy
        .as_ref()
        .and_then(|policy| policy.input_priority)
    {
        return priority;
    }
    let text = device_search_text(input);

    if text.contains("usb") {
        return 60;
    }
    if text.contains("bluez") || text.contains("bluetooth") {
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
        return 50;
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
    if let Some(priority) = output
        .active_routing_policy
        .as_ref()
        .and_then(|policy| policy.output_priority)
    {
        return priority;
    }
    let text = device_search_text(output);

    if text.contains("bluez") || text.contains("bluetooth") {
        return 50;
    }
    if text.contains("usb") {
        return 40;
    }
    if text.contains("headphone")
        || text.contains("headset")
        || text.contains("lineout")
        || text.contains("line-out")
        || text.contains("analog")
        || text.contains("aux")
        || text.contains("jack")
    {
        return 30;
    }
    if text.contains("speaker") {
        return 20;
    }
    if text.contains("hdmi") || text.contains("displayport") {
        return 10;
    }
    1
}

fn device_search_text(device: &DeviceInfo) -> String {
    format!("{} {} {}", device.id, device.name, device.description).to_ascii_lowercase()
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
) -> bool {
    auto_input_repair_needed(config, auto_input, managed_modules, source_outputs)
        || auto_output_repair_needed(config, auto_output, managed_modules, source_outputs)
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
    let expected_source = format!("{}.monitor", monitor_mix.virtual_sink_name);

    !managed_modules.iter().any(|module| {
        module.role.as_deref() == Some("mix_monitor")
            && module.mix_id.as_deref() == Some(monitor_mix.id.as_str())
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
        .filter(|route| capture_stream_should_move_to_locked_default_input(route, &expected_source))
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
    route: &SourceOutputRoute,
    expected_source: &str,
) -> bool {
    if route.id.trim().is_empty() || source_output_is_wavelinux_owned(route) {
        return false;
    }
    let Some(source_name) = route.source_name.as_deref() else {
        return false;
    };
    !audio_endpoint_names_match(source_name, expected_source)
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
            | "set-source-output-mute",
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
        .is_some_and(|revision| revision == EFFECT_CONFIG_REVISION)
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
    EffectEndpointReadiness {
        source_ready: inputs.iter().any(|source| {
            effect_node_matches_current_channel(source, channel, &source_name, "effect_output")
        }),
        input_ready: outputs.iter().any(|sink| {
            effect_node_matches_current_channel(sink, channel, &input_name, "effect_input")
        }),
    }
}

fn app_routing_graph_ready(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
) -> bool {
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
        if !output_names.contains(mix.virtual_sink_name.as_str())
            || !input_names.contains(mix.virtual_source_name.as_str())
        {
            return false;
        }
    }

    for mix in &config.mixes {
        let monitor_source = format!("{}.monitor", mix.virtual_sink_name);
        for output in mix.outputs() {
            if !managed_modules.iter().any(|module| {
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
            }) {
                return false;
            }
        }
    }

    config.channels.iter().all(|channel| {
        channel_route_ready(
            channel,
            &config.mixes,
            &config.settings,
            &output_names,
            managed_modules,
            effect_chain_endpoint_readiness_for_graph(graph, channel),
        )
    })
}

fn active_effect_routes_need_repair(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
) -> bool {
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
                &config.mixes,
                &config.settings,
                &output_names,
                managed_modules,
                effect_chain_endpoint_readiness_for_graph(graph, channel),
            )
        })
}

fn stream_route_ready(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
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
    channel_route_ready(
        channel,
        &config.mixes,
        &config.settings,
        &output_names,
        managed_modules,
        effect_chain_endpoint_readiness_for_graph(graph, channel),
    )
}

fn channel_route_ready(
    channel: &Channel,
    mixes: &[Mix],
    settings: &MixerSettings,
    output_names: &BTreeSet<&str>,
    managed_modules: &[ManagedModule],
    effect_readiness: EffectEndpointReadiness,
) -> bool {
    if !output_names.contains(channel.virtual_sink_name.as_str()) {
        return false;
    }
    let raw_source_name = format!("{}.monitor", channel.virtual_sink_name);
    let mut source_name = channel_mix_source_name(channel);
    if channel_has_active_effects(channel) {
        let effect_source_name = effect_chain_source_name(channel);
        let effect_input_name = effect_chain_input_name(channel);
        if effect_readiness.ready() {
            let effect_route_ready = managed_modules.iter().any(|module| {
                module.role.as_deref() == Some("channel_to_effect")
                    && module.channel_id.as_deref() == Some(channel.id.as_str())
                    && module.source_name.as_deref() == Some(raw_source_name.as_str())
                    && module.sink_name.as_deref() == Some(effect_input_name.as_str())
                    && module.route_revision.as_deref()
                        == Some(effect_route_revision(settings, channel).as_str())
            });
            if !effect_route_ready {
                return false;
            }
            source_name = effect_source_name;
        } else {
            return false;
        }
    }
    mixes
        .iter()
        .filter(|mix| {
            channel
                .mix_buses
                .get(&mix.id)
                .is_some_and(|bus| bus.enabled)
                && !channel_mix_route_uses_hardware_direct_monitoring(channel, mix, settings)
        })
        .all(|mix| {
            managed_modules.iter().any(|module| {
                module.role.as_deref() == Some("channel_to_mix")
                    && module.channel_id.as_deref() == Some(channel.id.as_str())
                    && module.mix_id.as_deref() == Some(mix.id.as_str())
                    && module.source_name.as_deref() == Some(source_name.as_str())
                    && module.sink_name.as_deref() == Some(mix.virtual_sink_name.as_str())
                    && module.route_revision.as_deref()
                        == Some(channel_mix_route_revision(settings, channel, mix).as_str())
            })
        })
}

fn is_restorable_device(device: &str) -> bool {
    !device.to_ascii_lowercase().contains("wavelinux")
}

fn effect_chain_log_mentions_recent(path: &Path, markers: &[&str]) -> bool {
    let Ok(log) = fs::read_to_string(path) else {
        return false;
    };
    let now_nanos = OffsetDateTime::now_utc().unix_timestamp_nanos();
    let window_nanos = FX_LOG_WARNING_WINDOW.as_nanos() as i128;
    let mut found_untimestamped_marker = false;

    for line in log.lines().rev() {
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
            return true;
        }
    }

    if !found_untimestamped_marker {
        return false;
    }

    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return true;
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age <= FX_LOG_WARNING_WINDOW,
        Err(_) => true,
    }
}

fn effect_chain_log_line_timestamp(line: &str) -> Option<OffsetDateTime> {
    let timestamp = line.split_whitespace().next()?;
    OffsetDateTime::parse(timestamp, &Rfc3339).ok()
}

fn realtime_fallback_effect(effect_id: &str) -> bool {
    matches!(effect_id, "deepfilternet" | "rnnoise" | "convolver")
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
                        Some(&format!("{}.monitor", mix.virtual_sink_name)),
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
                    if !channel_has_active_effects(channel) {
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
                    let expected =
                        format!("{}_fx_{}_chain", graph_prefix(), safe_node_id(&channel.id));
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

fn route_health_issues(
    config: &MixerConfig,
    graph: &RuntimeGraph,
    managed_modules: &[ManagedModule],
    source_outputs: &[SourceOutputRoute],
    sink_inputs: &[SinkInputRoute],
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
        let reason = if module_is_stale_for_config(module, config) {
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
                graph.inputs.iter().any(|source| source.name == source_name)
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
        Ok(import_stable_config_for_wavelinux5(&path).unwrap_or_default())
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
    wavelinux_dsp::DspChannelConfig::new(
        channel.id.clone(),
        channel.name.clone(),
        graph_prefix(),
        graph_property_prefix(),
        app_display_name(),
        effect_chain_input_name(channel),
        effect_chain_source_name(channel),
        channel.effects.clone(),
    )
}

fn effect_chain_launch_command(
    channel: &Channel,
    config_path: &Path,
    runtime: wavelinux_dsp::AudioRuntimeMode,
    dsp_bridge_allowed: bool,
) -> (String, Vec<String>) {
    let config = config_path.to_string_lossy().to_string();
    if dsp_bridge_allowed
        && matches!(
            runtime,
            wavelinux_dsp::AudioRuntimeMode::DspCpu
                | wavelinux_dsp::AudioRuntimeMode::DspAuto
                | wavelinux_dsp::AudioRuntimeMode::DspAccelerated
        )
    {
        if runtime == wavelinux_dsp::AudioRuntimeMode::DspCpu
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
        return (
            dsp_helper_program(),
            vec![
                "--run-filter-chain".into(),
                "--channel-id".into(),
                channel.id.clone(),
                "--config".into(),
                config,
            ],
        );
    }
    ("pipewire".into(), vec!["-c".into(), config])
}

fn dsp_helper_program() -> String {
    std::env::var(DSP_HELPER_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "wavelinux5-dsp-helper".into())
}

fn import_stable_config_for_wavelinux5(path: &Path) -> Option<MixerConfig> {
    if std::env::var("WAVELINUX_XDG_APP_NAME").ok().as_deref() != Some("WaveLinux5") {
        return None;
    }
    let dirs = ProjectDirs::from("io.github", "DuskyProjects", "WaveLinux")?;
    let stable_path = dirs.config_dir().join("config.json");
    if stable_path == path || !stable_path.exists() {
        return None;
    }
    let mut config: MixerConfig = read_json(&stable_path).ok()?;
    apply_graph_namespace(&mut config);
    let _ = write_json(path, &config);
    Some(config)
}

fn backup_invalid_config(path: &Path) {
    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let backup = path.with_file_name(format!("config.invalid.{timestamp}.json"));
    let _ = fs::rename(path, backup);
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
    fs::write(path, data)?;
    Ok(())
}

fn render_autostart_desktop_entry() -> String {
    let app_name = app_display_name();
    let icon = graph_prefix();
    let startup_wm_class = if graph_prefix() == "wavelinux5" {
        "io.github.duskyprojects.WaveLinux5"
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
    let stream_matchers = stream_matchers_for_config(config, stream);
    let matched = config
        .app_routes
        .iter()
        .filter(|route| {
            stream_matchers
                .iter()
                .any(|matcher| app_matcher_matches_matcher(&route.matcher, matcher))
                || app_matcher_matches_stream(&route.matcher, stream)
        })
        .max_by_key(|route| app_matcher_specificity(&route.matcher))?;
    config
        .channels
        .iter()
        .find(|channel| channel.id == matched.channel_id)
        .cloned()
}

fn configured_volume_for_stream(config: &MixerConfig, stream: &AppStream) -> Option<f32> {
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
        if !graph
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
        if !graph
            .outputs
            .iter()
            .any(|output| output.name == channel.virtual_sink_name)
        {
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
                if !managed_modules.iter().any(|module| {
                    managed_module_matches_route(
                        module,
                        "channel_to_effect",
                        Some(&channel.id),
                        None,
                        &raw_source_name,
                        &effect_input_name,
                        &effect_route_revision(&config.settings, channel),
                    )
                }) {
                    diagnostics.push(Diagnostic {
                        code: format!("graph.route_effect.{}", channel.id),
                        severity: DiagnosticSeverity::Warning,
                        message: format!("{} FX input route is missing", channel.name),
                        action: Some(
                            "Run Repair to reconnect the channel into its FX chain".into(),
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
        .filter(|(_, effect_id)| matches!(*effect_id, "deepfilternet" | "rnnoise" | "convolver"))
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

fn pipewire_audio_health_diagnostics() -> Vec<Diagnostic> {
    let output = host_command("journalctl")
        .args([
            "--user",
            "-u",
            "pipewire",
            "-u",
            "pipewire-pulse",
            "-u",
            "wireplumber",
            "--since",
            PIPEWIRE_HEALTH_LOG_SINCE,
            "--no-pager",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let log = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    let out_of_buffers = log.matches("out of buffers").count();
    let resyncs = log.matches("resync").count();
    if out_of_buffers == 0 && resyncs == 0 {
        return Vec::new();
    }

    vec![Diagnostic {
        code: "pipewire.audio_health.recent_buffer_resync".into(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "PipeWire recently reported audio buffering trouble ({out_of_buffers} out-of-buffer, {resyncs} resync)"
        ),
        action: Some(
            "Let WaveLinux repair stale routes first; if this continues during normal playback, reconnect the affected Bluetooth device or use a stable hardware profile"
                .into(),
        ),
    }]
}

fn dsp_runtime_diagnostics() -> Vec<Diagnostic> {
    let dsp_requested = std::env::var_os(wavelinux_dsp::AUDIO_RUNTIME_ENV).is_some()
        || std::env::var_os(wavelinux_dsp::DSP_PROVIDER_ENV).is_some()
        || graph_prefix() == "wavelinux5";
    if !dsp_requested {
        return Vec::new();
    }

    let status = effective_dsp_runtime_status(wavelinux_dsp::probe_backend_from_env());
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
            "Install the requested CUDA/OpenVINO runtime or set WAVELINUX_DSP_PROVIDER=cpu to make the CPU fallback explicit.".into()
        }),
    });

    if let Some(reason) = &status.runtime_fallback_reason {
        diagnostics.push(Diagnostic {
            code: "dsp.runtime_fallback".into(),
            severity: DiagnosticSeverity::Warning,
            message: reason.clone(),
            action: Some(
                "Use WAVELINUX_AUDIO_RUNTIME=pipewire_filter_chain for rollback, or continue with dsp_* modes while the live helper graph is under test."
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
                "Use WAVELINUX_DSP_PROVIDER=cuda|openvino|cpu to pin the test provider.".into(),
            ),
        });
    }

    diagnostics
}

fn effective_dsp_runtime_status(
    status: wavelinux_dsp::DspBackendStatus,
) -> wavelinux_dsp::DspBackendStatus {
    if matches!(
        status.runtime,
        wavelinux_dsp::AudioRuntimeMode::DspAuto | wavelinux_dsp::AudioRuntimeMode::DspAccelerated
    ) {
        return status.with_runtime_fallback(
            wavelinux_dsp::AudioRuntimeMode::PipewireFilterChain,
            DSP_LIVE_HELPER_FALLBACK_REASON,
        );
    }
    status
}

fn host_command(program: &str) -> Command {
    let mut command = Command::new(program);
    sanitize_host_command_env(&mut command);
    command
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
        == Some("wavelinux5-dsp-helper")
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

    fn assert_wavelinux5_stop_script_scope(script: &str) {
        assert!(script.contains("stop_wavelinux5_processes"));
        assert!(script.contains("cleanup_wavelinux5_audio_modules"));
        assert!(script.contains("wavelinux5-dsp-helper"));
        assert!(script.contains("WaveLinux5_[^ ]*_amd64"));
        assert!(script.contains(r"\/wavelinux5\/effects\/wavelinux5-chain-"));
        assert!(script.contains("/wavelinux5|WaveLinux5/"));
        assert!(script.contains("$2 == \"module-loopback\""));
        assert!(script.contains(
            "cleanup_wavelinux5_audio_modules\nstop_wavelinux5_processes\ncleanup_wavelinux5_audio_modules"
        ));
        assert!(!script.contains("(^|[/ ])wavelinux([ ]|$)"));
        assert!(!script.contains("WaveLinux_[^ ]*_amd64"));
        assert!(!script.contains(r"\/wavelinux\/effects\/wavelinux-chain-"));
    }

    #[test]
    fn install_script_process_matching_never_targets_stable_wavelinux() {
        assert_wavelinux5_stop_script_scope(include_str!("../../../scripts/install-local.sh"));
    }

    #[test]
    fn uninstall_script_process_matching_never_targets_stable_wavelinux() {
        assert_wavelinux5_stop_script_scope(include_str!("../../../scripts/uninstall-local.sh"));
    }

    #[test]
    fn dsp_runtime_reports_filter_chain_fallback_until_live_helper_graph_exists() {
        let inputs = wavelinux_dsp::ProviderProbeInputs {
            cuda_available: true,
            cuda_detail: "ok".into(),
            openvino_available: true,
            openvino_detail: "ok".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let status = wavelinux_dsp::select_provider(
            wavelinux_dsp::AudioRuntimeMode::DspAuto,
            wavelinux_dsp::DspProviderPreference::Auto,
            &inputs,
        );

        let effective = effective_dsp_runtime_status(status);

        assert_eq!(effective.runtime, wavelinux_dsp::AudioRuntimeMode::DspAuto);
        assert_eq!(
            effective.effective_runtime,
            wavelinux_dsp::AudioRuntimeMode::PipewireFilterChain
        );
        assert!(effective.fallback_active);
        assert_eq!(
            effective.runtime_fallback_reason.as_deref(),
            Some(DSP_LIVE_HELPER_FALLBACK_REASON)
        );
        assert!(!effective.accelerated);
    }

    #[test]
    fn dsp_cpu_runtime_uses_native_helper_without_runtime_fallback() {
        let inputs = wavelinux_dsp::ProviderProbeInputs {
            cuda_available: false,
            cuda_detail: "no cuda".into(),
            openvino_available: false,
            openvino_detail: "no openvino".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let status = wavelinux_dsp::select_provider(
            wavelinux_dsp::AudioRuntimeMode::DspCpu,
            wavelinux_dsp::DspProviderPreference::Auto,
            &inputs,
        );

        let effective = effective_dsp_runtime_status(status);

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

        assert_eq!(program, "wavelinux5-dsp-helper");
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

        assert_eq!(program, "wavelinux5-dsp-helper");
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
        channel.effects = vec![EffectInstance::new("rnnoise")];

        let (program, args) = effect_chain_launch_command(
            &channel,
            Path::new("/tmp/wavelinux5-chain-hardware_in.conf"),
            wavelinux_dsp::AudioRuntimeMode::DspCpu,
            true,
        );

        assert_eq!(program, "wavelinux5-dsp-helper");
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
        let graph = graph_for_config(&config);
        let mut modules = routing_modules_for_config(&config);

        assert!(app_routing_graph_ready(&config, &graph, &modules));

        modules.retain(|module| {
            !(module.role.as_deref() == Some("channel_to_mix")
                && module.channel_id.as_deref() == Some("music")
                && module.mix_id.as_deref() == Some("monitor"))
        });
        assert!(!app_routing_graph_ready(&config, &graph, &modules));

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
        assert!(!stream_route_ready(&config, &graph, &modules, &stream));
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

        assert!(stream_route_ready(&config, &graph, &modules, &stream));
        assert!(!engine
            .move_unready_routed_streams_to_default(&config, &graph, &modules)
            .unwrap());

        let mut stale_modules = modules.clone();
        stale_modules.retain(|module| {
            !(module.role.as_deref() == Some("channel_to_mix")
                && module.channel_id.as_deref() == Some("music")
                && module.mix_id.as_deref() == Some("stream"))
        });
        assert!(!stream_route_ready(
            &config,
            &graph,
            &stale_modules,
            &stream
        ));
        assert!(engine
            .move_unready_routed_streams_to_default(&config, &graph, &stale_modules)
            .unwrap());
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
        let graph = graph_for_config(&config);
        let modules = routing_modules_for_config(&config);

        assert!(app_routing_graph_ready(&config, &graph, &modules));

        let mut missing_fx_graph = graph.clone();
        missing_fx_graph
            .inputs
            .retain(|input| input.name != "wavelinux_fx_music_source");
        assert!(!app_routing_graph_ready(
            &config,
            &missing_fx_graph,
            &modules
        ));
        assert!(active_effect_routes_need_repair(
            &config,
            &missing_fx_graph,
            &modules
        ));

        let mut stale_fx_graph = graph.clone();
        stale_fx_graph
            .inputs
            .iter_mut()
            .find(|input| input.name == "wavelinux_fx_music_source")
            .unwrap()
            .pipewire_properties
            .insert(graph_prop("effect_config_revision"), "stale".into());
        assert!(!app_routing_graph_ready(&config, &stale_fx_graph, &modules));
        assert!(active_effect_routes_need_repair(
            &config,
            &stale_fx_graph,
            &modules
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
            &modules
        ));
        assert!(active_effect_routes_need_repair(
            &config,
            &wrong_channel_graph,
            &modules
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
            &raw_fallback_modules
        ));
        assert!(active_effect_routes_need_repair(
            &config,
            &missing_fx_graph,
            &raw_fallback_modules
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
        let update = supervisor.reconcile(
            vec![MeterTarget {
                node_id: "stream".into(),
                source_name: "wavelinux_mix_stream.monitor".into(),
                gain: 1.0,
                muted: false,
            }],
            true,
        );

        assert!(update.meters.is_empty());
        assert!(supervisor.handles.is_empty());
    }

    #[test]
    fn meter_sample_reader_tracks_real_rms_frames() {
        let sample = Arc::new(Mutex::new(MeterSample::default()));
        let mut pending = Vec::new();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0.25_f32.to_le_bytes());
        bytes.extend_from_slice(&(-0.5_f32).to_le_bytes());
        bytes.extend_from_slice(&0.1_f32.to_le_bytes());
        bytes.extend_from_slice(&0.2_f32.to_le_bytes());

        consume_meter_bytes(&bytes[..5], &mut pending, &sample);
        assert_eq!(sample.lock().unwrap().frames, 0);
        consume_meter_bytes(&bytes[5..], &mut pending, &sample);

        let sample = *sample.lock().unwrap();
        assert_eq!(sample.frames, 2);
        assert!(sample.updated_at.is_some());
        let expected_left = ((0.25_f32.powi(2) + 0.1_f32.powi(2)) / 2.0).sqrt();
        let expected_right = ((0.5_f32.powi(2) + 0.2_f32.powi(2)) / 2.0).sqrt();
        assert!((sample.peak_left - expected_left).abs() < 0.000_001);
        assert!((sample.peak_right - expected_right).abs() < 0.000_001);
    }

    #[test]
    fn meter_sample_tracks_current_rms_without_backend_peak_hold() {
        let sample = Arc::new(Mutex::new(MeterSample::default()));
        let mut pending = Vec::new();
        let mut hit = Vec::new();
        hit.extend_from_slice(&0.5_f32.to_le_bytes());
        hit.extend_from_slice(&(-0.75_f32).to_le_bytes());
        consume_meter_bytes(&hit, &mut pending, &sample);

        let hit_sample = *sample.lock().unwrap();
        assert!((hit_sample.peak_left - 0.5).abs() < f32::EPSILON);
        assert!((hit_sample.peak_right - 0.75).abs() < f32::EPSILON);

        let mut silence = Vec::new();
        silence.extend_from_slice(&0.0_f32.to_le_bytes());
        silence.extend_from_slice(&0.0_f32.to_le_bytes());
        consume_meter_bytes(&silence, &mut pending, &sample);
        let silent_sample = *sample.lock().unwrap();
        assert_eq!(silent_sample.peak_left, 0.0);
        assert_eq!(silent_sample.peak_right, 0.0);
    }

    #[test]
    fn meter_sample_ignores_floor_noise() {
        let sample = Arc::new(Mutex::new(MeterSample::default()));
        let mut pending = Vec::new();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(METER_NOISE_FLOOR * 0.5).to_le_bytes());
        bytes.extend_from_slice(&(-METER_NOISE_FLOOR * 0.5).to_le_bytes());
        consume_meter_bytes(&bytes, &mut pending, &sample);

        let sample = *sample.lock().unwrap();
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
    fn default_locks_choose_system_and_hardware_input_nodes() {
        let mut config = MixerConfig::default();
        assert_eq!(
            default_output_channel(&config).map(|channel| channel.virtual_sink_name.as_str()),
            Some("wavelinux_channel_system")
        );
        assert_eq!(
            default_input_source(&config).as_deref(),
            Some("wavelinux_channel_hardware_in.monitor")
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
    fn default_input_uses_raw_monitor_when_fx_nodes_are_missing() {
        let mut config = MixerConfig::default();
        config.settings.lock_default_input = true;
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("limiter")])
            .unwrap();

        let raw_source = "wavelinux_channel_hardware_in.monitor";
        let graph = RuntimeGraph {
            inputs: vec![device(raw_source, "Monitor of wavelinux-input", false)],
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
            Some(raw_source)
        );
        assert!(!default_input_lock_repair_needed(
            &effective,
            Some(raw_source)
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
        };
        let commands = capture_stream_move_commands_to_locked_default_input(
            &effective,
            std::slice::from_ref(&route),
        );
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].args, ["move-source-output", "99", raw_source]);
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
            Some("wavelinux_channel_hardware_in.monitor")
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
        let route_repair = auto_device_route_repair_needed(&config, None, None, &[], &[]);
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
            [
                "move-source-output",
                "99",
                "wavelinux_channel_hardware_in.monitor"
            ]
        );

        let already_routed = SourceOutputRoute {
            source_name: Some("wavelinux_channel_hardware_in.monitor".into()),
            ..route.clone()
        };
        assert!(
            capture_stream_move_commands_to_locked_default_input(&config, &[already_routed])
                .is_empty()
        );

        let old_stream_default = SourceOutputRoute {
            source_name: Some("wavelinux_mix_stream_source".into()),
            target_object: Some("wavelinux_mix_stream_source".into()),
            ..route.clone()
        };
        let commands =
            capture_stream_move_commands_to_locked_default_input(&config, &[old_stream_default]);
        assert_eq!(commands.len(), 1);
        assert_eq!(
            commands[0].args,
            [
                "move-source-output",
                "99",
                "wavelinux_channel_hardware_in.monitor"
            ]
        );

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
            ..route
        };
        assert!(
            capture_stream_move_commands_to_locked_default_input(&config, &[loopback_route])
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
            &[]
        ));
        assert!(!auto_output_repair_needed(
            &config,
            Some("bluez_output.sony"),
            std::slice::from_ref(&current_route),
            std::slice::from_ref(&live_current_route),
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
        };

        assert!(auto_output_repair_needed(
            &config,
            Some("bluez_output.sony"),
            std::slice::from_ref(&current_route),
            std::slice::from_ref(&unrelated_live_route),
        ));
        assert!(!auto_output_repair_needed(
            &config,
            Some("bluez_output.sony"),
            std::slice::from_ref(&current_route),
            std::slice::from_ref(&live_route),
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
            }
        ));
    }

    #[test]
    fn auto_output_prefers_bluetooth_then_usb_then_jack_then_speaker() {
        let outputs = vec![
            device("alsa_output.speaker", "Built-in Speakers", false),
            device("alsa_output.pci_headphones", "Headphones", false),
            device("alsa_output.usb_dac", "USB Audio DAC", false),
            device("bluez_output.sony", "WH-1000XM4 Bluetooth", false),
        ];

        assert_eq!(
            best_monitor_output(&outputs).as_deref(),
            Some("bluez_output.sony")
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
            Some("bluez_output.sony")
        );
        assert_eq!(
            preferred_monitor_output(&outputs, Some("wavelinux_channel_system"), None).as_deref(),
            Some("bluez_output.sony")
        );
        assert_eq!(
            preferred_monitor_output(
                &outputs,
                Some("alsa_output.speaker"),
                Some("bluez_output.sony")
            )
            .as_deref(),
            Some("bluez_output.sony")
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
        assert!(auto_device_route_repair_needed_for_profiled_devices(
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
    fn auto_input_prefers_system_default_microphone_when_safe() {
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
            Some("alsa_input.pci_mic")
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
        assert_eq!(input.priority, Some(60));
        assert_eq!(input.reason, AutoDeviceReason::Priority);
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
        };
        let failed_move = CommandExecution {
            command: plan_move_capture_stream_to_source("77", "wavelinux_mix_stream_source"),
            stdout: String::new(),
            stderr: String::new(),
            skipped: false,
            error: Some("Failure: Invalid argument".into()),
        };

        engine
            .remember_failed_capture_moves(&[failed_move], std::slice::from_ref(&route))
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
    fn startup_microphone_level_reset_targets_real_non_bluetooth_sources() {
        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("headset-head-unit".into()),
            preferred_a2dp_profile: Some("a2dp-sink".into()),
            profiles: Vec::new(),
        }];
        let mut usb = device("alsa_input.usb_mic", "USB Microphone", true);
        usb.bus = Some(wavelinux_model::DeviceBus::Usb);
        let mut monitor = device("alsa_output.pci.monitor", "Monitor of Speakers", false);
        monitor.bus = Some(wavelinux_model::DeviceBus::Pci);
        let mut virtual_source = device("wavelinux_mix_stream_source", "WaveLinux Stream", false);
        virtual_source.is_virtual = true;
        let mut bluetooth = device(
            "bluez_input.AC:80:0A:72:BD:10",
            "WH-1000XM4 Bluetooth Headset Microphone",
            false,
        );
        bluetooth.bus = Some(wavelinux_model::DeviceBus::Bluetooth);

        let commands = startup_microphone_level_reset_commands(
            &[usb, monitor, virtual_source, bluetooth],
            &cards,
        );

        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0].args,
            ["set-source-volume", "alsa_input.usb_mic", "100%"]
        );
        assert_eq!(
            commands[1].args,
            ["set-source-mute", "alsa_input.usb_mic", "0"]
        );
    }

    #[test]
    fn startup_microphone_level_reset_uses_safe_internal_mic_gain() {
        let mut internal = device(
            "alsa_input.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Mic1__source",
            "700 Series Chipset Family HD Audio Controller Digital Microphone",
            true,
        );
        internal.bus = Some(wavelinux_model::DeviceBus::Pci);

        let commands = startup_microphone_level_reset_commands(&[internal], &[]);

        assert_eq!(commands.len(), 2);
        assert_eq!(
            commands[0].args,
            [
                "set-source-volume",
                "alsa_input.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Mic1__source",
                "46%"
            ]
        );
        assert_eq!(
            commands[1].args,
            [
                "set-source-mute",
                "alsa_input.pci-0000_00_1f.3-platform-skl_hda_dsp_generic.HiFi__Mic1__source",
                "0"
            ]
        );
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

        engine
            .bypass_effect("hardware_in".into(), limiter.instance_id, true)
            .unwrap();
        engine.rebuild_effect_chain_configs().unwrap();
        assert!(!path.exists());
        assert!(!dsp_path.exists());
    }

    #[test]
    fn wavelinux5_effect_chain_configs_bypass_recent_underrun_heavy_effects() {
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
        assert_eq!(bypassed.get("deepfilternet"), Some(&true));
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
            .set_effect_chain("hardware_in", vec![EffectInstance::new("deepfilternet")])
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
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "effects.clipping.hardware_in"));
    }

    #[test]
    fn old_timestamped_fx_chain_log_warnings_are_ignored() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("deepfilternet")])
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
                "{old_timestamp} | WARN | deep_filter_ladspa | Underrun detected (RTF: 2.00). Processing too slow!\n"
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
            .set_effect_chain("hardware_in", vec![EffectInstance::new("deepfilternet")])
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
    fn wavelinux5_realtime_underrun_schedules_effect_chain_sync() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain(
                "hardware_in",
                vec![
                    EffectInstance::new("highpass"),
                    EffectInstance::new("deepfilternet"),
                    EffectInstance::new("gate"),
                ],
            )
            .unwrap();
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        let timestamp = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
        fs::write(
            engine.effect_chain_log_path(channel),
            format!(
                "{timestamp} | WARN | deep_filter_ladspa | Underrun detected (RTF: 1.57). Processing too slow!\n"
            ),
        )
        .unwrap();

        let stable_ids =
            engine.realtime_fallback_sync_channel_ids_for_runtime_prefix(&config, "wavelinux");
        let wavelinux5_ids =
            engine.realtime_fallback_sync_channel_ids_for_runtime_prefix(&config, "wavelinux5");
        let effective =
            engine.config_with_unhealthy_effects_bypassed_for_runtime_prefix(&config, "wavelinux5");
        let bypassed = effective
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap()
            .effects
            .iter()
            .map(|effect| (effect.effect_id.as_str(), effect.bypassed))
            .collect::<BTreeMap<_, _>>();

        assert!(stable_ids.is_empty());
        assert_eq!(wavelinux5_ids, BTreeSet::from(["hardware_in".to_string()]));
        assert_eq!(bypassed.get("deepfilternet"), Some(&true));
        assert_eq!(bypassed.get("highpass"), Some(&false));
        assert_eq!(bypassed.get("gate"), Some(&false));
    }

    #[test]
    fn realtime_fallback_bypasses_only_heavy_effects() {
        let mut channel = MixerConfig::default()
            .channels
            .into_iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        channel.effects = vec![
            EffectInstance::new("highpass"),
            EffectInstance::new("eq"),
            EffectInstance::new("deepfilternet"),
            EffectInstance::new("compressor"),
            EffectInstance::new("rnnoise"),
            EffectInstance::new("gate"),
            EffectInstance::new("limiter"),
        ];

        assert!(bypass_realtime_fallback_effects(&mut channel));

        let bypassed = channel
            .effects
            .iter()
            .map(|effect| (effect.effect_id.as_str(), effect.bypassed))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(bypassed.get("deepfilternet"), Some(&true));
        assert_eq!(bypassed.get("rnnoise"), Some(&true));
        assert_eq!(bypassed.get("highpass"), Some(&false));
        assert_eq!(bypassed.get("eq"), Some(&false));
        assert_eq!(bypassed.get("compressor"), Some(&false));
        assert_eq!(bypassed.get("gate"), Some(&false));
        assert_eq!(bypassed.get("limiter"), Some(&false));
    }

    #[test]
    fn quiet_fx_chain_runtime_keeps_processed_input() {
        let engine = test_engine();
        let mut config = MixerConfig::default();
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("deepfilternet")])
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
            .set_effect_chain("hardware_in", vec![EffectInstance::new("deepfilternet")])
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
    fn repair_starts_base_graph_before_fx_and_routes() {
        let engine = test_engine();
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
        let route_index = report
            .outputs
            .iter()
            .position(|output| output.command.description == "route 'Input' to 'Monitor'")
            .unwrap();

        assert!(base_graph_index < fx_index);
        assert!(fx_index < route_index);
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
        assert!(route_stream_to_configured_channel(&config, &discord_stream).is_none());
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
        let required = ["deepfilternet", "compressor", "limiter"];
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
                            ("high_freq_hz", 8000.0),
                            ("high_gain_db", 1.5),
                            ("low_freq_hz", 120.0),
                            ("low_gain_db", -2.0),
                            ("mid_freq_hz", 2500.0),
                            ("mid_gain_db", 2.0),
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
                    test_effect("deepfilternet", &[("attenuation_limit_db", 100.0)]),
                    test_effect("deepfilternet", &[("attenuation_limit_db", 12.0)]),
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
