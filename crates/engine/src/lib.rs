use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use directories::{BaseDirs, ProjectDirs};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;
use wavelinux_model::{
    setup_templates, AppMatcher, AppRoute, AppStateSnapshot, AppStream, AppVolumePreset, Channel,
    ChannelInputMode, ChannelKind, ConfigBackup, Diagnostic, DiagnosticSeverity,
    EffectAvailability, EffectCatalog, EffectInstance, EngineStatus, KnownApp, LevelMeter, Mix,
    MixerConfig, MixerSettings, ModelError, RuntimeGraph, Scene, SetupTemplate,
};
use wavelinux_pw::{
    channel_has_active_effects, channel_mix_source_name, effect_chain_source_name,
    meter_sampling_enabled, meter_targets_for_config, plan_ensure_graph, plan_kill_stale_processes,
    plan_move_app_stream, plan_move_app_stream_to_default, plan_set_channel_bus_mute,
    plan_set_channel_bus_source_output_mute, plan_set_channel_bus_source_output_volume,
    plan_set_channel_bus_volume, plan_set_default_sink, plan_set_default_source,
    plan_set_mix_mute as plan_pw_set_mix_mute, plan_set_mix_volume as plan_pw_set_mix_volume,
    plan_set_stream_mute, plan_set_stream_volume, plan_unload_modules, probe_effect_availability,
    render_filter_chain, CommandDomain, CommandOutput, CommandSpec, ManagedModule, MeterTarget,
    PlannedGraph, PwClient, PwError, SinkInputRoute, SourceOutputRoute, StaleProcess,
};

const DEBUG_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
const HOST_DIAGNOSTICS_TTL: Duration = Duration::from_secs(30);
const EFFECT_AVAILABILITY_TTL: Duration = Duration::from_secs(30);
const METER_RESTART_BACKOFF: Duration = Duration::from_secs(5);

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
    #[error("scene not found: {0}")]
    SceneNotFound(String),
    #[error("lock poisoned")]
    LockPoisoned,
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
        let dirs = ProjectDirs::from("io.github", "DuskyProjects", "WaveLinux")
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

    fn scenes_dir(&self) -> PathBuf {
        self.data_dir.join("scenes")
    }

    fn effect_chains_dir(&self) -> PathBuf {
        self.data_dir.join("effects")
    }

    fn autostart_file(&self) -> PathBuf {
        self.autostart_dir.join("wavelinux.desktop")
    }

    fn log_file(&self) -> PathBuf {
        self.config_dir.join("wavelinux-engine.log")
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
            poll_interval: Duration::from_millis(1_000),
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
pub struct CommandExecution {
    pub command: CommandSpec,
    pub stdout: String,
    pub stderr: String,
    pub skipped: bool,
    pub error: Option<String>,
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
    pub stale_processes: Vec<StaleProcess>,
    pub graph: RuntimeGraph,
    pub diagnostics: Vec<Diagnostic>,
    pub debug_log_path: PathBuf,
    pub recent_log_lines: Vec<String>,
}

#[derive(Debug)]
struct RuntimeCache {
    graph: RuntimeGraph,
    diagnostics: Vec<Diagnostic>,
    status: EngineStatus,
}

impl RuntimeCache {
    fn new(dry_run: bool) -> Self {
        Self {
            graph: RuntimeGraph::default(),
            diagnostics: Vec::new(),
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

#[derive(Debug, Default)]
struct TimedCache<T> {
    checked_at: Option<Instant>,
    value: T,
}

#[derive(Debug)]
struct MeterSupervisor {
    dry_run: bool,
    handles: BTreeMap<String, MeterProcess>,
    last_attempts: BTreeMap<String, Instant>,
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
    node_id: String,
    source_name: String,
    sample: Arc<Mutex<MeterSample>>,
    stop: Arc<AtomicBool>,
    child: Child,
    reader: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, Copy, Default)]
struct MeterSample {
    peak_left: f32,
    peak_right: f32,
    frames: u64,
}

impl MeterSupervisor {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            handles: BTreeMap::new(),
            last_attempts: BTreeMap::new(),
        }
    }

    fn reconcile(&mut self, targets: Vec<MeterTarget>) -> MeterSupervisorUpdate {
        let mut update = MeterSupervisorUpdate::default();
        if self.dry_run || !meter_sampling_enabled() {
            update.stopped += self.handles.len();
            self.stop_all();
            return update;
        }

        let targets = targets
            .into_iter()
            .map(|target| (target.node_id.clone(), target))
            .collect::<BTreeMap<_, _>>();
        let now = Instant::now();
        let mut stopped = Vec::new();
        for (node_id, handle) in &mut self.handles {
            let exited = handle.has_exited();
            if !targets.contains_key(node_id)
                || targets
                    .get(node_id)
                    .is_some_and(|target| target.source_name != handle.source_name)
                || exited
            {
                stopped.push((node_id.clone(), exited));
            }
        }
        update.stopped += stopped.len();
        for (node_id, exited) in stopped {
            self.handles.remove(&node_id);
            if exited {
                self.last_attempts.insert(node_id, now);
            } else {
                self.last_attempts.remove(&node_id);
            }
        }

        self.last_attempts
            .retain(|node_id, _| targets.contains_key(node_id));
        for target in targets.values() {
            if self.handles.contains_key(&target.node_id) {
                continue;
            }
            if self
                .last_attempts
                .get(&target.node_id)
                .is_some_and(|attempt| now.duration_since(*attempt) < METER_RESTART_BACKOFF)
            {
                continue;
            }
            self.last_attempts.insert(target.node_id.clone(), now);
            match MeterProcess::spawn(target) {
                Ok(handle) => {
                    self.last_attempts.remove(&target.node_id);
                    self.handles.insert(target.node_id.clone(), handle);
                    update.started += 1;
                }
                Err(err) => update.failed.push(format!(
                    "{} from {}: {err}",
                    target.node_id, target.source_name
                )),
            }
        }

        update.meters = self.snapshot();
        update
    }

    fn snapshot(&self) -> Vec<LevelMeter> {
        self.handles
            .values()
            .map(|handle| handle.level_meter())
            .collect()
    }

    fn stop_all(&mut self) {
        self.handles.clear();
        self.last_attempts.clear();
    }
}

impl MeterProcess {
    fn spawn(target: &MeterTarget) -> Result<Self, std::io::Error> {
        let mut child = Command::new("pw-record")
            .args([
                "--target",
                target.source_name.as_str(),
                "--rate",
                "48000",
                "--channels",
                "2",
                "--format",
                "f32",
                "--raw",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("pw-record stdout was not piped"))?;
        let sample = Arc::new(Mutex::new(MeterSample::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let reader_sample = Arc::clone(&sample);
        let reader_stop = Arc::clone(&stop);
        let thread_name = format!("wavelinux-meter-{}", safe_file_id(&target.node_id));
        let reader = thread::Builder::new()
            .name(thread_name)
            .spawn(move || read_meter_stream(stdout, reader_sample, reader_stop))
            .map_err(|err| {
                let _ = child.kill();
                let _ = child.wait();
                std::io::Error::other(err)
            })?;

        Ok(Self {
            node_id: target.node_id.clone(),
            source_name: target.source_name.clone(),
            sample,
            stop,
            child,
            reader: Some(reader),
        })
    }

    fn has_exited(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_some()
    }

    fn level_meter(&self) -> LevelMeter {
        let sample = self.sample.lock().map(|sample| *sample).unwrap_or_default();
        LevelMeter {
            node_id: self.node_id.clone(),
            peak_left: sample.peak_left.clamp(0.0, 1.0),
            peak_right: sample.peak_right.clamp(0.0, 1.0),
        }
    }
}

impl Drop for MeterProcess {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_meter_stream(
    mut stdout: ChildStdout,
    sample: Arc<Mutex<MeterSample>>,
    stop: Arc<AtomicBool>,
) {
    let mut buffer = [0_u8; 8192];
    let mut pending = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        match stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => consume_meter_bytes(&buffer[..read], &mut pending, &sample),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn consume_meter_bytes(bytes: &[u8], pending: &mut Vec<u8>, sample: &Arc<Mutex<MeterSample>>) {
    pending.extend_from_slice(bytes);
    let frame_bytes = (pending.len() / 8) * 8;
    if frame_bytes == 0 {
        return;
    }

    let mut peak_left = 0.0_f32;
    let mut peak_right = 0.0_f32;
    let mut frames = 0_u64;
    for frame in pending[..frame_bytes].chunks_exact(8) {
        let left = f32::from_le_bytes(frame[0..4].try_into().unwrap_or_default());
        let right = f32::from_le_bytes(frame[4..8].try_into().unwrap_or_default());
        if left.is_finite() {
            peak_left = peak_left.max(left.abs());
        }
        if right.is_finite() {
            peak_right = peak_right.max(right.abs());
        }
        frames += 1;
    }
    pending.drain(..frame_bytes);

    if let Ok(mut sample) = sample.lock() {
        sample.peak_left = peak_left.max(sample.peak_left * 0.9).clamp(0.0, 1.0);
        sample.peak_right = peak_right.max(sample.peak_right * 0.9).clamp(0.0, 1.0);
        sample.frames = sample.frames.saturating_add(frames);
    }
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
    runtime_refresh: Mutex<()>,
    host_diagnostics: Mutex<TimedCache<Vec<Diagnostic>>>,
    effect_availability: Mutex<TimedCache<Vec<EffectAvailability>>>,
    audio_commands: Mutex<()>,
    stop: AtomicBool,
}

impl WaveLinuxEngine {
    pub fn from_xdg() -> Result<Arc<Self>, EngineError> {
        Self::new(EnginePaths::from_xdg()?, EngineOptions::default())
    }

    pub fn new(paths: EnginePaths, options: EngineOptions) -> Result<Arc<Self>, EngineError> {
        fs::create_dir_all(&paths.config_dir)?;
        fs::create_dir_all(paths.scenes_dir())?;
        let config = load_config(&paths)?.normalized()?;
        let pw = PwClient::new(options.dry_run);
        let startup_defaults = DefaultDevices::capture(&pw);
        let engine = Arc::new(Self {
            pw,
            startup_defaults,
            runtime: RwLock::new(RuntimeCache::new(options.dry_run)),
            config: RwLock::new(config),
            meter_supervisor: Mutex::new(MeterSupervisor::new(options.dry_run)),
            runtime_refresh: Mutex::new(()),
            host_diagnostics: Mutex::new(TimedCache::default()),
            effect_availability: Mutex::new(TimedCache::default()),
            audio_commands: Mutex::new(()),
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
                    if meter_sampling_enabled() { "pw-record" } else { "disabled" },
                ),
            );
        }
        if engine.options.auto_repair_on_start
            && engine
                .read_config()
                .map(|config| config.settings.restore_audio_graph_on_launch)
                .unwrap_or(false)
        {
            let _ = engine.repair_audio_graph();
        }
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

    pub fn get_state(&self) -> Result<AppStateSnapshot, EngineError> {
        let _ = self.refresh_runtime();
        self.cached_state()
    }

    pub fn observe_state(&self) -> Result<AppStateSnapshot, EngineError> {
        self.cached_state()
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

    fn refresh_runtime_unlocked(&self) -> Result<(), EngineError> {
        let started = Instant::now();
        let config = self.read_config()?.clone();
        let mut graph = self.snapshot_for_config(Some(&config))?;
        let mut audio_graph_running = graph_has_wavelinux_nodes(&graph);
        if audio_graph_running {
            let routed_streams = self.route_configured_streams(&config, &graph.app_streams)?;
            let updated_volumes =
                self.apply_configured_stream_volumes(&config, &graph.app_streams)?;
            if routed_streams || updated_volumes {
                graph = self.snapshot_for_config(Some(&config))?;
                audio_graph_running = graph_has_wavelinux_nodes(&graph);
            }
        }
        graph.meters = self.refresh_meter_supervisor(&config, &graph, audio_graph_running)?;
        self.remember_observed_apps(&graph.app_streams)?;
        let diagnostics = self.host_diagnostics()?;
        let healthy = diagnostics
            .iter()
            .all(|item| item.severity != DiagnosticSeverity::Error);
        let mut runtime = self.write_runtime()?;
        runtime.graph = graph;
        runtime.diagnostics = diagnostics;
        runtime.status.healthy = healthy;
        runtime.status.audio_graph_running = audio_graph_running;
        runtime.status.last_refresh_unix = OffsetDateTime::now_utc().unix_timestamp();
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
        let elapsed = started.elapsed();
        if elapsed > Duration::from_millis(300) {
            self.log_engine_event(
                "runtime.refresh",
                format!(
                    "slow_refresh_ms={} inputs={} outputs={} streams={} meters={} graph_running={}",
                    elapsed.as_millis(),
                    runtime.graph.inputs.len(),
                    runtime.graph.outputs.len(),
                    runtime.graph.app_streams.len(),
                    runtime.graph.meters.len(),
                    runtime.status.audio_graph_running,
                ),
            );
        }
        Ok(())
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
        let config = self.effective_config_for_audio_graph(&self.read_config()?.clone());
        let mut outputs = self.cleanup_stale_modules_for_config(&config)?;
        self.log_command_executions("repair.cleanup", &outputs);
        self.rebuild_effect_chain_configs()?;

        let mut planned = plan_ensure_graph(&config);
        let planned_count = planned.commands.len();
        let existing_graph = self
            .pw
            .snapshot_for_config_with_effect_availability(None, Vec::new());
        let managed_modules = self.pw.managed_modules().unwrap_or_default();
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        planned.commands.retain(|command| {
            !repair_command_is_satisfied(
                command,
                &existing_graph,
                &source_outputs,
                &managed_modules,
            )
        });

        let (graph_commands, route_commands) = split_repair_commands(&planned.commands);
        self.log_engine_event(
            "repair.plan",
            format!(
                "planned={} retained={} graph_commands={} route_commands={} managed_modules={} source_outputs={} inputs={} outputs={}",
                planned_count,
                planned.commands.len(),
                graph_commands.len(),
                route_commands.len(),
                managed_modules.len(),
                source_outputs.len(),
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
        if config.channels.iter().any(channel_has_active_effects) {
            thread::sleep(Duration::from_millis(350));
        }

        outputs.extend(
            self.pw
                .execute_all(route_commands)
                .into_iter()
                .map(command_execution),
        );
        outputs.extend(self.apply_graph_levels(&config)?);
        outputs.extend(self.apply_default_device_locks(&config)?);
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

    pub fn run_diagnostics(&self) -> Result<SoundCheckReport, EngineError> {
        let state = self.get_state()?;
        let mut diagnostics = state.diagnostics.clone();
        diagnostics.extend(graph_diagnostics(&state.config, &state.graph));
        diagnostics.extend(self.effect_chain_diagnostics(&state.config, &state.graph));
        let missing_effects = state
            .graph
            .effect_availability
            .iter()
            .filter(|effect| !effect.available)
            .map(|effect| effect.effect_id.clone())
            .collect::<Vec<_>>();
        if !meter_sampling_enabled() {
            diagnostics.push(Diagnostic {
                code: "meters.external_sampler.disabled".into(),
                severity: DiagnosticSeverity::Info,
                message: "PipeWire VU meter supervisor is disabled".into(),
                action: Some(
                    "Install pw-record or unset WAVELINUX_DISABLE_PW_RECORD_METERS to show live fader meters".into(),
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
        graph.meters = self.refresh_meter_supervisor(&config, &graph, audio_graph_running)?;
        let mut diagnostics = self.host_diagnostics()?;
        diagnostics.extend(graph_diagnostics(&config, &graph));
        diagnostics.extend(self.effect_chain_diagnostics(&config, &graph));
        Ok(GraphDebugReport {
            dry_run: self.options.dry_run,
            audio_graph_running,
            planned,
            managed_modules: self.pw.managed_modules().unwrap_or_default(),
            sink_input_routes: self.pw.sink_input_routes().unwrap_or_default(),
            source_output_routes: self.pw.source_output_routes().unwrap_or_default(),
            stale_processes: self.pw.stale_processes().unwrap_or_default(),
            graph,
            diagnostics,
            debug_log_path: self.paths.log_file(),
            recent_log_lines: self.recent_log_lines(120),
        })
    }

    pub fn list_setup_templates(&self) -> Vec<SetupTemplate> {
        setup_templates()
    }

    pub fn apply_setup_template(&self, template_id: String) -> Result<SetupTemplate, EngineError> {
        let template = self.update_config(|config| config.apply_setup_template(&template_id))??;
        self.rebuild_effect_chain_configs()?;
        let _ = self.repair_audio_graph_if_running();
        self.log_engine_event(
            "template.apply",
            format!("template={} name={}", template.id, template.name),
        );
        Ok(template)
    }

    pub fn create_mix(&self, name: String) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.create_mix(name))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn rename_mix(&self, mix_id: String, name: String) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.rename_mix(mix_id, name))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn move_mix(&self, mix_id: String, direction: i32) -> Result<Mix, EngineError> {
        self.update_config(|config| config.move_mix(mix_id, direction))?
    }

    pub fn delete_mix(&self, mix_id: String) -> Result<Mix, EngineError> {
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
        }
        Ok(mix)
    }

    pub fn set_mix_monitor_output(
        &self,
        mix_id: String,
        output: Option<String>,
    ) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.set_mix_monitor_output(mix_id, output))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(mix)
    }

    pub fn create_channel(&self, name: String, kind: ChannelKind) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| config.create_channel(name, kind))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn rename_channel(&self, channel_id: String, name: String) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| config.rename_channel(channel_id, name))??;
        let _ = self.rebuild_effect_chain_configs();
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn move_channel(&self, channel_id: String, direction: i32) -> Result<Channel, EngineError> {
        self.update_config(|config| config.move_channel(channel_id, direction))?
    }

    pub fn delete_channel(&self, channel_id: String) -> Result<Channel, EngineError> {
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
        &self,
        channel_id: String,
        source_device: Option<String>,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.set_channel_input(channel_id, source_device))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn set_hardware_input_device(
        &self,
        channel_id: String,
        source_device: Option<String>,
    ) -> Result<Channel, EngineError> {
        self.set_channel_input(channel_id, source_device)
    }

    pub fn set_channel_input_mode(
        &self,
        channel_id: String,
        input_mode: ChannelInputMode,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.set_channel_input_mode(channel_id, input_mode))??;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn restore_device(&self, kind: String) -> Result<MixerConfig, EngineError> {
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
                            mix.monitor_output = Some(output.clone());
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

    pub fn set_settings(&self, settings: MixerSettings) -> Result<MixerSettings, EngineError> {
        self.apply_start_at_login(settings.start_at_login)?;
        let settings = self.update_config(|config| Ok(config.set_settings(settings)))??;
        if self.audio_graph_running_cached() {
            let config = self.read_config()?.clone();
            let _audio_commands = self.lock_audio_commands()?;
            let _ = self.apply_default_device_locks(&config);
        }
        Ok(settings)
    }

    pub fn keep_running_in_tray(&self) -> bool {
        self.read_config()
            .map(|config| config.settings.keep_running_in_tray)
            .unwrap_or(false)
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
                if let Some(output) = self.execute_channel_bus_volume_unlocked(
                    &channel.id,
                    linked_mix_id,
                    linked_bus.volume,
                ) {
                    outputs.push(output);
                }
            }
        } else if let Some(output) =
            self.execute_channel_bus_volume_unlocked(&channel.id, &mix_id, bus.volume)
        {
            outputs.push(output);
        }
        self.log_command_executions("level.channel", &outputs);
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
        if let Some(output) =
            self.execute_channel_bus_mute_unlocked(&channel_id, &mix_id, bus.muted)
        {
            self.log_command_executions("level.channel", &[output]);
        }
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
        let channel = self
            .read_config()?
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
        Ok(command_execution(self.pw.execute(command)))
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
        Ok(command_execution(self.pw.execute(command)))
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
        Ok(command_execution(self.pw.execute(command)))
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
        Ok(command_execution(self.pw.execute(command)))
    }

    pub fn set_effect_chain(
        &self,
        channel_id: String,
        effects: Vec<EffectInstance>,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.set_effect_chain(channel_id, effects))??;
        self.rebuild_effect_chain_configs()?;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn set_effect_param(
        &self,
        channel_id: String,
        instance_id: String,
        param_id: String,
        value: f32,
    ) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| {
            config.set_effect_param(channel_id, instance_id, param_id, value)
        })??;
        self.rebuild_effect_chain_configs()?;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn bypass_effect(
        &self,
        channel_id: String,
        instance_id: String,
        bypassed: bool,
    ) -> Result<Channel, EngineError> {
        let channel =
            self.update_config(|config| config.bypass_effect(channel_id, instance_id, bypassed))??;
        self.rebuild_effect_chain_configs()?;
        let _ = self.repair_audio_graph_if_running();
        Ok(channel)
    }

    pub fn save_scene(&self, name: String) -> Result<Scene, EngineError> {
        let config = self.read_config()?.clone();
        let mut scene = Scene::new(name, config)?;
        scene.id = format!("{}_{}", scene.id, Uuid::new_v4().simple());
        let path = self.paths.scenes_dir().join(format!("{}.json", scene.id));
        write_json(&path, &scene)?;
        Ok(scene)
    }

    pub fn import_scene(&self, scene: Scene) -> Result<Scene, EngineError> {
        let config = scene.config.normalized()?;
        let mut imported = Scene::new(scene.name, config)?;
        imported.id = format!("{}_{}", imported.id, Uuid::new_v4().simple());
        let path = self
            .paths
            .scenes_dir()
            .join(format!("{}.json", imported.id));
        write_json(&path, &imported)?;
        Ok(imported)
    }

    pub fn export_backup(&self) -> Result<ConfigBackup, EngineError> {
        let config = self.read_config()?.clone();
        let scenes = self.list_scenes()?;
        ConfigBackup::new(config, scenes).map_err(EngineError::from)
    }

    pub fn import_backup(&self, backup: ConfigBackup) -> Result<ConfigBackup, EngineError> {
        backup.validate()?;
        let config = backup.config.clone().normalized()?;
        {
            let mut current_config = self.write_config()?;
            *current_config = config;
        }
        self.persist_config()?;

        fs::create_dir_all(self.paths.scenes_dir())?;
        for entry in fs::read_dir(self.paths.scenes_dir())? {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
                fs::remove_file(entry.path())?;
            }
        }
        for scene in &backup.scenes {
            let mut scene = scene.clone();
            scene.config = scene.config.normalized()?;
            write_json(
                &self.paths.scenes_dir().join(format!("{}.json", scene.id)),
                &scene,
            )?;
        }

        self.rebuild_effect_chain_configs()?;
        let _ = self.repair_audio_graph_if_running();
        self.export_backup()
    }

    pub fn load_scene(&self, scene_id: String) -> Result<Scene, EngineError> {
        let path = self.paths.scenes_dir().join(format!("{scene_id}.json"));
        if !path.exists() {
            return Err(EngineError::SceneNotFound(scene_id));
        }
        let scene: Scene = read_json(&path)?;
        let config = scene.config.clone().normalized()?;
        {
            let mut current_config = self.write_config()?;
            *current_config = config.clone();
        }
        self.persist_config()?;
        self.rebuild_effect_chain_configs()?;
        let _ = self.repair_audio_graph_if_running();
        Ok(scene)
    }

    pub fn delete_scene(&self, scene_id: String) -> Result<Scene, EngineError> {
        let path = self.paths.scenes_dir().join(format!("{scene_id}.json"));
        if !path.exists() {
            return Err(EngineError::SceneNotFound(scene_id));
        }
        let scene: Scene = read_json(&path)?;
        fs::remove_file(path)?;
        Ok(scene)
    }

    pub fn list_scenes(&self) -> Result<Vec<Scene>, EngineError> {
        let mut scenes = Vec::new();
        for entry in fs::read_dir(self.paths.scenes_dir())? {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) == Some("json") {
                if let Ok(scene) = read_json::<Scene>(&entry.path()) {
                    scenes.push(scene);
                }
            }
        }
        scenes.sort_by_key(|scene| std::cmp::Reverse(scene.created_unix));
        Ok(scenes)
    }

    pub fn cleanup_audio_graph(&self) -> Result<Vec<CommandExecution>, EngineError> {
        self.log_engine_event("cleanup.full", "requested full graph cleanup");
        self.stop_meter_supervisor();
        let outputs = {
            let _audio_commands = self.lock_audio_commands()?;
            let mut outputs = self.cleanup_stale_processes()?;
            outputs.extend(self.cleanup_modules(|_| true)?);
            outputs.extend(self.restore_startup_default_devices());
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
        let outputs = self.cleanup_stale_modules_for_config(&config)?;
        self.log_command_executions("cleanup.stale", &outputs);
        Ok(outputs)
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
                Some((stream.id.clone(), channel.clone()))
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
                    .map(|(stream_id, channel)| format!("{stream_id}->{}", channel.id))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        );
        let _audio_commands = self.lock_audio_commands()?;
        for (stream_id, channel) in routes {
            let output =
                command_execution(self.pw.execute(plan_move_app_stream(&stream_id, &channel)));
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
            let output =
                command_execution(self.pw.execute(plan_set_stream_volume(&stream_id, volume)));
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
            for (mix_id, bus) in &channel.mix_buses {
                if let Some(output) =
                    self.execute_channel_bus_volume_unlocked(&channel.id, mix_id, bus.volume)
                {
                    outputs.push(output);
                }
                if let Some(output) =
                    self.execute_channel_bus_mute_unlocked(&channel.id, mix_id, bus.muted)
                {
                    outputs.push(output);
                }
            }
        }
        Ok(outputs)
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
            if let Some(mix) = default_input_mix(config) {
                commands.push(plan_set_default_source(&mix.virtual_source_name));
            }
        }

        Ok(self
            .pw
            .execute_all(commands)
            .into_iter()
            .map(command_execution)
            .collect())
    }

    fn restore_startup_default_devices(&self) -> Vec<CommandExecution> {
        let mut commands = Vec::new();
        if let Some(sink) = self.startup_defaults.sink.as_deref() {
            commands.push(CommandSpec::new(
                CommandDomain::Route,
                "pactl",
                ["set-default-sink", sink],
                format!("restore default output to {sink}"),
            ));
        }
        if let Some(source) = self.startup_defaults.source.as_deref() {
            commands.push(CommandSpec::new(
                CommandDomain::Route,
                "pactl",
                ["set-default-source", source],
                format!("restore default input to {source}"),
            ));
        }

        self.pw
            .execute_all(commands)
            .into_iter()
            .map(command_execution)
            .collect()
    }

    fn effective_config_for_audio_graph(&self, config: &MixerConfig) -> MixerConfig {
        let mut effective = config.clone();
        let default_source = self
            .pw
            .default_source()
            .ok()
            .flatten()
            .filter(|device| is_restorable_device(device));
        let default_sink = self
            .pw
            .default_sink()
            .ok()
            .flatten()
            .filter(|device| is_restorable_device(device));

        if let Some(default_source) = default_source {
            if let Some(channel) = effective.channels.iter_mut().find(|channel| {
                channel.kind.uses_hardware_slot() && channel.source_device.is_none()
            }) {
                channel.source_device = Some(default_source);
            }
        }

        if let Some(default_sink) = default_sink {
            let follows_default_output = effective.settings.monitor_follows_default_output;
            if let Some(mix) = effective
                .mixes
                .iter_mut()
                .find(|mix| mix.id == "monitor" && follows_default_output)
            {
                if mix.monitor_output.is_none() {
                    mix.monitor_output = Some(default_sink);
                }
            }
        }

        effective
    }

    fn rebuild_effect_chain_configs(&self) -> Result<Vec<PathBuf>, EngineError> {
        let config = self.read_config()?.clone();
        let dir = self.paths.effect_chains_dir();
        fs::create_dir_all(&dir)?;

        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with("wavelinux-chain-") && name.ends_with(".conf") {
                fs::remove_file(path)?;
            }
        }

        let catalog = EffectCatalog::default();
        let mut written = Vec::new();
        for channel in config
            .channels
            .iter()
            .filter(|channel| channel.effects.iter().any(|effect| !effect.bypassed))
        {
            let path = self.paths.effect_chains_dir().join(format!(
                "wavelinux-chain-{}.conf",
                safe_file_id(&channel.id)
            ));
            fs::write(&path, render_filter_chain(channel, &catalog))?;
            written.push(path);
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
            let path = self.paths.effect_chains_dir().join(format!(
                "wavelinux-chain-{}.conf",
                safe_file_id(&channel.id)
            ));
            let command = CommandSpec::new(
                CommandDomain::Effects,
                "pipewire",
                vec!["-c".to_string(), path.to_string_lossy().to_string()],
                format!("start '{}' effect chain", channel.name),
            );
            let log_path = self
                .paths
                .config_dir
                .join(format!("wavelinux-chain-{}.log", safe_file_id(&channel.id)));

            let result = if self.options.dry_run {
                Ok(CommandOutput {
                    command: command.clone(),
                    stdout: String::new(),
                    stderr: String::new(),
                    skipped: true,
                })
            } else {
                let stdout = OpenOptions::new().create(true).append(true).open(&log_path);
                let stderr = OpenOptions::new().create(true).append(true).open(&log_path);
                match (stdout, stderr) {
                    (Ok(stdout), Ok(stderr)) => Command::new("pipewire")
                        .arg("-c")
                        .arg(&path)
                        .stdin(Stdio::null())
                        .stdout(Stdio::from(stdout))
                        .stderr(Stdio::from(stderr))
                        .spawn()
                        .map(|_| CommandOutput {
                            command: command.clone(),
                            stdout: String::new(),
                            stderr: log_path.display().to_string(),
                            skipped: false,
                        })
                        .map_err(|err| {
                            if err.kind() == std::io::ErrorKind::NotFound {
                                PwError::CommandNotFound("pipewire".into())
                            } else {
                                PwError::Io(err.to_string())
                            }
                        }),
                    (Err(err), _) | (_, Err(err)) => Err(PwError::Io(err.to_string())),
                }
            };
            outputs.push(command_execution(result));
        }
        Ok(outputs)
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
            let path = self.paths.effect_chains_dir().join(format!(
                "wavelinux-chain-{}.conf",
                safe_file_id(&channel.id)
            ));
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
                    Some("Change an effect or reload the scene to rebuild effect configs".into())
                },
            });

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

    fn execute_channel_bus_volume_unlocked(
        &self,
        channel_id: &str,
        mix_id: &str,
        volume: f32,
    ) -> Option<CommandExecution> {
        if let Ok(Some(sink_input_id)) = self.pw.find_channel_bus_sink_input(channel_id, mix_id) {
            return Some(command_execution(
                self.pw
                    .execute(plan_set_channel_bus_volume(&sink_input_id, volume)),
            ));
        }

        if let Ok(Some(source_output_id)) =
            self.pw.find_channel_bus_source_output(channel_id, mix_id)
        {
            return Some(command_execution(self.pw.execute(
                plan_set_channel_bus_source_output_volume(&source_output_id, volume),
            )));
        }

        None
    }

    fn execute_channel_bus_mute_unlocked(
        &self,
        channel_id: &str,
        mix_id: &str,
        muted: bool,
    ) -> Option<CommandExecution> {
        if let Ok(Some(sink_input_id)) = self.pw.find_channel_bus_sink_input(channel_id, mix_id) {
            return Some(command_execution(
                self.pw
                    .execute(plan_set_channel_bus_mute(&sink_input_id, muted)),
            ));
        }

        if let Ok(Some(source_output_id)) =
            self.pw.find_channel_bus_source_output(channel_id, mix_id)
        {
            return Some(command_execution(self.pw.execute(
                plan_set_channel_bus_source_output_mute(&source_output_id, muted),
            )));
        }

        None
    }

    fn cleanup_stale_processes(&self) -> Result<Vec<CommandExecution>, EngineError> {
        let processes = self.pw.stale_processes()?;
        Ok(self
            .pw
            .execute_all(plan_kill_stale_processes(&processes))
            .into_iter()
            .map(command_execution)
            .collect())
    }

    fn cleanup_stale_modules_for_config(
        &self,
        config: &MixerConfig,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let mut outputs = self.cleanup_stale_processes()?;
        let mut seen = BTreeSet::new();
        outputs.extend(self.cleanup_modules(|module| {
            if module_is_stale_for_config(module, config) {
                return true;
            }

            module_dedupe_key_for_config(module, config).is_some_and(|key| !seen.insert(key))
        })?);
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

    fn lock_audio_commands(&self) -> Result<MutexGuard<'_, ()>, EngineError> {
        self.audio_commands
            .lock()
            .map_err(|_| EngineError::LockPoisoned)
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
        Ok(self
            .pw
            .snapshot_for_config_with_effect_availability(config, self.effect_availability()?))
    }

    fn host_diagnostics(&self) -> Result<Vec<Diagnostic>, EngineError> {
        let mut cache = self
            .host_diagnostics
            .lock()
            .map_err(|_| EngineError::LockPoisoned)?;
        if cache_expired(cache.checked_at, HOST_DIAGNOSTICS_TTL) {
            cache.value = self.pw.diagnostics();
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

    fn refresh_meter_supervisor(
        &self,
        config: &MixerConfig,
        graph: &RuntimeGraph,
        audio_graph_running: bool,
    ) -> Result<Vec<LevelMeter>, EngineError> {
        let targets = if audio_graph_running {
            let available_sources = graph
                .inputs
                .iter()
                .map(|source| source.name.clone())
                .collect::<BTreeSet<_>>();
            meter_targets_for_config(config, &available_sources)
        } else {
            Vec::new()
        };
        let update = {
            let mut supervisor = self
                .meter_supervisor
                .lock()
                .map_err(|_| EngineError::LockPoisoned)?;
            supervisor.reconcile(targets)
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

    fn stop_meter_supervisor(&self) {
        if let Ok(mut supervisor) = self.meter_supervisor.lock() {
            let stopped = supervisor.handles.len();
            supervisor.stop_all();
            if stopped > 0 {
                self.log_engine_event("meters.supervisor", format!("stopped={stopped}"));
            }
        }
    }

    fn repair_audio_graph_if_running(&self) -> Result<(), EngineError> {
        if self.audio_graph_running_cached() {
            self.log_engine_event(
                "repair.auto",
                "config changed while audio graph was running; repairing graph",
            );
            let _ = self.repair_audio_graph();
        } else {
            self.log_engine_event(
                "repair.auto",
                "config changed while audio graph was stopped; repair skipped",
            );
        }
        Ok(())
    }

    fn audio_graph_running_cached(&self) -> bool {
        self.read_runtime()
            .map(|runtime| runtime.status.audio_graph_running)
            .unwrap_or(false)
    }

    fn log_engine_event(&self, area: &str, message: impl AsRef<str>) {
        let path = self.paths.log_file();
        let _ = fs::create_dir_all(&self.paths.config_dir);
        if fs::metadata(&path)
            .map(|metadata| metadata.len() > DEBUG_LOG_MAX_BYTES)
            .unwrap_or(false)
        {
            let _ = fs::rename(&path, path.with_extension("log.1"));
        }

        let timestamp = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string());
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{timestamp} [{area}] {}", message.as_ref());
        }
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
        for output in outputs
            .iter()
            .filter(|output| output.error.is_some() || !output.skipped)
            .take(24)
        {
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

fn skipped_command(command: CommandSpec) -> CommandExecution {
    CommandExecution {
        command,
        stdout: String::new(),
        stderr: String::new(),
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

fn default_input_mix(config: &MixerConfig) -> Option<&Mix> {
    config
        .mixes
        .iter()
        .find(|mix| mix.id == "stream")
        .or_else(|| config.mixes.first())
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

fn is_restorable_device(device: &str) -> bool {
    !device.to_ascii_lowercase().contains("wavelinux")
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
                    let Some(output) = mix.monitor_output.as_deref() else {
                        return true;
                    };
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
                    let Some(source) = channel.source_device.as_deref() else {
                        return true;
                    };
                    route_endpoint_mismatch(module, Some(source), Some(&channel.virtual_sink_name))
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
            !channel.mix_buses.contains_key(mix_id)
                || route_endpoint_mismatch(
                    module,
                    Some(&channel_mix_source_name(channel)),
                    Some(&mix.virtual_sink_name),
                )
        }
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
        .is_some_and(|(actual, expected)| actual != expected)
        || module
            .sink_name
            .as_deref()
            .zip(expected_sink)
            .is_some_and(|(actual, expected)| actual != expected)
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
        "channel" | "input_to_channel" => {
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

fn repair_command_is_satisfied(
    command: &CommandSpec,
    graph: &RuntimeGraph,
    source_outputs: &[wavelinux_pw::SourceOutputRoute],
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
            let role = property_value_from_arg(properties, "wavelinux.role=");
            let channel_id = property_value_from_arg(properties, "wavelinux.channel_id=");
            let mix_id = property_value_from_arg(properties, "wavelinux.mix_id=");
            let source_name = command_arg_value(&command.args, "source=");
            let sink_name = command_arg_value(&command.args, "sink=");
            if managed_modules.iter().any(|module| {
                module.role.as_deref() == role
                    && module.channel_id.as_deref() == channel_id
                    && module.mix_id.as_deref() == mix_id
                    && source_name
                        .is_none_or(|source| module.source_name.as_deref() == Some(source))
                    && sink_name.is_none_or(|sink| module.sink_name.as_deref() == Some(sink))
            }) {
                return true;
            }

            source_outputs.iter().any(|route| {
                route.role.as_deref() == role
                    && route.channel_id.as_deref() == channel_id
                    && route.mix_id.as_deref() == mix_id
                    && source_name.is_none()
                    && sink_name.is_none()
            })
        }
        _ => false,
    }
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
        Ok(MixerConfig::default())
    }
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

const LINUX_STARTUP_WM_CLASS: &str = "io.github.duskyprojects.WaveLinux";

fn render_autostart_desktop_entry() -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=WaveLinux\nComment=Linux creator audio mixer\nExec={}\nIcon=wavelinux\nTerminal=false\nCategories=Audio;AudioVideo;Mixer;\nStartupWMClass={LINUX_STARTUP_WM_CLASS}\nX-GNOME-Autostart-enabled=true\n",
        desktop_quote(&installed_binary_path()),
    )
}

fn installed_binary_path() -> PathBuf {
    if let Some(bin_home) = std::env::var_os("XDG_BIN_HOME") {
        return PathBuf::from(bin_home).join("wavelinux");
    }
    if let Some(base_dirs) = BaseDirs::new() {
        return base_dirs.home_dir().join(".local/bin/wavelinux");
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("wavelinux"))
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
        && matcher_field_matches(&pattern.media_name, candidate.media_name.as_deref())
}

fn app_matcher_matches_stream(matcher: &AppMatcher, stream: &AppStream) -> bool {
    matcher_field_matches(&matcher.app_id, stream.app_id.as_deref())
        && matcher_field_matches(&matcher.process_name, stream.process_name.as_deref())
        && matcher_field_matches(
            &matcher.binary,
            stream.binary.as_deref().or(stream.process_name.as_deref()),
        )
        && matcher_field_matches(&matcher.window_class, stream.window_class.as_deref())
        && matcher_field_matches(&matcher.media_name, stream.media_name.as_deref())
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

fn graph_diagnostics(config: &MixerConfig, graph: &RuntimeGraph) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    if !graph_has_wavelinux_nodes(graph) {
        diagnostics.push(Diagnostic {
            code: "graph.stopped".into(),
            severity: DiagnosticSeverity::Info,
            message: "WaveLinux audio graph is stopped".into(),
            action: Some("Use Start Audio when you want to create virtual devices".into()),
        });
        return diagnostics;
    }

    if !meter_sampling_enabled() {
        diagnostics.push(Diagnostic {
            code: "meters.unavailable".into(),
            severity: DiagnosticSeverity::Info,
            message: "PipeWire VU meter supervisor is unavailable".into(),
            action: Some(
                "Install pw-record or unset WAVELINUX_DISABLE_PW_RECORD_METERS to show live fader meters"
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
        if channel_has_active_effects(channel)
            && !graph
                .inputs
                .iter()
                .any(|input| input.name == effect_chain_source_name(channel))
        {
            diagnostics.push(Diagnostic {
                code: format!("graph.effect_source.{}", channel.id),
                severity: DiagnosticSeverity::Warning,
                message: format!("{} FX output is not visible yet", channel.name),
                action: Some("Run Repair to restart the channel effect chain".into()),
            });
        }
    }

    diagnostics.extend(latency_diagnostics(config));

    diagnostics
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
        let paplay_available = Command::new("paplay")
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

        let child = Command::new("paplay")
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
    fn meter_supervisor_does_not_spawn_in_dry_run() {
        let mut supervisor = MeterSupervisor::new(true);
        let update = supervisor.reconcile(vec![MeterTarget {
            node_id: "stream".into(),
            source_name: "wavelinux_mix_stream.monitor".into(),
        }]);

        assert!(update.meters.is_empty());
        assert!(supervisor.handles.is_empty());
    }

    #[test]
    fn meter_sample_reader_tracks_real_peak_frames() {
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
        assert!((sample.peak_left - 0.25).abs() < f32::EPSILON);
        assert!((sample.peak_right - 0.5).abs() < f32::EPSILON);
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
    fn scenes_round_trip() {
        let engine = test_engine();
        engine.create_mix("Podcast".into()).unwrap();
        let scene = engine.save_scene("Podcast setup".into()).unwrap();
        engine.delete_mix("podcast".into()).unwrap();
        engine.load_scene(scene.id.clone()).unwrap();
        let state = engine.get_state().unwrap();
        assert!(state.config.mixes.iter().any(|mix| mix.name == "Podcast"));
        let imported = engine.import_scene(scene.clone()).unwrap();
        assert_eq!(imported.name, scene.name);
        assert_ne!(imported.id, scene.id);
        assert!(engine
            .list_scenes()
            .unwrap()
            .iter()
            .any(|item| item.id == imported.id));
        let removed = engine.delete_scene(scene.id.clone()).unwrap();
        assert_eq!(removed.id, scene.id);
        assert!(engine.delete_scene(scene.id).is_err());
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
            node_name: Some("wavelinux_channel_game.monitor".into()),
            source_name: Some("wavelinux_channel_game.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };
        let old_untagged = ManagedModule {
            module_id: "2".into(),
            role: None,
            channel_id: None,
            mix_id: None,
            node_name: Some("wavelinux_system.monitor".into()),
            source_name: Some("wavelinux_system.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };
        let removed_channel = ManagedModule {
            module_id: "3".into(),
            role: Some("channel_to_mix".into()),
            channel_id: Some("voice_chat".into()),
            mix_id: Some("stream".into()),
            node_name: Some("wavelinux_voice_chat.monitor".into()),
            source_name: Some("wavelinux_voice_chat.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };

        assert!(!module_is_stale_for_config(&current_channel, &config));
        assert!(module_is_stale_for_config(&old_untagged, &config));
        assert!(module_is_stale_for_config(&removed_channel, &config));
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
            target_object: Some("wavelinux_channel_game".into()),
        };

        assert!(!repair_command_is_satisfied(
            &command,
            &RuntimeGraph::default(),
            &[hydrated_route],
            &[wrong_endpoint]
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
            node_name: Some("wavelinux_channel_game.monitor".into()),
            source_name: Some("wavelinux_channel_game.monitor".into()),
            sink_name: Some("wavelinux_mix_stream".into()),
        };

        assert!(repair_command_is_satisfied(
            &command,
            &RuntimeGraph::default(),
            &[],
            &[matching_endpoint]
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
    fn default_locks_choose_system_and_stream_nodes() {
        let config = MixerConfig::default();
        assert_eq!(
            default_output_channel(&config).map(|channel| channel.virtual_sink_name.as_str()),
            Some("wavelinux_channel_system")
        );
        assert_eq!(
            default_input_mix(&config).map(|mix| mix.virtual_source_name.as_str()),
            Some("wavelinux_mix_stream_source")
        );
    }

    #[test]
    fn default_device_restore_ignores_wavelinux_nodes() {
        assert!(is_restorable_device("alsa_output.speaker"));
        assert!(!is_restorable_device("wavelinux_channel_system"));
        assert!(!is_restorable_device("WAVELINUX_mix_stream_source"));
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
        let config = fs::read_to_string(&path).unwrap();
        assert!(config.contains("WaveLinux FX Hardware In"));
        assert!(config.contains("limiter-1"));

        engine
            .bypass_effect("hardware_in".into(), limiter.instance_id, true)
            .unwrap();
        assert!(!path.exists());
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
                && diagnostic.message.contains("Limiter on Hardware In")
        }));
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
            .position(|output| output.command.description == "create channel sink 'Hardware In'")
            .unwrap();
        let fx_index = report
            .outputs
            .iter()
            .position(|output| output.command.description == "start 'Hardware In' effect chain")
            .unwrap();
        let route_index = report
            .outputs
            .iter()
            .position(|output| output.command.description == "route 'Hardware In' to 'Monitor'")
            .unwrap();

        assert!(base_graph_index < fx_index);
        assert!(fx_index < route_index);
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
    fn setup_templates_are_listed_and_applied() {
        let engine = test_engine();
        assert!(engine
            .list_setup_templates()
            .iter()
            .any(|template| template.id == "streaming"));

        let template = engine.apply_setup_template("podcast".into()).unwrap();
        let state = engine.get_state().unwrap();

        assert_eq!(template.id, "podcast");
        assert!(state.config.mixes.iter().any(|mix| mix.id == "podcast"));
        assert!(state
            .config
            .channels
            .iter()
            .any(|channel| channel.id == "guest"));
        assert!(state
            .config
            .app_volume_presets
            .iter()
            .any(|preset| (preset.volume - 0.82).abs() < f32::EPSILON));
    }

    #[test]
    fn backup_export_import_replaces_config_and_scenes() {
        let engine = test_engine();
        engine.apply_setup_template("discord_mix".into()).unwrap();
        let saved = engine.save_scene("Discord setup".into()).unwrap();
        let backup = engine.export_backup().unwrap();

        assert!(backup
            .config
            .mixes
            .iter()
            .any(|mix| mix.id == "discord_mix"));
        assert!(backup.scenes.iter().any(|scene| scene.id == saved.id));

        engine.apply_setup_template("podcast".into()).unwrap();
        engine.import_backup(backup).unwrap();

        let state = engine.get_state().unwrap();
        assert!(state.config.mixes.iter().any(|mix| mix.id == "discord_mix"));
        assert!(!state.config.mixes.iter().any(|mix| mix.id == "podcast"));
        assert!(engine
            .list_scenes()
            .unwrap()
            .iter()
            .any(|scene| scene.id == saved.id));
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
    #[ignore = "mutates the live user PipeWire graph"]
    fn live_audio_graph_effect_chain_starts_routes_and_cleans_up() {
        let root = tempdir().unwrap();
        let engine = live_test_engine(root.path());
        let _cleanup = LiveGraphCleanup(engine.clone());

        engine.cleanup_audio_graph().unwrap();
        engine
            .set_effect_chain("hardware_in".into(), vec![EffectInstance::new("highpass")])
            .unwrap();

        let config_path = engine
            .paths
            .effect_chains_dir()
            .join("wavelinux-chain-hardware_in.conf");
        let effect_log_path = engine
            .paths
            .config_dir
            .join("wavelinux-chain-hardware_in.log");
        let config_text = fs::read_to_string(&config_path).unwrap();
        assert!(config_text.contains("wavelinux_fx_hardware_in_source"));
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
                && output.command.description == "start 'Hardware In' effect chain"
                && !output.skipped
        }));

        let state = refresh_until(&engine, Duration::from_secs(3), |state| {
            state
                .graph
                .inputs
                .iter()
                .any(|input| input.name == "wavelinux_fx_hardware_in_source")
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
                .any(|input| input.name == "wavelinux_fx_hardware_in_source"),
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
}
