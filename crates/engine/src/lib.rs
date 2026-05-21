use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;
use wavelinux_model::{
    AppMatcher, AppRoute, AppStateSnapshot, AppStream, Channel, ChannelKind, Diagnostic,
    DiagnosticSeverity, EffectCatalog, EffectInstance, EngineStatus, Mix, MixerConfig,
    MixerSettings, ModelError, RuntimeGraph, Scene,
};
use wavelinux_pw::{
    plan_ensure_graph, plan_move_app_stream, plan_move_app_stream_to_default,
    plan_route_mix_to_output, plan_set_channel_bus_mute, plan_set_channel_bus_volume,
    plan_set_mix_mute as plan_pw_set_mix_mute, plan_set_mix_volume as plan_pw_set_mix_volume,
    plan_set_stream_mute, plan_set_stream_volume, plan_unload_modules, CommandOutput, CommandSpec,
    ManagedModule, PlannedGraph, PwClient, PwError,
};

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
}

impl EnginePaths {
    pub fn from_xdg() -> Result<Self, EngineError> {
        let dirs = ProjectDirs::from("io.github", "DuskyProjects", "WaveLinux")
            .ok_or(EngineError::ConfigPathUnavailable)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
        })
    }

    pub fn for_tests(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
        }
    }

    fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }

    fn scenes_dir(&self) -> PathBuf {
        self.data_dir.join("scenes")
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

#[derive(Debug)]
pub struct WaveLinuxEngine {
    paths: EnginePaths,
    options: EngineOptions,
    pw: PwClient,
    config: RwLock<MixerConfig>,
    runtime: RwLock<RuntimeCache>,
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
        let engine = Arc::new(Self {
            pw: PwClient::new(options.dry_run),
            runtime: RwLock::new(RuntimeCache::new(options.dry_run)),
            config: RwLock::new(config),
            paths,
            options,
            stop: AtomicBool::new(false),
        });
        engine.persist_config()?;
        if engine.options.auto_repair_on_start {
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
        let config = self.read_config()?.clone();
        let mut graph = self.pw.snapshot_for_config(Some(&config));
        if self.route_configured_streams(&config, &graph.app_streams)? {
            graph = self.pw.snapshot_for_config(Some(&config));
        }
        let diagnostics = self.pw.diagnostics();
        let healthy = diagnostics
            .iter()
            .all(|item| item.severity != DiagnosticSeverity::Error);
        let mut runtime = self.write_runtime()?;
        runtime.graph = graph;
        runtime.diagnostics = diagnostics;
        runtime.status.healthy = healthy;
        runtime.status.last_refresh_unix = OffsetDateTime::now_utc().unix_timestamp();
        runtime.status.message = if healthy {
            if self.options.dry_run {
                "Dry-run mode".into()
            } else {
                "Ready".into()
            }
        } else {
            "Host audio dependencies are missing".into()
        };
        Ok(())
    }

    pub fn repair_audio_graph(&self) -> Result<RepairReport, EngineError> {
        let config = self.read_config()?.clone();
        let mut planned = plan_ensure_graph(&config);
        let existing_graph = self.pw.snapshot();
        let source_outputs = self.pw.source_output_routes().unwrap_or_default();
        planned.commands.retain(|command| {
            !repair_command_is_satisfied(command, &existing_graph, &source_outputs)
        });
        let outputs = self
            .pw
            .execute_all(planned.commands.clone())
            .into_iter()
            .map(command_execution)
            .collect();
        let mut outputs: Vec<_> = outputs;
        outputs.extend(self.apply_graph_levels(&config)?);
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
        let missing_effects = state
            .graph
            .effect_availability
            .iter()
            .filter(|effect| !effect.available)
            .map(|effect| effect.effect_id.clone())
            .collect();
        Ok(SoundCheckReport {
            diagnostics,
            active_stream_count: state.graph.app_streams.len(),
            virtual_mix_count: state.config.mixes.len(),
            missing_effects,
        })
    }

    pub fn create_mix(&self, name: String) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.create_mix(name))??;
        let _ = self.repair_audio_graph();
        Ok(mix)
    }

    pub fn rename_mix(&self, mix_id: String, name: String) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.rename_mix(mix_id, name))??;
        let target_mix_id = mix.id.clone();
        let _ =
            self.cleanup_modules(|module| module.mix_id.as_deref() == Some(target_mix_id.as_str()));
        let _ = self.repair_audio_graph();
        Ok(mix)
    }

    pub fn delete_mix(&self, mix_id: String) -> Result<Mix, EngineError> {
        let removed = self.update_config(|config| config.delete_mix(mix_id))??;
        let removed_id = removed.id.clone();
        let _ =
            self.cleanup_modules(|module| module.mix_id.as_deref() == Some(removed_id.as_str()));
        let _ = self.repair_audio_graph();
        Ok(removed)
    }

    pub fn set_mix_volume(&self, mix_id: String, volume: f32) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.set_mix_volume(mix_id, volume))??;
        let _ = self.pw.execute(plan_pw_set_mix_volume(&mix, mix.volume));
        Ok(mix)
    }

    pub fn set_mix_mute(&self, mix_id: String, muted: bool) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.set_mix_mute(mix_id, muted))??;
        let _ = self.pw.execute(plan_pw_set_mix_mute(&mix, mix.muted));
        Ok(mix)
    }

    pub fn set_mix_monitor_output(
        &self,
        mix_id: String,
        output: Option<String>,
    ) -> Result<Mix, EngineError> {
        let mix = self.update_config(|config| config.set_mix_monitor_output(mix_id, output))??;
        let target_mix_id = mix.id.clone();
        let _ = self.cleanup_modules(|module| {
            module.role.as_deref() == Some("mix_monitor")
                && module.mix_id.as_deref() == Some(target_mix_id.as_str())
        });
        if let Some(output) = &mix.monitor_output {
            let _ = self.pw.execute_all(plan_route_mix_to_output(&mix, output));
        }
        Ok(mix)
    }

    pub fn create_channel(&self, name: String, kind: ChannelKind) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| config.create_channel(name, kind))??;
        let _ = self.repair_audio_graph();
        Ok(channel)
    }

    pub fn rename_channel(&self, channel_id: String, name: String) -> Result<Channel, EngineError> {
        let channel = self.update_config(|config| config.rename_channel(channel_id, name))??;
        let target_channel_id = channel.id.clone();
        let _ = self.cleanup_modules(|module| {
            module.channel_id.as_deref() == Some(target_channel_id.as_str())
        });
        let _ = self.repair_audio_graph();
        Ok(channel)
    }

    pub fn delete_channel(&self, channel_id: String) -> Result<Channel, EngineError> {
        let removed = self.update_config(|config| config.delete_channel(channel_id))??;
        let removed_id = removed.id.clone();
        let _ = self
            .cleanup_modules(|module| module.channel_id.as_deref() == Some(removed_id.as_str()));
        let _ = self.repair_audio_graph();
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
        let target_channel_id = channel.id.clone();
        let _ = self.cleanup_modules(|module| {
            module.role.as_deref() == Some("input_to_channel")
                && module.channel_id.as_deref() == Some(target_channel_id.as_str())
        });
        let _ = self.repair_audio_graph();
        Ok(channel)
    }

    pub fn set_settings(&self, settings: MixerSettings) -> Result<MixerSettings, EngineError> {
        self.update_config(|config| Ok(config.set_settings(settings)))?
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

        if channel.linked {
            for (linked_mix_id, linked_bus) in &channel.mix_buses {
                if let Ok(Some(source_output_id)) = self
                    .pw
                    .find_channel_bus_source_output(&channel.id, linked_mix_id)
                {
                    let _ = self.pw.execute(plan_set_channel_bus_volume(
                        &source_output_id,
                        linked_bus.volume,
                    ));
                }
            }
        } else if let Ok(Some(source_output_id)) =
            self.pw.find_channel_bus_source_output(&channel.id, &mix_id)
        {
            let _ = self
                .pw
                .execute(plan_set_channel_bus_volume(&source_output_id, bus.volume));
        }
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
        if let Ok(Some(source_output_id)) =
            self.pw.find_channel_bus_source_output(&channel_id, &mix_id)
        {
            let _ = self
                .pw
                .execute(plan_set_channel_bus_mute(&source_output_id, bus.muted));
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
        Ok(command_execution(
            self.pw.execute(plan_move_app_stream(&stream_id, &channel)),
        ))
    }

    pub fn move_app_stream_to_default(
        &self,
        stream_id: String,
    ) -> Result<CommandExecution, EngineError> {
        Ok(command_execution(
            self.pw.execute(plan_move_app_stream_to_default(&stream_id)),
        ))
    }

    pub fn set_app_stream_volume(
        &self,
        stream_id: String,
        volume: f32,
    ) -> Result<CommandExecution, EngineError> {
        Ok(command_execution(
            self.pw.execute(plan_set_stream_volume(&stream_id, volume)),
        ))
    }

    pub fn set_app_stream_mute(
        &self,
        stream_id: String,
        muted: bool,
    ) -> Result<CommandExecution, EngineError> {
        Ok(command_execution(
            self.pw.execute(plan_set_stream_mute(&stream_id, muted)),
        ))
    }

    pub fn set_effect_chain(
        &self,
        channel_id: String,
        effects: Vec<EffectInstance>,
    ) -> Result<Channel, EngineError> {
        self.update_config(|config| config.set_effect_chain(channel_id, effects))?
    }

    pub fn set_effect_param(
        &self,
        channel_id: String,
        instance_id: String,
        param_id: String,
        value: f32,
    ) -> Result<Channel, EngineError> {
        self.update_config(|config| {
            config.set_effect_param(channel_id, instance_id, param_id, value)
        })?
    }

    pub fn bypass_effect(
        &self,
        channel_id: String,
        instance_id: String,
        bypassed: bool,
    ) -> Result<Channel, EngineError> {
        self.update_config(|config| config.bypass_effect(channel_id, instance_id, bypassed))?
    }

    pub fn save_scene(&self, name: String) -> Result<Scene, EngineError> {
        let config = self.read_config()?.clone();
        let mut scene = Scene::new(name, config)?;
        scene.id = format!("{}_{}", scene.id, Uuid::new_v4().simple());
        let path = self.paths.scenes_dir().join(format!("{}.json", scene.id));
        write_json(&path, &scene)?;
        Ok(scene)
    }

    pub fn load_scene(&self, scene_id: String) -> Result<Scene, EngineError> {
        let path = self.paths.scenes_dir().join(format!("{scene_id}.json"));
        if !path.exists() {
            return Err(EngineError::SceneNotFound(scene_id));
        }
        let scene: Scene = read_json(&path)?;
        {
            let mut config = self.write_config()?;
            *config = scene.config.clone().normalized()?;
        }
        self.persist_config()?;
        let _ = self.cleanup_audio_graph();
        let _ = self.repair_audio_graph();
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
        scenes.sort_by(|left, right| right.created_unix.cmp(&left.created_unix));
        Ok(scenes)
    }

    pub fn cleanup_audio_graph(&self) -> Result<Vec<CommandExecution>, EngineError> {
        self.cleanup_modules(|_| true)
    }

    fn route_configured_streams(
        &self,
        config: &MixerConfig,
        streams: &[AppStream],
    ) -> Result<bool, EngineError> {
        let mut moved = false;
        for stream in streams {
            let Some(channel) = route_stream_to_configured_channel(config, stream) else {
                continue;
            };
            if stream.routed_channel_id.as_deref() == Some(channel.id.as_str()) {
                continue;
            }
            let _ = self.pw.execute(plan_move_app_stream(&stream.id, &channel));
            moved = true;
        }
        Ok(moved)
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
                if let Ok(Some(source_output_id)) =
                    self.pw.find_channel_bus_source_output(&channel.id, mix_id)
                {
                    outputs
                        .push(command_execution(self.pw.execute(
                            plan_set_channel_bus_volume(&source_output_id, bus.volume),
                        )));
                    outputs
                        .push(command_execution(self.pw.execute(
                            plan_set_channel_bus_mute(&source_output_id, bus.muted),
                        )));
                }
            }
        }
        Ok(outputs)
    }

    fn cleanup_modules(
        &self,
        should_unload: impl Fn(&ManagedModule) -> bool,
    ) -> Result<Vec<CommandExecution>, EngineError> {
        let modules = self
            .pw
            .managed_modules()?
            .into_iter()
            .filter(should_unload)
            .collect::<Vec<_>>();
        Ok(self
            .pw
            .execute_all(plan_unload_modules(&modules))
            .into_iter()
            .map(command_execution)
            .collect())
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

fn repair_command_is_satisfied(
    command: &CommandSpec,
    graph: &RuntimeGraph,
    source_outputs: &[wavelinux_pw::SourceOutputRoute],
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
            source_outputs.iter().any(|route| {
                route.role.as_deref() == role
                    && route.channel_id.as_deref() == channel_id
                    && route.mix_id.as_deref() == mix_id
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

pub fn route_stream_to_configured_channel(
    config: &MixerConfig,
    stream: &AppStream,
) -> Option<Channel> {
    let matched = config.app_routes.iter().find(|route| {
        route
            .matcher
            .app_id
            .as_ref()
            .is_some_and(|value| stream.app_id.as_deref() == Some(value.as_str()))
            || route
                .matcher
                .process_name
                .as_ref()
                .is_some_and(|value| stream.process_name.as_deref() == Some(value.as_str()))
            || route
                .matcher
                .binary
                .as_ref()
                .is_some_and(|value| stream.process_name.as_deref() == Some(value.as_str()))
    })?;
    config
        .channels
        .iter()
        .find(|channel| channel.id == matched.channel_id)
        .cloned()
}

fn graph_diagnostics(config: &MixerConfig, graph: &RuntimeGraph) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

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
        if !graph.meters.iter().any(|meter| meter.node_id == mix.id) {
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
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use wavelinux_model::{percent_to_unit, AppMatcher};

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
    fn scenes_round_trip() {
        let engine = test_engine();
        engine.create_mix("Podcast".into()).unwrap();
        let scene = engine.save_scene("Podcast setup".into()).unwrap();
        engine.delete_mix("podcast".into()).unwrap();
        engine.load_scene(scene.id.clone()).unwrap();
        let state = engine.get_state().unwrap();
        assert!(state.config.mixes.iter().any(|mix| mix.name == "Podcast"));
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
    fn app_matcher_routes_to_channel() {
        let mut config = MixerConfig::default();
        config
            .assign_app_to_channel("chat", AppMatcher::from_app_id("discord"))
            .unwrap();
        let stream = AppStream {
            id: "1".into(),
            app_id: Some("discord".into()),
            process_name: Some("Discord".into()),
            display_name: "Discord".into(),
            media_name: None,
            routed_channel_id: None,
            volume: percent_to_unit(80.0),
            muted: false,
        };
        let channel = route_stream_to_configured_channel(&config, &stream).unwrap();
        assert_eq!(channel.id, "chat");
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
    fn sound_check_counts_virtual_mixes() {
        let engine = test_engine();
        let report = engine.run_diagnostics().unwrap();
        assert_eq!(report.virtual_mix_count, 2);
    }

    #[test]
    fn settings_are_persisted() {
        let engine = test_engine();
        let mut settings = engine.get_state().unwrap().config.settings;
        settings.lock_default_output = true;
        engine.set_settings(settings).unwrap();
        assert!(
            engine
                .get_state()
                .unwrap()
                .config
                .settings
                .lock_default_output
        );
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
        engine.set_channel_linked("mic".into(), true).unwrap();
        engine
            .set_channel_volume("mic".into(), "stream".into(), 0.35)
            .unwrap();
        let mic = engine
            .get_state()
            .unwrap()
            .config
            .channels
            .into_iter()
            .find(|channel| channel.id == "mic")
            .unwrap();
        assert!(mic
            .mix_buses
            .values()
            .all(|bus| (bus.volume - 0.35).abs() < f32::EPSILON));
    }

    #[test]
    fn channel_input_is_persisted() {
        let engine = test_engine();
        engine
            .set_channel_input("mic".into(), Some("alsa_input.usb_mic".into()))
            .unwrap();
        let mic = engine
            .get_state()
            .unwrap()
            .config
            .channels
            .into_iter()
            .find(|channel| channel.id == "mic")
            .unwrap();
        assert_eq!(mic.source_device.as_deref(), Some("alsa_input.usb_mic"));
    }
}
