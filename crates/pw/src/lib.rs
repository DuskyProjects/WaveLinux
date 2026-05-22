use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wavelinux_model::{
    safe_node_id, AppMatcher, AppStream, Channel, DeviceInfo, Diagnostic, DiagnosticSeverity,
    EffectAvailability, EffectCatalog, EffectInstance, Mix, MixerConfig, PluginHint, RuntimeGraph,
    SAMPLE_RATE_HZ,
};

pub const PW_RECORD_METERS_ENV: &str = "WAVELINUX_ENABLE_PW_RECORD_METERS";
pub const PW_RECORD_METERS_DISABLE_ENV: &str = "WAVELINUX_DISABLE_PW_RECORD_METERS";

#[derive(Debug, Error)]
pub enum PwError {
    #[error("command failed: {program} {args:?}: {stderr}")]
    CommandFailed {
        program: String,
        args: Vec<String>,
        stderr: String,
    },
    #[error("command not found: {0}")]
    CommandNotFound(String),
    #[error("json parse failed: {0}")]
    Json(String),
    #[error("io failed: {0}")]
    Io(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CommandDomain {
    Graph,
    Route,
    Level,
    Effects,
    Diagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandSpec {
    pub domain: CommandDomain,
    pub program: String,
    pub args: Vec<String>,
    pub description: String,
}

impl CommandSpec {
    pub fn new(
        domain: CommandDomain,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            domain,
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            description: description.into(),
        }
    }

    pub fn shell_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandOutput {
    pub command: CommandSpec,
    pub stdout: String,
    pub stderr: String,
    pub skipped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MeterTarget {
    pub node_id: String,
    pub source_name: String,
}

#[derive(Debug, Clone)]
pub struct PwClient {
    dry_run: bool,
}

impl PwClient {
    pub fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }

    pub fn dry_run(&self) -> bool {
        self.dry_run
    }

    pub fn execute(&self, spec: CommandSpec) -> Result<CommandOutput, PwError> {
        if self.dry_run {
            return Ok(CommandOutput {
                command: spec,
                stdout: String::new(),
                stderr: String::new(),
                skipped: true,
            });
        }

        let output = Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    PwError::CommandNotFound(spec.program.clone())
                } else {
                    PwError::Io(err.to_string())
                }
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            return Err(PwError::CommandFailed {
                program: spec.program,
                args: spec.args,
                stderr,
            });
        }

        Ok(CommandOutput {
            command: spec,
            stdout,
            stderr,
            skipped: false,
        })
    }

    pub fn execute_all(&self, specs: Vec<CommandSpec>) -> Vec<Result<CommandOutput, PwError>> {
        specs.into_iter().map(|spec| self.execute(spec)).collect()
    }

    pub fn snapshot_for_config_with_effect_availability(
        &self,
        config: Option<&MixerConfig>,
        effect_availability: Vec<EffectAvailability>,
    ) -> RuntimeGraph {
        let inputs = self.list_sources().unwrap_or_default();
        let outputs = self.list_sinks().unwrap_or_default();
        let sink_names_by_index = outputs
            .iter()
            .filter_map(|sink| Some((sink.index.clone()?, sink.name.clone())))
            .collect();
        let app_streams = self
            .list_sink_inputs_with_routes(config, &sink_names_by_index)
            .unwrap_or_default();
        RuntimeGraph {
            inputs,
            outputs,
            app_streams,
            meters: Vec::new(),
            effect_availability,
        }
    }

    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for program in [
            "pipewire",
            "wireplumber",
            "pactl",
            "wpctl",
            "pw-cli",
            "pw-dump",
        ] {
            let found = command_exists(program);
            diagnostics.push(Diagnostic {
                code: format!("host_command.{program}"),
                severity: if found {
                    DiagnosticSeverity::Info
                } else {
                    DiagnosticSeverity::Error
                },
                message: if found {
                    format!("{program} is available")
                } else {
                    format!("{program} is missing")
                },
                action: if found {
                    None
                } else {
                    Some("Install PipeWire, WirePlumber, and pipewire-pulse host tools".into())
                },
            });
        }
        diagnostics
    }

    pub fn default_sink(&self) -> Result<Option<String>, PwError> {
        self.default_device(["get-default-sink"])
    }

    pub fn default_source(&self) -> Result<Option<String>, PwError> {
        self.default_device(["get-default-source"])
    }

    pub fn find_channel_bus_sink_input(
        &self,
        channel_id: &str,
        mix_id: &str,
    ) -> Result<Option<String>, PwError> {
        Ok(self
            .sink_input_routes()?
            .into_iter()
            .find(|input| {
                input.role.as_deref() == Some("channel_to_mix")
                    && input.channel_id.as_deref() == Some(channel_id)
                    && input.mix_id.as_deref() == Some(mix_id)
            })
            .map(|input| input.id))
    }

    pub fn sink_input_routes(&self) -> Result<Vec<SinkInputRoute>, PwError> {
        let json = self.pactl_json(["list", "sink-inputs"])?;
        let modules = self.pactl_text(["list", "modules", "short"])?;
        let modules = parse_managed_modules_short(&modules);
        Ok(hydrate_sink_input_routes_from_modules(
            parse_sink_input_routes_json(&json),
            &modules,
        ))
    }

    pub fn find_channel_bus_source_output(
        &self,
        channel_id: &str,
        mix_id: &str,
    ) -> Result<Option<String>, PwError> {
        Ok(self
            .source_output_routes()?
            .into_iter()
            .find(|output| {
                output.role.as_deref() == Some("channel_to_mix")
                    && output.channel_id.as_deref() == Some(channel_id)
                    && output.mix_id.as_deref() == Some(mix_id)
            })
            .map(|output| output.id))
    }

    pub fn source_output_routes(&self) -> Result<Vec<SourceOutputRoute>, PwError> {
        let json = self.pactl_json(["list", "source-outputs"])?;
        let modules = self.pactl_text(["list", "modules", "short"])?;
        let modules = parse_managed_modules_short(&modules);
        Ok(hydrate_source_output_routes_from_modules(
            parse_source_outputs_json(&json),
            &modules,
        ))
    }

    pub fn managed_modules(&self) -> Result<Vec<ManagedModule>, PwError> {
        let modules = self.pactl_text(["list", "modules", "short"])?;
        let sinks = self.pactl_json(["list", "sinks"])?;
        let sources = self.pactl_json(["list", "sources"])?;
        let sink_inputs = self.pactl_json(["list", "sink-inputs"])?;
        let source_outputs = self.pactl_json(["list", "source-outputs"])?;
        Ok(parse_managed_modules_json(
            &modules,
            &sinks,
            &sources,
            &sink_inputs,
            &source_outputs,
        ))
    }

    pub fn stale_processes(&self) -> Result<Vec<StaleProcess>, PwError> {
        if self.dry_run {
            return Ok(Vec::new());
        }

        let output = Command::new("pgrep")
            .args(["-af", "pipewire"])
            .stdin(Stdio::null())
            .output()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    PwError::CommandNotFound("pgrep".into())
                } else {
                    PwError::Io(err.to_string())
                }
            })?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        Ok(parse_stale_processes(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    fn list_sources(&self) -> Result<Vec<DeviceInfo>, PwError> {
        let json = self.pactl_json(["list", "sources"])?;
        Ok(parse_devices_json(&json, "Source"))
    }

    fn list_sinks(&self) -> Result<Vec<DeviceInfo>, PwError> {
        let json = self.pactl_json(["list", "sinks"])?;
        Ok(parse_devices_json(&json, "Sink"))
    }

    fn list_sink_inputs_with_routes(
        &self,
        config: Option<&MixerConfig>,
        sink_names_by_index: &BTreeMap<String, String>,
    ) -> Result<Vec<AppStream>, PwError> {
        let json = self.pactl_json(["list", "sink-inputs"])?;
        let clients_json = self.pactl_json(["list", "clients"]).unwrap_or_default();
        let client_properties = parse_client_properties_json(&clients_json);
        Ok(parse_sink_inputs_json_with_client_properties(
            &json,
            config,
            sink_names_by_index,
            &client_properties,
        ))
    }

    fn pactl_json<I, S>(&self, args: I) -> Result<String, PwError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        if self.dry_run {
            return Ok("[]".into());
        }
        let output = Command::new("pactl")
            .arg("--format=json")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    PwError::CommandNotFound("pactl".into())
                } else {
                    PwError::Io(err.to_string())
                }
            })?;

        if !output.status.success() {
            return Err(PwError::CommandFailed {
                program: "pactl".into(),
                args: vec!["--format=json".into()],
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn pactl_text<I, S>(&self, args: I) -> Result<String, PwError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        if self.dry_run {
            return Ok(String::new());
        }
        let args = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let output = Command::new("pactl")
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| {
                if err.kind() == std::io::ErrorKind::NotFound {
                    PwError::CommandNotFound("pactl".into())
                } else {
                    PwError::Io(err.to_string())
                }
            })?;

        if !output.status.success() {
            return Err(PwError::CommandFailed {
                program: "pactl".into(),
                args,
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn default_device<I, S>(&self, args: I) -> Result<Option<String>, PwError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let value = self.pactl_text(args)?.trim().to_string();
        Ok((!value.is_empty()).then_some(value))
    }
}

pub fn meter_targets_for_config(
    config: &MixerConfig,
    available_sources: &BTreeSet<String>,
) -> Vec<MeterTarget> {
    let mut targets = Vec::new();
    for mix in &config.mixes {
        let source_name = format!("{}.monitor", mix.virtual_sink_name);
        if available_sources.contains(&source_name) {
            targets.push(MeterTarget {
                node_id: mix.id.clone(),
                source_name,
            });
        }
    }
    for channel in &config.channels {
        let source_name = format!("{}.monitor", channel.virtual_sink_name);
        if available_sources.contains(&source_name) {
            targets.push(MeterTarget {
                node_id: channel.id.clone(),
                source_name,
            });
        }
    }
    targets
}

pub fn meter_sampling_enabled() -> bool {
    meter_sampling_enabled_from_env(
        std::env::var(PW_RECORD_METERS_ENV).ok().as_deref(),
        std::env::var(PW_RECORD_METERS_DISABLE_ENV).ok().as_deref(),
        command_exists("pw-record"),
    )
}

fn meter_sampling_enabled_from_env(
    enable_value: Option<&str>,
    disable_value: Option<&str>,
    pw_record_available: bool,
) -> bool {
    if env_truthy(disable_value) {
        return false;
    }
    if env_falsey(enable_value) {
        return false;
    }
    pw_record_available
}

fn env_truthy(value: Option<&str>) -> bool {
    value
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn env_falsey(value: Option<&str>) -> bool {
    value
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlannedGraph {
    pub commands: Vec<CommandSpec>,
    pub managed_nodes: Vec<String>,
}

pub fn plan_ensure_graph(config: &MixerConfig) -> PlannedGraph {
    let mut commands = Vec::new();
    let mut managed_nodes = Vec::new();

    for mix in &config.mixes {
        managed_nodes.push(mix.virtual_sink_name.clone());
        managed_nodes.push(mix.virtual_source_name.clone());
        commands.extend(plan_ensure_mix(mix));
    }

    for channel in &config.channels {
        managed_nodes.push(channel.virtual_sink_name.clone());
        commands.extend(plan_ensure_channel(channel));
        if let Some(source) = &channel.source_device {
            commands.extend(plan_route_input_to_channel(channel, source));
        }
        for mix in &config.mixes {
            if channel.mix_buses.contains_key(&mix.id) {
                commands.extend(plan_route_channel_to_mix(channel, mix));
            }
        }
    }

    for mix in &config.mixes {
        if let Some(output) = &mix.monitor_output {
            commands.extend(plan_route_mix_to_output(mix, output));
        }
    }

    PlannedGraph {
        commands,
        managed_nodes,
    }
}

pub fn plan_ensure_mix(mix: &Mix) -> Vec<CommandSpec> {
    let display_name = wavelinux_display_name(&mix.name);
    let display_value = property_value(&display_name);
    vec![
        CommandSpec::new(
            CommandDomain::Graph,
            "pactl",
            [
                "load-module".into(),
                "module-null-sink".into(),
                format!("sink_name={}", mix.virtual_sink_name),
                format!("rate={SAMPLE_RATE_HZ}"),
                "channels=2".into(),
                "channel_map=front-left,front-right".into(),
                format!(
                    "sink_properties=device.description={0} node.description={0} node.nick={0} media.name={0} application.name=WaveLinux media.class=Audio/Sink wavelinux.managed=1 wavelinux.role=mix wavelinux.mix_id={1}",
                    display_value,
                    mix.id
                ),
            ],
            format!("create virtual mix sink '{}'", mix.name),
        ),
        CommandSpec::new(
            CommandDomain::Graph,
            "pactl",
            [
                "load-module".into(),
                "module-remap-source".into(),
                format!("master={}.monitor", mix.virtual_sink_name),
                format!("source_name={}", mix.virtual_source_name),
                "channels=2".into(),
                "channel_map=front-left,front-right".into(),
                format!(
                    "source_properties=device.description={0} node.description={0} node.nick={0} media.name={0} application.name=WaveLinux media.class=Audio/Source/Virtual wavelinux.managed=1 wavelinux.role=mix_source wavelinux.mix_id={1}",
                    display_value,
                    mix.id
                ),
            ],
            format!("expose '{}' as virtual source", mix.name),
        ),
    ]
}

pub fn plan_ensure_channel(channel: &Channel) -> Vec<CommandSpec> {
    let display_name = wavelinux_display_name(&channel.name);
    let display_value = property_value(&display_name);
    vec![CommandSpec::new(
        CommandDomain::Graph,
        "pactl",
        [
            "load-module".into(),
            "module-null-sink".into(),
            format!("sink_name={}", channel.virtual_sink_name),
            format!("rate={SAMPLE_RATE_HZ}"),
            "channels=2".into(),
            "channel_map=front-left,front-right".into(),
            format!(
                "sink_properties=device.description={0} node.description={0} node.nick={0} media.name={0} application.name=WaveLinux media.class=Audio/Sink wavelinux.managed=1 wavelinux.role=channel wavelinux.channel_id={1}",
                display_value,
                channel.id
            ),
        ],
        format!("create channel sink '{}'", channel.name),
    )]
}

pub fn plan_route_channel_to_mix(channel: &Channel, mix: &Mix) -> Vec<CommandSpec> {
    let source_name = channel_mix_source_name(channel);
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={source_name}"),
            format!("sink={}", mix.virtual_sink_name),
            "latency_msec=10".into(),
            "channels=2".into(),
            "channel_map=front-left,front-right".into(),
            format!(
                "source_output_properties=wavelinux.managed=1 wavelinux.role=channel_to_mix wavelinux.channel_id={} wavelinux.mix_id={}",
                channel.id, mix.id
            ),
            format!(
                "sink_input_properties=wavelinux.managed=1 wavelinux.role=channel_to_mix wavelinux.channel_id={} wavelinux.mix_id={}",
                channel.id, mix.id
            ),
        ],
        format!("route '{}' to '{}'", channel.name, mix.name),
    )]
}

pub fn channel_mix_source_name(channel: &Channel) -> String {
    if channel_has_active_effects(channel) {
        effect_chain_source_name(channel)
    } else {
        format!("{}.monitor", channel.virtual_sink_name)
    }
}

pub fn channel_has_active_effects(channel: &Channel) -> bool {
    channel.effects.iter().any(|effect| !effect.bypassed)
}

pub fn effect_chain_input_name(channel: &Channel) -> String {
    format!("wavelinux_fx_{}_input", safe_node_id(&channel.id))
}

pub fn effect_chain_source_name(channel: &Channel) -> String {
    format!("wavelinux_fx_{}_source", safe_node_id(&channel.id))
}

pub fn plan_route_input_to_channel(channel: &Channel, source_name: &str) -> Vec<CommandSpec> {
    let channels = channel.input_mode.channels();
    let channel_map = channel.input_mode.channel_map();
    let mode_id = channel.input_mode.id();
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={source_name}"),
            format!("sink={}", channel.virtual_sink_name),
            "latency_msec=10".into(),
            format!("channels={channels}"),
            format!("channel_map={channel_map}"),
            "remix=yes".into(),
            format!(
                "source_output_properties=wavelinux.managed=1 wavelinux.role=input_to_channel wavelinux.channel_id={} wavelinux.input_mode={}",
                channel.id, mode_id
            ),
            format!(
                "sink_input_properties=wavelinux.managed=1 wavelinux.role=input_to_channel wavelinux.channel_id={} wavelinux.input_mode={}",
                channel.id, mode_id
            ),
        ],
        format!("route input {source_name} to '{}'", channel.name),
    )]
}

pub fn plan_route_mix_to_output(mix: &Mix, sink_name: &str) -> Vec<CommandSpec> {
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={}.monitor", mix.virtual_sink_name),
            format!("sink={sink_name}"),
            "latency_msec=10".into(),
            "channels=2".into(),
            "channel_map=front-left,front-right".into(),
            format!(
                "source_output_properties=wavelinux.managed=1 wavelinux.role=mix_monitor wavelinux.mix_id={}",
                mix.id
            ),
            format!(
                "sink_input_properties=wavelinux.managed=1 wavelinux.role=mix_monitor wavelinux.mix_id={}",
                mix.id
            ),
        ],
        format!("monitor '{}' through {sink_name}", mix.name),
    )]
}

pub fn plan_move_app_stream(stream_id: &str, channel: &Channel) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "move-sink-input".into(),
            stream_id.into(),
            channel.virtual_sink_name.clone(),
        ],
        format!("move app stream {stream_id} to '{}'", channel.name),
    )
}

pub fn plan_move_app_stream_to_default(stream_id: &str) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            String::from("move-sink-input"),
            stream_id.to_owned(),
            String::from("@DEFAULT_SINK@"),
        ],
        format!("move app stream {stream_id} to the default output"),
    )
}

pub fn plan_set_default_sink(sink_name: &str) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        vec!["set-default-sink".to_string(), sink_name.to_string()],
        format!("lock default output to {sink_name}"),
    )
}

pub fn plan_set_default_source(source_name: &str) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        vec!["set-default-source".to_string(), source_name.to_string()],
        format!("lock default input to {source_name}"),
    )
}

pub fn plan_set_stream_volume(stream_id: &str, volume: f32) -> CommandSpec {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-input-volume".into(),
            stream_id.into(),
            format!("{percent}%"),
        ],
        format!("set stream {stream_id} volume"),
    )
}

pub fn plan_set_stream_mute(stream_id: &str, muted: bool) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-input-mute".into(),
            stream_id.into(),
            (if muted { "1" } else { "0" }).to_string(),
        ],
        format!("set stream {stream_id} mute"),
    )
}

pub fn plan_set_channel_bus_volume(sink_input_id: &str, volume: f32) -> CommandSpec {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-input-volume".into(),
            sink_input_id.into(),
            format!("{percent}%"),
        ],
        format!("set channel bus sink-input {sink_input_id} volume"),
    )
}

pub fn plan_set_channel_bus_mute(sink_input_id: &str, muted: bool) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-input-mute".into(),
            sink_input_id.into(),
            (if muted { "1" } else { "0" }).to_string(),
        ],
        format!("set channel bus sink-input {sink_input_id} mute"),
    )
}

pub fn plan_set_channel_bus_source_output_volume(
    source_output_id: &str,
    volume: f32,
) -> CommandSpec {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-source-output-volume".into(),
            source_output_id.into(),
            format!("{percent}%"),
        ],
        format!("set channel bus source-output {source_output_id} volume"),
    )
}

pub fn plan_set_channel_bus_source_output_mute(source_output_id: &str, muted: bool) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-source-output-mute".into(),
            source_output_id.into(),
            (if muted { "1" } else { "0" }).to_string(),
        ],
        format!("set channel bus source-output {source_output_id} mute"),
    )
}

pub fn plan_set_mix_volume(mix: &Mix, volume: f32) -> CommandSpec {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-volume".into(),
            mix.virtual_sink_name.clone(),
            format!("{percent}%"),
        ],
        format!("set '{}' mix volume", mix.name),
    )
}

pub fn plan_set_mix_mute(mix: &Mix, muted: bool) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-mute".into(),
            mix.virtual_sink_name.clone(),
            (if muted { "1" } else { "0" }).to_string(),
        ],
        format!("set '{}' mix mute", mix.name),
    )
}

pub fn plan_unload_modules(modules: &[ManagedModule]) -> Vec<CommandSpec> {
    let mut modules = modules.to_vec();
    modules.sort_by(|left, right| {
        unload_priority(left.role.as_deref())
            .cmp(&unload_priority(right.role.as_deref()))
            .then_with(|| left.module_id.cmp(&right.module_id))
    });

    let mut seen = std::collections::BTreeSet::new();
    modules
        .into_iter()
        .filter(|module| seen.insert(module.module_id.clone()))
        .map(|module| {
            let description = module
                .role
                .as_deref()
                .map(|role| format!("unload managed {role} module {}", module.module_id))
                .unwrap_or_else(|| format!("unload managed module {}", module.module_id));
            CommandSpec::new(
                CommandDomain::Graph,
                "pactl",
                ["unload-module".into(), module.module_id],
                description,
            )
        })
        .collect()
}

pub fn plan_kill_stale_processes(processes: &[StaleProcess]) -> Vec<CommandSpec> {
    processes
        .iter()
        .map(|process| {
            CommandSpec::new(
                CommandDomain::Graph,
                "kill",
                [process.pid.clone()],
                format!("stop stale WaveLinux audio helper {}", process.pid),
            )
        })
        .collect()
}

pub fn render_filter_chain(channel: &Channel, catalog: &EffectCatalog) -> String {
    let chain_name = format!("wavelinux_fx_{}_chain", safe_node_id(&channel.id));
    let input_name = effect_chain_input_name(channel);
    let source_name = effect_chain_source_name(channel);
    let effect_nodes = channel
        .effects
        .iter()
        .filter(|effect| !effect.bypassed)
        .map(|effect| {
            let definition = catalog
                .effects
                .iter()
                .find(|item| item.id == effect.effect_id);
            render_effect_node(effect, definition)
        })
        .collect::<Vec<_>>();
    let mut rendered = String::new();
    rendered.push_str("context.properties = {\n");
    rendered.push_str("  log.level = 0\n");
    rendered.push_str("}\n\n");
    rendered.push_str("context.spa-libs = {\n");
    rendered.push_str("  audio.convert.* = audioconvert/libspa-audioconvert\n");
    rendered.push_str("  support.* = support/libspa-support\n");
    rendered.push_str("}\n\n");
    rendered.push_str("context.modules = [\n");
    rendered.push_str("  { name = libpipewire-module-rt flags = [ ifexists nofail ] }\n");
    rendered.push_str("  { name = libpipewire-module-protocol-native }\n");
    rendered.push_str("  { name = libpipewire-module-client-node }\n");
    rendered.push_str("  { name = libpipewire-module-adapter }\n");
    rendered.push_str("  { name = libpipewire-module-filter-chain\n");
    rendered.push_str("    flags = [ nofail ]\n");
    rendered.push_str("    args = {\n");
    rendered.push_str("      node.name = \"");
    rendered.push_str(&escape_pw(&chain_name));
    rendered.push_str("\"\n");
    rendered.push_str("      wavelinux.managed = \"1\"\n");
    rendered.push_str("      wavelinux.role = \"effect_chain\"\n");
    rendered.push_str("      wavelinux.channel_id = \"");
    rendered.push_str(&escape_pw(&channel.id));
    rendered.push_str("\"\n");
    rendered.push_str("      audio.channels = 2\n");
    rendered.push_str("      audio.position = [ FL FR ]\n");
    rendered.push_str("      node.description = \"WaveLinux FX ");
    rendered.push_str(&escape_pw(&channel.name));
    rendered.push_str("\"\n");
    rendered.push_str("      media.name = \"WaveLinux FX ");
    rendered.push_str(&escape_pw(&channel.name));
    rendered.push_str("\"\n");
    rendered.push_str("      filter.graph = {\n");
    rendered.push_str("        nodes = [\n");

    for node in &effect_nodes {
        rendered.push_str(&node.config);
    }

    rendered.push_str("        ]\n");
    if !effect_nodes.is_empty() {
        rendered.push_str("        links = [\n");
        for pair in effect_nodes.windows(2) {
            let source = &pair[0];
            let target = &pair[1];
            append_stereo_filter_links(&mut rendered, source, target);
        }
        rendered.push_str("        ]\n");

        let first = &effect_nodes[0];
        let last = &effect_nodes[effect_nodes.len() - 1];
        append_port_ref_list(
            &mut rendered,
            "        inputs = [",
            [
                port_ref(&first.name, first.ports.left_input),
                port_ref(&first.name, first.ports.right_input),
            ],
        );
        append_port_ref_list(
            &mut rendered,
            "        outputs = [",
            [
                port_ref(&last.name, last.ports.left_output),
                port_ref(&last.name, last.ports.right_output),
            ],
        );
    }
    rendered.push_str("      }\n");
    rendered.push_str("      capture.props = {\n");
    rendered.push_str("        node.name = \"");
    rendered.push_str(&escape_pw(&input_name));
    rendered.push_str("\"\n");
    rendered.push_str("        target.object = \"");
    rendered.push_str(&escape_pw(&format!(
        "{}.monitor",
        channel.virtual_sink_name
    )));
    rendered.push_str("\"\n");
    rendered.push_str("        node.passive = true\n");
    rendered.push_str("        wavelinux.managed = \"1\"\n");
    rendered.push_str("        wavelinux.role = \"effect_input\"\n");
    rendered.push_str("        wavelinux.channel_id = \"");
    rendered.push_str(&escape_pw(&channel.id));
    rendered.push_str("\"\n");
    rendered.push_str("      }\n");
    rendered.push_str("      playback.props = {\n");
    rendered.push_str("        node.name = \"");
    rendered.push_str(&escape_pw(&source_name));
    rendered.push_str("\"\n");
    rendered.push_str("        media.class = Audio/Source\n");
    rendered.push_str("        wavelinux.managed = \"1\"\n");
    rendered.push_str("        wavelinux.role = \"effect_output\"\n");
    rendered.push_str("        wavelinux.channel_id = \"");
    rendered.push_str(&escape_pw(&channel.id));
    rendered.push_str("\"\n");
    rendered.push_str("      }\n");
    rendered.push_str("    }\n");
    rendered.push_str("  }\n");
    rendered.push_str("]\n");
    rendered
}

pub fn probe_effect_availability(catalog: &EffectCatalog) -> Vec<EffectAvailability> {
    catalog
        .effects
        .iter()
        .map(|effect| match &effect.plugin_hint {
            PluginHint::PipeWireBuiltin => EffectAvailability {
                effect_id: effect.id.clone(),
                available: true,
                detail: "PipeWire builtin".into(),
            },
            PluginHint::Ladspa { library_names } => {
                let found = find_plugin_file(library_names);
                EffectAvailability {
                    effect_id: effect.id.clone(),
                    available: found.is_some(),
                    detail: found
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| format!("Missing one of: {}", library_names.join(", "))),
                }
            }
            PluginHint::Lv2 { uri_hint } => EffectAvailability {
                effect_id: effect.id.clone(),
                available: std::env::var_os("LV2_PATH").is_some(),
                detail: format!("LV2 URI hint: {uri_hint}"),
            },
        })
        .collect()
}

#[cfg(feature = "pipewire-rs")]
pub fn pipewire_rs_available() -> bool {
    pipewire::init();
    true
}

#[cfg(not(feature = "pipewire-rs"))]
pub fn pipewire_rs_available() -> bool {
    false
}

#[derive(Debug, Deserialize)]
struct PactlDevice {
    #[serde(default)]
    index: JsonNumberOrString,
    #[serde(default)]
    owner_module: JsonNumberOrString,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    properties: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct PactlSinkInput {
    #[serde(default)]
    index: JsonNumberOrString,
    #[serde(default)]
    owner_module: JsonNumberOrString,
    #[serde(default)]
    sink: JsonNumberOrString,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: BTreeMap<String, PactlVolumeEntry>,
    #[serde(default)]
    properties: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct PactlClient {
    #[serde(default)]
    index: JsonNumberOrString,
    #[serde(default)]
    properties: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct PactlSourceOutput {
    #[serde(default)]
    index: JsonNumberOrString,
    #[serde(default)]
    owner_module: JsonNumberOrString,
    #[serde(default)]
    properties: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceOutputRoute {
    pub id: String,
    pub module_id: Option<String>,
    pub role: Option<String>,
    pub channel_id: Option<String>,
    pub mix_id: Option<String>,
    pub target_object: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SinkInputRoute {
    pub id: String,
    pub module_id: Option<String>,
    pub role: Option<String>,
    pub channel_id: Option<String>,
    pub mix_id: Option<String>,
    pub sink: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedModule {
    pub module_id: String,
    pub role: Option<String>,
    pub channel_id: Option<String>,
    pub mix_id: Option<String>,
    pub node_name: Option<String>,
    pub source_name: Option<String>,
    pub sink_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleProcess {
    pub pid: String,
    pub command: String,
}

#[derive(Debug, Deserialize)]
struct PactlVolumeEntry {
    #[serde(default)]
    value_percent: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
enum JsonNumberOrString {
    Number(u64),
    String(String),
    #[default]
    Missing,
}

impl fmt::Display for JsonNumberOrString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JsonNumberOrString::Number(value) => write!(formatter, "{value}"),
            JsonNumberOrString::String(value) => formatter.write_str(value),
            JsonNumberOrString::Missing => Ok(()),
        }
    }
}

impl JsonNumberOrString {
    fn module_id(&self) -> Option<String> {
        let value = self.to_string();
        (!value.is_empty() && value != "4294967295").then_some(value)
    }
}

pub fn parse_devices_json(json: &str, fallback_prefix: &str) -> Vec<DeviceInfo> {
    let devices: Vec<PactlDevice> = serde_json::from_str(json).unwrap_or_default();
    devices
        .into_iter()
        .map(|device| {
            let id = if device.name.is_empty() {
                device.index.to_string()
            } else {
                device.name.clone()
            };
            let description = if device.description.is_empty() {
                device
                    .properties
                    .get("device.description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&id)
                    .to_string()
            } else {
                device.description
            };
            let is_default = device
                .properties
                .get("node.default")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let is_virtual = looks_like_wavelinux_node(&id)
                || looks_like_wavelinux_node(&device.name)
                || looks_like_wavelinux_node(&description)
                || device
                    .properties
                    .get("wavelinux.managed")
                    .and_then(serde_json::Value::as_str)
                    == Some("1");
            DeviceInfo {
                id,
                index: Some(device.index.to_string()).filter(|value| !value.is_empty()),
                name: device.name,
                description: if description.is_empty() {
                    format!("{fallback_prefix} {}", device.index)
                } else {
                    description
                },
                is_default,
                is_virtual,
            }
        })
        .collect()
}

pub fn parse_sink_inputs_json(json: &str) -> Vec<AppStream> {
    parse_sink_inputs_json_with_routes(json, None, &BTreeMap::new())
}

pub fn parse_sink_inputs_json_with_routes(
    json: &str,
    config: Option<&MixerConfig>,
    sink_names_by_index: &BTreeMap<String, String>,
) -> Vec<AppStream> {
    parse_sink_inputs_json_with_client_properties(
        json,
        config,
        sink_names_by_index,
        &BTreeMap::new(),
    )
}

fn parse_sink_inputs_json_with_client_properties(
    json: &str,
    config: Option<&MixerConfig>,
    sink_names_by_index: &BTreeMap<String, String>,
    client_properties_by_id: &BTreeMap<String, BTreeMap<String, serde_json::Value>>,
) -> Vec<AppStream> {
    let inputs: Vec<PactlSinkInput> = serde_json::from_str(json).unwrap_or_default();
    inputs
        .into_iter()
        .filter(|input| !is_managed_or_loopback_sink_input(&input.properties))
        .map(|input| {
            let properties = merged_sink_input_properties(&input, client_properties_by_id);
            let binary = property_string(&properties, "application.process.binary");
            let window_class = property_string(&properties, "window.x11.class")
                .or_else(|| property_string(&properties, "window.class"))
                .or_else(|| property_string(&properties, "application.window.class"));
            let app_id = property_string(&properties, "application.id")
                .or_else(|| property_string(&properties, "application.process.binary"))
                .or_else(|| property_string(&properties, "module-stream-restore.id"));
            let process_name = property_string(&properties, "application.process.name")
                .or_else(|| binary.clone())
                .or_else(|| property_string(&properties, "application.name"))
                .or_else(|| property_string(&properties, "node.name"))
                .or_else(|| property_string(&properties, "media.name"));
            let display_name = property_string(&properties, "application.name")
                .or_else(|| app_id.clone())
                .unwrap_or_else(|| format!("Stream {}", input.index));
            let media_name = property_string(&properties, "media.name");
            let sink_name = sink_names_by_index.get(&input.sink.to_string());
            let routed_channel_id =
                property_string(&properties, "wavelinux.channel_id").or_else(|| {
                    let sink_name = sink_name?;
                    config?
                        .channels
                        .iter()
                        .find(|channel| channel.virtual_sink_name == *sink_name)
                        .map(|channel| channel.id.clone())
                });
            let mut stream = AppStream {
                id: input.index.to_string(),
                app_id,
                binary,
                process_name,
                window_class,
                display_name,
                media_name,
                routed_channel_id,
                volume: parse_first_volume(&input.volume).unwrap_or(1.0),
                muted: input.mute,
            };
            if let Some(config) = config {
                apply_configured_app_label(config, &mut stream);
            }
            stream
        })
        .collect()
}

fn apply_configured_app_label(config: &MixerConfig, stream: &mut AppStream) {
    let Some(raw) = AppMatcher::from_stream(stream) else {
        return;
    };
    let resolved = config.resolve_app_matcher(&raw);
    if let Some(label) = config
        .label_for_matcher(&resolved)
        .or_else(|| config.label_for_matcher(&raw))
    {
        stream.display_name = label;
    }
}

fn parse_client_properties_json(
    json: &str,
) -> BTreeMap<String, BTreeMap<String, serde_json::Value>> {
    let clients: Vec<PactlClient> = serde_json::from_str(json).unwrap_or_default();
    let mut properties_by_id = BTreeMap::new();
    for client in clients {
        let mut keys = vec![client.index.to_string()];
        for property in ["object.id", "object.serial", "client.id"] {
            if let Some(value) = property_string(&client.properties, property) {
                keys.push(value);
            }
        }
        for key in keys {
            if !key.trim().is_empty() {
                properties_by_id.insert(key, client.properties.clone());
            }
        }
    }
    properties_by_id
}

fn merged_sink_input_properties(
    input: &PactlSinkInput,
    client_properties_by_id: &BTreeMap<String, BTreeMap<String, serde_json::Value>>,
) -> BTreeMap<String, serde_json::Value> {
    let Some(client_id) = property_string(&input.properties, "client.id") else {
        return input.properties.clone();
    };
    let Some(client_properties) = client_properties_by_id.get(&client_id) else {
        return input.properties.clone();
    };
    let mut merged = client_properties.clone();
    merged.extend(input.properties.clone());
    merged
}

pub fn parse_sink_input_routes_json(json: &str) -> Vec<SinkInputRoute> {
    let inputs: Vec<PactlSinkInput> = serde_json::from_str(json).unwrap_or_default();
    inputs
        .into_iter()
        .map(|input| SinkInputRoute {
            id: input.index.to_string(),
            module_id: input.owner_module.module_id(),
            role: property_string(&input.properties, "wavelinux.role"),
            channel_id: property_string(&input.properties, "wavelinux.channel_id"),
            mix_id: property_string(&input.properties, "wavelinux.mix_id"),
            sink: Some(input.sink.to_string()).filter(|value| !value.is_empty()),
        })
        .collect()
}

fn is_managed_or_loopback_sink_input(properties: &BTreeMap<String, serde_json::Value>) -> bool {
    if property_string(properties, "wavelinux.managed").as_deref() == Some("1") {
        return true;
    }
    let node_name = property_string(properties, "node.name");
    let media_name = property_string(properties, "media.name");
    node_name
        .as_deref()
        .is_some_and(|value| value.starts_with("output.loopback-"))
        || media_name
            .as_deref()
            .is_some_and(|value| value.starts_with("loopback-"))
}

fn unload_priority(role: Option<&str>) -> u8 {
    match role {
        Some("channel_to_mix") | Some("input_to_channel") | Some("mix_monitor") => 0,
        Some("mix_source") => 1,
        Some("channel") | Some("mix") => 2,
        _ => 3,
    }
}

pub fn parse_source_outputs_json(json: &str) -> Vec<SourceOutputRoute> {
    let outputs: Vec<PactlSourceOutput> = serde_json::from_str(json).unwrap_or_default();
    outputs
        .into_iter()
        .map(|output| SourceOutputRoute {
            id: output.index.to_string(),
            module_id: output.owner_module.module_id(),
            role: property_string(&output.properties, "wavelinux.role"),
            channel_id: property_string(&output.properties, "wavelinux.channel_id"),
            mix_id: property_string(&output.properties, "wavelinux.mix_id"),
            target_object: property_string(&output.properties, "target.object"),
        })
        .collect()
}

pub fn parse_managed_modules_json(
    modules_text: &str,
    sinks_json: &str,
    sources_json: &str,
    sink_inputs_json: &str,
    source_outputs_json: &str,
) -> Vec<ManagedModule> {
    let mut modules = Vec::new();

    modules.extend(parse_managed_modules_short(modules_text));

    let sinks: Vec<PactlDevice> = serde_json::from_str(sinks_json).unwrap_or_default();
    modules.extend(sinks.into_iter().filter_map(|device| {
        managed_module_from_parts(
            device.owner_module.module_id(),
            Some(device.name),
            None,
            &device.properties,
        )
    }));

    let sources: Vec<PactlDevice> = serde_json::from_str(sources_json).unwrap_or_default();
    modules.extend(sources.into_iter().filter_map(|device| {
        managed_module_from_parts(
            device.owner_module.module_id(),
            Some(device.name),
            None,
            &device.properties,
        )
    }));

    let sink_inputs: Vec<PactlSinkInput> =
        serde_json::from_str(sink_inputs_json).unwrap_or_default();
    modules.extend(sink_inputs.into_iter().filter_map(|input| {
        managed_module_from_parts(
            input.owner_module.module_id(),
            None,
            None,
            &input.properties,
        )
    }));

    let source_outputs: Vec<PactlSourceOutput> =
        serde_json::from_str(source_outputs_json).unwrap_or_default();
    modules.extend(source_outputs.into_iter().filter_map(|output| {
        managed_module_from_parts(
            output.owner_module.module_id(),
            None,
            None,
            &output.properties,
        )
    }));

    let mut seen = std::collections::BTreeSet::new();
    modules
        .into_iter()
        .filter(|module| seen.insert(module.module_id.clone()))
        .collect()
}

fn parse_managed_modules_short(modules_text: &str) -> Vec<ManagedModule> {
    modules_text
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            let module_id = parts.next()?.trim();
            let module_name = parts.next().unwrap_or_default().trim();
            let argument = parts.next().unwrap_or_default().trim();
            managed_module_from_module_line(module_id, module_name, argument)
        })
        .collect()
}

fn managed_module_from_module_line(
    module_id: &str,
    module_name: &str,
    argument: &str,
) -> Option<ManagedModule> {
    if module_id.is_empty() {
        return None;
    }

    let node_name = wavelinux_node_name_from_module_argument(argument);
    let source_name = command_arg_value_from_text(argument, "source=");
    let sink_name = command_arg_value_from_text(argument, "sink=");
    let role = property_value_from_arg(argument, "wavelinux.role=").map(ToOwned::to_owned);
    let channel_id =
        property_value_from_arg(argument, "wavelinux.channel_id=").map(ToOwned::to_owned);
    let mix_id = property_value_from_arg(argument, "wavelinux.mix_id=").map(ToOwned::to_owned);
    let managed = looks_like_wavelinux_node(module_name)
        || looks_like_wavelinux_node(argument)
        || role.is_some()
        || channel_id.is_some()
        || mix_id.is_some();

    managed.then(|| ManagedModule {
        module_id: module_id.to_string(),
        role,
        channel_id,
        mix_id,
        node_name,
        source_name,
        sink_name,
    })
}

pub fn parse_stale_processes(processes_text: &str) -> Vec<StaleProcess> {
    let self_pid = std::process::id().to_string();
    processes_text
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(char::is_whitespace)?;
            let command = command.trim();
            (pid != self_pid && is_stale_wavelinux_audio_process(command)).then(|| StaleProcess {
                pid: pid.to_string(),
                command: command.to_string(),
            })
        })
        .collect()
}

fn is_stale_wavelinux_audio_process(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    command.contains("pipewire")
        && (command.contains("wavelinux-chain")
            || command.contains("wavelinux.fx")
            || command.contains("/wavelinux-chain-"))
}

fn wavelinux_node_name_from_module_argument(argument: &str) -> Option<String> {
    let prefixes = ["sink_name=", "source_name=", "source=", "sink=", "master="];
    let values = prefixes
        .iter()
        .filter_map(|prefix| command_arg_value_from_text(argument, prefix))
        .collect::<Vec<_>>();
    values
        .iter()
        .find(|value| looks_like_wavelinux_node(value))
        .cloned()
        .or_else(|| values.into_iter().next())
}

fn managed_module_from_parts(
    module_id: Option<String>,
    node_name: Option<String>,
    argument: Option<&str>,
    properties: &BTreeMap<String, serde_json::Value>,
) -> Option<ManagedModule> {
    let role = property_string(properties, "wavelinux.role");
    let channel_id = property_string(properties, "wavelinux.channel_id");
    let mix_id = property_string(properties, "wavelinux.mix_id");
    let node_name = node_name
        .filter(|value| !value.is_empty())
        .or_else(|| property_string(properties, "node.name"));
    let managed = property_string(properties, "wavelinux.managed").as_deref() == Some("1")
        || role.is_some()
        || node_name.as_deref().is_some_and(looks_like_wavelinux_node)
        || argument.is_some_and(looks_like_wavelinux_node);

    if !managed {
        return None;
    }

    Some(ManagedModule {
        module_id: module_id?,
        role,
        channel_id,
        mix_id,
        node_name,
        source_name: None,
        sink_name: None,
    })
}

fn hydrate_source_output_routes_from_modules(
    mut routes: Vec<SourceOutputRoute>,
    modules: &[ManagedModule],
) -> Vec<SourceOutputRoute> {
    let modules_by_id = modules
        .iter()
        .map(|module| (module.module_id.as_str(), module))
        .collect::<BTreeMap<_, _>>();

    for route in &mut routes {
        let Some(module_id) = route.module_id.as_deref() else {
            continue;
        };
        let Some(module) = modules_by_id.get(module_id) else {
            continue;
        };

        if route.role.is_none() {
            route.role = module.role.clone();
        }
        if route.channel_id.is_none() {
            route.channel_id = module.channel_id.clone();
        }
        if route.mix_id.is_none() {
            route.mix_id = module.mix_id.clone();
        }
        if route.target_object.is_none() {
            route.target_object = module.source_name.clone();
        }
    }

    routes
}

fn hydrate_sink_input_routes_from_modules(
    mut routes: Vec<SinkInputRoute>,
    modules: &[ManagedModule],
) -> Vec<SinkInputRoute> {
    let modules_by_id = modules
        .iter()
        .map(|module| (module.module_id.as_str(), module))
        .collect::<BTreeMap<_, _>>();

    for route in &mut routes {
        let Some(module_id) = route.module_id.as_deref() else {
            continue;
        };
        let Some(module) = modules_by_id.get(module_id) else {
            continue;
        };

        if route.role.is_none() {
            route.role = module.role.clone();
        }
        if route.channel_id.is_none() {
            route.channel_id = module.channel_id.clone();
        }
        if route.mix_id.is_none() {
            route.mix_id = module.mix_id.clone();
        }
    }

    routes
}

fn looks_like_wavelinux_node(value: &str) -> bool {
    value.to_ascii_lowercase().contains("wavelinux")
}

#[derive(Debug, Clone)]
struct RenderedEffectNode {
    name: String,
    ports: EffectAudioPorts,
    config: String,
}

#[derive(Debug, Clone, Copy)]
struct EffectAudioPorts {
    left_input: &'static str,
    right_input: &'static str,
    left_output: &'static str,
    right_output: &'static str,
}

const BUILTIN_STEREO_PORTS: EffectAudioPorts = EffectAudioPorts {
    left_input: "In",
    right_input: "In",
    left_output: "Out",
    right_output: "Out",
};

const DEEPFILTER_STEREO_PORTS: EffectAudioPorts = EffectAudioPorts {
    left_input: "Audio In L",
    right_input: "Audio In R",
    left_output: "Audio Out L",
    right_output: "Audio Out R",
};

const RNNOISE_STEREO_PORTS: EffectAudioPorts = EffectAudioPorts {
    left_input: "Input (L)",
    right_input: "Input (R)",
    left_output: "Output (L)",
    right_output: "Output (R)",
};

const SC4_STEREO_PORTS: EffectAudioPorts = EffectAudioPorts {
    left_input: "Left input",
    right_input: "Right input",
    left_output: "Left output",
    right_output: "Right output",
};

const FAST_LIMITER_STEREO_PORTS: EffectAudioPorts = EffectAudioPorts {
    left_input: "Input 1",
    right_input: "Input 2",
    left_output: "Output 1",
    right_output: "Output 2",
};

fn render_effect_node(
    effect: &EffectInstance,
    definition: Option<&wavelinux_model::EffectDefinition>,
) -> RenderedEffectNode {
    let Some(definition) = definition else {
        return render_builtin_node(effect, "copy", &[], BUILTIN_STEREO_PORTS);
    };

    match effect.effect_id.as_str() {
        "deepfilternet" => render_ladspa_node(
            effect,
            "libdeep_filter_ladspa",
            "deep_filter_stereo",
            &[
                (
                    "Attenuation Limit (dB)",
                    effect_param(effect, definition, "attenuation_limit_db"),
                ),
                (
                    "Min processing threshold (dB)",
                    effect_param(effect, definition, "min_processing_threshold_db"),
                ),
                (
                    "Max ERB processing threshold (dB)",
                    effect_param(effect, definition, "max_erb_processing_threshold_db"),
                ),
                (
                    "Max DF processing threshold (dB)",
                    effect_param(effect, definition, "max_df_processing_threshold_db"),
                ),
                (
                    "Min Processing Buffer (frames)",
                    effect_param(effect, definition, "min_processing_buffer_frames"),
                ),
                (
                    "Post Filter Beta",
                    effect_param(effect, definition, "post_filter_beta"),
                ),
            ],
            DEEPFILTER_STEREO_PORTS,
        ),
        "rnnoise" => render_ladspa_node(
            effect,
            "librnnoise_ladspa",
            "noise_suppressor_stereo",
            &[
                (
                    "VAD Threshold (%)",
                    effect_param(effect, definition, "vad_threshold"),
                ),
                (
                    "VAD Grace Period (ms)",
                    effect_param(effect, definition, "hold_ms"),
                ),
                (
                    "Retroactive VAD Grace (ms)",
                    effect_param(effect, definition, "lead_in_ms"),
                ),
            ],
            RNNOISE_STEREO_PORTS,
        ),
        "highpass" => render_builtin_node(
            effect,
            "bq_highpass",
            &[
                ("Freq", effect_param(effect, definition, "frequency_hz")),
                ("Q", 0.707),
                ("Gain", 0.0),
            ],
            BUILTIN_STEREO_PORTS,
        ),
        "eq" => render_param_eq_node(effect, definition),
        "compressor" => render_ladspa_node(
            effect,
            "sc4_1882",
            "sc4",
            &[
                ("RMS/peak", 0.0),
                (
                    "Attack time (ms)",
                    effect_param(effect, definition, "attack_ms"),
                ),
                (
                    "Release time (ms)",
                    effect_param(effect, definition, "release_ms"),
                ),
                (
                    "Threshold level (dB)",
                    effect_param(effect, definition, "threshold_db"),
                ),
                ("Ratio (1:n)", effect_param(effect, definition, "ratio")),
                ("Knee radius (dB)", 3.25),
                (
                    "Makeup gain (dB)",
                    effect_param(effect, definition, "makeup_gain_db"),
                ),
            ],
            SC4_STEREO_PORTS,
        ),
        "gate" => render_builtin_node(
            effect,
            "noisegate",
            &[
                (
                    "Close threshold",
                    effect_param(effect, definition, "threshold_db"),
                ),
                (
                    "Open threshold",
                    effect_param(effect, definition, "threshold_db") + 3.0,
                ),
                (
                    "Attack (s)",
                    effect_param(effect, definition, "attack_ms") / 1000.0,
                ),
                (
                    "Hold (s)",
                    effect_param(effect, definition, "hold_ms") / 1000.0,
                ),
                (
                    "Release (s)",
                    effect_param(effect, definition, "release_ms") / 1000.0,
                ),
            ],
            BUILTIN_STEREO_PORTS,
        ),
        "limiter" => render_ladspa_node(
            effect,
            "fast_lookahead_limiter_1913",
            "fastLookaheadLimiter",
            &[
                (
                    "Input gain (dB)",
                    effect_param(effect, definition, "input_gain_db"),
                ),
                ("Limit (dB)", effect_param(effect, definition, "ceiling_db")),
                ("Release time (s)", 0.08),
            ],
            FAST_LIMITER_STEREO_PORTS,
        ),
        _ => render_builtin_node(effect, "copy", &[], BUILTIN_STEREO_PORTS),
    }
}

fn effect_param(
    effect: &EffectInstance,
    definition: &wavelinux_model::EffectDefinition,
    param_id: &str,
) -> f32 {
    definition
        .params
        .iter()
        .find(|param| param.id == param_id)
        .map(|param| {
            effect
                .params
                .get(param_id)
                .copied()
                .unwrap_or(param.default)
                .clamp(param.min, param.max)
        })
        .unwrap_or(0.0)
}

fn render_ladspa_node(
    effect: &EffectInstance,
    plugin: &str,
    label: &str,
    controls: &[(&str, f32)],
    ports: EffectAudioPorts,
) -> RenderedEffectNode {
    let name = effect_node_name(effect);
    let mut rendered = String::new();
    rendered.push_str("          { type = ladspa plugin = \"");
    rendered.push_str(plugin);
    rendered.push_str("\" label = \"");
    rendered.push_str(label);
    rendered.push_str("\" name = \"");
    rendered.push_str(&escape_pw(&name));
    rendered.push('"');
    append_control_block(&mut rendered, controls);
    rendered.push_str(" }\n");
    RenderedEffectNode {
        name,
        ports,
        config: rendered,
    }
}

fn render_builtin_node(
    effect: &EffectInstance,
    label: &str,
    controls: &[(&str, f32)],
    ports: EffectAudioPorts,
) -> RenderedEffectNode {
    let name = effect_node_name(effect);
    let mut rendered = String::new();
    rendered.push_str("          { type = builtin label = \"");
    rendered.push_str(label);
    rendered.push_str("\" name = \"");
    rendered.push_str(&escape_pw(&name));
    rendered.push('"');
    append_control_block(&mut rendered, controls);
    rendered.push_str(" }\n");
    RenderedEffectNode {
        name,
        ports,
        config: rendered,
    }
}

fn render_param_eq_node(
    effect: &EffectInstance,
    definition: &wavelinux_model::EffectDefinition,
) -> RenderedEffectNode {
    let name = effect_node_name(effect);
    let low_freq = effect_param(effect, definition, "low_freq_hz");
    let low_gain = effect_param(effect, definition, "low_gain_db");
    let mid_freq = effect_param(effect, definition, "mid_freq_hz");
    let mid_gain = effect_param(effect, definition, "mid_gain_db");
    let high_freq = effect_param(effect, definition, "high_freq_hz");
    let high_gain = effect_param(effect, definition, "high_gain_db");

    let mut rendered = String::new();
    rendered.push_str("          { type = builtin label = \"param_eq\" name = \"");
    rendered.push_str(&escape_pw(&name));
    rendered.push_str("\" config = { filters = [");
    for (kind, freq, gain, q) in [
        ("bq_lowshelf", low_freq, low_gain, 0.707),
        ("bq_peaking", mid_freq, mid_gain, 1.0),
        ("bq_highshelf", high_freq, high_gain, 0.707),
    ] {
        rendered.push_str(" { type = ");
        rendered.push_str(kind);
        rendered.push_str(" freq = ");
        rendered.push_str(&format!("{freq:.3}"));
        rendered.push_str(" gain = ");
        rendered.push_str(&format!("{gain:.3}"));
        rendered.push_str(" q = ");
        rendered.push_str(&format!("{q:.3}"));
        rendered.push_str(" }");
    }
    rendered.push_str(" ] } }\n");
    RenderedEffectNode {
        name,
        ports: BUILTIN_STEREO_PORTS,
        config: rendered,
    }
}

fn effect_node_name(effect: &EffectInstance) -> String {
    let name = effect.instance_id.trim();
    if name.is_empty() {
        safe_node_id(&effect.effect_id)
    } else {
        name.to_string()
    }
}

fn append_stereo_filter_links(
    rendered: &mut String,
    source: &RenderedEffectNode,
    target: &RenderedEffectNode,
) {
    let left = (
        port_ref(&source.name, source.ports.left_output),
        port_ref(&target.name, target.ports.left_input),
    );
    let right = (
        port_ref(&source.name, source.ports.right_output),
        port_ref(&target.name, target.ports.right_input),
    );
    append_filter_link(rendered, &left.0, &left.1);
    if right != left {
        append_filter_link(rendered, &right.0, &right.1);
    }
}

fn append_filter_link(rendered: &mut String, source: &str, target: &str) {
    rendered.push_str("          { output = \"");
    rendered.push_str(&escape_pw(source));
    rendered.push_str("\" input = \"");
    rendered.push_str(&escape_pw(target));
    rendered.push_str("\" }\n");
}

fn append_port_ref_list(rendered: &mut String, prefix: &str, refs: [String; 2]) {
    rendered.push_str(prefix);
    let mut seen = std::collections::BTreeSet::new();
    for reference in refs {
        if seen.insert(reference.clone()) {
            rendered.push_str(" \"");
            rendered.push_str(&escape_pw(&reference));
            rendered.push('"');
        }
    }
    rendered.push_str(" ]\n");
}

fn port_ref(node: &str, port: &str) -> String {
    format!("{node}:{port}")
}

fn append_control_block(rendered: &mut String, controls: &[(&str, f32)]) {
    if controls.is_empty() {
        return;
    }
    rendered.push_str(" control = {");
    for (name, value) in controls {
        rendered.push_str(" \"");
        rendered.push_str(&escape_pw(name));
        rendered.push_str("\" = ");
        rendered.push_str(&format!("{value:.3}"));
    }
    rendered.push_str(" }");
}

fn parse_first_volume(volume: &BTreeMap<String, PactlVolumeEntry>) -> Option<f32> {
    volume.values().next().and_then(|entry| {
        entry
            .value_percent
            .trim_end_matches('%')
            .parse::<f32>()
            .ok()
            .map(|percent| (percent / 100.0).clamp(0.0, 1.5))
    })
}

fn property_string(map: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn command_arg_value_from_text(text: &str, prefix: &str) -> Option<String> {
    text.split_whitespace()
        .find_map(|part| part.strip_prefix(prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_matches('"').to_string())
}

fn property_value_from_arg<'a>(properties: &'a str, key: &str) -> Option<&'a str> {
    properties
        .split_whitespace()
        .find_map(|part| part.strip_prefix(key))
        .filter(|value| !value.is_empty())
}

fn wavelinux_display_name(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() || slug == "wavelinux" {
        return "wavelinux-source".into();
    }
    if slug.starts_with("wavelinux-") {
        slug.into()
    } else {
        format!("wavelinux-{slug}")
    }
}

fn property_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn escape_pw(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:=@%+".contains(ch))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|path| path.join(program))
                .find(|path| path.exists())
        })
        .is_some()
}

fn find_plugin_file(names: &[String]) -> Option<PathBuf> {
    let roots = plugin_roots();
    for root in roots {
        for name in names {
            let candidate = root.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

fn plugin_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(paths) = std::env::var_os("LADSPA_PATH") {
        roots.extend(std::env::split_paths(&paths));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".ladspa"));
        roots.push(home.join(".local/lib/ladspa"));
        roots.push(home.join(".local/lib64/ladspa"));
    }
    roots.extend(
        [
            "/usr/lib/ladspa",
            "/usr/lib64/ladspa",
            "/usr/local/lib/ladspa",
            "/usr/local/lib64/ladspa",
            "/usr/lib/x86_64-linux-gnu/ladspa",
            "/usr/lib/aarch64-linux-gnu/ladspa",
            "/usr/lib/arm-linux-gnueabihf/ladspa",
        ]
        .iter()
        .map(Path::new)
        .map(Path::to_path_buf),
    );
    roots.sort();
    roots.dedup();
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavelinux_model::{ChannelInputMode, ChannelKind, MixerConfig};

    #[test]
    fn planned_graph_creates_mixes_channels_and_routes() {
        let config = MixerConfig::default();
        let plan = plan_ensure_graph(&config);
        assert!(plan
            .commands
            .iter()
            .any(|command| command.description.contains("create virtual mix sink")));
        assert!(plan.commands.iter().any(|command| command
            .description
            .contains("route 'Hardware In' to 'Monitor'")));
        assert!(plan.managed_nodes.contains(&"wavelinux_mix_monitor".into()));
        assert!(plan
            .commands
            .iter()
            .flat_map(|command| command.args.iter())
            .any(|arg| arg.contains("device.description=wavelinux-monitor")));
        assert!(plan
            .commands
            .iter()
            .flat_map(|command| command.args.iter())
            .any(|arg| arg.contains("device.description=wavelinux-hardware-in")));
    }

    #[test]
    fn display_names_are_wavelinux_prefixed_and_space_free() {
        assert_eq!(
            wavelinux_display_name("Discord Mix"),
            "wavelinux-discord-mix"
        );
        assert_eq!(
            wavelinux_display_name("Hardware In"),
            "wavelinux-hardware-in"
        );
        assert_eq!(
            wavelinux_display_name("wavelinux-stream"),
            "wavelinux-stream"
        );
        assert_eq!(wavelinux_display_name(""), "wavelinux-source");
        assert!(!wavelinux_display_name("Music Browser").contains(' '));
    }

    #[test]
    fn pw_record_meters_default_on_when_available() {
        assert!(meter_sampling_enabled_from_env(None, None, true));
        assert!(!meter_sampling_enabled_from_env(None, None, false));
        assert!(!meter_sampling_enabled_from_env(Some("0"), None, true));
        assert!(!meter_sampling_enabled_from_env(Some("false"), None, true));
        assert!(!meter_sampling_enabled_from_env(Some("1"), Some("1"), true));
        assert!(meter_sampling_enabled_from_env(Some("1"), Some("0"), true));
    }

    #[test]
    fn meter_targets_follow_available_wavelinux_sources() {
        let config = MixerConfig::default();
        let available_sources = BTreeSet::from([
            "wavelinux_mix_monitor.monitor".to_string(),
            "wavelinux_mix_stream.monitor".to_string(),
            "wavelinux_channel_game.monitor".to_string(),
            "alsa_input.real".to_string(),
        ]);

        let targets = meter_targets_for_config(&config, &available_sources);

        assert!(targets.iter().any(|target| target.node_id == "monitor"
            && target.source_name == "wavelinux_mix_monitor.monitor"));
        assert!(targets.iter().any(|target| target.node_id == "stream"
            && target.source_name == "wavelinux_mix_stream.monitor"));
        assert!(targets.iter().any(|target| target.node_id == "game"
            && target.source_name == "wavelinux_channel_game.monitor"));
        assert!(!targets
            .iter()
            .any(|target| target.source_name == "alsa_input.real"));
    }

    #[test]
    fn move_stream_targets_channel_sink() {
        let channel = Channel::new_fixed("discord", "Discord", ChannelKind::Application);
        let spec = plan_move_app_stream("42", &channel);
        assert_eq!(spec.program, "pactl");
        assert_eq!(spec.args[2], "wavelinux_channel_discord");
    }

    #[test]
    fn move_stream_to_default_targets_default_sink() {
        let spec = plan_move_app_stream_to_default("42");
        assert_eq!(spec.program, "pactl");
        assert_eq!(spec.args[0], "move-sink-input");
        assert_eq!(spec.args[2], "@DEFAULT_SINK@");
    }

    #[test]
    fn default_device_locks_target_named_nodes() {
        let sink = plan_set_default_sink("wavelinux_channel_system");
        assert_eq!(sink.args, ["set-default-sink", "wavelinux_channel_system"]);

        let source = plan_set_default_source("wavelinux_mix_stream_source");
        assert_eq!(
            source.args,
            ["set-default-source", "wavelinux_mix_stream_source"]
        );
    }

    #[test]
    fn input_route_targets_channel_sink() {
        let channel = Channel::new_fixed("mic", "Mic", ChannelKind::Microphone);
        let spec = plan_route_input_to_channel(&channel, "alsa_input.usb_mic")
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(spec.program, "pactl");
        assert!(spec.args.contains(&"source=alsa_input.usb_mic".into()));
        assert!(spec.args.contains(&"sink=wavelinux_channel_mic".into()));
        assert!(spec.args.contains(&"channels=2".into()));
        assert!(spec
            .args
            .contains(&"channel_map=front-left,front-right".into()));
        assert!(spec.args.contains(&"remix=yes".into()));
        assert!(spec
            .args
            .iter()
            .any(|arg| arg.contains("wavelinux.role=input_to_channel")));
    }

    #[test]
    fn input_route_uses_selected_input_mode() {
        let mut channel = Channel::new_fixed("capture_card", "Capture Card", ChannelKind::Generic);
        channel.input_mode = ChannelInputMode::SumMono;
        let spec = plan_route_input_to_channel(&channel, "alsa_input.capture")
            .into_iter()
            .next()
            .unwrap();

        assert!(spec.args.contains(&"channels=1".into()));
        assert!(spec.args.contains(&"channel_map=mono".into()));
        assert!(spec
            .args
            .iter()
            .any(|arg| arg.contains("wavelinux.input_mode=sum_mono")));

        channel.input_mode = ChannelInputMode::MonoLeft;
        let spec = plan_route_input_to_channel(&channel, "alsa_input.capture")
            .into_iter()
            .next()
            .unwrap();
        assert!(spec.args.contains(&"channels=2".into()));
        assert!(spec
            .args
            .contains(&"channel_map=front-left,front-left".into()));
    }

    #[test]
    fn active_effects_route_channel_to_fx_source() {
        let mut channel = Channel::new_fixed("hardware_in", "Hardware In", ChannelKind::Generic);
        channel.effects.push(EffectInstance::new("limiter"));
        let mix = Mix::new_fixed("stream", "Stream");
        let spec = plan_route_channel_to_mix(&channel, &mix)
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(
            channel_mix_source_name(&channel),
            "wavelinux_fx_hardware_in_source"
        );
        assert!(spec
            .args
            .contains(&"source=wavelinux_fx_hardware_in_source".into()));
        assert!(spec.args.contains(&"sink=wavelinux_mix_stream".into()));
    }

    #[test]
    fn parses_pactl_devices() {
        let json = r#"
        [
          {
            "index": 1,
            "name": "alsa_output.test",
            "description": "Speakers",
            "properties": {"device.description": "Speakers"}
          },
          {
            "index": 2,
            "name": "wavelinux_mix_stream",
            "description": "WaveLinux Stream",
            "properties": {"wavelinux.managed": "1"}
          },
          {
            "index": 3,
            "name": "output.wavelinux.fx.alsa_input.source",
            "description": "WaveLinux FX Source",
            "properties": {}
          }
        ]
        "#;
        let devices = parse_devices_json(json, "Sink");
        assert_eq!(devices.len(), 3);
        assert!(devices[1].is_virtual);
        assert!(devices[2].is_virtual);
    }

    #[test]
    fn parses_sink_input_identity_and_volume() {
        let json = r#"
        [
          {
            "index": 72,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "66%"}},
            "properties": {
              "application.name": "Firefox",
              "application.process.binary": "firefox",
              "window.x11.class": "firefox",
              "media.name": "AudioStream"
            }
          }
        ]
        "#;
        let streams = parse_sink_inputs_json(json);
        assert_eq!(streams[0].id, "72");
        assert_eq!(streams[0].display_name, "Firefox");
        assert_eq!(streams[0].binary.as_deref(), Some("firefox"));
        assert_eq!(streams[0].window_class.as_deref(), Some("firefox"));
        assert!((streams[0].volume - 0.66).abs() < 0.001);
    }

    #[test]
    fn enriches_sink_input_identity_from_client_properties() {
        let sink_inputs = r#"
        [
          {
            "index": 31821,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "100%"}},
            "properties": {
              "client.id": "389",
              "media.name": "audio-src",
              "node.name": "audio-src",
              "module-stream-restore.id": "sink-input-by-media-role:music"
            }
          }
        ]
        "#;
        let clients = r#"
        [
          {
            "index": 31820,
            "properties": {
              "object.id": "389",
              "application.name": "spotify",
              "application.process.binary": "spotify"
            }
          }
        ]
        "#;
        let client_properties = parse_client_properties_json(clients);
        let streams = parse_sink_inputs_json_with_client_properties(
            sink_inputs,
            None,
            &BTreeMap::new(),
            &client_properties,
        );

        assert_eq!(streams[0].display_name, "spotify");
        assert_eq!(streams[0].app_id.as_deref(), Some("spotify"));
        assert_eq!(streams[0].binary.as_deref(), Some("spotify"));
        assert_eq!(streams[0].media_name.as_deref(), Some("audio-src"));
    }

    #[test]
    fn applies_saved_app_label_to_active_streams() {
        let json = r#"
        [
          {
            "index": 72,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "66%"}},
            "properties": {
              "application.name": "Ferdium",
              "application.process.binary": "ferdium",
              "media.name": "Slack"
            }
          }
        ]
        "#;
        let mut config = MixerConfig::default();
        let matcher = AppMatcher {
            app_id: Some("ferdium".into()),
            binary: Some("ferdium".into()),
            process_name: Some("ferdium".into()),
            window_class: None,
            media_name: Some("Slack".into()),
        };
        config.pin_app_identity(matcher, "Work Slack").unwrap();

        let streams = parse_sink_inputs_json_with_routes(json, Some(&config), &BTreeMap::new());

        assert_eq!(streams[0].display_name, "Work Slack");
    }

    #[test]
    fn parses_sink_input_route_from_target_sink() {
        let json = r#"
        [
          {
            "index": 72,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "66%"}},
            "properties": {
              "application.name": "Firefox",
              "application.process.binary": "firefox"
            }
          }
        ]
        "#;
        let config = MixerConfig::default();
        let sinks = BTreeMap::from([("2".to_string(), "wavelinux_channel_browser".to_string())]);
        let streams = parse_sink_inputs_json_with_routes(json, Some(&config), &sinks);
        assert_eq!(streams[0].routed_channel_id.as_deref(), Some("browser"));
    }

    #[test]
    fn hides_managed_loopbacks_from_app_streams() {
        let json = r#"
        [
          {
            "index": 11,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "100%"}},
            "properties": {
              "wavelinux.managed": "1",
              "wavelinux.role": "channel_to_mix"
            }
          },
          {
            "index": 12,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "100%"}},
            "properties": {
              "node.name": "output.loopback-123",
              "media.name": "loopback-123 output"
            }
          },
          {
            "index": 13,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "80%"}},
            "properties": {
              "application.name": "Firefox"
            }
          }
        ]
        "#;
        let streams = parse_sink_inputs_json(json);
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].display_name, "Firefox");
    }

    #[test]
    fn parses_managed_sink_input_routes() {
        let json = r#"
        [
          {
            "index": 73,
            "owner_module": 536870922,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "74%"}},
            "properties": {
              "wavelinux.role": "channel_to_mix",
              "wavelinux.channel_id": "music",
              "wavelinux.mix_id": "stream"
            }
          }
        ]
        "#;
        let inputs = parse_sink_input_routes_json(json);
        assert_eq!(inputs[0].id, "73");
        assert_eq!(inputs[0].module_id.as_deref(), Some("536870922"));
        assert_eq!(inputs[0].role.as_deref(), Some("channel_to_mix"));
        assert_eq!(inputs[0].channel_id.as_deref(), Some("music"));
        assert_eq!(inputs[0].mix_id.as_deref(), Some("stream"));
        assert_eq!(inputs[0].sink.as_deref(), Some("2"));
    }

    #[test]
    fn sink_input_routes_fall_back_to_module_arguments() {
        let modules_text = "\
102\tmodule-loopback\tsource=wavelinux_channel_music.monitor sink=wavelinux_mix_stream sink_input_properties=wavelinux.managed=1 wavelinux.role=channel_to_mix wavelinux.channel_id=music wavelinux.mix_id=stream\t\n";
        let json = r#"
        [
          {
            "index": 73,
            "owner_module": 102,
            "sink": 2,
            "mute": false,
            "volume": {"front-left": {"value_percent": "74%"}},
            "properties": {}
          }
        ]
        "#;
        let modules = parse_managed_modules_short(modules_text);
        let inputs =
            hydrate_sink_input_routes_from_modules(parse_sink_input_routes_json(json), &modules);

        assert_eq!(inputs[0].id, "73");
        assert_eq!(inputs[0].role.as_deref(), Some("channel_to_mix"));
        assert_eq!(inputs[0].channel_id.as_deref(), Some("music"));
        assert_eq!(inputs[0].mix_id.as_deref(), Some("stream"));
    }

    #[test]
    fn channel_bus_level_commands_target_sink_input() {
        let volume = plan_set_channel_bus_volume("73", 0.735);
        assert_eq!(volume.args, vec!["set-sink-input-volume", "73", "74%"]);

        let mute = plan_set_channel_bus_mute("73", true);
        assert_eq!(mute.args, vec!["set-sink-input-mute", "73", "1"]);
    }

    #[test]
    fn channel_bus_level_commands_can_target_source_output() {
        let volume = plan_set_channel_bus_source_output_volume("91", 0.735);
        assert_eq!(volume.args, vec!["set-source-output-volume", "91", "74%"]);

        let mute = plan_set_channel_bus_source_output_mute("91", true);
        assert_eq!(mute.args, vec!["set-source-output-mute", "91", "1"]);
    }

    #[test]
    fn parses_managed_source_output_routes() {
        let json = r#"
        [
          {
            "index": 91,
            "owner_module": 536870922,
            "properties": {
              "wavelinux.role": "channel_to_mix",
              "wavelinux.channel_id": "mic",
              "wavelinux.mix_id": "stream",
              "target.object": "wavelinux_mix_stream"
            }
          }
        ]
        "#;
        let outputs = parse_source_outputs_json(json);
        assert_eq!(outputs[0].id, "91");
        assert_eq!(outputs[0].module_id.as_deref(), Some("536870922"));
        assert_eq!(outputs[0].role.as_deref(), Some("channel_to_mix"));
        assert_eq!(outputs[0].channel_id.as_deref(), Some("mic"));
        assert_eq!(outputs[0].mix_id.as_deref(), Some("stream"));
        assert_eq!(
            outputs[0].target_object.as_deref(),
            Some("wavelinux_mix_stream")
        );
    }

    #[test]
    fn source_output_routes_fall_back_to_module_arguments() {
        let modules_text = "\
102\tmodule-loopback\tsource=wavelinux_channel_music.monitor sink=wavelinux_mix_stream source_output_properties=wavelinux.managed=1 wavelinux.role=channel_to_mix wavelinux.channel_id=music wavelinux.mix_id=stream\t\n";
        let json = r#"
        [
          {
            "index": 91,
            "owner_module": 102,
            "properties": {
              "target.object": "wavelinux_channel_music"
            }
          }
        ]
        "#;
        let modules = parse_managed_modules_short(modules_text);
        let outputs =
            hydrate_source_output_routes_from_modules(parse_source_outputs_json(json), &modules);

        assert_eq!(outputs[0].id, "91");
        assert_eq!(outputs[0].role.as_deref(), Some("channel_to_mix"));
        assert_eq!(outputs[0].channel_id.as_deref(), Some("music"));
        assert_eq!(outputs[0].mix_id.as_deref(), Some("stream"));
        assert_eq!(
            outputs[0].target_object.as_deref(),
            Some("wavelinux_channel_music")
        );
    }

    #[test]
    fn filter_chain_skips_bypassed_effects() {
        let mut config = MixerConfig::default();
        let mut active = EffectInstance::new("limiter");
        active.instance_id = "active".into();
        let mut bypassed = EffectInstance::new("gate");
        bypassed.instance_id = "bypassed".into();
        bypassed.bypassed = true;
        config
            .set_effect_chain("hardware_in", vec![active, bypassed])
            .unwrap();
        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());
        assert!(rendered.contains("context.spa-libs = {"));
        assert!(rendered.contains("libpipewire-module-protocol-native"));
        assert!(rendered.contains("active"));
        assert!(!rendered.contains("bypassed"));
        assert!(rendered.contains("target.object = \"wavelinux_channel_hardware_in.monitor\""));
        assert!(rendered.contains("node.name = \"wavelinux_fx_hardware_in_source\""));
    }

    #[test]
    fn filter_chain_wires_stereo_effects_in_order() {
        let mut config = MixerConfig::default();
        let mut deepfilter = EffectInstance::new("deepfilternet");
        deepfilter.instance_id = "deepfilter".into();
        let mut rnnoise = EffectInstance::new("rnnoise");
        rnnoise.instance_id = "rnnoise".into();
        let mut eq = EffectInstance::new("eq");
        eq.instance_id = "voice_eq".into();
        let mut limiter = EffectInstance::new("limiter");
        limiter.instance_id = "limiter".into();
        config
            .set_effect_chain("hardware_in", vec![deepfilter, rnnoise, eq, limiter])
            .unwrap();

        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());
        assert!(rendered.contains("links = ["));
        assert!(
            rendered.contains("output = \"deepfilter:Audio Out L\" input = \"rnnoise:Input (L)\"")
        );
        assert!(
            rendered.contains("output = \"deepfilter:Audio Out R\" input = \"rnnoise:Input (R)\"")
        );
        assert!(rendered.contains("output = \"rnnoise:Output (L)\" input = \"voice_eq:In\""));
        assert!(rendered.contains("output = \"voice_eq:Out\" input = \"limiter:Input 1\""));
        assert!(
            rendered.contains("inputs = [ \"deepfilter:Audio In L\" \"deepfilter:Audio In R\" ]")
        );
        assert!(rendered.contains("outputs = [ \"limiter:Output 1\" \"limiter:Output 2\" ]"));
    }

    #[test]
    fn detects_deepfilternet_from_ladspa_path() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("libdeep_filter_ladspa.so"), "").unwrap();
        let old_ladspa_path = std::env::var_os("LADSPA_PATH");
        std::env::set_var("LADSPA_PATH", root.path());

        let availability = probe_effect_availability(&EffectCatalog::default());
        let deepfilternet = availability
            .iter()
            .find(|effect| effect.effect_id == "deepfilternet")
            .unwrap();

        if let Some(old_ladspa_path) = old_ladspa_path {
            std::env::set_var("LADSPA_PATH", old_ladspa_path);
        } else {
            std::env::remove_var("LADSPA_PATH");
        }

        assert!(deepfilternet.available);
        assert!(deepfilternet.detail.contains("libdeep_filter_ladspa.so"));
    }

    #[test]
    fn parses_managed_modules_and_unload_plan() {
        let listed_modules = "\
200\tmodule-loopback\tsource=wavelinux_system.monitor sink=wavelinux_mix_monitor latency_msec=20 adjust_time=0\t\n\
102\tmodule-loopback\tsource=wavelinux_channel_mic.monitor sink=wavelinux_mix_stream source_output_properties=wavelinux.managed=1 wavelinux.role=channel_to_mix wavelinux.channel_id=mic wavelinux.mix_id=stream\t\n\
300\tmodule-loopback\tsource=alsa_input.real sink=alsa_output.real\t\n";
        let sinks = r#"
        [
          {
            "index": 1,
            "owner_module": 100,
            "name": "wavelinux_mix_stream",
            "properties": {"wavelinux.managed": "1", "wavelinux.role": "mix", "wavelinux.mix_id": "stream"}
          },
          {
            "index": 2,
            "owner_module": 4294967295,
            "name": "alsa_output.real",
            "properties": {}
          }
        ]
        "#;
        let sources = r#"
        [
          {
            "index": 3,
            "owner_module": 101,
            "name": "wavelinux_mix_stream_source",
            "properties": {"wavelinux.managed": "1", "wavelinux.role": "mix_source", "wavelinux.mix_id": "stream"}
          },
          {
            "index": 5,
            "owner_module": 103,
            "name": "output.wavelinux.fx.alsa_input.source",
            "properties": {}
          }
        ]
        "#;
        let sink_inputs = r#"[]"#;
        let source_outputs = r#"
        [
          {
            "index": 4,
            "owner_module": 102,
            "properties": {"wavelinux.managed": "1", "wavelinux.role": "channel_to_mix", "wavelinux.channel_id": "mic", "wavelinux.mix_id": "stream"}
          }
        ]
        "#;

        let modules =
            parse_managed_modules_json(listed_modules, sinks, sources, sink_inputs, source_outputs);
        assert_eq!(modules.len(), 5);
        assert!(modules.iter().any(|module| module.module_id == "100"));
        assert!(modules.iter().any(|module| module.module_id == "103"));
        assert!(modules.iter().any(|module| {
            module.module_id == "200"
                && module.node_name.as_deref() == Some("wavelinux_system.monitor")
                && module.source_name.as_deref() == Some("wavelinux_system.monitor")
                && module.sink_name.as_deref() == Some("wavelinux_mix_monitor")
        }));
        assert!(!modules.iter().any(|module| module.module_id == "300"));

        let commands = plan_unload_modules(&modules);
        assert_eq!(commands.len(), 5);
        assert_eq!(commands[0].args[0], "unload-module");
        assert_eq!(commands[0].args[1], "102");
    }

    #[test]
    fn parses_stale_wavelinux_audio_processes() {
        let processes = parse_stale_processes(
            "42 pipewire -c /home/dusky/.config/pipewire/wavelinux-chain-mic.conf\n\
             43 /home/dusky/.local/bin/wavelinux\n\
             44 /usr/bin/bash -lc pgrep -af pipewire\n\
             45 pipewire -c /tmp/regular.conf\n",
        );

        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].pid, "42");
        assert!(processes[0].command.contains("wavelinux-chain-mic.conf"));

        let commands = plan_kill_stale_processes(&processes);
        assert_eq!(commands[0].program, "kill");
        assert_eq!(commands[0].args, ["42"]);
    }
}
