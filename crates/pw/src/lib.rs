use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wavelinux_model::{
    AppStream, Channel, DeviceInfo, Diagnostic, DiagnosticSeverity, EffectAvailability,
    EffectCatalog, EffectInstance, Mix, MixerConfig, PluginHint, RuntimeGraph, SAMPLE_RATE_HZ,
};

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

    pub fn snapshot(&self) -> RuntimeGraph {
        self.snapshot_for_config(None)
    }

    pub fn snapshot_for_config(&self, config: Option<&MixerConfig>) -> RuntimeGraph {
        let inputs = self.list_sources().unwrap_or_default();
        let outputs = self.list_sinks().unwrap_or_default();
        let sink_names_by_index = outputs
            .iter()
            .filter_map(|sink| Some((sink.index.clone()?, sink.name.clone())))
            .collect();
        let available_sources = inputs.iter().map(|source| source.name.clone()).collect();
        let app_streams = self
            .list_sink_inputs_with_routes(config, &sink_names_by_index)
            .unwrap_or_default();
        let meters = config
            .map(|config| self.sample_config_meters(config, &available_sources))
            .unwrap_or_default();
        RuntimeGraph {
            inputs,
            outputs,
            app_streams,
            meters,
            effect_availability: probe_effect_availability(&EffectCatalog::default()),
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

    pub fn find_channel_bus_source_output(
        &self,
        channel_id: &str,
        mix_id: &str,
    ) -> Result<Option<String>, PwError> {
        let json = self.pactl_json(["list", "source-outputs"])?;
        Ok(parse_source_outputs_json(&json)
            .into_iter()
            .find(|output| {
                output.channel_id.as_deref() == Some(channel_id)
                    && output.mix_id.as_deref() == Some(mix_id)
            })
            .map(|output| output.id))
    }

    pub fn source_output_routes(&self) -> Result<Vec<SourceOutputRoute>, PwError> {
        let json = self.pactl_json(["list", "source-outputs"])?;
        Ok(parse_source_outputs_json(&json))
    }

    pub fn managed_modules(&self) -> Result<Vec<ManagedModule>, PwError> {
        let sinks = self.pactl_json(["list", "sinks"])?;
        let sources = self.pactl_json(["list", "sources"])?;
        let sink_inputs = self.pactl_json(["list", "sink-inputs"])?;
        let source_outputs = self.pactl_json(["list", "source-outputs"])?;
        Ok(parse_managed_modules_json(
            &sinks,
            &sources,
            &sink_inputs,
            &source_outputs,
        ))
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
        Ok(parse_sink_inputs_json_with_routes(
            &json,
            config,
            sink_names_by_index,
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

    fn sample_config_meters(
        &self,
        config: &MixerConfig,
        available_sources: &std::collections::BTreeSet<String>,
    ) -> Vec<wavelinux_model::LevelMeter> {
        if self.dry_run {
            return Vec::new();
        }

        let mut meters = Vec::new();
        for mix in &config.mixes {
            let source = format!("{}.monitor", mix.virtual_sink_name);
            if available_sources.contains(&source) {
                if let Some((left, right)) = sample_peak_from_source(&source) {
                    meters.push(wavelinux_model::LevelMeter {
                        node_id: mix.id.clone(),
                        peak_left: left,
                        peak_right: right,
                    });
                }
            }
        }
        for channel in &config.channels {
            let source = format!("{}.monitor", channel.virtual_sink_name);
            if available_sources.contains(&source) {
                if let Some((left, right)) = sample_peak_from_source(&source) {
                    meters.push(wavelinux_model::LevelMeter {
                        node_id: channel.id.clone(),
                        peak_left: left,
                        peak_right: right,
                    });
                }
            }
        }
        meters
    }
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
                    "sink_properties=device.description={} media.class=Audio/Sink wavelinux.managed=1 wavelinux.role=mix wavelinux.mix_id={}",
                    property_value(&format!("WaveLinux {}", mix.name)),
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
                    "source_properties=device.description={} media.class=Audio/Source/Virtual wavelinux.managed=1 wavelinux.role=mix_source wavelinux.mix_id={}",
                    property_value(&format!("WaveLinux {}", mix.name)),
                    mix.id
                ),
            ],
            format!("expose '{}' as virtual source", mix.name),
        ),
    ]
}

pub fn plan_ensure_channel(channel: &Channel) -> Vec<CommandSpec> {
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
                "sink_properties=device.description={} media.class=Audio/Sink wavelinux.managed=1 wavelinux.role=channel wavelinux.channel_id={}",
                property_value(&format!("WaveLinux Channel {}", channel.name)),
                channel.id
            ),
        ],
        format!("create channel sink '{}'", channel.name),
    )]
}

pub fn plan_route_channel_to_mix(channel: &Channel, mix: &Mix) -> Vec<CommandSpec> {
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={}.monitor", channel.virtual_sink_name),
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

pub fn plan_set_channel_bus_volume(source_output_id: &str, volume: f32) -> CommandSpec {
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

pub fn plan_set_channel_bus_mute(source_output_id: &str, muted: bool) -> CommandSpec {
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

pub fn render_filter_chain(channel: &Channel, catalog: &EffectCatalog) -> String {
    let mut rendered = String::new();
    rendered.push_str("context.modules = [\n");
    rendered.push_str("  { name = libpipewire-module-filter-chain\n");
    rendered.push_str("    args = {\n");
    rendered.push_str("      node.description = \"WaveLinux FX ");
    rendered.push_str(&escape_pw(&channel.name));
    rendered.push_str("\"\n");
    rendered.push_str("      media.name = \"WaveLinux FX ");
    rendered.push_str(&escape_pw(&channel.name));
    rendered.push_str("\"\n");
    rendered.push_str("      filter.graph = {\n");
    rendered.push_str("        nodes = [\n");

    for effect in channel.effects.iter().filter(|effect| !effect.bypassed) {
        let definition = catalog
            .effects
            .iter()
            .find(|item| item.id == effect.effect_id);
        rendered.push_str(&render_effect_node(effect, definition));
    }

    rendered.push_str("        ]\n");
    rendered.push_str("      }\n");
    rendered.push_str("      capture.props = { node.name = \"");
    rendered.push_str(&channel.virtual_sink_name);
    rendered.push_str(".monitor\" }\n");
    rendered.push_str("      playback.props = { node.name = \"");
    rendered.push_str(&channel.virtual_sink_name);
    rendered.push_str(".fx\" }\n");
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
pub struct ManagedModule {
    pub module_id: String,
    pub role: Option<String>,
    pub channel_id: Option<String>,
    pub mix_id: Option<String>,
    pub node_name: Option<String>,
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

impl ToString for JsonNumberOrString {
    fn to_string(&self) -> String {
        match self {
            JsonNumberOrString::Number(value) => value.to_string(),
            JsonNumberOrString::String(value) => value.clone(),
            JsonNumberOrString::Missing => String::new(),
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
            let is_virtual = id.contains("wavelinux_")
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
                    format!("{fallback_prefix} {}", device.index.to_string())
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
    let inputs: Vec<PactlSinkInput> = serde_json::from_str(json).unwrap_or_default();
    inputs
        .into_iter()
        .filter(|input| !is_managed_or_loopback_sink_input(&input.properties))
        .map(|input| {
            let app_id = property_string(&input.properties, "application.id")
                .or_else(|| property_string(&input.properties, "application.process.binary"));
            let process_name = property_string(&input.properties, "application.process.binary")
                .or_else(|| property_string(&input.properties, "application.name"));
            let display_name = property_string(&input.properties, "application.name")
                .or_else(|| app_id.clone())
                .unwrap_or_else(|| format!("Stream {}", input.index.to_string()));
            let media_name = property_string(&input.properties, "media.name");
            let sink_name = sink_names_by_index.get(&input.sink.to_string());
            let routed_channel_id = property_string(&input.properties, "wavelinux.channel_id")
                .or_else(|| {
                    let sink_name = sink_name?;
                    config?
                        .channels
                        .iter()
                        .find(|channel| channel.virtual_sink_name == *sink_name)
                        .map(|channel| channel.id.clone())
                });
            AppStream {
                id: input.index.to_string(),
                app_id,
                process_name,
                display_name,
                media_name,
                routed_channel_id,
                volume: parse_first_volume(&input.volume).unwrap_or(1.0),
                muted: input.mute,
            }
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
        Some("channel_to_mix") | Some("mix_monitor") => 0,
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
    sinks_json: &str,
    sources_json: &str,
    sink_inputs_json: &str,
    source_outputs_json: &str,
) -> Vec<ManagedModule> {
    let mut modules = Vec::new();

    let sinks: Vec<PactlDevice> = serde_json::from_str(sinks_json).unwrap_or_default();
    modules.extend(sinks.into_iter().filter_map(|device| {
        managed_module_from_parts(
            device.owner_module.module_id(),
            Some(device.name),
            &device.properties,
        )
    }));

    let sources: Vec<PactlDevice> = serde_json::from_str(sources_json).unwrap_or_default();
    modules.extend(sources.into_iter().filter_map(|device| {
        managed_module_from_parts(
            device.owner_module.module_id(),
            Some(device.name),
            &device.properties,
        )
    }));

    let sink_inputs: Vec<PactlSinkInput> =
        serde_json::from_str(sink_inputs_json).unwrap_or_default();
    modules.extend(sink_inputs.into_iter().filter_map(|input| {
        managed_module_from_parts(input.owner_module.module_id(), None, &input.properties)
    }));

    let source_outputs: Vec<PactlSourceOutput> =
        serde_json::from_str(source_outputs_json).unwrap_or_default();
    modules.extend(source_outputs.into_iter().filter_map(|output| {
        managed_module_from_parts(output.owner_module.module_id(), None, &output.properties)
    }));

    let mut seen = std::collections::BTreeSet::new();
    modules
        .into_iter()
        .filter(|module| seen.insert(module.module_id.clone()))
        .collect()
}

fn managed_module_from_parts(
    module_id: Option<String>,
    node_name: Option<String>,
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
        || node_name
            .as_deref()
            .is_some_and(|value| value.starts_with("wavelinux_"));

    if !managed {
        return None;
    }

    Some(ManagedModule {
        module_id: module_id?,
        role,
        channel_id,
        mix_id,
        node_name,
    })
}

fn render_effect_node(
    effect: &EffectInstance,
    definition: Option<&wavelinux_model::EffectDefinition>,
) -> String {
    let mut rendered = String::new();
    rendered.push_str("          { type = builtin label = \"");
    rendered.push_str(&effect.effect_id);
    rendered.push_str("\" name = \"");
    rendered.push_str(&effect.instance_id);
    rendered.push_str("\"");
    if let Some(definition) = definition {
        rendered.push_str(" config = {");
        for param in &definition.params {
            let value = effect
                .params
                .get(&param.id)
                .copied()
                .unwrap_or(param.default)
                .clamp(param.min, param.max);
            rendered.push(' ');
            rendered.push_str(&param.id);
            rendered.push_str(" = ");
            rendered.push_str(&format!("{value:.3}"));
        }
        rendered.push_str(" }");
    }
    rendered.push_str(" }\n");
    rendered
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

fn sample_peak_from_source(source_name: &str) -> Option<(f32, f32)> {
    if !command_exists("pw-record") {
        return None;
    }

    let output = Command::new("pw-record")
        .args([
            "--target",
            source_name,
            "--rate",
            "48000",
            "--channels",
            "2",
            "--format",
            "f32",
            "--raw",
            "--sample-count",
            "960",
            "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;

    if output.stdout.len() < 8 {
        return None;
    }

    let mut peak_left = 0.0_f32;
    let mut peak_right = 0.0_f32;
    for frame in output.stdout.chunks_exact(8) {
        let left = f32::from_le_bytes(frame[0..4].try_into().ok()?);
        let right = f32::from_le_bytes(frame[4..8].try_into().ok()?);
        if left.is_finite() {
            peak_left = peak_left.max(left.abs());
        }
        if right.is_finite() {
            peak_right = peak_right.max(right.abs());
        }
    }

    Some((peak_left.clamp(0.0, 1.0), peak_right.clamp(0.0, 1.0)))
}

fn property_string(map: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
    use wavelinux_model::{ChannelKind, MixerConfig};

    #[test]
    fn planned_graph_creates_mixes_channels_and_routes() {
        let config = MixerConfig::default();
        let plan = plan_ensure_graph(&config);
        assert!(plan
            .commands
            .iter()
            .any(|command| command.description.contains("create virtual mix sink")));
        assert!(plan
            .commands
            .iter()
            .any(|command| command.description.contains("route 'Mic' to 'Monitor'")));
        assert!(plan.managed_nodes.contains(&"wavelinux_mix_monitor".into()));
    }

    #[test]
    fn move_stream_targets_channel_sink() {
        let channel = Channel::new_fixed("discord", "Discord", ChannelKind::Application);
        let spec = plan_move_app_stream("42", &channel);
        assert_eq!(spec.program, "pactl");
        assert_eq!(spec.args[2], "wavelinux_channel_discord");
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
          }
        ]
        "#;
        let devices = parse_devices_json(json, "Sink");
        assert_eq!(devices.len(), 2);
        assert!(devices[1].is_virtual);
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
              "media.name": "AudioStream"
            }
          }
        ]
        "#;
        let streams = parse_sink_inputs_json(json);
        assert_eq!(streams[0].id, "72");
        assert_eq!(streams[0].display_name, "Firefox");
        assert!((streams[0].volume - 0.66).abs() < 0.001);
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
    fn filter_chain_skips_bypassed_effects() {
        let mut config = MixerConfig::default();
        let mut active = EffectInstance::new("limiter");
        active.instance_id = "active".into();
        let mut bypassed = EffectInstance::new("gate");
        bypassed.instance_id = "bypassed".into();
        bypassed.bypassed = true;
        config
            .set_effect_chain("mic", vec![active, bypassed])
            .unwrap();
        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());
        assert!(rendered.contains("active"));
        assert!(!rendered.contains("bypassed"));
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

        let modules = parse_managed_modules_json(sinks, sources, sink_inputs, source_outputs);
        assert_eq!(modules.len(), 3);
        assert!(modules.iter().any(|module| module.module_id == "100"));

        let commands = plan_unload_modules(&modules);
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].args[0], "unload-module");
        assert_eq!(commands[0].args[1], "102");
    }
}
