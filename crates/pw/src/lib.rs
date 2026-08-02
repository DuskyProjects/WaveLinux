use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wavelinux_model::{
    app_display_name, graph_prefix, graph_property_prefix, safe_node_id, AppMatcher, AppStream,
    Channel, ChannelInputMode, DeviceBus, DeviceInfo, DevicePortInfo, Diagnostic,
    DiagnosticSeverity, EffectAvailability, EffectCatalog, EffectInstance, Mix, MixerConfig,
    MixerSettings, OptimizationMode, PluginHint, RuntimeGraph, SAMPLE_RATE_HZ,
};

mod registry;
pub use registry::{
    NativeStreamRoute, PipeWireRegistryCache, RegistryBatch, RegistryEventKind, StreamRouteBackend,
};

pub const INPUT_ROUTE_REVISION: &str = "5";
pub const EFFECT_ROUTE_REVISION: &str = "4";
pub const EFFECT_ADAPTIVE_BRIDGE_ROUTE_REVISION: &str = "2";
pub const EFFECT_CONFIG_REVISION: &str = "3";
pub const CHANNEL_MIX_ROUTE_REVISION: &str = "4";
pub const MIX_MONITOR_ROUTE_REVISION: &str = "3";
pub const CHANNEL_CONFIG_REVISION: &str = "2";
// Fallback latencies for direct route helpers; profiles drive normal graph plans.
pub const STABLE_LOOPBACK_LATENCY_MSEC: u16 = 80;
pub const LOW_LATENCY_LOOPBACK_MSEC: u16 = 60;
pub const BLUETOOTH_MONITOR_LOOPBACK_MSEC: u16 = 240;
pub const EFFECT_ADAPTIVE_BRIDGE_TRANSPORT_MSEC: u16 = 28;
pub const METERS_ENV: &str = "WAVELINUX_ENABLE_METERS";
pub const METERS_DISABLE_ENV: &str = "WAVELINUX_DISABLE_METERS";
pub const PW_RECORD_METERS_ENV: &str = "WAVELINUX_ENABLE_PW_RECORD_METERS";
pub const PW_RECORD_METERS_DISABLE_ENV: &str = "WAVELINUX_DISABLE_PW_RECORD_METERS";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
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

fn graph_prop(name: &str) -> String {
    format!("{}.{}", graph_property_prefix(), name)
}

fn graph_prop_assignment(name: &str, value: impl AsRef<str>) -> String {
    format!("{}={}", graph_prop(name), property_value(value.as_ref()))
}

fn graph_property_string(
    properties: &BTreeMap<String, serde_json::Value>,
    name: &str,
) -> Option<String> {
    property_string(properties, &graph_prop(name))
}

fn graph_property_value_from_arg<'a>(argument: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{}=", graph_prop(name));
    property_value_from_arg(argument, &key)
}

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
    #[error("command timed out after {timeout_ms}ms: {program} {args:?}")]
    CommandTimedOut {
        program: String,
        args: Vec<String>,
        timeout_ms: u128,
    },
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeterTarget {
    pub node_id: String,
    pub source_name: String,
    pub gain: f32,
    pub muted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotCommandTiming {
    pub label: String,
    pub elapsed_ms: u128,
    pub succeeded: bool,
}

type TimedPactlResult = (Result<String, PwError>, SnapshotCommandTiming);

fn failed_snapshot_worker(label: &str) -> TimedPactlResult {
    (
        Err(PwError::Io(format!(
            "snapshot command worker panicked: {label}"
        ))),
        SnapshotCommandTiming {
            label: label.into(),
            elapsed_ms: 0,
            succeeded: false,
        },
    )
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

        let output = command_output_with_timeout(&spec.program, &spec.args, COMMAND_TIMEOUT)?;

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
        self.snapshot_for_config_with_effect_availability_timed(config, effect_availability)
            .0
    }

    pub fn snapshot_for_config_with_effect_availability_timed(
        &self,
        config: Option<&MixerConfig>,
        effect_availability: Vec<EffectAvailability>,
    ) -> (RuntimeGraph, Vec<SnapshotCommandTiming>) {
        let mut timings = Vec::new();
        let inputs = self.list_sources_timed(&mut timings).unwrap_or_default();
        let outputs = self.list_sinks_timed(&mut timings).unwrap_or_default();
        let sink_names_by_index = outputs
            .iter()
            .filter_map(|sink| Some((sink.index.clone()?, sink.name.clone())))
            .collect();
        let app_streams = self
            .list_sink_inputs_with_routes_timed(config, &sink_names_by_index, &mut timings)
            .unwrap_or_default();
        (
            RuntimeGraph {
                inputs,
                outputs,
                app_streams,
                meters: Vec::new(),
                auto_devices: Vec::new(),
                effect_availability,
            },
            timings,
        )
    }

    /// Capture the graph and all managed route state from one coherent set of
    /// Pulse queries. Callers that need both views should use this instead of
    /// composing `snapshot_*`, `managed_modules`, and the route helpers: those
    /// helpers otherwise fetch the same sources, sinks, and streams repeatedly.
    pub fn audio_state_snapshot_with_effect_availability_timed(
        &self,
        config: Option<&MixerConfig>,
        effect_availability: Vec<EffectAvailability>,
    ) -> (AudioStateSnapshot, Vec<SnapshotCommandTiming>) {
        // These are independent, read-only views of one Pulse server state. Run
        // them together so refresh latency tracks the slowest query instead of
        // the sum of host-command round trips.
        let (
            (sources_result, sources_timing),
            (sinks_result, sinks_timing),
            (sink_inputs_result, sink_inputs_timing),
            (source_outputs_result, source_outputs_timing),
            (clients_result, clients_timing),
            (modules_result, modules_timing),
            (cards_result, cards_timing),
            (default_source_result, default_source_timing),
            (default_sink_result, default_sink_timing),
        ) = thread::scope(|scope| {
            let sources = scope.spawn(|| {
                self.pactl_json_with_timing(["list", "sources"], "pactl --format=json list sources")
            });
            let sinks = scope.spawn(|| {
                self.pactl_json_with_timing(["list", "sinks"], "pactl --format=json list sinks")
            });
            let sink_inputs = scope.spawn(|| {
                self.pactl_json_with_timing(
                    ["list", "sink-inputs"],
                    "pactl --format=json list sink-inputs",
                )
            });
            let source_outputs = scope.spawn(|| {
                self.pactl_json_with_timing(
                    ["list", "source-outputs"],
                    "pactl --format=json list source-outputs",
                )
            });
            let clients = scope.spawn(|| {
                self.pactl_json_with_timing(["list", "clients"], "pactl --format=json list clients")
            });
            let modules = scope.spawn(|| {
                self.pactl_text_with_timing(
                    ["list", "modules", "short"],
                    "pactl list modules short",
                )
            });
            let cards = scope.spawn(|| {
                self.pactl_json_with_timing(["list", "cards"], "pactl --format=json list cards")
            });
            let default_source = scope.spawn(|| {
                self.pactl_text_with_timing(["get-default-source"], "pactl get-default-source")
            });
            let default_sink = scope.spawn(|| {
                self.pactl_text_with_timing(["get-default-sink"], "pactl get-default-sink")
            });

            (
                sources
                    .join()
                    .unwrap_or_else(|_| failed_snapshot_worker("pactl --format=json list sources")),
                sinks
                    .join()
                    .unwrap_or_else(|_| failed_snapshot_worker("pactl --format=json list sinks")),
                sink_inputs.join().unwrap_or_else(|_| {
                    failed_snapshot_worker("pactl --format=json list sink-inputs")
                }),
                source_outputs.join().unwrap_or_else(|_| {
                    failed_snapshot_worker("pactl --format=json list source-outputs")
                }),
                clients
                    .join()
                    .unwrap_or_else(|_| failed_snapshot_worker("pactl --format=json list clients")),
                modules
                    .join()
                    .unwrap_or_else(|_| failed_snapshot_worker("pactl list modules short")),
                cards
                    .join()
                    .unwrap_or_else(|_| failed_snapshot_worker("pactl --format=json list cards")),
                default_source
                    .join()
                    .unwrap_or_else(|_| failed_snapshot_worker("pactl get-default-source")),
                default_sink
                    .join()
                    .unwrap_or_else(|_| failed_snapshot_worker("pactl get-default-sink")),
            )
        });
        let timings = vec![
            sources_timing,
            sinks_timing,
            sink_inputs_timing,
            source_outputs_timing,
            clients_timing,
            modules_timing,
            cards_timing,
            default_source_timing,
            default_sink_timing,
        ];
        let sources = sources_result.unwrap_or_else(|_| "[]".into());
        let sinks = sinks_result.unwrap_or_else(|_| "[]".into());
        let sink_inputs = sink_inputs_result.unwrap_or_else(|_| "[]".into());
        let source_outputs = source_outputs_result.unwrap_or_else(|_| "[]".into());
        let clients = clients_result.unwrap_or_else(|_| "[]".into());
        let modules = modules_result.unwrap_or_default();
        let cards = cards_result.unwrap_or_else(|_| "[]".into());
        let default_source = default_source_result.ok();
        let default_sink = default_sink_result.ok();

        (
            parse_audio_state_snapshot(
                AudioStateSnapshotJson {
                    sources: &sources,
                    sinks: &sinks,
                    sink_inputs: &sink_inputs,
                    source_outputs: &source_outputs,
                    clients: &clients,
                    modules: &modules,
                    cards: &cards,
                    default_source: default_source.as_deref(),
                    default_sink: default_sink.as_deref(),
                },
                config,
                effect_availability,
            ),
            timings,
        )
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
        let pactl_info =
            command_output_with_timeout("pactl", &["info".to_string()], COMMAND_TIMEOUT);
        diagnostics.push(match pactl_info {
            Ok(output) if output.status.success() => Diagnostic {
                code: "host_audio.pactl_info".into(),
                severity: DiagnosticSeverity::Info,
                message: "pactl can connect to the host audio server".into(),
                action: None,
            },
            Ok(output) => Diagnostic {
                code: "host_audio.pactl_info".into(),
                severity: DiagnosticSeverity::Error,
                message: format!(
                    "pactl cannot connect to the host audio server: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                action: Some(
                    "Start the user PipeWire stack: systemctl --user start pipewire pipewire-pulse wireplumber".into(),
                ),
            },
            Err(err) => Diagnostic {
                code: "host_audio.pactl_info".into(),
                severity: DiagnosticSeverity::Error,
                message: format!("pactl audio server probe failed: {err}"),
                action: Some(
                    "Install and start PipeWire, WirePlumber, and pipewire-pulse for the current user session".into(),
                ),
            },
        });
        diagnostics
    }

    pub fn default_sink(&self) -> Result<Option<String>, PwError> {
        self.default_device(["get-default-sink"])
    }

    pub fn default_source(&self) -> Result<Option<String>, PwError> {
        self.default_device(["get-default-source"])
    }

    pub fn active_playback_sink(&self) -> Result<Option<String>, PwError> {
        let outputs = self.list_sinks()?;
        let sink_names_by_index = outputs
            .iter()
            .filter_map(|sink| Some((sink.index.clone()?, sink.name.clone())))
            .collect::<BTreeMap<_, _>>();
        let sink_inputs = self.pactl_json(["list", "sink-inputs"])?;
        Ok(active_playback_sink_from_sink_inputs_json(
            &sink_inputs,
            &sink_names_by_index,
        ))
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

    pub fn find_channel_bus_route_ids(
        &self,
        channel_id: &str,
        mix_id: &str,
    ) -> Result<ChannelBusRouteIds, PwError> {
        let sink_inputs_json = self.pactl_json(["list", "sink-inputs"])?;
        let source_outputs_json = self.pactl_json(["list", "source-outputs"])?;
        let sink_inputs = parse_sink_input_routes_json(&sink_inputs_json);
        let source_outputs = parse_source_outputs_json(&source_outputs_json);
        let direct =
            channel_bus_route_ids_from_routes(channel_id, mix_id, &sink_inputs, &source_outputs);
        if !direct.is_empty() {
            return Ok(direct);
        }

        let modules = self.pactl_text(["list", "modules", "short"])?;
        let modules = parse_managed_modules_short(&modules);
        Ok(channel_bus_route_ids_from_routes(
            channel_id,
            mix_id,
            &hydrate_sink_input_routes_from_modules(sink_inputs, &modules),
            &hydrate_source_output_routes_from_modules(source_outputs, &modules),
        ))
    }

    pub fn sink_input_routes(&self) -> Result<Vec<SinkInputRoute>, PwError> {
        let json = self.pactl_json(["list", "sink-inputs"])?;
        let sinks_json = self.pactl_json(["list", "sinks"])?;
        let modules = self.pactl_text(["list", "modules", "short"])?;
        let modules = parse_managed_modules_short(&modules);
        let sink_names = parse_device_names_by_index_json(&sinks_json);
        let routes =
            hydrate_sink_input_routes_from_sinks(parse_sink_input_routes_json(&json), &sink_names);
        Ok(hydrate_sink_input_routes_from_modules(routes, &modules))
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
        let sources_json = self.pactl_json(["list", "sources"])?;
        let modules = self.pactl_text(["list", "modules", "short"])?;
        let modules = parse_managed_modules_short(&modules);
        let source_names = parse_device_names_by_index_json(&sources_json);
        let routes = hydrate_source_output_routes_from_sources(
            parse_source_outputs_json(&json),
            &source_names,
        );
        Ok(hydrate_source_output_routes_from_modules(routes, &modules))
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

    pub fn route_snapshot(&self) -> Result<RouteSnapshot, PwError> {
        let modules = self.pactl_text(["list", "modules", "short"])?;
        let sinks = self.pactl_json(["list", "sinks"])?;
        let sources = self.pactl_json(["list", "sources"])?;
        let sink_inputs = self.pactl_json(["list", "sink-inputs"])?;
        let source_outputs = self.pactl_json(["list", "source-outputs"])?;

        let managed_modules =
            parse_managed_modules_json(&modules, &sinks, &sources, &sink_inputs, &source_outputs);
        let sink_names = parse_device_names_by_index_json(&sinks);
        let source_names = parse_device_names_by_index_json(&sources);
        let sink_input_routes = hydrate_sink_input_routes_from_modules(
            hydrate_sink_input_routes_from_sinks(
                parse_sink_input_routes_json(&sink_inputs),
                &sink_names,
            ),
            &managed_modules,
        );
        let source_output_routes = hydrate_source_output_routes_from_modules(
            hydrate_source_output_routes_from_sources(
                parse_source_outputs_json(&source_outputs),
                &source_names,
            ),
            &managed_modules,
        );

        Ok(RouteSnapshot {
            managed_modules,
            sink_input_routes,
            source_output_routes,
        })
    }

    pub fn bluetooth_audio_cards(&self) -> Result<Vec<BluetoothAudioCard>, PwError> {
        let json = self.pactl_json(["list", "cards"])?;
        Ok(parse_bluetooth_audio_cards_json(&json))
    }

    pub fn stale_processes(&self) -> Result<Vec<StaleProcess>, PwError> {
        if self.dry_run {
            return Ok(Vec::new());
        }

        let output = command_output_with_timeout(
            "pgrep",
            &["-af".to_string(), "pipewire".to_string()],
            COMMAND_TIMEOUT,
        )?;

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

    fn list_sources_timed(
        &self,
        timings: &mut Vec<SnapshotCommandTiming>,
    ) -> Result<Vec<DeviceInfo>, PwError> {
        let json = self.pactl_json_timed(
            ["list", "sources"],
            "pactl --format=json list sources",
            timings,
        )?;
        Ok(parse_devices_json(&json, "Source"))
    }

    pub fn list_inputs(&self) -> Result<Vec<DeviceInfo>, PwError> {
        self.list_sources()
    }

    fn list_sinks(&self) -> Result<Vec<DeviceInfo>, PwError> {
        let json = self.pactl_json(["list", "sinks"])?;
        Ok(parse_devices_json(&json, "Sink"))
    }

    fn list_sinks_timed(
        &self,
        timings: &mut Vec<SnapshotCommandTiming>,
    ) -> Result<Vec<DeviceInfo>, PwError> {
        let json =
            self.pactl_json_timed(["list", "sinks"], "pactl --format=json list sinks", timings)?;
        Ok(parse_devices_json(&json, "Sink"))
    }

    pub fn list_outputs(&self) -> Result<Vec<DeviceInfo>, PwError> {
        self.list_sinks()
    }

    /// Read only application playback streams using an already-cached sink list.
    ///
    /// This is the hot path used after a Pulse `sink-input` event. Keeping device
    /// discovery out of this query avoids re-running card/profile policy whenever
    /// a dormant browser or game begins playback.
    pub fn list_app_streams(
        &self,
        config: Option<&MixerConfig>,
        outputs: &[DeviceInfo],
    ) -> Result<Vec<AppStream>, PwError> {
        let sink_names_by_index = outputs
            .iter()
            .filter_map(|sink| Some((sink.index.clone()?, sink.name.clone())))
            .collect::<BTreeMap<_, _>>();
        let json = self.pactl_json(["list", "sink-inputs"])?;
        let streams = parse_sink_inputs_json_with_client_properties(
            &json,
            config,
            &sink_names_by_index,
            &BTreeMap::new(),
        );
        if !streams.iter().any(app_stream_needs_client_properties) {
            return Ok(streams);
        }

        let clients_json = self.pactl_json(["list", "clients"]).unwrap_or_default();
        let client_properties = parse_client_properties_json(&clients_json);
        Ok(parse_sink_inputs_json_with_client_properties(
            &json,
            config,
            &sink_names_by_index,
            &client_properties,
        ))
    }

    fn list_sink_inputs_with_routes_timed(
        &self,
        config: Option<&MixerConfig>,
        sink_names_by_index: &BTreeMap<String, String>,
        timings: &mut Vec<SnapshotCommandTiming>,
    ) -> Result<Vec<AppStream>, PwError> {
        let json = self.pactl_json_timed(
            ["list", "sink-inputs"],
            "pactl --format=json list sink-inputs",
            timings,
        )?;
        let clients_json = self
            .pactl_json_timed(
                ["list", "clients"],
                "pactl --format=json list clients",
                timings,
            )
            .unwrap_or_default();
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
        let args = std::iter::once("--format=json".to_string())
            .chain(
                args.into_iter()
                    .map(|arg| arg.as_ref().to_string_lossy().to_string()),
            )
            .collect::<Vec<_>>();
        let output = command_output_with_timeout("pactl", &args, COMMAND_TIMEOUT)?;

        if !output.status.success() {
            return Err(PwError::CommandFailed {
                program: "pactl".into(),
                args,
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn pactl_json_timed<I, S>(
        &self,
        args: I,
        label: &str,
        timings: &mut Vec<SnapshotCommandTiming>,
    ) -> Result<String, PwError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let (result, timing) = self.pactl_json_with_timing(args, label);
        timings.push(timing);
        result
    }

    fn pactl_json_with_timing<I, S>(&self, args: I, label: &str) -> TimedPactlResult
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let started = Instant::now();
        let result = self.pactl_json(args);
        let timing = SnapshotCommandTiming {
            label: label.into(),
            elapsed_ms: started.elapsed().as_millis(),
            succeeded: result.is_ok(),
        };
        (result, timing)
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
        let output = command_output_with_timeout("pactl", &args, COMMAND_TIMEOUT)?;

        if !output.status.success() {
            return Err(PwError::CommandFailed {
                program: "pactl".into(),
                args,
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    fn pactl_text_with_timing<I, S>(&self, args: I, label: &str) -> TimedPactlResult
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let started = Instant::now();
        let result = self.pactl_text(args);
        let timing = SnapshotCommandTiming {
            label: label.into(),
            elapsed_ms: started.elapsed().as_millis(),
            succeeded: result.is_ok(),
        };
        (result, timing)
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

fn app_stream_needs_client_properties(stream: &AppStream) -> bool {
    stream.app_id.is_none()
        && stream.binary.is_none()
        && stream.process_name.is_none()
        && stream.window_class.is_none()
}

pub fn meter_targets_for_config(
    config: &MixerConfig,
    available_sources: &BTreeSet<String>,
) -> Vec<MeterTarget> {
    meter_targets_for_config_inner(config, available_sources, &BTreeMap::new())
}

pub fn meter_targets_for_config_with_devices(
    config: &MixerConfig,
    available_sources: &[DeviceInfo],
) -> Vec<MeterTarget> {
    let available_source_names = available_sources
        .iter()
        .map(|source| source.name.clone())
        .collect::<BTreeSet<_>>();
    let effect_sources = effect_meter_sources_by_channel(available_sources);
    meter_targets_for_config_inner(config, &available_source_names, &effect_sources)
}

fn meter_targets_for_config_inner(
    config: &MixerConfig,
    available_sources: &BTreeSet<String>,
    effect_sources: &BTreeMap<String, String>,
) -> Vec<MeterTarget> {
    let mut targets = Vec::new();
    let mixes_by_id = config
        .mixes
        .iter()
        .map(|mix| (mix.id.as_str(), mix))
        .collect::<BTreeMap<_, _>>();
    for mix in &config.mixes {
        let source_name = if available_sources.contains(&mix.virtual_source_name) {
            mix.virtual_source_name.clone()
        } else {
            format!("{}.monitor", mix.virtual_sink_name)
        };
        if available_sources.contains(&source_name) {
            targets.push(MeterTarget {
                node_id: mix.id.clone(),
                source_name,
                gain: mix.volume,
                muted: mix.muted,
            });
        }
    }
    for channel in &config.channels {
        let raw_source_name = format!("{}.monitor", channel.virtual_sink_name);
        let Some(channel_source_name) = channel_input_meter_source_name(
            channel,
            available_sources,
            effect_sources,
            &raw_source_name,
        ) else {
            continue;
        };
        targets.push(MeterTarget {
            node_id: channel.id.clone(),
            source_name: channel_source_name,
            gain: 1.0,
            muted: false,
        });

        let Some(bus_source_name) = channel_bus_meter_source_name(
            channel,
            available_sources,
            effect_sources,
            &raw_source_name,
        ) else {
            continue;
        };
        for (mix_id, bus) in &channel.mix_buses {
            if !bus.enabled {
                continue;
            }
            let Some(mix) = mixes_by_id.get(mix_id.as_str()) else {
                continue;
            };
            targets.push(MeterTarget {
                node_id: channel_bus_meter_id(&channel.id, mix_id),
                source_name: bus_source_name.clone(),
                gain: bus.volume * mix.volume,
                muted: bus.muted || mix.muted,
            });
        }
    }
    targets
}

fn effect_meter_sources_by_channel(available_sources: &[DeviceInfo]) -> BTreeMap<String, String> {
    available_sources
        .iter()
        .filter_map(|source| {
            let role = source.pipewire_properties.get(&graph_prop("role"))?;
            if role != "effect_output" {
                return None;
            }
            let channel_id = source.pipewire_properties.get(&graph_prop("channel_id"))?;
            (!channel_id.trim().is_empty() && !source.name.trim().is_empty())
                .then(|| (channel_id.clone(), source.name.clone()))
        })
        .collect()
}

fn channel_input_meter_source_name(
    channel: &Channel,
    available_sources: &BTreeSet<String>,
    effect_sources: &BTreeMap<String, String>,
    raw_source_name: &str,
) -> Option<String> {
    if channel.kind.uses_hardware_slot() {
        if channel_has_active_effects(channel) {
            let processed = effect_chain_source_name(channel);
            if available_sources.contains(&processed) {
                return Some(processed);
            }
            if let Some(source) = effect_sources
                .get(&channel.id)
                .filter(|source| available_sources.contains(*source))
            {
                return Some(source.clone());
            }
        }

        if let Some(source) = channel
            .source_device
            .as_deref()
            .filter(|source| available_sources.contains(*source))
        {
            return Some(source.to_string());
        }

        return available_sources
            .contains(raw_source_name)
            .then(|| raw_source_name.to_string());
    }

    let processed = channel_mix_source_name(channel);
    if available_sources.contains(&processed) {
        return Some(processed);
    }
    if channel_has_active_effects(channel) {
        if let Some(source) = effect_sources
            .get(&channel.id)
            .filter(|source| available_sources.contains(*source))
        {
            return Some(source.clone());
        }
    }

    if available_sources.contains(raw_source_name) {
        return Some(raw_source_name.to_string());
    }

    None
}

fn channel_bus_meter_source_name(
    channel: &Channel,
    available_sources: &BTreeSet<String>,
    effect_sources: &BTreeMap<String, String>,
    raw_source_name: &str,
) -> Option<String> {
    if channel.kind.uses_hardware_slot() {
        return channel_input_meter_source_name(
            channel,
            available_sources,
            effect_sources,
            raw_source_name,
        );
    }

    let preferred = channel_mix_source_name(channel);
    if available_sources.contains(&preferred) {
        return Some(preferred);
    }
    if channel_has_active_effects(channel) {
        if let Some(source) = effect_sources
            .get(&channel.id)
            .filter(|source| available_sources.contains(*source))
        {
            return Some(source.clone());
        }
    }

    available_sources
        .contains(raw_source_name)
        .then(|| raw_source_name.to_string())
}

pub fn channel_bus_meter_id(channel_id: &str, mix_id: &str) -> String {
    format!("channel:{channel_id}:mix:{mix_id}")
}

pub fn meter_sampling_enabled() -> bool {
    meter_sampling_enabled_from_env(
        std::env::var(METERS_ENV)
            .ok()
            .or_else(|| std::env::var(PW_RECORD_METERS_ENV).ok())
            .as_deref(),
        std::env::var(METERS_DISABLE_ENV)
            .ok()
            .or_else(|| std::env::var(PW_RECORD_METERS_DISABLE_ENV).ok())
            .as_deref(),
        command_exists("pipewire"),
    )
}

fn meter_sampling_enabled_from_env(
    enable_value: Option<&str>,
    disable_value: Option<&str>,
    meter_backend_available: bool,
) -> bool {
    if env_truthy(disable_value) {
        return false;
    }
    if env_falsey(enable_value) {
        return false;
    }
    meter_backend_available
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteSnapshot {
    pub managed_modules: Vec<ManagedModule>,
    pub sink_input_routes: Vec<SinkInputRoute>,
    pub source_output_routes: Vec<SourceOutputRoute>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SinkLevelState {
    pub volume_percent: Option<u8>,
    pub muted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AudioStateSnapshot {
    pub graph: RuntimeGraph,
    pub routes: RouteSnapshot,
    pub sink_levels: BTreeMap<String, SinkLevelState>,
    pub active_playback_sink: Option<String>,
    pub bluetooth_cards: Vec<BluetoothAudioCard>,
    pub default_source: Option<String>,
    pub default_sink: Option<String>,
}

pub fn plan_ensure_graph(config: &MixerConfig) -> PlannedGraph {
    let active_app_channel_ids = config
        .channels
        .iter()
        .filter(|channel| !channel.kind.uses_hardware_slot())
        .map(|channel| channel.id.clone())
        .collect::<BTreeSet<_>>();
    plan_ensure_graph_for_active_app_channels(config, &active_app_channel_ids)
}

pub fn plan_ensure_graph_for_active_app_channels(
    config: &MixerConfig,
    active_app_channel_ids: &BTreeSet<String>,
) -> PlannedGraph {
    let active_mix_ids = config
        .mixes
        .iter()
        .map(|mix| mix.id.clone())
        .collect::<BTreeSet<_>>();
    plan_ensure_graph_for_active_routes(config, active_app_channel_ids, &active_mix_ids)
}

pub fn plan_ensure_graph_for_active_routes(
    config: &MixerConfig,
    active_app_channel_ids: &BTreeSet<String>,
    active_mix_ids: &BTreeSet<String>,
) -> PlannedGraph {
    let mut commands = Vec::new();
    let mut managed_nodes = Vec::new();
    let route_settings = route_settings_for_config(config);

    for mix in &config.mixes {
        if !mix_uses_persistent_audio_core(mix) {
            managed_nodes.push(mix.virtual_sink_name.clone());
        }
        managed_nodes.push(mix.virtual_source_name.clone());
        commands.extend(plan_ensure_mix(mix));
    }

    for channel in &config.channels {
        managed_nodes.push(channel.virtual_sink_name.clone());
        if !channel_uses_persistent_audio_core(channel) {
            commands.extend(plan_ensure_channel(channel));
        }
        if channel_uses_passthrough_mic_source(channel) {
            managed_nodes.push(effect_chain_source_name(channel));
            commands.extend(plan_ensure_passthrough_mic_source(channel));
        }
        if !channel_uses_persistent_audio_core(channel) {
            if let Some(source) = &channel.source_device {
                commands.extend(plan_route_input_to_channel(
                    channel,
                    source,
                    &route_settings,
                ));
            }
        }
        if channel_uses_persistent_audio_core(channel) || channel_has_active_effects(channel) {
            commands.extend(plan_route_channel_to_effect(channel, &route_settings));
            commands.extend(plan_route_effect_to_adaptive_bridge(
                channel,
                &route_settings,
            ));
        }
        for mix in &config.mixes {
            if channel_mix_route_expected_for_active_routes(
                channel,
                mix,
                &route_settings,
                active_app_channel_ids,
                active_mix_ids,
            ) {
                commands.extend(plan_route_channel_to_mix(channel, mix, &route_settings));
            }
        }
    }

    for mix in config
        .mixes
        .iter()
        .filter(|mix| active_mix_ids.contains(&mix.id) && !mix_uses_persistent_audio_core(mix))
    {
        for output in mix.outputs() {
            commands.extend(plan_route_mix_to_output(mix, &output, &route_settings));
        }
    }

    PlannedGraph {
        commands,
        managed_nodes,
    }
}

pub fn channel_mix_route_expected_for_active_app_channels(
    channel: &Channel,
    mix: &Mix,
    settings: &MixerSettings,
    active_app_channel_ids: &BTreeSet<String>,
) -> bool {
    let active_mix_ids = std::iter::once(mix.id.clone()).collect::<BTreeSet<_>>();
    channel_mix_route_expected_for_active_routes(
        channel,
        mix,
        settings,
        active_app_channel_ids,
        &active_mix_ids,
    )
}

pub fn channel_mix_route_expected_for_active_routes(
    channel: &Channel,
    mix: &Mix,
    settings: &MixerSettings,
    active_app_channel_ids: &BTreeSet<String>,
    active_mix_ids: &BTreeSet<String>,
) -> bool {
    let Some(bus) = channel.mix_buses.get(&mix.id) else {
        return false;
    };
    if !bus.enabled {
        return false;
    }
    if channel_mix_route_uses_hardware_direct_monitoring(channel, mix, settings) {
        return false;
    }
    // WaveLinux 6 keeps app-facing nodes and their configured bus sends stable.
    // A dormant browser or a newly opened recorder must never need a Pulse
    // module load before its first frames can flow.
    if channel_uses_persistent_audio_core(channel) {
        return true;
    }
    if bus.muted {
        return false;
    }
    if !active_mix_ids.contains(&mix.id) {
        return false;
    }
    channel.kind.uses_hardware_slot() || active_app_channel_ids.contains(&channel.id)
}

fn route_settings_for_config(config: &MixerConfig) -> MixerSettings {
    let mut settings = config.settings.clone();
    if settings.runtime_latency_policy.is_none() {
        settings.runtime_latency_policy = Some(
            config
                .device_policy
                .fallback_hardware_profile
                .latency_policy
                .clone(),
        );
    }
    settings
}

pub fn plan_ensure_mix(mix: &Mix) -> Vec<CommandSpec> {
    if mix_uses_persistent_audio_core(mix) {
        return Vec::new();
    }
    let display_name = wavelinux_display_name(&mix.name);
    let display_value = property_value(&display_name);
    let app_name = property_value(&app_display_name());
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
                    "sink_properties=device.description={0} node.description={0} node.nick={0} media.name={0} application.name={1} media.class=Audio/Sink {2} {3} {4}",
                    display_value,
                    app_name,
                    graph_prop_assignment("managed", "1"),
                    graph_prop_assignment("role", "mix"),
                    graph_prop_assignment("mix_id", &mix.id),
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
                    "source_properties=device.description={0} node.description={0} node.nick={0} media.name={0} application.name={1} media.class=Audio/Source/Virtual {2} {3} {4}",
                    display_value,
                    app_name,
                    graph_prop_assignment("managed", "1"),
                    graph_prop_assignment("role", "mix_source"),
                    graph_prop_assignment("mix_id", &mix.id),
                ),
            ],
            format!("expose '{}' as virtual source", mix.name),
        ),
    ]
}

pub fn plan_ensure_channel(channel: &Channel) -> Vec<CommandSpec> {
    let display_name = wavelinux_display_name(&channel.name);
    let display_value = property_value(&display_name);
    let app_name = property_value(&app_display_name());
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
                "sink_properties=device.description={0} node.description={0} node.nick={0} media.name={0} application.name={1} media.class=Audio/Sink {2} {3} {4} {5}",
                display_value,
                app_name,
                graph_prop_assignment("managed", "1"),
                graph_prop_assignment("role", "channel"),
                graph_prop_assignment("channel_id", &channel.id),
                graph_prop_assignment("channel_config_revision", CHANNEL_CONFIG_REVISION),
            ),
        ],
        format!("create channel sink '{}'", channel.name),
    )]
}

pub fn plan_ensure_passthrough_mic_source(channel: &Channel) -> Vec<CommandSpec> {
    if !channel_uses_passthrough_mic_source(channel) {
        return Vec::new();
    }

    let source_name = effect_chain_source_name(channel);
    let source_label = effect_chain_source_label(channel);
    let source_value = property_value(&source_label);
    let app_name = property_value(&app_display_name());
    vec![CommandSpec::new(
        CommandDomain::Graph,
        "pactl",
        [
            "load-module".into(),
            "module-remap-source".into(),
            format!("master={}.monitor", channel.virtual_sink_name),
            format!("source_name={source_name}"),
            "channels=2".into(),
            "channel_map=front-left,front-right".into(),
            format!(
                "source_properties=device.description={0} node.description={0} node.nick={0} media.name={0} application.name={1} media.class=Audio/Source/Virtual {2} {3} {4} {5}",
                source_value,
                app_name,
                graph_prop_assignment("managed", "1"),
                graph_prop_assignment("role", "mic_passthrough"),
                graph_prop_assignment("effect_config_revision", EFFECT_CONFIG_REVISION),
                graph_prop_assignment("channel_id", &channel.id),
            ),
        ],
        format!("expose '{}' as public mic source", channel.name),
    )]
}

pub fn plan_route_channel_to_mix(
    channel: &Channel,
    mix: &Mix,
    settings: &MixerSettings,
) -> Vec<CommandSpec> {
    if channel_uses_persistent_audio_core(channel) {
        return Vec::new();
    }
    let source_name = channel_mix_source_name(channel);
    let latency_msec = channel_mix_latency_msec(channel, mix, settings);
    let route_revision = route_revision_with_latency(CHANNEL_MIX_ROUTE_REVISION, latency_msec);
    let route_properties = [
        (graph_prop("role"), "channel_to_mix".to_string()),
        (graph_prop("channel_id"), channel.id.clone()),
        (graph_prop("mix_id"), mix.id.clone()),
        (graph_prop("route_revision"), route_revision.clone()),
    ];
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={source_name}"),
            format!("sink={}", mix.virtual_sink_name),
            latency_arg(latency_msec),
            "adjust_time=0".into(),
            "channels=2".into(),
            "channel_map=front-left,front-right".into(),
            "remix=yes".into(),
            format!(
                "source_output_properties={}",
                managed_loopback_properties(
                    "source",
                    "channel-to-mix",
                    &[channel.id.as_str(), mix.id.as_str()],
                    &route_properties,
                )
            ),
            format!(
                "sink_input_properties={}",
                managed_loopback_properties(
                    "sink",
                    "channel-to-mix",
                    &[channel.id.as_str(), mix.id.as_str()],
                    &route_properties,
                )
            ),
        ],
        format!("route '{}' to '{}'", channel.name, mix.name),
    )]
}

pub fn channel_mix_route_uses_hardware_direct_monitoring(
    channel: &Channel,
    mix: &Mix,
    settings: &MixerSettings,
) -> bool {
    settings.hardware_direct_mic_monitoring
        && channel.kind.uses_hardware_slot()
        && mix.id == "monitor"
        && channel
            .source_device
            .as_deref()
            .is_some_and(source_name_looks_like_wave_xlr)
}

pub fn channel_mix_source_name(channel: &Channel) -> String {
    if channel_uses_persistent_audio_core(channel)
        || channel_exposes_public_mic_source(channel)
        || channel_has_active_effects(channel)
    {
        effect_chain_source_name(channel)
    } else {
        format!("{}.monitor", channel.virtual_sink_name)
    }
}

fn source_name_looks_like_wave_xlr(source_name: &str) -> bool {
    let compact = source_name
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    compact.contains("wavexlr") || compact.contains("0fd9007d")
}

pub fn channel_has_active_effects(channel: &Channel) -> bool {
    channel.effects_enabled && channel.effects.iter().any(|effect| !effect.bypassed)
}

pub fn channel_exposes_public_mic_source(channel: &Channel) -> bool {
    channel.id == "hardware_in"
}

pub fn channel_uses_passthrough_mic_source(channel: &Channel) -> bool {
    channel_exposes_public_mic_source(channel)
        && !channel_uses_persistent_audio_core(channel)
        && !channel_has_active_effects(channel)
}

pub fn channel_uses_persistent_audio_core(channel: &Channel) -> bool {
    graph_prefix_for_channel(channel) == "wavelinux6"
}

pub fn mix_uses_persistent_audio_core(mix: &Mix) -> bool {
    let suffix = format!("_mix_{}", safe_node_id(&mix.id));
    mix.virtual_sink_name
        .strip_suffix(&suffix)
        .is_some_and(|prefix| prefix == "wavelinux6")
}

pub fn effect_chain_input_name(channel: &Channel) -> String {
    if channel_uses_persistent_audio_core(channel) {
        return channel.virtual_sink_name.clone();
    }
    format!(
        "{}_fx_{}_input",
        graph_prefix_for_channel(channel),
        safe_node_id(&channel.id)
    )
}

pub fn effect_chain_filter_output_name(channel: &Channel) -> String {
    if channel_uses_adaptive_latency_bridge(channel) {
        return format!(
            "{}_fx_{}_processed",
            graph_prefix_for_channel(channel),
            safe_node_id(&channel.id)
        );
    }
    effect_chain_source_name(channel)
}

pub fn effect_chain_adaptive_bridge_input_name(channel: &Channel) -> String {
    format!(
        "{}_fx_{}_adaptive_input",
        graph_prefix_for_channel(channel),
        safe_node_id(&channel.id)
    )
}

pub fn effect_chain_source_name(channel: &Channel) -> String {
    let prefix = graph_prefix_for_channel(channel);
    if channel.id == "hardware_in" {
        return format!("{prefix}-mic");
    }
    format!("{}_fx_{}_source", prefix, safe_node_id(&channel.id))
}

pub fn effect_chain_node_name(channel: &Channel) -> String {
    format!(
        "{}_fx_{}_chain",
        graph_prefix_for_channel(channel),
        safe_node_id(&channel.id)
    )
}

fn effect_chain_source_label(channel: &Channel) -> String {
    if channel.id == "hardware_in" {
        format!("{}-mic", app_display_name())
    } else {
        format!("{} FX {} Output", app_display_name(), channel.name)
    }
}

fn effect_chain_filter_output_label(channel: &Channel) -> String {
    if channel_uses_adaptive_latency_bridge(channel) {
        return format!("{} FX {} Processed", app_display_name(), channel.name);
    }
    effect_chain_source_label(channel)
}

fn effect_chain_filter_output_role(channel: &Channel) -> &'static str {
    if channel_uses_adaptive_latency_bridge(channel) {
        "effect_processed"
    } else {
        "effect_output"
    }
}

pub fn channel_uses_adaptive_latency_bridge(channel: &Channel) -> bool {
    graph_prefix_for_channel(channel) == "wavelinux5"
        && channel.id == "hardware_in"
        && channel_has_active_effects(channel)
}

fn graph_prefix_for_channel(channel: &Channel) -> String {
    let suffix = format!("_channel_{}", safe_node_id(&channel.id));
    channel
        .virtual_sink_name
        .strip_suffix(&suffix)
        .filter(|prefix| !prefix.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(graph_prefix)
}

pub fn plan_route_effect_to_adaptive_bridge(
    channel: &Channel,
    _settings: &MixerSettings,
) -> Vec<CommandSpec> {
    if !channel_uses_adaptive_latency_bridge(channel) {
        return Vec::new();
    }

    let source_name = effect_chain_filter_output_name(channel);
    let sink_name = effect_chain_adaptive_bridge_input_name(channel);
    let route_properties = [
        (graph_prop("role"), "effect_to_adaptive_bridge".to_string()),
        (graph_prop("channel_id"), channel.id.clone()),
        (
            graph_prop("route_revision"),
            EFFECT_ADAPTIVE_BRIDGE_ROUTE_REVISION.to_string(),
        ),
    ];
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={source_name}"),
            format!("sink={sink_name}"),
            latency_arg(EFFECT_ADAPTIVE_BRIDGE_TRANSPORT_MSEC),
            "adjust_time=0".into(),
            "channels=2".into(),
            "channel_map=front-left,front-right".into(),
            "remix=yes".into(),
            format!(
                "source_output_properties={}",
                managed_loopback_properties(
                    "source",
                    "effect-to-adaptive-bridge",
                    &[channel.id.as_str()],
                    &route_properties,
                )
            ),
            format!(
                "sink_input_properties={}",
                managed_loopback_properties(
                    "sink",
                    "effect-to-adaptive-bridge",
                    &[channel.id.as_str()],
                    &route_properties,
                )
            ),
        ],
        format!("route '{}' FX into adaptive mic bridge", channel.name),
    )]
}

pub fn plan_route_channel_to_effect(
    channel: &Channel,
    settings: &MixerSettings,
) -> Vec<CommandSpec> {
    if channel_uses_persistent_audio_core(channel) {
        return Vec::new();
    }
    let raw_source = format!("{}.monitor", channel.virtual_sink_name);
    let latency_msec = hardware_route_latency_msec(channel, settings);
    let route_revision = route_revision_with_latency(EFFECT_ROUTE_REVISION, latency_msec);
    let route_properties = [
        (graph_prop("role"), "channel_to_effect".to_string()),
        (graph_prop("channel_id"), channel.id.clone()),
        (graph_prop("route_revision"), route_revision.clone()),
    ];
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={raw_source}"),
            format!("sink={}", effect_chain_input_name(channel)),
            latency_arg(latency_msec),
            "adjust_time=0".into(),
            "channels=2".into(),
            "channel_map=front-left,front-right".into(),
            format!(
                "source_output_properties={}",
                managed_loopback_properties(
                    "source",
                    "channel-to-effect",
                    &[channel.id.as_str()],
                    &route_properties,
                )
            ),
            format!(
                "sink_input_properties={}",
                managed_loopback_properties(
                    "sink",
                    "channel-to-effect",
                    &[channel.id.as_str()],
                    &route_properties,
                )
            ),
        ],
        format!("route '{}' into its effect chain", channel.name),
    )]
}

pub fn plan_route_input_to_channel(
    channel: &Channel,
    source_name: &str,
    settings: &MixerSettings,
) -> Vec<CommandSpec> {
    let (channels, channel_map) = input_loopback_audio_shape(channel.input_mode);
    let mode_id = channel.input_mode.id();
    let latency_msec = hardware_route_latency_msec(channel, settings);
    let route_revision = route_revision_with_latency(INPUT_ROUTE_REVISION, latency_msec);
    let route_properties = [
        (graph_prop("role"), "input_to_channel".to_string()),
        (graph_prop("channel_id"), channel.id.clone()),
        (graph_prop("input_mode"), mode_id.to_string()),
        (graph_prop("route_revision"), route_revision.clone()),
    ];
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={source_name}"),
            format!("sink={}", channel.virtual_sink_name),
            latency_arg(latency_msec),
            "adjust_time=0".into(),
            format!("channels={channels}"),
            format!("channel_map={channel_map}"),
            "remix=yes".into(),
            format!(
                "source_output_properties={}",
                managed_loopback_properties(
                    "source",
                    "input-to-channel",
                    &[channel.id.as_str(), mode_id],
                    &route_properties,
                )
            ),
            format!(
                "sink_input_properties={}",
                managed_loopback_properties(
                    "sink",
                    "input-to-channel",
                    &[channel.id.as_str(), mode_id],
                    &route_properties,
                )
            ),
        ],
        format!("route input {source_name} to '{}'", channel.name),
    )]
}

fn input_loopback_audio_shape(input_mode: ChannelInputMode) -> (u8, &'static str) {
    match input_mode {
        ChannelInputMode::SumMono => (1, "mono"),
        _ => (input_mode.channels(), input_mode.channel_map()),
    }
}

pub fn hardware_route_latency_msec(channel: &Channel, settings: &MixerSettings) -> u16 {
    if channel.kind.uses_hardware_slot()
        && (settings.low_latency_mic_monitoring
            || settings.optimization_mode == OptimizationMode::Performance)
    {
        low_latency_loopback_msec(settings)
    } else {
        stable_loopback_latency_msec(settings)
    }
}

pub fn channel_mix_latency_msec(channel: &Channel, mix: &Mix, settings: &MixerSettings) -> u16 {
    let base = if channel.kind.uses_hardware_slot() {
        hardware_route_latency_msec(channel, settings)
    } else if mix.id == "monitor"
        && (settings.low_latency_mic_monitoring
            || settings.optimization_mode == OptimizationMode::Performance)
    {
        low_latency_loopback_msec(settings)
    } else {
        stable_loopback_latency_msec(settings)
    };
    let extra = if channel.kind.uses_hardware_slot() {
        0
    } else if mix.id == "stream" {
        settings.stream_sync_delay_msec
    } else if mix.id == "monitor" {
        settings.monitor_sync_delay_msec
    } else {
        0
    };
    base.saturating_add(extra).min(500)
}

pub fn mix_monitor_latency_msec(mix: &Mix, settings: &MixerSettings) -> u16 {
    if mix.id == "monitor"
        && (settings.low_latency_mic_monitoring
            || settings.optimization_mode == OptimizationMode::Performance)
    {
        low_latency_loopback_msec(settings)
    } else {
        stable_loopback_latency_msec(settings)
    }
}

pub fn mix_monitor_latency_msec_for_sink(
    mix: &Mix,
    sink_name: &str,
    settings: &MixerSettings,
) -> u16 {
    let base = mix_monitor_latency_msec(mix, settings);
    if is_bluetooth_output_name(sink_name) {
        base.max(bluetooth_monitor_loopback_msec(settings))
    } else {
        base
    }
}

fn stable_loopback_latency_msec(settings: &MixerSettings) -> u16 {
    settings
        .runtime_latency_policy
        .as_ref()
        .and_then(|policy| policy.stable_msec)
        .unwrap_or(STABLE_LOOPBACK_LATENCY_MSEC)
        .clamp(5, 500)
}

fn low_latency_loopback_msec(settings: &MixerSettings) -> u16 {
    settings
        .runtime_latency_policy
        .as_ref()
        .and_then(|policy| policy.low_latency_msec)
        .unwrap_or(LOW_LATENCY_LOOPBACK_MSEC)
        .clamp(5, 500)
}

fn bluetooth_monitor_loopback_msec(settings: &MixerSettings) -> u16 {
    settings
        .runtime_latency_policy
        .as_ref()
        .and_then(|policy| policy.bluetooth_floor_msec)
        .unwrap_or(BLUETOOTH_MONITOR_LOOPBACK_MSEC)
        .clamp(50, 500)
}

pub fn input_route_revision(settings: &MixerSettings, channel: &Channel) -> String {
    route_revision_with_latency(
        INPUT_ROUTE_REVISION,
        hardware_route_latency_msec(channel, settings),
    )
}

pub fn effect_route_revision(settings: &MixerSettings, channel: &Channel) -> String {
    route_revision_with_latency(
        EFFECT_ROUTE_REVISION,
        hardware_route_latency_msec(channel, settings),
    )
}

pub fn channel_mix_route_revision(
    settings: &MixerSettings,
    channel: &Channel,
    mix: &Mix,
) -> String {
    route_revision_with_latency(
        CHANNEL_MIX_ROUTE_REVISION,
        channel_mix_latency_msec(channel, mix, settings),
    )
}

pub fn mix_monitor_route_revision(settings: &MixerSettings, mix: &Mix) -> String {
    route_revision_with_latency(
        MIX_MONITOR_ROUTE_REVISION,
        mix_monitor_latency_msec(mix, settings),
    )
}

pub fn mix_monitor_route_revision_for_sink(
    settings: &MixerSettings,
    mix: &Mix,
    sink_name: &str,
) -> String {
    route_revision_with_latency(
        MIX_MONITOR_ROUTE_REVISION,
        mix_monitor_latency_msec_for_sink(mix, sink_name, settings),
    )
}

fn route_revision_with_latency(base: &str, latency_msec: u16) -> String {
    format!("{base}-latency-{latency_msec}")
}

fn latency_arg(latency_msec: u16) -> String {
    format!("latency_msec={latency_msec}")
}

fn managed_loopback_properties(
    side: &str,
    route_kind: &str,
    route_parts: &[&str],
    route_properties: &[(String, String)],
) -> String {
    let media_name = managed_loopback_media_name(side, route_kind, route_parts);
    let mut properties = vec![
        graph_prop_assignment("managed", "1"),
        format!("application.name={}", property_value(&app_display_name())),
        format!("media.name={}", property_value(&media_name)),
        "node.dont-move=true".to_string(),
        "state.restore-props=false".to_string(),
        "state.restore-target=false".to_string(),
    ];
    properties.extend(
        route_properties
            .iter()
            .map(|(key, value)| format!("{key}={}", property_value(value))),
    );
    properties.join(" ")
}

fn managed_loopback_media_name(side: &str, route_kind: &str, route_parts: &[&str]) -> String {
    let mut parts = vec![
        format!("{}-route", graph_prefix()),
        safe_node_id(side),
        safe_node_id(route_kind),
    ];
    parts.extend(route_parts.iter().map(|part| safe_node_id(part)));
    parts.join("-")
}

pub fn plan_route_mix_to_output(
    mix: &Mix,
    sink_name: &str,
    settings: &MixerSettings,
) -> Vec<CommandSpec> {
    let source_name = if mix_uses_persistent_audio_core(mix) {
        mix.virtual_source_name.clone()
    } else {
        format!("{}.monitor", mix.virtual_sink_name)
    };
    let latency_msec = mix_monitor_latency_msec_for_sink(mix, sink_name, settings);
    let route_revision = route_revision_with_latency(MIX_MONITOR_ROUTE_REVISION, latency_msec);
    let route_properties = [
        (graph_prop("role"), "mix_monitor".to_string()),
        (graph_prop("mix_id"), mix.id.clone()),
        (graph_prop("route_revision"), route_revision.clone()),
    ];
    vec![CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "load-module".into(),
            "module-loopback".into(),
            format!("source={source_name}"),
            format!("sink={sink_name}"),
            latency_arg(latency_msec),
            "adjust_time=0".into(),
            "channels=2".into(),
            "channel_map=front-left,front-right".into(),
            format!(
                "source_output_properties={}",
                managed_loopback_properties(
                    "source",
                    "mix-monitor",
                    &[mix.id.as_str(), sink_name],
                    &route_properties,
                )
            ),
            format!(
                "sink_input_properties={}",
                managed_loopback_properties(
                    "sink",
                    "mix-monitor",
                    &[mix.id.as_str(), sink_name],
                    &route_properties,
                )
            ),
        ],
        format!("monitor '{}' through {sink_name}", mix.name),
    )]
}

fn is_bluetooth_output_name(sink_name: &str) -> bool {
    sink_name
        .trim()
        .to_ascii_lowercase()
        .starts_with("bluez_output.")
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

pub fn plan_move_native_app_stream(
    stream_node_id: u32,
    target_object_serial: &str,
    target_node_name: &str,
) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Route,
        "pw-metadata",
        [
            "-n".into(),
            "default".into(),
            stream_node_id.to_string(),
            "target.object".into(),
            target_object_serial.into(),
            "Spa:Id".into(),
        ],
        format!("move native stream {stream_node_id} to {target_node_name}"),
    )
}

pub fn plan_move_native_capture_stream(
    stream_node_id: u32,
    target_object_serial: &str,
    target_node_name: &str,
) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Route,
        "pw-metadata",
        [
            "-n".into(),
            "default".into(),
            stream_node_id.to_string(),
            "target.object".into(),
            target_object_serial.into(),
            "Spa:Id".into(),
        ],
        format!("move native capture stream {stream_node_id} to {target_node_name}"),
    )
}

pub fn plan_set_native_stream_volume(stream_node_id: u32, volume: f32) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "wpctl",
        [
            "set-volume".into(),
            stream_node_id.to_string(),
            volume.clamp(0.0, 1.5).to_string(),
        ],
        format!("set native stream {stream_node_id} volume"),
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

pub fn plan_move_capture_stream_to_source(
    source_output_id: &str,
    source_name: &str,
) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            String::from("move-source-output"),
            source_output_id.to_owned(),
            source_name.to_owned(),
        ],
        format!("move capture stream {source_output_id} to {source_name}"),
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

pub fn plan_set_source_volume(source_name: &str, volume: f32) -> CommandSpec {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-source-volume".into(),
            source_name.into(),
            format!("{percent}%"),
        ],
        format!("set source {source_name} volume"),
    )
}

pub fn plan_set_source_mute(source_name: &str, muted: bool) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-source-mute".into(),
            source_name.into(),
            (if muted { "1" } else { "0" }).to_string(),
        ],
        format!("set source {source_name} mute"),
    )
}

pub fn plan_set_card_profile(card_name: &str, profile_name: &str) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Route,
        "pactl",
        [
            "set-card-profile".to_string(),
            card_name.to_string(),
            profile_name.to_string(),
        ],
        format!("set Bluetooth card {card_name} to {profile_name}"),
    )
}

pub fn plan_bluetooth_a2dp_profiles(
    cards: &[BluetoothAudioCard],
    initialized_cards: &BTreeMap<String, String>,
    force_all_a2dp: bool,
) -> Vec<CommandSpec> {
    cards
        .iter()
        .filter(|card| {
            if !card.a2dp_active() {
                return true;
            }
            if !force_all_a2dp && initialized_cards.contains_key(&card.name) {
                return false;
            }
            !card.preferred_a2dp_active()
        })
        .filter_map(|card| {
            card.preferred_a2dp_profile
                .as_deref()
                .map(|profile| plan_set_card_profile(&card.name, profile))
        })
        .collect()
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

pub fn plan_set_route_sink_input_volume(sink_input_id: &str, volume: f32) -> CommandSpec {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-input-volume".into(),
            sink_input_id.into(),
            format!("{percent}%"),
        ],
        format!("set managed route sink-input {sink_input_id} volume"),
    )
}

pub fn plan_set_route_sink_input_mute(sink_input_id: &str, muted: bool) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-input-mute".into(),
            sink_input_id.into(),
            (if muted { "1" } else { "0" }).to_string(),
        ],
        format!("set managed route sink-input {sink_input_id} mute"),
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

pub fn plan_set_route_source_output_volume(source_output_id: &str, volume: f32) -> CommandSpec {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-source-output-volume".into(),
            source_output_id.into(),
            format!("{percent}%"),
        ],
        format!("set managed route source-output {source_output_id} volume"),
    )
}

pub fn plan_set_route_source_output_mute(source_output_id: &str, muted: bool) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-source-output-mute".into(),
            source_output_id.into(),
            (if muted { "1" } else { "0" }).to_string(),
        ],
        format!("set managed route source-output {source_output_id} mute"),
    )
}

pub fn plan_set_managed_sink_volume(sink_name: &str, volume: f32) -> CommandSpec {
    let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-volume".into(),
            sink_name.into(),
            format!("{percent}%"),
        ],
        format!("set managed sink {sink_name} volume"),
    )
}

pub fn plan_set_managed_sink_mute(sink_name: &str, muted: bool) -> CommandSpec {
    CommandSpec::new(
        CommandDomain::Level,
        "pactl",
        [
            "set-sink-mute".into(),
            sink_name.into(),
            (if muted { "1" } else { "0" }).to_string(),
        ],
        format!("set managed sink {sink_name} mute"),
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
    let chain_name = effect_chain_node_name(channel);
    let input_name = effect_chain_input_name(channel);
    let source_name = effect_chain_filter_output_name(channel);
    let source_label = effect_chain_filter_output_label(channel);
    let app_name = app_display_name();
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
    append_filter_property(&mut rendered, 6, "managed", "1");
    append_filter_property(&mut rendered, 6, "role", "effect_chain");
    rendered.push_str("      ");
    rendered.push_str(&graph_prop("effect_config_revision"));
    rendered.push_str(" = \"");
    rendered.push_str(EFFECT_CONFIG_REVISION);
    rendered.push_str("\"\n");
    rendered.push_str("      ");
    rendered.push_str(&graph_prop("channel_id"));
    rendered.push_str(" = \"");
    rendered.push_str(&escape_pw(&channel.id));
    rendered.push_str("\"\n");
    rendered.push_str("      audio.channels = 2\n");
    rendered.push_str("      audio.position = [ FL FR ]\n");
    rendered.push_str("      node.description = \"");
    rendered.push_str(&escape_pw(&app_name));
    rendered.push_str(" FX ");
    rendered.push_str(&escape_pw(&channel.name));
    rendered.push_str("\"\n");
    rendered.push_str("      media.name = \"");
    rendered.push_str(&escape_pw(&app_name));
    rendered.push_str(" FX ");
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
        for node in &effect_nodes {
            for (source, target) in &node.internal_links {
                append_filter_link(&mut rendered, source, target);
            }
        }
        for pair in effect_nodes.windows(2) {
            let source = &pair[0];
            let target = &pair[1];
            append_stereo_filter_links(&mut rendered, source, target);
        }
        rendered.push_str("        ]\n");

        let first = &effect_nodes[0];
        let last = &effect_nodes[effect_nodes.len() - 1];
        append_port_ref_list(&mut rendered, "        inputs = [", first.inputs());
        append_port_ref_list(&mut rendered, "        outputs = [", last.outputs());
    }
    rendered.push_str("      }\n");
    rendered.push_str("      capture.props = {\n");
    rendered.push_str("        node.name = \"");
    rendered.push_str(&escape_pw(&input_name));
    rendered.push_str("\"\n");
    rendered.push_str("        node.description = \"");
    rendered.push_str(&escape_pw(&app_name));
    rendered.push_str(" FX ");
    rendered.push_str(&escape_pw(&channel.name));
    rendered.push_str(" Input\"\n");
    rendered.push_str("        node.nick = \"");
    rendered.push_str(&escape_pw(&app_name));
    rendered.push_str(" FX Input\"\n");
    rendered.push_str("        media.name = \"");
    rendered.push_str(&escape_pw(&app_name));
    rendered.push_str(" FX ");
    rendered.push_str(&escape_pw(&channel.name));
    rendered.push_str(" Input\"\n");
    rendered.push_str("        media.class = Audio/Sink\n");
    rendered.push_str("        node.virtual = true\n");
    rendered.push_str("        audio.rate = 48000\n");
    rendered.push_str("        audio.channels = 2\n");
    rendered.push_str("        audio.position = [ FL FR ]\n");
    if channel.id == "hardware_in" {
        rendered.push_str("        node.always-process = true\n");
    }
    append_filter_property(&mut rendered, 8, "managed", "1");
    append_filter_property(&mut rendered, 8, "role", "effect_input");
    rendered.push_str("        ");
    rendered.push_str(&graph_prop("effect_config_revision"));
    rendered.push_str(" = \"");
    rendered.push_str(EFFECT_CONFIG_REVISION);
    rendered.push_str("\"\n");
    rendered.push_str("        ");
    rendered.push_str(&graph_prop("channel_id"));
    rendered.push_str(" = \"");
    rendered.push_str(&escape_pw(&channel.id));
    rendered.push_str("\"\n");
    rendered.push_str("      }\n");
    rendered.push_str("      playback.props = {\n");
    rendered.push_str("        node.name = \"");
    rendered.push_str(&escape_pw(&source_name));
    rendered.push_str("\"\n");
    rendered.push_str("        device.description = \"");
    rendered.push_str(&escape_pw(&source_label));
    rendered.push_str("\"\n");
    rendered.push_str("        node.description = \"");
    rendered.push_str(&escape_pw(&source_label));
    rendered.push_str("\"\n");
    rendered.push_str("        node.nick = \"");
    rendered.push_str(&escape_pw(&source_label));
    rendered.push_str("\"\n");
    rendered.push_str("        media.name = \"");
    rendered.push_str(&escape_pw(&source_label));
    rendered.push_str("\"\n");
    rendered.push_str("        media.class = Audio/Source\n");
    rendered.push_str("        node.virtual = true\n");
    rendered.push_str("        audio.rate = 48000\n");
    rendered.push_str("        audio.channels = 2\n");
    rendered.push_str("        audio.position = [ FL FR ]\n");
    if channel.id == "hardware_in" {
        rendered.push_str("        node.always-process = true\n");
    }
    append_filter_property(&mut rendered, 8, "managed", "1");
    append_filter_property(
        &mut rendered,
        8,
        "role",
        effect_chain_filter_output_role(channel),
    );
    rendered.push_str("        ");
    rendered.push_str(&graph_prop("effect_config_revision"));
    rendered.push_str(" = \"");
    rendered.push_str(EFFECT_CONFIG_REVISION);
    rendered.push_str("\"\n");
    rendered.push_str("        ");
    rendered.push_str(&graph_prop("channel_id"));
    rendered.push_str(" = \"");
    rendered.push_str(&escape_pw(&channel.id));
    rendered.push_str("\"\n");
    rendered.push_str("      }\n");
    rendered.push_str("    }\n");
    rendered.push_str("  }\n");
    rendered.push_str("]\n");
    rendered
}

fn append_filter_property(rendered: &mut String, indent: usize, name: &str, value: &str) {
    rendered.push_str(&" ".repeat(indent));
    rendered.push_str(&graph_prop(name));
    rendered.push_str(" = \"");
    rendered.push_str(&escape_pw(value));
    rendered.push_str("\"\n");
}

pub fn probe_effect_availability(catalog: &EffectCatalog) -> Vec<EffectAvailability> {
    catalog
        .effects
        .iter()
        .map(|effect| match &effect.plugin_hint {
            PluginHint::Native => EffectAvailability {
                effect_id: effect.id.clone(),
                available: true,
                detail: "Bundled WaveLinux 6 native DSP".into(),
            },
            PluginHint::PipeWireBuiltin => EffectAvailability {
                effect_id: effect.id.clone(),
                available: true,
                detail: "PipeWire builtin".into(),
            },
            PluginHint::Ladspa { library_names } => {
                let found = find_plugin_file(library_names);
                let available = found
                    .as_ref()
                    .map(|path| ladspa_plugin_available(&effect.id, path))
                    .unwrap_or(false);
                EffectAvailability {
                    effect_id: effect.id.clone(),
                    available,
                    detail: found
                        .map(|path| ladspa_plugin_detail(&effect.id, &path))
                        .unwrap_or_else(|| format!("Missing one of: {}", library_names.join(", "))),
                }
            }
            PluginHint::LadspaAll { library_names } => {
                let missing = library_names
                    .iter()
                    .filter(|name| find_plugin_file(&[(*name).clone()]).is_none())
                    .cloned()
                    .collect::<Vec<_>>();
                EffectAvailability {
                    effect_id: effect.id.clone(),
                    available: missing.is_empty(),
                    detail: if missing.is_empty() {
                        format!(
                            "Found required LADSPA plugins: {}",
                            library_names.join(", ")
                        )
                    } else {
                        format!("Missing required LADSPA plugins: {}", missing.join(", "))
                    },
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

fn ladspa_plugin_available(effect_id: &str, path: &Path) -> bool {
    let _ = (effect_id, path);
    true
}

fn ladspa_plugin_detail(effect_id: &str, path: &Path) -> String {
    let _ = effect_id;
    path.display().to_string()
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
    mute: bool,
    #[serde(default)]
    volume: BTreeMap<String, PactlVolumeEntry>,
    #[serde(default)]
    properties: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    active_port: serde_json::Value,
    #[serde(default)]
    ports: serde_json::Value,
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
    source: JsonNumberOrString,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    volume: BTreeMap<String, PactlVolumeEntry>,
    #[serde(default)]
    properties: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct PactlCard {
    #[serde(default)]
    name: String,
    #[serde(default)]
    properties: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    profiles: BTreeMap<String, PactlCardProfile>,
    #[serde(default)]
    active_profile: PactlActiveProfile,
}

#[derive(Debug, Deserialize)]
struct PactlCardProfile {
    #[serde(default)]
    description: String,
    #[serde(default)]
    sinks: u32,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    available: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
enum PactlActiveProfile {
    Name(String),
    Object {
        name: String,
    },
    #[default]
    Missing,
}

impl PactlActiveProfile {
    fn name(&self) -> Option<&str> {
        match self {
            Self::Name(name) | Self::Object { name } => {
                (!name.trim().is_empty()).then_some(name.as_str())
            }
            Self::Missing => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceOutputRoute {
    pub id: String,
    pub module_id: Option<String>,
    pub role: Option<String>,
    pub channel_id: Option<String>,
    pub mix_id: Option<String>,
    pub muted: Option<bool>,
    pub volume_percent: Option<u8>,
    pub source_id: Option<String>,
    pub source_name: Option<String>,
    pub target_object: Option<String>,
    pub application_name: Option<String>,
    pub node_name: Option<String>,
    pub media_name: Option<String>,
    #[serde(default)]
    pub managed: Option<String>,
    #[serde(default)]
    pub dont_move: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SinkInputRoute {
    pub id: String,
    pub module_id: Option<String>,
    pub role: Option<String>,
    pub channel_id: Option<String>,
    pub mix_id: Option<String>,
    pub muted: Option<bool>,
    pub volume_percent: Option<u8>,
    pub sink: Option<String>,
    pub sink_name: Option<String>,
    pub target_object: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelBusRouteIds {
    pub sink_input_id: Option<String>,
    pub source_output_id: Option<String>,
}

impl ChannelBusRouteIds {
    pub fn is_empty(&self) -> bool {
        self.sink_input_id.is_none() && self.source_output_id.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedModule {
    pub module_id: String,
    pub role: Option<String>,
    pub channel_id: Option<String>,
    pub mix_id: Option<String>,
    pub route_revision: Option<String>,
    pub node_name: Option<String>,
    pub source_name: Option<String>,
    pub sink_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaleProcess {
    pub pid: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BluetoothCardProfile {
    pub name: String,
    pub description: String,
    pub sinks: u32,
    pub priority: i32,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BluetoothAudioCard {
    pub name: String,
    pub device_key: String,
    pub active_profile: Option<String>,
    pub preferred_a2dp_profile: Option<String>,
    #[serde(default)]
    pub profiles: Vec<BluetoothCardProfile>,
}

impl BluetoothAudioCard {
    pub fn a2dp_available(&self) -> bool {
        self.preferred_a2dp_profile.is_some()
    }

    pub fn a2dp_active(&self) -> bool {
        self.active_profile
            .as_deref()
            .is_some_and(is_a2dp_profile_name)
    }

    pub fn preferred_a2dp_active(&self) -> bool {
        self.preferred_a2dp_profile
            .as_deref()
            .is_some_and(|preferred| self.active_profile.as_deref() == Some(preferred))
    }
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
    fn object_id(&self) -> Option<String> {
        let value = self.to_string();
        (!value.is_empty()).then_some(value)
    }

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
            let pipewire_properties = string_properties(&device.properties);
            let description = if device.description.is_empty() {
                device
                    .properties
                    .get("device.description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&id)
                    .to_string()
            } else {
                device.description.clone()
            };
            let is_default = device
                .properties
                .get("node.default")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let is_virtual = looks_like_wavelinux_family_node(&id)
                || looks_like_wavelinux_family_node(&device.name)
                || looks_like_wavelinux_family_node(&description)
                || graph_property_string(&device.properties, "managed").as_deref() == Some("1");
            let active_port = active_port_name(&device.active_port);
            let ports = device_ports(&device.ports);
            let is_available = device_is_available(&device);
            let bus = detect_device_bus(&id, &device.properties, is_virtual);
            DeviceInfo {
                id,
                index: Some(device.index.to_string()).filter(|value| !value.is_empty()),
                name: device.name,
                description: if description.is_empty() {
                    format!("{fallback_prefix} {}", device.index)
                } else {
                    description
                },
                is_available,
                active_port,
                ports,
                is_default,
                is_virtual,
                bus,
                vendor_id: property_string(&device.properties, "device.vendor.id")
                    .or_else(|| property_string(&device.properties, "api.usb.vendor.id"))
                    .map(|value| normalize_hex_id(&value)),
                product_id: property_string(&device.properties, "device.product.id")
                    .or_else(|| property_string(&device.properties, "api.usb.product.id"))
                    .map(|value| normalize_hex_id(&value)),
                alsa_card: property_string(&device.properties, "alsa.card")
                    .or_else(|| property_string(&device.properties, "api.alsa.card")),
                alsa_device: property_string(&device.properties, "alsa.device")
                    .or_else(|| property_string(&device.properties, "api.alsa.pcm.device")),
                driver: property_string(&device.properties, "alsa.driver_name")
                    .or_else(|| property_string(&device.properties, "device.driver")),
                bluetooth_modalias: property_string(&device.properties, "api.bluez5.modalias")
                    .or_else(|| property_string(&device.properties, "bluez5.modalias")),
                active_profile: property_string(&device.properties, "device.profile.name")
                    .or_else(|| property_string(&device.properties, "api.bluez5.profile")),
                active_codec: property_string(&device.properties, "api.bluez5.codec")
                    .or_else(|| property_string(&device.properties, "bluez5.codec")),
                pipewire_properties,
                matched_profile_id: None,
                matched_profile_source: None,
                profile_confidence: None,
                active_latency_policy: None,
                active_routing_policy: None,
                active_bluetooth_mic_policy: None,
            }
        })
        .collect()
}

struct AudioStateSnapshotJson<'a> {
    sources: &'a str,
    sinks: &'a str,
    sink_inputs: &'a str,
    source_outputs: &'a str,
    clients: &'a str,
    modules: &'a str,
    cards: &'a str,
    default_source: Option<&'a str>,
    default_sink: Option<&'a str>,
}

fn parse_audio_state_snapshot(
    json: AudioStateSnapshotJson<'_>,
    config: Option<&MixerConfig>,
    effect_availability: Vec<EffectAvailability>,
) -> AudioStateSnapshot {
    let inputs = parse_devices_json(json.sources, "Source");
    let outputs = parse_devices_json(json.sinks, "Sink");
    let sink_names = parse_device_names_by_index_json(json.sinks);
    let source_names = parse_device_names_by_index_json(json.sources);
    let client_properties = parse_client_properties_json(json.clients);
    let app_streams = parse_sink_inputs_json_with_client_properties(
        json.sink_inputs,
        config,
        &sink_names,
        &client_properties,
    );
    let managed_modules = parse_managed_modules_json(
        json.modules,
        json.sinks,
        json.sources,
        json.sink_inputs,
        json.source_outputs,
    );
    let sink_input_routes = hydrate_sink_input_routes_from_modules(
        hydrate_sink_input_routes_from_sinks(
            parse_sink_input_routes_json(json.sink_inputs),
            &sink_names,
        ),
        &managed_modules,
    );
    let source_output_routes = hydrate_source_output_routes_from_modules(
        hydrate_source_output_routes_from_sources(
            parse_source_outputs_json(json.source_outputs),
            &source_names,
        ),
        &managed_modules,
    );
    let sink_levels = serde_json::from_str::<Vec<PactlDevice>>(json.sinks)
        .unwrap_or_default()
        .into_iter()
        .filter(|sink| !sink.name.trim().is_empty())
        .map(|sink| {
            (
                sink.name,
                SinkLevelState {
                    volume_percent: parse_first_volume_percent(&sink.volume),
                    muted: sink.mute,
                },
            )
        })
        .collect();
    let active_playback_sink =
        active_playback_sink_from_sink_inputs_json(json.sink_inputs, &sink_names);

    AudioStateSnapshot {
        graph: RuntimeGraph {
            inputs,
            outputs,
            app_streams,
            meters: Vec::new(),
            auto_devices: Vec::new(),
            effect_availability,
        },
        routes: RouteSnapshot {
            managed_modules,
            sink_input_routes,
            source_output_routes,
        },
        sink_levels,
        active_playback_sink,
        bluetooth_cards: parse_bluetooth_audio_cards_json(json.cards),
        default_source: json
            .default_source
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        default_sink: json
            .default_sink
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string),
    }
}

fn parse_device_names_by_index_json(json: &str) -> BTreeMap<String, String> {
    let devices: Vec<PactlDevice> = serde_json::from_str(json).unwrap_or_default();
    devices
        .into_iter()
        .filter_map(|device| {
            let index = device.index.object_id()?;
            (!device.name.trim().is_empty()).then_some((index, device.name))
        })
        .collect()
}

fn device_is_available(device: &PactlDevice) -> bool {
    let active_port = active_port_name(&device.active_port);
    let port_availabilities = port_availabilities(&device.ports, active_port.as_deref());
    if port_availabilities
        .iter()
        .any(|availability| availability_is_unavailable(availability))
    {
        return false;
    }
    if port_availabilities.is_empty() {
        return true;
    }
    port_availabilities
        .iter()
        .any(|availability| !availability_is_unavailable(availability))
}

fn active_port_name(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .filter(|value| !value.trim().is_empty())
}

fn device_ports(ports: &serde_json::Value) -> Vec<DevicePortInfo> {
    let values = match ports {
        serde_json::Value::Array(items) => items.iter().collect::<Vec<_>>(),
        serde_json::Value::Object(items) => items.values().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    values
        .into_iter()
        .filter_map(|port| {
            let name = port
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let description = port
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let availability = port
                .get("availability")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if name.is_empty() && description.is_empty() && availability.is_empty() {
                return None;
            }
            Some(DevicePortInfo {
                name,
                description,
                availability,
                direction: port
                    .get("direction")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                port_type: port
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
            })
        })
        .collect()
}

fn port_availabilities(ports: &serde_json::Value, active_port: Option<&str>) -> Vec<String> {
    let values = match ports {
        serde_json::Value::Array(items) => items.iter().collect::<Vec<_>>(),
        serde_json::Value::Object(items) => items.values().collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let mut matches = values
        .iter()
        .filter(|port| {
            active_port.is_none_or(|active| {
                port.get("name")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| name == active)
            })
        })
        .filter_map(|port| {
            port.get("availability")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    if matches.is_empty() && active_port.is_some() {
        matches = values
            .iter()
            .filter_map(|port| {
                port.get("availability")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .collect();
    }
    matches
}

fn availability_is_unavailable(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    matches!(value.as_str(), "not available" | "unavailable" | "no")
}

pub fn parse_bluetooth_audio_cards_json(json: &str) -> Vec<BluetoothAudioCard> {
    let cards: Vec<PactlCard> = serde_json::from_str(json).unwrap_or_default();
    cards
        .into_iter()
        .filter(is_bluetooth_card)
        .filter_map(|card| {
            let device_key = bluetooth_card_device_key(&card)?;
            let preferred_a2dp_profile = card
                .profiles
                .iter()
                .filter(|(name, profile)| {
                    profile.available
                        && profile.sinks > 0
                        && (is_a2dp_profile_name(name)
                            || profile.description.to_ascii_lowercase().contains("a2dp"))
                })
                .max_by_key(|(name, profile)| {
                    (
                        a2dp_codec_rank(name, &profile.description),
                        profile.priority,
                    )
                })
                .map(|(name, _)| name.clone());
            let parsed_profiles = card
                .profiles
                .iter()
                .map(|(name, p)| BluetoothCardProfile {
                    name: name.clone(),
                    description: p.description.clone(),
                    sinks: p.sinks,
                    priority: p.priority,
                    available: p.available,
                })
                .collect();
            Some(BluetoothAudioCard {
                name: card.name,
                device_key,
                active_profile: card.active_profile.name().map(ToOwned::to_owned),
                preferred_a2dp_profile,
                profiles: parsed_profiles,
            })
        })
        .collect()
}

fn is_bluetooth_card(card: &PactlCard) -> bool {
    card.name.starts_with("bluez_card.")
        || property_string(&card.properties, "device.bus").as_deref() == Some("bluetooth")
        || property_string(&card.properties, "device.api")
            .as_deref()
            .is_some_and(|api| api.starts_with("bluez"))
}

fn bluetooth_card_device_key(card: &PactlCard) -> Option<String> {
    property_string(&card.properties, "api.bluez5.address")
        .or_else(|| property_string(&card.properties, "device.string"))
        .or_else(|| card.name.strip_prefix("bluez_card.").map(ToOwned::to_owned))
        .map(|value| normalize_bluetooth_device_key(&value))
        .filter(|value| !value.is_empty())
}

fn normalize_bluetooth_device_key(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("bluez_card.")
        .trim_start_matches("bluez_output.")
        .trim_start_matches("bluez_input.")
        .split('.')
        .next()
        .unwrap_or_default()
        .replace(':', "_")
        .to_ascii_uppercase()
}

fn is_a2dp_profile_name(profile: &str) -> bool {
    profile.to_ascii_lowercase().contains("a2dp")
}

fn a2dp_codec_rank(profile: &str, description: &str) -> u8 {
    let text = format!("{profile} {description}").to_ascii_lowercase();
    if text.contains("aptx-ll-duplex")
        || text.contains("aptx_ll_duplex")
        || text.contains("faststream-duplex")
        || text.contains("faststream_duplex")
        || text.contains("opus_05_duplex")
    {
        return 95;
    }
    if text.contains("aptx-ll") || text.contains("aptx_ll") {
        return 90;
    }
    if text.contains("aptx-adaptive") || text.contains("aptx_adaptive") {
        return 85;
    }
    if text.contains("sbc_xq") || text.contains("sbc-xq") {
        return 80;
    }
    if text.contains("aptx") {
        return 75;
    }
    if text.contains("aac") {
        return 70;
    }
    if text.contains("ldac") {
        return 65;
    }
    if text.contains("aptx-hd") || text.contains("aptx_hd") {
        return 60;
    }
    if text.contains("faststream") {
        return 50;
    }
    if text.contains("sbc") {
        return 40;
    }
    1
}

pub fn a2dp_codec_rank_with_preferences(
    profile: &str,
    description: &str,
    preferred_codecs: &[String],
) -> u16 {
    if !preferred_codecs.is_empty() {
        for (index, codec) in preferred_codecs.iter().enumerate() {
            if profile_matches_codec_name(profile, description, codec) && index < 100 {
                return 1000 - index as u16;
            }
        }
    }
    a2dp_codec_rank(profile, description) as u16
}

pub fn profile_matches_codec_name(profile: &str, description: &str, codec: &str) -> bool {
    let text = format!("{profile} {description}")
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    let normalized_codec = codec.to_ascii_lowercase().replace(['-', ' '], "_");
    if normalized_codec == "sbc_xq" {
        text.contains("sbc_xq") || text.contains("sbc_extra")
    } else if normalized_codec == "sbc" {
        text.contains("sbc") && !text.contains("sbc_xq") && !text.contains("sbc_extra")
    } else if normalized_codec == "aptx" {
        text.contains("aptx")
            && !text.contains("aptx_hd")
            && !text.contains("aptx_adaptive")
            && !text.contains("aptx_ll")
    } else {
        text.contains(&normalized_codec)
    }
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
                graph_property_string(&properties, "channel_id").or_else(|| {
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
            role: graph_property_string(&input.properties, "role"),
            channel_id: graph_property_string(&input.properties, "channel_id"),
            mix_id: graph_property_string(&input.properties, "mix_id"),
            muted: Some(input.mute),
            volume_percent: parse_first_volume_percent(&input.volume),
            sink: Some(input.sink.to_string()).filter(|value| !value.is_empty()),
            sink_name: property_string(&input.properties, "sink.name"),
            target_object: property_string(&input.properties, "target.object"),
        })
        .collect()
}

fn active_playback_sink_from_sink_inputs_json(
    json: &str,
    sink_names_by_index: &BTreeMap<String, String>,
) -> Option<String> {
    let inputs: Vec<PactlSinkInput> = serde_json::from_str(json).unwrap_or_default();
    inputs.into_iter().find_map(|input| {
        if input.mute || is_managed_or_loopback_sink_input(&input.properties) {
            return None;
        }
        let sink = sink_names_by_index.get(&input.sink.to_string())?;
        (!looks_like_wavelinux_family_node(sink)).then(|| sink.clone())
    })
}

fn is_managed_or_loopback_sink_input(properties: &BTreeMap<String, serde_json::Value>) -> bool {
    if graph_property_string(properties, "managed").as_deref() == Some("1") {
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
        Some("channel_to_mix")
        | Some("channel_to_effect")
        | Some("input_to_channel")
        | Some("mix_monitor") => 0,
        Some("mix_source") | Some("mic_passthrough") => 1,
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
            role: graph_property_string(&output.properties, "role"),
            channel_id: graph_property_string(&output.properties, "channel_id"),
            mix_id: graph_property_string(&output.properties, "mix_id"),
            muted: Some(output.mute),
            volume_percent: parse_first_volume_percent(&output.volume),
            source_id: output.source.object_id(),
            source_name: property_string(&output.properties, "source.name"),
            target_object: property_string(&output.properties, "target.object"),
            application_name: property_string(&output.properties, "application.name"),
            node_name: property_string(&output.properties, "node.name"),
            media_name: property_string(&output.properties, "media.name"),
            managed: graph_property_string(&output.properties, "managed"),
            dont_move: property_string(&output.properties, "node.dont-move")
                .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on")),
        })
        .collect()
}

pub fn channel_bus_route_ids_from_routes(
    channel_id: &str,
    mix_id: &str,
    sink_inputs: &[SinkInputRoute],
    source_outputs: &[SourceOutputRoute],
) -> ChannelBusRouteIds {
    ChannelBusRouteIds {
        sink_input_id: sink_inputs
            .iter()
            .find(|input| {
                input.role.as_deref() == Some("channel_to_mix")
                    && input.channel_id.as_deref() == Some(channel_id)
                    && input.mix_id.as_deref() == Some(mix_id)
            })
            .map(|input| input.id.clone()),
        source_output_id: source_outputs
            .iter()
            .find(|output| {
                output.role.as_deref() == Some("channel_to_mix")
                    && output.channel_id.as_deref() == Some(channel_id)
                    && output.mix_id.as_deref() == Some(mix_id)
            })
            .map(|output| output.id.clone()),
    }
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
    _module_name: &str,
    argument: &str,
) -> Option<ManagedModule> {
    if module_id.is_empty() {
        return None;
    }

    let node_name = wavelinux_node_name_from_module_argument(argument);
    let source_name = command_arg_value_from_text(argument, "source=");
    let sink_name = command_arg_value_from_text(argument, "sink=");
    let role = graph_property_value_from_arg(argument, "role").map(ToOwned::to_owned);
    let channel_id = graph_property_value_from_arg(argument, "channel_id").map(ToOwned::to_owned);
    let mix_id = graph_property_value_from_arg(argument, "mix_id").map(ToOwned::to_owned);
    let route_revision =
        graph_property_value_from_arg(argument, "route_revision").map(ToOwned::to_owned);
    let managed_flag = argument_has_wavelinux_managed_flag(argument);
    let managed = node_name
        .as_deref()
        .is_some_and(looks_like_wavelinux_family_node)
        || managed_flag
        || role.is_some()
        || channel_id.is_some()
        || mix_id.is_some();

    managed.then(|| ManagedModule {
        module_id: module_id.to_string(),
        role,
        channel_id,
        mix_id,
        route_revision,
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
    let prefix = graph_prefix();
    command.contains("pipewire")
        && (command.contains(&format!("{prefix}-chain"))
            || command.contains(&format!("{prefix}.fx"))
            || command.contains(&format!("/{prefix}-chain-")))
}

fn wavelinux_node_name_from_module_argument(argument: &str) -> Option<String> {
    let prefixes = ["sink_name=", "source_name=", "source=", "sink=", "master="];
    let values = prefixes
        .iter()
        .filter_map(|prefix| command_arg_value_from_text(argument, prefix))
        .collect::<Vec<_>>();
    values
        .iter()
        .find(|value| looks_like_wavelinux_family_node(value))
        .cloned()
        .or_else(|| values.into_iter().next())
}

fn managed_module_from_parts(
    module_id: Option<String>,
    node_name: Option<String>,
    argument: Option<&str>,
    properties: &BTreeMap<String, serde_json::Value>,
) -> Option<ManagedModule> {
    let role = graph_property_string(properties, "role");
    let channel_id = graph_property_string(properties, "channel_id");
    let mix_id = graph_property_string(properties, "mix_id");
    let route_revision = graph_property_string(properties, "route_revision");
    let argument_node_name = argument.and_then(wavelinux_node_name_from_module_argument);
    let node_name = node_name
        .filter(|value| !value.is_empty())
        .or_else(|| property_string(properties, "node.name"))
        .or(argument_node_name);
    let managed = graph_property_string(properties, "managed").as_deref() == Some("1")
        || role.is_some()
        || node_name
            .as_deref()
            .is_some_and(looks_like_wavelinux_family_node);

    if !managed {
        return None;
    }

    Some(ManagedModule {
        module_id: module_id?,
        role,
        channel_id,
        mix_id,
        route_revision,
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

fn hydrate_source_output_routes_from_sources(
    mut routes: Vec<SourceOutputRoute>,
    source_names_by_id: &BTreeMap<String, String>,
) -> Vec<SourceOutputRoute> {
    for route in &mut routes {
        if route.source_name.is_some() {
            continue;
        }
        let Some(source_id) = route.source_id.as_deref() else {
            continue;
        };
        if let Some(source_name) = source_names_by_id.get(source_id) {
            route.source_name = Some(source_name.clone());
        } else if source_id.contains('.') || starts_with_graph_prefix(source_id) {
            route.source_name = Some(source_id.to_owned());
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
        if route.target_object.is_none() {
            route.target_object = module.sink_name.clone();
        }
    }

    routes
}

fn hydrate_sink_input_routes_from_sinks(
    mut routes: Vec<SinkInputRoute>,
    sink_names_by_id: &BTreeMap<String, String>,
) -> Vec<SinkInputRoute> {
    for route in &mut routes {
        if route.sink_name.is_some() {
            continue;
        }
        let Some(sink_id) = route.sink.as_deref() else {
            continue;
        };
        if let Some(sink_name) = sink_names_by_id.get(sink_id) {
            route.sink_name = Some(sink_name.clone());
        } else if sink_id.contains('.') || starts_with_graph_prefix(sink_id) {
            route.sink_name = Some(sink_id.to_owned());
        }
    }

    routes
}

fn looks_like_wavelinux_node(value: &str) -> bool {
    let value = normalized_node_leaf(value);
    let compact = compact_node_name(&value);
    let prefix = graph_prefix();
    value == prefix
        || compact == prefix
        || value.starts_with(&format!("{prefix}_"))
        || value.starts_with(&format!("{prefix}-"))
        || value.starts_with(&format!("{prefix}."))
        || compact.starts_with(&format!("{prefix}_"))
        || value.starts_with(&format!("output.{prefix}.fx."))
}

fn starts_with_graph_prefix(value: &str) -> bool {
    let prefix = graph_prefix();
    value.starts_with(&format!("{prefix}_")) || value.starts_with(&format!("{prefix}-"))
}

fn looks_like_wavelinux_family_node(value: &str) -> bool {
    looks_like_wavelinux_node(value) || looks_like_legacy_openwave_node(value)
}

fn looks_like_legacy_openwave_node(value: &str) -> bool {
    let value = normalized_node_leaf(value);
    let compact = compact_node_name(&value);
    value == "openwave"
        || compact == "openwave"
        || value.starts_with("openwave_")
        || value.starts_with("openwave-")
        || value.starts_with("openwave.")
        || compact.starts_with("openwave_")
        || value.starts_with("output.openwave.fx.")
}

fn normalized_node_leaf(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn compact_node_name(value: &str) -> String {
    let mut compact = String::with_capacity(value.len());
    let mut last_was_separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            compact.push(ch);
            last_was_separator = false;
        } else if !last_was_separator {
            compact.push('_');
            last_was_separator = true;
        }
    }
    compact.trim_matches('_').to_string()
}

#[derive(Debug, Clone)]
struct RenderedEffectNode {
    left_input: String,
    right_input: String,
    left_output: String,
    right_output: String,
    config: String,
    internal_links: Vec<(String, String)>,
}

impl RenderedEffectNode {
    fn inputs(&self) -> [String; 2] {
        [self.left_input.clone(), self.right_input.clone()]
    }

    fn outputs(&self) -> [String; 2] {
        [self.left_output.clone(), self.right_output.clone()]
    }
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

const PARAM_EQ_STEREO_PORTS: EffectAudioPorts = EffectAudioPorts {
    left_input: "In 1",
    right_input: "In 2",
    left_output: "Out 1",
    right_output: "Out 2",
};

fn render_effect_node(
    effect: &EffectInstance,
    definition: Option<&wavelinux_model::EffectDefinition>,
) -> RenderedEffectNode {
    let Some(definition) = definition else {
        return render_builtin_node(effect, "copy", &[]);
    };

    match effect.effect_id.as_str() {
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
                ("Retroactive VAD Grace (ms)", 0.0),
                ("Dry Mix", effect_param(effect, definition, "dry_mix")),
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
        "gate" => render_ladspa_mono_pair_node(
            effect,
            "gate_1410",
            "gate",
            &[
                ("LF key filter (Hz)", 30.0),
                ("HF key filter (Hz)", 20_000.0),
                (
                    "Threshold (dB)",
                    effect_param(effect, definition, "threshold_db"),
                ),
                ("Attack (ms)", effect_param(effect, definition, "attack_ms")),
                ("Hold (ms)", effect_param(effect, definition, "hold_ms")),
                ("Decay (ms)", effect_param(effect, definition, "release_ms")),
                ("Range (dB)", effect_param(effect, definition, "range_db")),
                ("Output select (-1 = key listen, 0 = gate, 1 = bypass)", 0.0),
            ],
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
        "karaoke_stage" => render_karaoke_stage_node(effect, definition),
        _ => render_builtin_node(effect, "copy", &[]),
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
    let left_input = port_ref(&name, ports.left_input);
    let right_input = port_ref(&name, ports.right_input);
    let left_output = port_ref(&name, ports.left_output);
    let right_output = port_ref(&name, ports.right_output);
    RenderedEffectNode {
        left_input,
        right_input,
        left_output,
        right_output,
        config: rendered,
        internal_links: Vec::new(),
    }
}

fn render_ladspa_mono_pair_node(
    effect: &EffectInstance,
    plugin: &str,
    label: &str,
    controls: &[(&str, f32)],
) -> RenderedEffectNode {
    let base_name = effect_node_name(effect);
    let left_name = format!("{base_name}_left");
    let right_name = format!("{base_name}_right");
    let mut rendered = String::new();
    append_ladspa_node(&mut rendered, plugin, label, &left_name, controls);
    append_ladspa_node(&mut rendered, plugin, label, &right_name, controls);
    RenderedEffectNode {
        left_input: port_ref(&left_name, "Input"),
        right_input: port_ref(&right_name, "Input"),
        left_output: port_ref(&left_name, "Output"),
        right_output: port_ref(&right_name, "Output"),
        config: rendered,
        internal_links: Vec::new(),
    }
}

fn append_ladspa_node(
    rendered: &mut String,
    plugin: &str,
    label: &str,
    name: &str,
    controls: &[(&str, f32)],
) {
    rendered.push_str("          { type = ladspa plugin = \"");
    rendered.push_str(plugin);
    rendered.push_str("\" label = \"");
    rendered.push_str(label);
    rendered.push_str("\" name = \"");
    rendered.push_str(&escape_pw(name));
    rendered.push('"');
    append_control_block(rendered, controls);
    rendered.push_str(" }\n");
}

fn render_karaoke_stage_node(
    effect: &EffectInstance,
    definition: &wavelinux_model::EffectDefinition,
) -> RenderedEffectNode {
    let base = effect_node_name(effect);
    let input_left = format!("{base}_in_left");
    let input_right = format!("{base}_in_right");
    let tone_highpass_left = format!("{base}_tone_highpass_left");
    let tone_highpass_right = format!("{base}_tone_highpass_right");
    let tone_lowpass_left = format!("{base}_tone_lowpass_left");
    let tone_lowpass_right = format!("{base}_tone_lowpass_right");
    let tone_gain_left = format!("{base}_tone_gain_left");
    let tone_gain_right = format!("{base}_tone_gain_right");
    let dry_left = format!("{base}_dry_left");
    let dry_right = format!("{base}_dry_right");
    let pitch_left = format!("{base}_pitch_left");
    let pitch_right = format!("{base}_pitch_right");
    let delay_left = format!("{base}_delay_left");
    let delay_right = format!("{base}_delay_right");
    let double_left = format!("{base}_double_left");
    let double_right = format!("{base}_double_right");
    let room = format!("{base}_room");
    let mix_left = format!("{base}_mix_left");
    let mix_right = format!("{base}_mix_right");

    let dry_mix = effect_param(effect, definition, "dry_mix");
    let tone_highpass_hz = effect_param(effect, definition, "tone_highpass_hz");
    let tone_lowpass_hz = effect_param(effect, definition, "tone_lowpass_hz");
    let tone_gain = db_to_linear(effect_param(effect, definition, "tone_gain_db"));
    let double_mix = effect_param(effect, definition, "double_mix");
    let double_delay_s = effect_param(effect, definition, "double_delay_ms") / 1000.0;
    let right_delay_s = (double_delay_s + 0.012).min(0.12);
    let detune_cents = effect_param(effect, definition, "detune_cents");
    let pitch_down = 2.0_f32.powf(-detune_cents / 1200.0);
    let pitch_up = 2.0_f32.powf(detune_cents / 1200.0);
    let room_level = effect_param(effect, definition, "room_level_db");
    let tail_level = (room_level - 6.0).max(-70.0);

    let mut rendered = String::new();
    append_builtin_node(
        &mut rendered,
        "linear",
        &input_left,
        &[("Mult", 1.0), ("Add", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "linear",
        &input_right,
        &[("Mult", 1.0), ("Add", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "bq_highpass",
        &tone_highpass_left,
        &[("Freq", tone_highpass_hz), ("Q", 0.707), ("Gain", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "bq_highpass",
        &tone_highpass_right,
        &[("Freq", tone_highpass_hz), ("Q", 0.707), ("Gain", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "bq_lowpass",
        &tone_lowpass_left,
        &[("Freq", tone_lowpass_hz), ("Q", 0.707), ("Gain", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "bq_lowpass",
        &tone_lowpass_right,
        &[("Freq", tone_lowpass_hz), ("Q", 0.707), ("Gain", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "linear",
        &tone_gain_left,
        &[("Mult", tone_gain), ("Add", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "linear",
        &tone_gain_right,
        &[("Mult", tone_gain), ("Add", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "linear",
        &dry_left,
        &[("Mult", dry_mix), ("Add", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "linear",
        &dry_right,
        &[("Mult", dry_mix), ("Add", 0.0)],
    );
    append_ladspa_node(
        &mut rendered,
        "pitch_scale_1193",
        "pitchScale",
        &pitch_left,
        &[("Pitch co-efficient", pitch_down)],
    );
    append_ladspa_node(
        &mut rendered,
        "pitch_scale_1193",
        "pitchScale",
        &pitch_right,
        &[("Pitch co-efficient", pitch_up)],
    );
    append_builtin_node_with_config(
        &mut rendered,
        "delay",
        &delay_left,
        &[("\"max-delay\"", 1.0)],
        &[("Delay (s)", double_delay_s)],
    );
    append_builtin_node_with_config(
        &mut rendered,
        "delay",
        &delay_right,
        &[("\"max-delay\"", 1.0)],
        &[("Delay (s)", right_delay_s)],
    );
    append_builtin_node(
        &mut rendered,
        "linear",
        &double_left,
        &[("Mult", double_mix), ("Add", 0.0)],
    );
    append_builtin_node(
        &mut rendered,
        "linear",
        &double_right,
        &[("Mult", double_mix), ("Add", 0.0)],
    );
    append_ladspa_node(
        &mut rendered,
        "gverb_1216",
        "gverb",
        &room,
        &[
            (
                "Roomsize (m)",
                effect_param(effect, definition, "room_size_m"),
            ),
            (
                "Reverb time (s)",
                effect_param(effect, definition, "reverb_time_s"),
            ),
            ("Damping", 0.45),
            ("Input bandwidth", 0.75),
            ("Dry signal level (dB)", -70.0),
            ("Early reflection level (dB)", room_level),
            ("Tail level (dB)", tail_level),
        ],
    );
    append_builtin_node(&mut rendered, "mixer", &mix_left, &[]);
    append_builtin_node(&mut rendered, "mixer", &mix_right, &[]);

    let internal_links = vec![
        (
            port_ref(&input_left, "Out"),
            port_ref(&tone_highpass_left, "In"),
        ),
        (
            port_ref(&input_right, "Out"),
            port_ref(&tone_highpass_right, "In"),
        ),
        (
            port_ref(&tone_highpass_left, "Out"),
            port_ref(&tone_lowpass_left, "In"),
        ),
        (
            port_ref(&tone_highpass_right, "Out"),
            port_ref(&tone_lowpass_right, "In"),
        ),
        (
            port_ref(&tone_lowpass_left, "Out"),
            port_ref(&tone_gain_left, "In"),
        ),
        (
            port_ref(&tone_lowpass_right, "Out"),
            port_ref(&tone_gain_right, "In"),
        ),
        (port_ref(&tone_gain_left, "Out"), port_ref(&dry_left, "In")),
        (
            port_ref(&tone_gain_right, "Out"),
            port_ref(&dry_right, "In"),
        ),
        (
            port_ref(&tone_gain_left, "Out"),
            port_ref(&pitch_left, "Input"),
        ),
        (
            port_ref(&tone_gain_right, "Out"),
            port_ref(&pitch_right, "Input"),
        ),
        (port_ref(&pitch_left, "Output"), port_ref(&delay_left, "In")),
        (
            port_ref(&pitch_right, "Output"),
            port_ref(&delay_right, "In"),
        ),
        (port_ref(&delay_left, "Out"), port_ref(&double_left, "In")),
        (port_ref(&delay_right, "Out"), port_ref(&double_right, "In")),
        (port_ref(&tone_gain_left, "Out"), port_ref(&room, "Input")),
        (port_ref(&dry_left, "Out"), port_ref(&mix_left, "In 1")),
        (port_ref(&dry_right, "Out"), port_ref(&mix_right, "In 1")),
        (port_ref(&double_right, "Out"), port_ref(&mix_left, "In 2")),
        (port_ref(&double_left, "Out"), port_ref(&mix_right, "In 2")),
        (port_ref(&room, "Left output"), port_ref(&mix_left, "In 3")),
        (
            port_ref(&room, "Right output"),
            port_ref(&mix_right, "In 3"),
        ),
    ];

    RenderedEffectNode {
        left_input: port_ref(&input_left, "In"),
        right_input: port_ref(&input_right, "In"),
        left_output: port_ref(&mix_left, "Out"),
        right_output: port_ref(&mix_right, "Out"),
        config: rendered,
        internal_links,
    }
}

fn render_builtin_node(
    effect: &EffectInstance,
    label: &str,
    controls: &[(&str, f32)],
) -> RenderedEffectNode {
    render_builtin_stereo_pair_node(&effect_node_name(effect), label, controls)
}

fn render_builtin_stereo_pair_node(
    base_name: &str,
    label: &str,
    controls: &[(&str, f32)],
) -> RenderedEffectNode {
    let left_name = format!("{base_name}_left");
    let right_name = format!("{base_name}_right");
    let mut rendered = String::new();
    append_builtin_node(&mut rendered, label, &left_name, controls);
    append_builtin_node(&mut rendered, label, &right_name, controls);
    RenderedEffectNode {
        left_input: port_ref(&left_name, BUILTIN_STEREO_PORTS.left_input),
        right_input: port_ref(&right_name, BUILTIN_STEREO_PORTS.right_input),
        left_output: port_ref(&left_name, BUILTIN_STEREO_PORTS.left_output),
        right_output: port_ref(&right_name, BUILTIN_STEREO_PORTS.right_output),
        config: rendered,
        internal_links: Vec::new(),
    }
}

fn append_builtin_node(rendered: &mut String, label: &str, name: &str, controls: &[(&str, f32)]) {
    append_builtin_node_with_config(rendered, label, name, &[], controls);
}

fn append_builtin_node_with_config(
    rendered: &mut String,
    label: &str,
    name: &str,
    config: &[(&str, f32)],
    controls: &[(&str, f32)],
) {
    rendered.push_str("          { type = builtin label = \"");
    rendered.push_str(label);
    rendered.push_str("\" name = \"");
    rendered.push_str(&escape_pw(name));
    rendered.push('"');
    if !config.is_empty() {
        rendered.push_str(" config = {");
        for (key, value) in config {
            rendered.push(' ');
            rendered.push_str(key);
            rendered.push_str(" = ");
            rendered.push_str(&format!("{:.3}", value));
        }
        rendered.push_str(" }");
    }
    append_control_block(rendered, controls);
    rendered.push_str(" }\n");
}

fn render_param_eq_node(
    effect: &EffectInstance,
    definition: &wavelinux_model::EffectDefinition,
) -> RenderedEffectNode {
    let name = effect_node_name(effect);
    let filters = [
        (
            "bq_lowshelf",
            63.0,
            effect_param(effect, definition, "band_63_gain_db"),
            0.707,
        ),
        (
            "bq_peaking",
            125.0,
            effect_param(effect, definition, "band_125_gain_db"),
            1.0,
        ),
        (
            "bq_peaking",
            250.0,
            effect_param(effect, definition, "band_250_gain_db"),
            1.0,
        ),
        (
            "bq_peaking",
            500.0,
            effect_param(effect, definition, "band_500_gain_db"),
            1.0,
        ),
        (
            "bq_peaking",
            1000.0,
            effect_param(effect, definition, "band_1k_gain_db"),
            1.0,
        ),
        (
            "bq_peaking",
            2000.0,
            effect_param(effect, definition, "band_2k_gain_db"),
            1.0,
        ),
        (
            "bq_peaking",
            4000.0,
            effect_param(effect, definition, "band_4k_gain_db"),
            1.0,
        ),
        (
            "bq_highshelf",
            8000.0,
            effect_param(effect, definition, "band_8k_gain_db"),
            0.707,
        ),
    ];

    let mut rendered = String::new();
    rendered.push_str("          { type = builtin label = \"param_eq\" name = \"");
    rendered.push_str(&escape_pw(&name));
    rendered.push_str("\" config = {\n");
    append_param_eq_filters(&mut rendered, "filters1", &filters);
    append_param_eq_filters(&mut rendered, "filters2", &filters);
    rendered.push_str("          } }\n");
    RenderedEffectNode {
        left_input: port_ref(&name, PARAM_EQ_STEREO_PORTS.left_input),
        right_input: port_ref(&name, PARAM_EQ_STEREO_PORTS.right_input),
        left_output: port_ref(&name, PARAM_EQ_STEREO_PORTS.left_output),
        right_output: port_ref(&name, PARAM_EQ_STEREO_PORTS.right_output),
        config: rendered,
        internal_links: Vec::new(),
    }
}

fn append_param_eq_filters(rendered: &mut String, key: &str, filters: &[(&str, f32, f32, f32)]) {
    rendered.push_str("            ");
    rendered.push_str(key);
    rendered.push_str(" = [");
    for (kind, freq, gain, q) in filters {
        rendered.push_str(" { type = ");
        rendered.push_str(kind);
        rendered.push_str(" freq = ");
        rendered.push_str(&format!("{:.3}", freq));
        rendered.push_str(" gain = ");
        rendered.push_str(&format!("{:.3}", gain));
        rendered.push_str(" q = ");
        rendered.push_str(&format!("{:.3}", q));
        rendered.push_str(" }");
    }
    rendered.push_str(" ]\n");
}

fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
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
    if target.left_input == target.right_input {
        append_filter_link(rendered, &source.left_output, &target.left_input);
        return;
    }
    if source.left_output == source.right_output {
        append_filter_link(rendered, &source.left_output, &target.left_input);
        append_filter_link(rendered, &source.left_output, &target.right_input);
        return;
    }

    let left = (&source.left_output, &target.left_input);
    let right = (&source.right_output, &target.right_input);
    append_filter_link(rendered, left.0, left.1);
    if right != left {
        append_filter_link(rendered, right.0, right.1);
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
    parse_first_volume_percent(volume).map(|percent| f32::from(percent) / 100.0)
}

fn parse_first_volume_percent(volume: &BTreeMap<String, PactlVolumeEntry>) -> Option<u8> {
    volume.values().next().and_then(|entry| {
        entry
            .value_percent
            .trim_end_matches('%')
            .parse::<f32>()
            .ok()
            .map(|percent| percent.round().clamp(0.0, 150.0) as u8)
    })
}

fn property_string(map: &BTreeMap<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn string_properties(map: &BTreeMap<String, serde_json::Value>) -> BTreeMap<String, String> {
    map.iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| (key.clone(), value.to_string()))
        })
        .collect()
}

fn detect_device_bus(
    node_name: &str,
    properties: &BTreeMap<String, serde_json::Value>,
    is_virtual: bool,
) -> Option<DeviceBus> {
    if is_virtual {
        return Some(DeviceBus::Virtual);
    }
    let mut values = vec![node_name.to_string()];
    for key in [
        "device.bus",
        "device.api",
        "device.string",
        "alsa.card_name",
        "device.description",
    ] {
        if let Some(value) = property_string(properties, key) {
            values.push(value);
        }
    }
    let text = values.join(" ").to_ascii_lowercase();

    if text.contains("bluez") || text.contains("bluetooth") {
        Some(DeviceBus::Bluetooth)
    } else if text.contains("usb") {
        Some(DeviceBus::Usb)
    } else if text.contains("pci") || text.contains("hda") {
        Some(DeviceBus::Pci)
    } else if text.contains("platform") || text.contains("sof") {
        Some(DeviceBus::Platform)
    } else {
        Some(DeviceBus::Unknown)
    }
}

fn normalize_hex_id(value: &str) -> String {
    let raw = value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("usb:")
        .trim_start_matches("pci:")
        .trim_start_matches("USB:")
        .trim_start_matches("PCI:");
    raw.chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase()
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

fn argument_has_wavelinux_managed_flag(argument: &str) -> bool {
    argument.split_whitespace().any(|part| {
        graph_property_value_from_arg(part, "managed") == Some("1")
            || part
                .strip_prefix("sink_input_properties=")
                .is_some_and(|properties| {
                    graph_property_value_from_arg(properties, "managed") == Some("1")
                })
            || part
                .strip_prefix("source_output_properties=")
                .is_some_and(|properties| {
                    graph_property_value_from_arg(properties, "managed") == Some("1")
                })
    })
}

fn wavelinux_display_name(value: &str) -> String {
    let prefix = graph_prefix();
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
    if slug.is_empty() || slug == prefix {
        return format!("{prefix}-source");
    }
    if slug.starts_with(&format!("{prefix}-")) {
        slug.into()
    } else {
        format!("{prefix}-{slug}")
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

fn command_output_with_timeout(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<Output, PwError> {
    let mut child = host_command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                PwError::CommandNotFound(program.into())
            } else {
                PwError::Io(err.to_string())
            }
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PwError::Io("failed to capture command stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| PwError::Io("failed to capture command stderr".into()))?;
    let stdout_reader = spawn_pipe_reader(stdout);
    let stderr_reader = spawn_pipe_reader(stderr);
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| PwError::Io(err.to_string()))?
        {
            let stdout = join_pipe_reader(stdout_reader)?;
            let stderr = join_pipe_reader(stderr_reader)?;
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = join_pipe_reader(stdout_reader);
            let _ = join_pipe_reader(stderr_reader);
            return Err(PwError::CommandTimedOut {
                program: program.into(),
                args: args.to_vec(),
                timeout_ms: timeout.as_millis(),
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_pipe_reader<R>(mut reader: R) -> thread::JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output)?;
        Ok(output)
    })
}

fn join_pipe_reader(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<Vec<u8>, PwError> {
    handle
        .join()
        .map_err(|_| PwError::Io("command pipe reader panicked".into()))?
        .map_err(|err| PwError::Io(err.to_string()))
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

fn host_command(program: &str) -> Command {
    let mut command = Command::new(program);
    sanitize_host_command_env(&mut command);
    command
}

fn sanitize_host_command_env(command: &mut Command) {
    for key in HOST_COMMAND_ENV_REMOVE {
        command.env_remove(key);
    }
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
    let mut seen = BTreeSet::new();
    roots.retain(|root| seen.insert(root.clone()));
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavelinux_model::{ChannelInputMode, ChannelKind, MixerConfig, MixerSettings};

    fn assert_managed_loopback_disables_stream_restore(spec: &CommandSpec) {
        for prefix in ["source_output_properties=", "sink_input_properties="] {
            let properties = spec
                .args
                .iter()
                .find_map(|arg| arg.strip_prefix(prefix))
                .unwrap_or_else(|| panic!("missing {prefix} in {:?}", spec.args));
            assert!(
                properties.contains("wavelinux.managed=1"),
                "{prefix}{properties}"
            );
            assert!(
                properties.contains("application.name=WaveLinux"),
                "{prefix}{properties}"
            );
            assert!(
                properties.contains("media.name=wavelinux-route-"),
                "{prefix}{properties}"
            );
            assert!(
                properties.contains("node.dont-move=true"),
                "{prefix}{properties}"
            );
            assert!(
                properties.contains("state.restore-props=false"),
                "{prefix}{properties}"
            );
            assert!(
                properties.contains("state.restore-target=false"),
                "{prefix}{properties}"
            );
        }
    }

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
            .any(|command| command.description.contains("route 'Input' to 'Monitor'")));
        assert!(plan
            .commands
            .iter()
            .any(|command| command.description == "expose 'Input' as public mic source"));
        assert!(plan.managed_nodes.contains(&"wavelinux-mic".into()));
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
            .any(|arg| arg.contains("device.description=wavelinux-input")));
    }

    #[test]
    fn wavelinux6_core_replaces_channel_null_sinks_effect_and_input_loopbacks() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();
        config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap()
            .source_device = Some("alsa_input.usb-test-mic.mono-fallback".into());

        let plan = plan_ensure_graph(&config);
        let channel = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();

        assert_eq!(effect_chain_input_name(channel), channel.virtual_sink_name);
        assert!(!plan.commands.iter().any(|command| {
            command
                .args
                .iter()
                .any(|argument| argument == &format!("sink_name={}", channel.virtual_sink_name))
        }));
        assert!(!plan.commands.iter().any(|command| {
            command
                .args
                .iter()
                .any(|argument| argument.contains("wavelinux6.role=channel_to_effect"))
        }));
        assert!(!plan.commands.iter().any(|command| {
            command.args.iter().any(|argument| {
                argument.contains("wavelinux6.role=input_to_channel")
                    || argument.contains("wavelinux6.role=mix_monitor")
            })
        }));
    }

    #[test]
    fn display_names_are_wavelinux_prefixed_and_space_free() {
        assert_eq!(
            wavelinux_display_name("Discord Mix"),
            "wavelinux-discord-mix"
        );
        assert_eq!(wavelinux_display_name("Input"), "wavelinux-input");
        assert_eq!(
            wavelinux_display_name("wavelinux-stream"),
            "wavelinux-stream"
        );
        assert_eq!(wavelinux_display_name(""), "wavelinux-source");
        assert!(!wavelinux_display_name("Music Browser").contains(' '));
    }

    #[test]
    fn meters_default_on_when_available() {
        assert!(meter_sampling_enabled_from_env(None, None, true));
        assert!(!meter_sampling_enabled_from_env(None, None, false));
        assert!(!meter_sampling_enabled_from_env(Some("0"), None, true));
        assert!(!meter_sampling_enabled_from_env(Some("false"), None, true));
        assert!(!meter_sampling_enabled_from_env(Some("1"), Some("1"), true));
        assert!(meter_sampling_enabled_from_env(Some("1"), Some("0"), true));
    }

    #[test]
    fn command_output_with_timeout_drains_large_stdout_while_waiting() {
        let args = vec!["-c".into(), "yes 0123456789abcdef | head -c 200000".into()];

        let output = command_output_with_timeout("sh", &args, Duration::from_secs(2)).unwrap();

        assert!(output.status.success());
        assert!(output.stdout.len() >= 200_000);
    }

    #[test]
    fn meter_targets_follow_available_wavelinux_sources() {
        let mut config = MixerConfig::default();
        config.set_mix_volume("stream", 0.5).unwrap();
        config.set_mix_mute("monitor", true).unwrap();
        config.set_channel_volume("game", "stream", 0.42).unwrap();
        config.set_channel_mute("chat", "monitor", true).unwrap();
        let available_sources = BTreeSet::from([
            "wavelinux_mix_monitor.monitor".to_string(),
            "wavelinux_mix_stream.monitor".to_string(),
            "wavelinux_channel_game.monitor".to_string(),
            "wavelinux_channel_chat.monitor".to_string(),
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
            .any(|target| target.node_id.ends_with(":raw")));
        assert!(targets.iter().any(|target| target.node_id
            == channel_bus_meter_id("game", "stream")
            && target.source_name == "wavelinux_channel_game.monitor"
            && (target.gain - 0.21).abs() < f32::EPSILON
            && !target.muted));
        assert!(targets.iter().any(|target| target.node_id
            == channel_bus_meter_id("game", "monitor")
            && target.source_name == "wavelinux_channel_game.monitor"
            && target.muted));
        assert!(targets.iter().any(|target| target.node_id
            == channel_bus_meter_id("chat", "monitor")
            && target.source_name == "wavelinux_channel_chat.monitor"
            && target.muted));
        assert!(!targets
            .iter()
            .any(|target| target.source_name == "alsa_input.real"));
    }

    #[test]
    fn hardware_input_meters_use_selected_microphone_without_effects() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.source_device = Some("alsa_input.real".into());
        let available_sources = BTreeSet::from([
            "alsa_input.real".into(),
            "wavelinux_channel_hardware_in.monitor".into(),
        ]);

        let targets = meter_targets_for_config(&config, &available_sources);

        assert!(targets.iter().any(|target| {
            target.node_id == "hardware_in" && target.source_name == "alsa_input.real"
        }));
        assert!(!targets
            .iter()
            .any(|target| target.node_id.ends_with(":raw")));
        assert!(targets.iter().any(|target| {
            target.node_id == channel_bus_meter_id("hardware_in", "stream")
                && target.source_name == "alsa_input.real"
        }));
        assert!(!targets.iter().any(|target| {
            target.node_id == "hardware_in"
                && target.source_name == "wavelinux_channel_hardware_in.monitor"
        }));
    }

    #[test]
    fn hardware_input_meters_use_raw_source_until_fx_source_is_available() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.source_device = Some("alsa_input.real".into());
        hardware.effects = vec![EffectInstance::new("limiter")];
        let available_sources = BTreeSet::from([
            "alsa_input.real".into(),
            "wavelinux_channel_hardware_in.monitor".into(),
        ]);

        let targets = meter_targets_for_config(&config, &available_sources);

        assert!(!targets
            .iter()
            .any(|target| target.node_id.ends_with(":raw")));
        assert!(targets.iter().any(|target| {
            target.node_id == "hardware_in" && target.source_name == "alsa_input.real"
        }));
        assert!(targets.iter().any(|target| {
            target.node_id == channel_bus_meter_id("hardware_in", "stream")
                && target.source_name == "alsa_input.real"
        }));
    }

    #[test]
    fn hardware_input_meters_follow_effect_source_when_available() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.source_device = Some("alsa_input.real".into());
        hardware.effects = vec![EffectInstance::new("limiter")];
        let available_sources = BTreeSet::from([
            "alsa_input.real".into(),
            "wavelinux_channel_hardware_in.monitor".into(),
            "wavelinux-mic".into(),
        ]);

        let targets = meter_targets_for_config(&config, &available_sources);

        assert!(targets.iter().any(|target| {
            target.node_id == "hardware_in" && target.source_name == "wavelinux-mic"
        }));
        assert!(!targets
            .iter()
            .any(|target| target.node_id.ends_with(":raw")));
        assert!(targets.iter().any(|target| {
            target.node_id == channel_bus_meter_id("hardware_in", "stream")
                && target.source_name == "wavelinux-mic"
        }));
    }

    #[test]
    fn hardware_input_meters_follow_effect_source_by_pipewire_metadata() {
        let mut config = MixerConfig::default();
        let hardware = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        hardware.source_device = Some("alsa_input.real".into());
        hardware.effects = vec![EffectInstance::new("limiter")];
        let sources = vec![
            test_source("alsa_input.real", BTreeMap::new()),
            test_source("wavelinux_channel_hardware_in.monitor", BTreeMap::new()),
            test_source(
                "output.wavelinux.fx.randomized.source",
                BTreeMap::from([
                    ("wavelinux.role".into(), "effect_output".into()),
                    ("wavelinux.channel_id".into(), "hardware_in".into()),
                ]),
            ),
        ];

        let targets = meter_targets_for_config_with_devices(&config, &sources);

        assert!(targets.iter().any(|target| {
            target.node_id == "hardware_in"
                && target.source_name == "output.wavelinux.fx.randomized.source"
        }));
        assert!(!targets
            .iter()
            .any(|target| target.node_id.ends_with(":raw")));
        assert!(targets.iter().any(|target| {
            target.node_id == channel_bus_meter_id("hardware_in", "stream")
                && target.source_name == "output.wavelinux.fx.randomized.source"
        }));
    }

    #[test]
    fn app_channel_and_bus_meters_follow_effect_source_when_available() {
        let mut config = MixerConfig::default();
        let music = config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "music")
            .unwrap();
        music.effects = vec![EffectInstance::new("limiter")];
        let available_sources = BTreeSet::from([
            "wavelinux_channel_music.monitor".into(),
            "wavelinux_fx_music_source".into(),
        ]);

        let targets = meter_targets_for_config(&config, &available_sources);

        assert!(targets.iter().any(|target| {
            target.node_id == "music" && target.source_name == "wavelinux_fx_music_source"
        }));
        assert!(!targets
            .iter()
            .any(|target| target.node_id.ends_with(":raw")));
        assert!(targets.iter().any(|target| {
            target.node_id == channel_bus_meter_id("music", "stream")
                && target.source_name == "wavelinux_fx_music_source"
        }));
    }

    #[test]
    fn move_stream_targets_channel_sink() {
        let channel = Channel::new_fixed("discord", "Discord", ChannelKind::Application);
        let spec = plan_move_app_stream("42", &channel);
        assert_eq!(spec.program, "pactl");
        assert_eq!(spec.args[2], "wavelinux_channel_discord");
    }

    #[test]
    fn native_stream_move_uses_target_object_metadata() {
        let spec = plan_move_native_app_stream(72, "991", "wavelinux_channel_music");
        assert_eq!(spec.program, "pw-metadata");
        assert_eq!(
            spec.args,
            ["-n", "default", "72", "target.object", "991", "Spa:Id"]
        );
    }

    #[test]
    fn native_stream_volume_uses_wireplumber_node_control() {
        let spec = plan_set_native_stream_volume(72, 0.75);
        assert_eq!(spec.program, "wpctl");
        assert_eq!(spec.args, ["set-volume", "72", "0.75"]);
    }

    #[test]
    fn native_capture_move_uses_target_object_metadata() {
        let spec = plan_move_native_capture_stream(73, "992", "wavelinux6-mic");
        assert_eq!(spec.program, "pw-metadata");
        assert_eq!(
            spec.args,
            ["-n", "default", "73", "target.object", "992", "Spa:Id"]
        );
    }

    #[test]
    fn move_stream_to_default_targets_default_sink() {
        let spec = plan_move_app_stream_to_default("42");
        assert_eq!(spec.program, "pactl");
        assert_eq!(spec.args[0], "move-sink-input");
        assert_eq!(spec.args[2], "@DEFAULT_SINK@");
    }

    fn test_source(name: &str, pipewire_properties: BTreeMap<String, String>) -> DeviceInfo {
        DeviceInfo {
            id: name.into(),
            index: None,
            name: name.into(),
            description: name.into(),
            is_available: true,
            active_port: None,
            ports: Vec::new(),
            is_default: false,
            is_virtual: name.contains("wavelinux"),
            bus: None,
            vendor_id: None,
            product_id: None,
            alsa_card: None,
            alsa_device: None,
            driver: None,
            bluetooth_modalias: None,
            active_profile: None,
            active_codec: None,
            pipewire_properties,
            matched_profile_id: None,
            matched_profile_source: None,
            profile_confidence: None,
            active_latency_policy: None,
            active_routing_policy: None,
            active_bluetooth_mic_policy: None,
        }
    }

    #[test]
    fn move_capture_stream_targets_source() {
        let spec = plan_move_capture_stream_to_source("99", "wavelinux_mix_stream_source");
        assert_eq!(spec.program, "pactl");
        assert_eq!(
            spec.args,
            ["move-source-output", "99", "wavelinux_mix_stream_source"]
        );
    }

    #[test]
    fn fast_stream_snapshot_only_needs_client_fallback_without_identity() {
        let mut stream = AppStream {
            id: "42".into(),
            app_id: None,
            binary: None,
            process_name: None,
            window_class: None,
            display_name: "Stream 42".into(),
            media_name: Some("Playback".into()),
            routed_channel_id: None,
            volume: 1.0,
            muted: false,
        };
        assert!(app_stream_needs_client_properties(&stream));

        stream.binary = Some("brave".into());
        assert!(!app_stream_needs_client_properties(&stream));
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
    fn parses_bluetooth_cards_and_plans_a2dp_profile_restore() {
        let cards = parse_bluetooth_audio_cards_json(
            r#"
            [
              {
                "name": "bluez_card.AC_80_0A_72_BD_10",
                "properties": {
                  "device.bus": "bluetooth",
                  "api.bluez5.address": "AC:80:0A:72:BD:10"
                },
                "active_profile": "headset-head-unit",
                "profiles": {
                  "off": {"description": "Off", "sinks": 0, "priority": 0, "available": true},
                  "a2dp-sink-sbc": {"description": "High Fidelity Playback (A2DP Sink, codec SBC)", "sinks": 1, "priority": 132, "available": true},
                  "a2dp-sink-aac": {"description": "High Fidelity Playback (A2DP Sink, codec AAC)", "sinks": 1, "priority": 133, "available": true},
                  "a2dp-sink": {"description": "High Fidelity Playback (A2DP Sink, codec LDAC)", "sinks": 1, "priority": 134, "available": true},
                  "headset-head-unit": {"description": "Headset Head Unit (HSP/HFP, codec MSBC)", "sinks": 1, "priority": 7, "available": true}
                }
              }
            ]
            "#,
        );

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].device_key, "AC_80_0A_72_BD_10");
        assert_eq!(
            cards[0].preferred_a2dp_profile.as_deref(),
            Some("a2dp-sink-aac")
        );
        assert!(!cards[0].a2dp_active());

        let commands = plan_bluetooth_a2dp_profiles(&cards, &BTreeMap::new(), true);
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
    fn bluetooth_a2dp_restore_uses_ldac_when_stable_codecs_are_unavailable() {
        let cards = parse_bluetooth_audio_cards_json(
            r#"
            [
              {
                "name": "bluez_card.AC_80_0A_72_BD_10",
                "properties": {
                  "device.bus": "bluetooth",
                  "api.bluez5.address": "AC:80:0A:72:BD:10"
                },
                "active_profile": "headset-head-unit",
                "profiles": {
                  "a2dp-sink-sbc": {"description": "High Fidelity Playback (A2DP Sink, codec SBC)", "sinks": 1, "priority": 132, "available": true},
                  "a2dp-sink": {"description": "High Fidelity Playback (A2DP Sink, codec LDAC)", "sinks": 1, "priority": 134, "available": true}
                }
              }
            ]
            "#,
        );

        assert_eq!(
            cards[0].preferred_a2dp_profile.as_deref(),
            Some("a2dp-sink")
        );
    }

    #[test]
    fn codec_preference_matching_handles_descriptions_and_variant_boundaries() {
        assert!(profile_matches_codec_name(
            "a2dp-sink",
            "High Fidelity Playback (A2DP Sink, codec aptX Adaptive)",
            "aptx_adaptive"
        ));
        assert!(!profile_matches_codec_name(
            "a2dp-sink-aptx_hd",
            "High Fidelity Playback (A2DP Sink, codec aptX HD)",
            "aptx"
        ));
        assert!(profile_matches_codec_name(
            "a2dp-sink",
            "High Fidelity Playback (A2DP Sink, codec SBC XQ)",
            "sbc_xq"
        ));
        assert!(!profile_matches_codec_name(
            "a2dp-sink-sbc_xq",
            "High Fidelity Playback (A2DP Sink, codec SBC XQ)",
            "sbc"
        ));
    }

    #[test]
    fn bluetooth_cards_already_on_preferred_a2dp_do_not_plan_profile_changes() {
        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("a2dp-sink-aac".into()),
            preferred_a2dp_profile: Some("a2dp-sink-aac".into()),
            profiles: Vec::new(),
        }];

        assert!(cards[0].a2dp_active());
        assert!(cards[0].preferred_a2dp_active());
        assert!(plan_bluetooth_a2dp_profiles(&cards, &BTreeMap::new(), true).is_empty());
    }

    #[test]
    fn bluetooth_cards_switch_from_ldac_a2dp_to_preferred_stable_a2dp() {
        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("a2dp-sink".into()),
            preferred_a2dp_profile: Some("a2dp-sink-aac".into()),
            profiles: Vec::new(),
        }];

        assert!(cards[0].a2dp_active());
        assert!(!cards[0].preferred_a2dp_active());
        let commands = plan_bluetooth_a2dp_profiles(&cards, &BTreeMap::new(), true);
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
    fn bluetooth_background_repair_skips_initialized_a2dp_cards() {
        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("a2dp-sink".into()),
            preferred_a2dp_profile: Some("a2dp-sink-aac".into()),
            profiles: Vec::new(),
        }];
        let initialized = BTreeMap::from([(
            "bluez_card.AC_80_0A_72_BD_10".to_string(),
            "a2dp-sink-aac".to_string(),
        )]);

        assert!(cards[0].a2dp_active());
        assert!(plan_bluetooth_a2dp_profiles(&cards, &initialized, false).is_empty());
    }

    #[test]
    fn bluetooth_background_repair_initializes_new_a2dp_cards_to_preferred_profile() {
        let cards = vec![BluetoothAudioCard {
            name: "bluez_card.AC_80_0A_72_BD_10".into(),
            device_key: "AC_80_0A_72_BD_10".into(),
            active_profile: Some("a2dp-sink".into()),
            preferred_a2dp_profile: Some("a2dp-sink-aac".into()),
            profiles: Vec::new(),
        }];

        let commands = plan_bluetooth_a2dp_profiles(&cards, &BTreeMap::new(), false);

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
    fn input_route_targets_channel_sink() {
        let channel = Channel::new_fixed("mic", "Mic", ChannelKind::Microphone);
        let settings = MixerSettings::default();
        let spec = plan_route_input_to_channel(&channel, "alsa_input.usb_mic", &settings)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(spec.program, "pactl");
        assert!(spec.args.contains(&"source=alsa_input.usb_mic".into()));
        assert!(spec.args.contains(&"sink=wavelinux_channel_mic".into()));
        assert!(spec.args.contains(&"channels=1".into()));
        assert!(spec.args.contains(&"channel_map=mono".into()));
        assert!(spec.args.contains(&"adjust_time=0".into()));
        assert!(spec.args.contains(&"remix=yes".into()));
        assert!(spec
            .args
            .iter()
            .any(|arg| arg.contains("wavelinux.role=input_to_channel")));
        assert!(spec
            .args
            .iter()
            .any(|arg| arg.contains("wavelinux.route_revision=5-latency-60")));
        assert_managed_loopback_disables_stream_restore(&spec);
    }

    #[test]
    fn input_route_uses_selected_input_mode() {
        let mut channel = Channel::new_fixed("capture_card", "Capture Card", ChannelKind::Generic);
        let settings = MixerSettings::default();
        channel.input_mode = ChannelInputMode::SumMono;
        let spec = plan_route_input_to_channel(&channel, "alsa_input.capture", &settings)
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
        let spec = plan_route_input_to_channel(&channel, "alsa_input.capture", &settings)
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
        let mut channel = Channel::new_fixed("hardware_in", "Input", ChannelKind::Generic);
        channel.effects.push(EffectInstance::new("limiter"));
        let mix = Mix::new_fixed("stream", "Stream");
        let settings = MixerSettings::default();
        let spec = plan_route_channel_to_mix(&channel, &mix, &settings)
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(channel_mix_source_name(&channel), "wavelinux-mic");
        assert!(spec.args.contains(&"source=wavelinux-mic".into()));
        assert!(spec.args.contains(&"sink=wavelinux_mix_stream".into()));
    }

    #[test]
    fn inactive_hardware_fx_still_exposes_stable_public_mic_source() {
        let channel = Channel::new_fixed("hardware_in", "Input", ChannelKind::Generic);
        let spec = plan_ensure_passthrough_mic_source(&channel)
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(channel_mix_source_name(&channel), "wavelinux-mic");
        assert_eq!(spec.args[1], "module-remap-source");
        assert!(spec
            .args
            .contains(&"master=wavelinux_channel_hardware_in.monitor".into()));
        assert!(spec.args.contains(&"source_name=wavelinux-mic".into()));
        assert!(spec
            .args
            .iter()
            .any(|arg| arg.contains("wavelinux.role=mic_passthrough")));
    }

    #[test]
    fn active_effects_route_channel_monitor_into_fx_input_sink() {
        let mut channel = Channel::new_fixed("hardware_in", "Input", ChannelKind::Generic);
        channel.effects.push(EffectInstance::new("limiter"));
        let settings = MixerSettings::default();

        let spec = plan_route_channel_to_effect(&channel, &settings)
            .into_iter()
            .next()
            .unwrap();

        assert!(spec
            .args
            .contains(&"source=wavelinux_channel_hardware_in.monitor".into()));
        assert!(spec
            .args
            .contains(&"sink=wavelinux_fx_hardware_in_input".into()));
        assert!(spec.args.contains(&"adjust_time=0".into()));
        assert!(spec
            .args
            .iter()
            .any(|arg| arg.contains("wavelinux.role=channel_to_effect")));
        assert!(spec
            .args
            .iter()
            .any(|arg| arg.contains("wavelinux.route_revision=4-latency-60")));
        assert_managed_loopback_disables_stream_restore(&spec);
    }

    #[test]
    fn sync_settings_delay_non_hardware_stream_sources_without_delaying_input() {
        let mut config = MixerConfig::default();
        config.settings.low_latency_mic_monitoring = true;
        config.settings.stream_sync_delay_msec = 80;
        config.settings.monitor_sync_delay_msec = 30;
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.speakers".into()))
            .unwrap();
        let plan = plan_ensure_graph(&config);
        let route = |channel_id: &str, mix_id: &str| {
            plan.commands
                .iter()
                .find(|command| {
                    command.args.iter().any(|arg| {
                        arg.contains("wavelinux.role=channel_to_mix")
                            && arg.contains(&format!("wavelinux.channel_id={channel_id}"))
                            && arg.contains(&format!("wavelinux.mix_id={mix_id}"))
                    })
                })
                .unwrap()
        };

        assert!(route("hardware_in", "stream")
            .args
            .contains(&"latency_msec=60".into()));
        assert!(route("music", "stream")
            .args
            .contains(&"latency_msec=160".into()));
        assert!(route("music", "monitor")
            .args
            .contains(&"latency_msec=90".into()));
        assert!(plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=60".into())
                && command
                    .args
                    .iter()
                    .any(|arg| arg.contains("wavelinux.role=mix_monitor"))
        }));
    }

    #[test]
    fn hardware_direct_mic_monitoring_skips_software_monitor_route() {
        let mut config = MixerConfig::default();
        config.settings.hardware_direct_mic_monitoring = true;
        config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap()
            .source_device = Some("alsa_input.usb-Elgato_Wave_XLR.analog-stereo".into());

        let plan = plan_ensure_graph(&config);
        let has_route = |channel_id: &str, mix_id: &str| {
            plan.commands.iter().any(|command| {
                command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains(&format!("wavelinux.channel_id={channel_id}"))
                        && arg.contains(&format!("wavelinux.mix_id={mix_id}"))
                })
            })
        };

        assert!(has_route("hardware_in", "stream"));
        assert!(!has_route("hardware_in", "monitor"));
        assert!(has_route("music", "monitor"));
    }

    #[test]
    fn hardware_direct_mic_monitoring_keeps_software_route_without_wave_xlr_source() {
        let mut config = MixerConfig::default();
        config.settings.hardware_direct_mic_monitoring = true;

        let plan = plan_ensure_graph(&config);
        let has_route = |channel_id: &str, mix_id: &str| {
            plan.commands.iter().any(|command| {
                command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains(&format!("wavelinux.channel_id={channel_id}"))
                        && arg.contains(&format!("wavelinux.mix_id={mix_id}"))
                })
            })
        };

        assert!(has_route("hardware_in", "monitor"));
    }

    #[test]
    fn runtime_aware_plan_keeps_hardware_routes_but_skips_inactive_app_routes() {
        let config = MixerConfig::default();
        let active_app_channel_ids = BTreeSet::new();

        let plan = plan_ensure_graph_for_active_app_channels(&config, &active_app_channel_ids);
        let has_route = |channel_id: &str, mix_id: &str| {
            plan.commands.iter().any(|command| {
                command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains(&format!("wavelinux.channel_id={channel_id}"))
                        && arg.contains(&format!("wavelinux.mix_id={mix_id}"))
                })
            })
        };

        assert!(has_route("hardware_in", "monitor"));
        assert!(has_route("hardware_in", "stream"));
        assert!(!has_route("browser", "monitor"));
        assert!(!has_route("browser", "stream"));
        assert!(!has_route("music", "monitor"));
    }

    #[test]
    fn runtime_aware_plan_builds_only_active_app_channel_mix_routes() {
        let config = MixerConfig::default();
        let active_app_channel_ids = BTreeSet::from(["browser".to_string()]);

        let plan = plan_ensure_graph_for_active_app_channels(&config, &active_app_channel_ids);
        let has_route = |channel_id: &str, mix_id: &str| {
            plan.commands.iter().any(|command| {
                command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains(&format!("wavelinux.channel_id={channel_id}"))
                        && arg.contains(&format!("wavelinux.mix_id={mix_id}"))
                })
            })
        };

        assert!(has_route("browser", "monitor"));
        assert!(has_route("browser", "stream"));
        assert!(!has_route("music", "monitor"));
        assert!(!has_route("game", "stream"));
    }

    #[test]
    fn active_route_plan_skips_muted_and_inactive_mix_sends() {
        let mut config = MixerConfig::default();
        config.channels[0]
            .mix_buses
            .get_mut("monitor")
            .unwrap()
            .muted = true;
        let active_app_channel_ids = BTreeSet::from(["browser".to_string()]);
        let active_mix_ids = BTreeSet::from(["monitor".to_string()]);

        let plan =
            plan_ensure_graph_for_active_routes(&config, &active_app_channel_ids, &active_mix_ids);
        let has_route = |channel_id: &str, mix_id: &str| {
            plan.commands.iter().any(|command| {
                command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains(&format!("wavelinux.channel_id={channel_id}"))
                        && arg.contains(&format!("wavelinux.mix_id={mix_id}"))
                })
            })
        };

        assert!(!has_route("hardware_in", "monitor"));
        assert!(!has_route("hardware_in", "stream"));
        assert!(has_route("browser", "monitor"));
        assert!(!has_route("browser", "stream"));
    }

    #[test]
    fn wavelinux6_plan_keeps_native_mix_sources_without_pulse_mix_modules() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");

        let plan = plan_ensure_graph_for_active_routes(&config, &BTreeSet::new(), &BTreeSet::new());
        assert!(plan.commands.iter().all(|command| {
            !command.args.iter().any(|arg| {
                arg == "module-null-sink"
                    || arg == "module-remap-source"
                    || arg.contains("wavelinux6.role=channel_to_mix")
            })
        }));
        assert!(config
            .mixes
            .iter()
            .all(|mix| plan.managed_nodes.contains(&mix.virtual_source_name)));
        assert!(config
            .mixes
            .iter()
            .all(|mix| !plan.managed_nodes.contains(&mix.virtual_sink_name)));
    }

    #[test]
    fn wavelinux6_monitor_route_reads_from_the_native_mix_source() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
        let monitor = config.mixes.iter().find(|mix| mix.id == "monitor").unwrap();

        let commands = plan_route_mix_to_output(
            monitor,
            "alsa_output.test",
            &route_settings_for_config(&config),
        );

        assert_eq!(commands.len(), 1);
        assert!(commands[0]
            .args
            .contains(&"source=wavelinux6_mix_monitor_source".into()));
        assert!(!commands[0]
            .args
            .contains(&"source=wavelinux6_mix_monitor.monitor".into()));
    }

    #[test]
    fn wavelinux6_native_mix_policy_keeps_muted_buses_but_respects_disabled_buses() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux6");
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

        let active_channels = BTreeSet::new();
        let active_mixes = BTreeSet::new();
        let input = config
            .channels
            .iter()
            .find(|channel| channel.id == "hardware_in")
            .unwrap();
        let music = config
            .channels
            .iter()
            .find(|channel| channel.id == "music")
            .unwrap();
        let monitor = config.mixes.iter().find(|mix| mix.id == "monitor").unwrap();
        let stream = config.mixes.iter().find(|mix| mix.id == "stream").unwrap();

        assert!(channel_mix_route_expected_for_active_routes(
            input,
            stream,
            &config.settings,
            &active_channels,
            &active_mixes,
        ));
        assert!(channel_mix_route_expected_for_active_routes(
            input,
            monitor,
            &config.settings,
            &active_channels,
            &active_mixes,
        ));
        assert!(!channel_mix_route_expected_for_active_routes(
            music,
            stream,
            &config.settings,
            &active_channels,
            &active_mixes,
        ));
    }

    #[test]
    fn active_route_plan_suspends_outputs_for_inactive_mixes() {
        let mut config = MixerConfig::default();
        config
            .set_mix_monitor_output("monitor", Some("alsa_output.usb".into()))
            .unwrap();

        let idle_plan =
            plan_ensure_graph_for_active_routes(&config, &BTreeSet::new(), &BTreeSet::new());
        assert!(!idle_plan.commands.iter().any(|command| {
            command
                .args
                .iter()
                .any(|arg| arg.contains("wavelinux.role=mix_monitor"))
        }));

        let active_plan = plan_ensure_graph_for_active_routes(
            &config,
            &BTreeSet::new(),
            &BTreeSet::from(["monitor".to_string()]),
        );
        assert!(active_plan.commands.iter().any(|command| {
            command
                .args
                .iter()
                .any(|arg| arg.contains("wavelinux.role=mix_monitor"))
        }));
    }

    #[test]
    fn bluetooth_monitor_route_uses_safe_latency_floor() {
        let mut settings = MixerSettings {
            low_latency_mic_monitoring: true,
            ..MixerSettings::default()
        };
        let mix = Mix::new_fixed("monitor", "Monitor");
        let command = plan_route_mix_to_output(&mix, "bluez_output.AC_80_0A_72_BD_10.1", &settings)
            .into_iter()
            .next()
            .unwrap();

        assert!(command.args.contains(&"latency_msec=240".into()));
        assert!(command.args.contains(&"adjust_time=0".into()));
        assert!(command
            .args
            .iter()
            .any(|arg| arg.contains("wavelinux.route_revision=3-latency-240")));
        assert_managed_loopback_disables_stream_restore(&command);

        settings.low_latency_mic_monitoring = false;
        settings.optimization_mode = OptimizationMode::Safe;
        assert_eq!(
            mix_monitor_latency_msec_for_sink(&mix, "alsa_output.usb", &settings),
            STABLE_LOOPBACK_LATENCY_MSEC
        );
    }

    #[test]
    fn runtime_latency_policy_controls_profiled_route_floors() {
        let mut settings = MixerSettings {
            low_latency_mic_monitoring: true,
            ..MixerSettings::default()
        };
        settings.runtime_latency_policy = Some(wavelinux_model::LatencyPolicy {
            stable_msec: Some(60),
            low_latency_msec: Some(35),
            bluetooth_floor_msec: Some(240),
        });
        let channel = Channel::new_fixed("hardware_in", "Input", ChannelKind::Generic);
        let music = Channel::new_fixed("music", "Music", ChannelKind::Application);
        let monitor = Mix::new_fixed("monitor", "Monitor");

        assert_eq!(hardware_route_latency_msec(&channel, &settings), 35);
        assert_eq!(channel_mix_latency_msec(&music, &monitor, &settings), 35);
        assert_eq!(
            mix_monitor_latency_msec_for_sink(
                &monitor,
                "alsa_output.pci.realtek.HiFi__Speaker__sink",
                &settings
            ),
            35
        );
        assert_eq!(
            mix_monitor_latency_msec_for_sink(
                &monitor,
                "bluez_output.AC_80_0A_72_BD_10.1",
                &settings
            ),
            240
        );
    }

    #[test]
    fn planned_graph_uses_generic_fallback_profile_when_no_runtime_profile_is_set() {
        let mut config = MixerConfig::default();
        config.settings.low_latency_mic_monitoring = true;
        config
            .device_policy
            .fallback_hardware_profile
            .latency_policy = wavelinux_model::LatencyPolicy {
            stable_msec: Some(80),
            low_latency_msec: Some(45),
            bluetooth_floor_msec: Some(160),
        };
        config
            .channels
            .iter_mut()
            .find(|channel| channel.id == "hardware_in")
            .unwrap()
            .source_device = Some("alsa_input.realtek".into());
        config
            .set_mix_monitor_output("monitor", Some("bluez_output.AC_80_0A_72_BD_10.1".into()))
            .unwrap();

        let plan = plan_ensure_graph(&config);

        assert!(plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=45".into())
                && command
                    .args
                    .iter()
                    .any(|arg| arg.contains("wavelinux.role=input_to_channel"))
        }));
        assert!(plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=80".into())
                && command.args.iter().any(|arg| {
                    arg.contains("wavelinux.role=channel_to_mix")
                        && arg.contains("wavelinux.channel_id=music")
                })
        }));
        assert!(plan.commands.iter().any(|command| {
            command.args.contains(&"latency_msec=160".into())
                && command
                    .args
                    .iter()
                    .any(|arg| arg.contains("wavelinux.role=mix_monitor"))
        }));
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
          },
          {
            "index": 4,
            "name": "alsa_input.headset",
            "description": "Headset Mono Microphone",
            "active_port": "[In] Headset",
            "ports": [
              {"name": "[In] Headset", "availability": "not available"}
            ],
            "properties": {}
          }
        ]
        "#;
        let devices = parse_devices_json(json, "Sink");
        assert_eq!(devices.len(), 4);
        assert!(devices[1].is_virtual);
        assert!(devices[2].is_virtual);
        assert!(!devices[3].is_available);
        assert_eq!(devices[3].active_port.as_deref(), Some("[In] Headset"));
        assert_eq!(devices[3].ports.len(), 1);
        assert_eq!(devices[3].ports[0].availability, "not available");
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
              "wavelinux.mix_id": "stream",
              "sink.name": "wavelinux_mix_stream",
              "target.object": "wavelinux_mix_stream"
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
        assert_eq!(inputs[0].sink_name.as_deref(), Some("wavelinux_mix_stream"));
        assert_eq!(
            inputs[0].target_object.as_deref(),
            Some("wavelinux_mix_stream")
        );
        assert_eq!(inputs[0].muted, Some(false));
        assert_eq!(inputs[0].volume_percent, Some(74));
    }

    #[test]
    fn active_playback_sink_prefers_real_current_playback_sink() {
        let json = r#"
        [
          {
            "index": 8,
            "sink": 51,
            "mute": false,
            "properties": {
              "application.name": "Spotify"
            }
          },
          {
            "index": 9,
            "sink": 125,
            "mute": false,
            "properties": {
              "application.name": "WaveLinux",
              "wavelinux.managed": "1"
            }
          }
        ]
        "#;
        let sink_names = BTreeMap::from([
            (
                "51".to_string(),
                "bluez_output.AC_80_0A_72_BD_10.1".to_string(),
            ),
            ("125".to_string(), "wavelinux_channel_music".to_string()),
        ]);

        assert_eq!(
            active_playback_sink_from_sink_inputs_json(json, &sink_names).as_deref(),
            Some("bluez_output.AC_80_0A_72_BD_10.1")
        );
    }

    #[test]
    fn active_playback_sink_ignores_wavelinux_and_muted_streams() {
        let json = r#"
        [
          {
            "index": 8,
            "sink": 125,
            "mute": false,
            "properties": {
              "application.name": "Spotify"
            }
          },
          {
            "index": 9,
            "sink": 51,
            "mute": true,
            "properties": {
              "application.name": "Muted"
            }
          }
        ]
        "#;
        let sink_names = BTreeMap::from([
            (
                "51".to_string(),
                "bluez_output.AC_80_0A_72_BD_10.1".to_string(),
            ),
            ("125".to_string(), "wavelinux_channel_music".to_string()),
        ]);

        assert_eq!(
            active_playback_sink_from_sink_inputs_json(json, &sink_names),
            None
        );
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
        assert_eq!(
            inputs[0].target_object.as_deref(),
            Some("wavelinux_mix_stream")
        );
    }

    #[test]
    fn sink_input_routes_hydrate_sink_name_from_sink_index() {
        let routes_json = r#"
        [
          {
            "index": 73,
            "owner_module": 102,
            "sink": 2,
            "properties": {
              "wavelinux.role": "channel_to_mix",
              "wavelinux.channel_id": "music",
              "wavelinux.mix_id": "stream"
            }
          }
        ]
        "#;
        let sinks_json = r#"
        [
          {
            "index": 2,
            "name": "wavelinux_mix_stream",
            "properties": {}
          }
        ]
        "#;
        let sink_names = parse_device_names_by_index_json(sinks_json);
        let inputs = hydrate_sink_input_routes_from_sinks(
            parse_sink_input_routes_json(routes_json),
            &sink_names,
        );

        assert_eq!(inputs[0].sink_name.as_deref(), Some("wavelinux_mix_stream"));
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
    fn source_level_commands_target_named_source() {
        let volume = plan_set_source_volume("alsa_input.usb_mic", 1.0);
        assert_eq!(
            volume.args,
            vec!["set-source-volume", "alsa_input.usb_mic", "100%"]
        );

        let mute = plan_set_source_mute("alsa_input.usb_mic", false);
        assert_eq!(
            mute.args,
            vec!["set-source-mute", "alsa_input.usb_mic", "0"]
        );
    }

    #[test]
    fn managed_sink_level_commands_target_named_sink() {
        let volume = plan_set_managed_sink_volume("wavelinux_channel_music", 1.0);
        assert_eq!(
            volume.args,
            vec!["set-sink-volume", "wavelinux_channel_music", "100%"]
        );

        let mute = plan_set_managed_sink_mute("wavelinux_channel_music", false);
        assert_eq!(
            mute.args,
            vec!["set-sink-mute", "wavelinux_channel_music", "0"]
        );
    }

    #[test]
    fn parses_managed_source_output_routes() {
        let json = r#"
        [
          {
            "index": 91,
            "owner_module": 536870922,
            "source": 55,
            "mute": true,
            "volume": {"front-left": {"value_percent": "82%"}},
            "properties": {
              "application.name": "Chromium input",
              "node.name": "Chromium input",
              "media.name": "RecordStream",
              "wavelinux.role": "channel_to_mix",
              "wavelinux.channel_id": "mic",
              "wavelinux.mix_id": "stream",
              "node.dont-move": "true",
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
        assert_eq!(outputs[0].source_id.as_deref(), Some("55"));
        assert_eq!(
            outputs[0].target_object.as_deref(),
            Some("wavelinux_mix_stream")
        );
        assert_eq!(
            outputs[0].application_name.as_deref(),
            Some("Chromium input")
        );
        assert_eq!(outputs[0].node_name.as_deref(), Some("Chromium input"));
        assert_eq!(outputs[0].media_name.as_deref(), Some("RecordStream"));
        assert_eq!(outputs[0].muted, Some(true));
        assert_eq!(outputs[0].volume_percent, Some(82));
        assert!(outputs[0].dont_move);
    }

    #[test]
    fn channel_bus_route_ids_match_direct_route_properties() {
        let sink_input_json = r#"
        [
          {
            "index": 73,
            "owner_module": 102,
            "sink": 2,
            "mute": false,
            "properties": {
              "wavelinux.role": "channel_to_mix",
              "wavelinux.channel_id": "hardware_in",
              "wavelinux.mix_id": "monitor"
            }
          }
        ]
        "#;
        let source_output_json = r#"
        [
          {
            "index": 91,
            "owner_module": 102,
            "source": 55,
            "mute": false,
            "properties": {
              "wavelinux.role": "channel_to_mix",
              "wavelinux.channel_id": "hardware_in",
              "wavelinux.mix_id": "monitor"
            }
          }
        ]
        "#;

        let route_ids = channel_bus_route_ids_from_routes(
            "hardware_in",
            "monitor",
            &parse_sink_input_routes_json(sink_input_json),
            &parse_source_outputs_json(source_output_json),
        );

        assert_eq!(route_ids.sink_input_id.as_deref(), Some("73"));
        assert_eq!(route_ids.source_output_id.as_deref(), Some("91"));
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
    fn channel_bus_route_ids_match_module_argument_fallback() {
        let modules_text = "\
102\tmodule-loopback\tsource=wavelinux_channel_music.monitor sink=wavelinux_mix_stream source_output_properties=wavelinux.managed=1 wavelinux.role=channel_to_mix wavelinux.channel_id=music wavelinux.mix_id=stream sink_input_properties=wavelinux.managed=1 wavelinux.role=channel_to_mix wavelinux.channel_id=music wavelinux.mix_id=stream\t\n";
        let sink_input_json = r#"[{"index":73,"owner_module":102,"properties":{}}]"#;
        let source_output_json = r#"[{"index":91,"owner_module":102,"properties":{}}]"#;
        let modules = parse_managed_modules_short(modules_text);
        let sink_inputs = hydrate_sink_input_routes_from_modules(
            parse_sink_input_routes_json(sink_input_json),
            &modules,
        );
        let source_outputs = hydrate_source_output_routes_from_modules(
            parse_source_outputs_json(source_output_json),
            &modules,
        );

        let route_ids =
            channel_bus_route_ids_from_routes("music", "stream", &sink_inputs, &source_outputs);

        assert_eq!(route_ids.sink_input_id.as_deref(), Some("73"));
        assert_eq!(route_ids.source_output_id.as_deref(), Some("91"));
    }

    #[test]
    fn source_output_routes_hydrate_source_name_from_source_index() {
        let routes_json = r#"
        [
          {
            "index": 91,
            "source": 55,
            "owner_module": null,
            "properties": {"application.name": "Discord"}
          }
        ]
        "#;
        let sources_json = r#"
        [
          {"index": 55, "name": "alsa_input.usb_mic", "properties": {}}
        ]
        "#;
        let source_names = parse_device_names_by_index_json(sources_json);
        let outputs = hydrate_source_output_routes_from_sources(
            parse_source_outputs_json(routes_json),
            &source_names,
        );

        assert_eq!(
            outputs[0].source_name.as_deref(),
            Some("alsa_input.usb_mic")
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
        assert!(rendered.contains("log.level = 0"));
        assert!(rendered.contains("libpipewire-module-protocol-native"));
        assert!(rendered.contains("active"));
        assert!(!rendered.contains("bypassed"));
        assert!(rendered.contains("node.name = \"wavelinux_fx_hardware_in_input\""));
        assert!(rendered.contains("media.class = Audio/Sink"));
        assert!(rendered.contains("node.name = \"wavelinux-mic\""));
        assert!(rendered.contains("device.description = \"WaveLinux-mic\""));
        assert!(rendered.contains("node.description = \"WaveLinux-mic\""));
        assert!(rendered.contains("wavelinux.effect_config_revision = \"3\""));
        assert!(!rendered.contains("priority.session"));
        assert!(!rendered.contains("priority.driver"));
    }

    #[test]
    fn filter_chain_wavelinux5_hardware_input_outputs_to_processed_bridge_source() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux5");
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();

        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());

        assert!(channel_uses_adaptive_latency_bridge(&config.channels[0]));
        assert!(rendered.contains("node.name = \"wavelinux5_fx_hardware_in_chain\""));
        assert!(rendered.contains("node.name = \"wavelinux5_fx_hardware_in_input\""));
        assert!(rendered.contains("node.name = \"wavelinux5_fx_hardware_in_processed\""));
        assert!(rendered.contains("device.description = \"WaveLinux FX Input Processed\""));
        assert!(!rendered.contains("node.name = \"wavelinux5-mic\""));
    }

    #[test]
    fn planned_graph_wavelinux5_routes_processed_fx_into_adaptive_bridge() {
        let mut config = MixerConfig::default();
        wavelinux_model::apply_graph_namespace_with_prefix(&mut config, "wavelinux5");
        config
            .set_effect_chain("hardware_in", vec![EffectInstance::new("rnnoise")])
            .unwrap();

        let plan = plan_ensure_graph(&config);
        let command = plan
            .commands
            .iter()
            .find(|command| {
                command
                    .args
                    .iter()
                    .any(|arg| arg.contains("effect_to_adaptive_bridge"))
            })
            .expect("adaptive bridge route");

        assert_eq!(command.program, "pactl");
        assert_eq!(command.domain, CommandDomain::Route);
        assert!(command
            .args
            .contains(&"source=wavelinux5_fx_hardware_in_processed".into()));
        assert!(command
            .args
            .contains(&"sink=wavelinux5_fx_hardware_in_adaptive_input".into()));
        assert!(command.args.contains(&format!(
            "latency_msec={EFFECT_ADAPTIVE_BRIDGE_TRANSPORT_MSEC}"
        )));
        assert!(command
            .args
            .iter()
            .any(|arg| arg.contains("route_revision=2")));
    }

    #[test]
    fn filter_chain_wires_stereo_effects_in_order() {
        let mut config = MixerConfig::default();
        config.channels[0].input_mode = ChannelInputMode::Stereo;
        let mut rnnoise = EffectInstance::new("rnnoise");
        rnnoise.instance_id = "rnnoise".into();
        let mut eq = EffectInstance::new("eq");
        eq.instance_id = "voice_eq".into();
        let mut limiter = EffectInstance::new("limiter");
        limiter.instance_id = "limiter".into();
        config.channels[0].effects = vec![rnnoise, eq, limiter];

        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());
        assert!(rendered.contains("log.level = 0"));
        assert!(rendered.contains("links = ["));
        assert!(rendered.contains(
            "plugin = \"librnnoise_ladspa\" label = \"noise_suppressor_stereo\" name = \"rnnoise\""
        ));
        assert!(rendered.contains("\"VAD Threshold (%)\" = 25.000"));
        assert!(rendered.contains("\"VAD Grace Period (ms)\" = 200.000"));
        assert!(rendered.contains("filters1 = ["));
        assert!(rendered.contains("filters2 = ["));
        assert!(rendered.contains("output = \"rnnoise:Output (L)\" input = \"voice_eq:In 1\""));
        assert!(rendered.contains("output = \"rnnoise:Output (R)\" input = \"voice_eq:In 2\""));
        assert!(rendered.contains("output = \"voice_eq:Out 1\" input = \"limiter:Input 1\""));
        assert!(rendered.contains("output = \"voice_eq:Out 2\" input = \"limiter:Input 2\""));
        assert!(rendered.contains("inputs = [ \"rnnoise:Input (L)\" \"rnnoise:Input (R)\" ]"));
        assert!(rendered.contains("outputs = [ \"limiter:Output 1\" \"limiter:Output 2\" ]"));
    }

    #[test]
    fn filter_chain_uses_stereo_rnnoise_for_sum_mono_hardware_input() {
        let mut config = MixerConfig::default();
        let mut highpass = EffectInstance::new("highpass");
        highpass.instance_id = "highpass".into();
        let mut rnnoise = EffectInstance::new("rnnoise");
        rnnoise.instance_id = "rnnoise".into();
        let mut eq = EffectInstance::new("eq");
        eq.instance_id = "voice_eq".into();
        config.channels[0].input_mode = ChannelInputMode::SumMono;
        config.channels[0].effects = vec![highpass, rnnoise, eq];

        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());

        assert!(rendered.contains(
            "plugin = \"librnnoise_ladspa\" label = \"noise_suppressor_stereo\" name = \"rnnoise\""
        ));
        assert!(rendered.contains("\"VAD Threshold (%)\" = 25.000"));
        assert!(rendered.contains("\"VAD Grace Period (ms)\" = 200.000"));
        assert!(rendered.contains("\"Retroactive VAD Grace (ms)\" = 0.000"));
        assert!(rendered.contains("\"Dry Mix\" = 0.000"));
        assert!(rendered.contains("output = \"highpass_left:Out\" input = \"rnnoise:Input (L)\""));
        assert!(rendered.contains("output = \"highpass_right:Out\" input = \"rnnoise:Input (R)\""));
        assert!(rendered.contains("output = \"rnnoise:Output (L)\" input = \"voice_eq:In 1\""));
        assert!(rendered.contains("output = \"rnnoise:Output (R)\" input = \"voice_eq:In 2\""));
    }

    #[test]
    fn filter_chain_renders_eq_as_eight_band_graphic_eq() {
        let mut config = MixerConfig::default();
        let mut eq = EffectInstance::new("eq");
        eq.instance_id = "voice_eq".into();
        eq.params.insert("band_63_gain_db".into(), -4.0);
        eq.params.insert("band_2k_gain_db".into(), 2.5);
        eq.params.insert("band_8k_gain_db".into(), 1.0);
        config.channels[0].effects = vec![eq];

        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());

        assert!(rendered.contains("label = \"param_eq\" name = \"voice_eq\""));
        assert!(rendered.contains("type = bq_lowshelf freq = 63.000 gain = -4.000"));
        assert!(rendered.contains("type = bq_peaking freq = 2000.000 gain = 2.500"));
        assert!(rendered.contains("type = bq_highshelf freq = 8000.000 gain = 1.000"));
        assert_eq!(rendered.matches("type = bq_").count(), 16);
    }

    #[test]
    fn filter_chain_renders_noise_gate_as_stereo_ladspa_pair() {
        let mut config = MixerConfig::default();
        let mut gate = EffectInstance::new("gate");
        gate.instance_id = "voice_gate".into();
        config.set_effect_chain("hardware_in", vec![gate]).unwrap();

        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());
        assert!(
            rendered.contains("plugin = \"gate_1410\" label = \"gate\" name = \"voice_gate_left\"")
        );
        assert!(rendered
            .contains("plugin = \"gate_1410\" label = \"gate\" name = \"voice_gate_right\""));
        assert!(rendered.contains("\"Threshold (dB)\" = -60.000"));
        assert!(rendered.contains("\"Hold (ms)\" = 120.000"));
        assert!(rendered.contains("\"Decay (ms)\" = 220.000"));
        assert!(rendered.contains("\"Range (dB)\" = -30.000"));
        assert!(
            rendered.contains("inputs = [ \"voice_gate_left:Input\" \"voice_gate_right:Input\" ]")
        );
        assert!(rendered
            .contains("outputs = [ \"voice_gate_left:Output\" \"voice_gate_right:Output\" ]"));
    }

    #[test]
    fn filter_chain_renders_karaoke_stage_as_composite_width_effect() {
        let mut config = MixerConfig::default();
        let mut karaoke = EffectInstance::new("karaoke_stage");
        karaoke.instance_id = "karaoke_stage".into();
        karaoke.params.insert("tone_highpass_hz".into(), 120.0);
        karaoke.params.insert("tone_lowpass_hz".into(), 4600.0);
        karaoke.params.insert("tone_gain_db".into(), 2.5);
        config.channels[0].effects = vec![karaoke];

        let rendered = render_filter_chain(&config.channels[0], &EffectCatalog::default());

        assert!(rendered
            .contains("label = \"bq_highpass\" name = \"karaoke_stage_tone_highpass_left\""));
        assert!(
            rendered.contains("label = \"bq_lowpass\" name = \"karaoke_stage_tone_lowpass_left\"")
        );
        assert!(rendered.contains("\"Freq\" = 120.000"));
        assert!(rendered.contains("\"Freq\" = 4600.000"));
        assert!(rendered.contains("\"Mult\" = 1.334"));
        assert!(rendered.contains(
            "plugin = \"pitch_scale_1193\" label = \"pitchScale\" name = \"karaoke_stage_pitch_left\""
        ));
        assert!(rendered
            .contains("plugin = \"gverb_1216\" label = \"gverb\" name = \"karaoke_stage_room\""));
        assert!(rendered.contains("label = \"delay\" name = \"karaoke_stage_delay_left\""));
        assert!(rendered.contains("label = \"mixer\" name = \"karaoke_stage_mix_left\""));
        assert!(rendered.contains(
            "output = \"karaoke_stage_double_right:Out\" input = \"karaoke_stage_mix_left:In 2\""
        ));
        assert!(rendered.contains(
            "outputs = [ \"karaoke_stage_mix_left:Out\" \"karaoke_stage_mix_right:Out\" ]"
        ));
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
    fn managed_module_detection_ignores_unowned_wavelinux_mentions() {
        let listed_modules = "\
300\tmodule-loopback\tsource=alsa_input.real sink=alsa_output.real node.description=not-a-wavelinux-node\t\n\
301\tmodule-loopback\tsource=alsa_input.real sink=wavelinux_channel_music\t\n\
302\tmodule-loopback\tsource=alsa_input.real sink=alsa_output.real sink_input_properties=wavelinux.managed=1\t\n";

        let modules = parse_managed_modules_json(listed_modules, "[]", "[]", "[]", "[]");

        assert!(!modules.iter().any(|module| module.module_id == "300"));
        assert!(modules.iter().any(|module| module.module_id == "301"));
        assert!(modules.iter().any(|module| module.module_id == "302"));
    }

    #[test]
    fn managed_module_detection_catches_legacy_openwave_nodes() {
        let listed_modules = "\
701\tmodule-null-sink\tsink_name=openwave_chat_mix sink_properties=device.description=\"OpenWave Chat Mix\"\t\n";
        let sinks = r#"
        [
          {
            "index": 7,
            "owner_module": 702,
            "name": "openwave_record_mix",
            "properties": {"node.description": "OpenWave Record Mix"}
          }
        ]
        "#;

        let modules = parse_managed_modules_json(listed_modules, sinks, "[]", "[]", "[]");

        assert!(modules.iter().any(|module| {
            module.module_id == "701" && module.node_name.as_deref() == Some("openwave_chat_mix")
        }));
        assert!(modules.iter().any(|module| {
            module.module_id == "702" && module.node_name.as_deref() == Some("openwave_record_mix")
        }));

        let commands = plan_unload_modules(&modules);
        assert_eq!(commands.len(), 2);
        assert!(commands.iter().any(|command| command.args[1] == "701"));
        assert!(commands.iter().any(|command| command.args[1] == "702"));
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

    #[test]
    fn consolidated_audio_snapshot_shares_stream_and_sink_state() {
        let sources = r#"[
          {"index": 1, "name": "alsa_input.usb-mic", "description": "USB Mic"}
        ]"#;
        let sinks = r#"[
          {
            "index": 10,
            "name": "alsa_output.usb-speakers",
            "description": "USB Speakers",
            "mute": false,
            "volume": {"front-left": {"value_percent": "73%"}}
          }
        ]"#;
        let sink_inputs = r#"[
          {
            "index": 20,
            "sink": 10,
            "mute": false,
            "properties": {
              "client.id": "30",
              "application.name": "Fallback Name"
            }
          }
        ]"#;
        let clients = r#"[
          {"index": 30, "properties": {"application.name": "Browser"}}
        ]"#;

        let snapshot = parse_audio_state_snapshot(
            AudioStateSnapshotJson {
                sources,
                sinks,
                sink_inputs,
                source_outputs: "[]",
                clients,
                modules: "",
                cards: "[]",
                default_source: Some("alsa_input.usb-mic\n"),
                default_sink: Some("alsa_output.usb-speakers\n"),
            },
            None,
            Vec::new(),
        );

        assert_eq!(snapshot.graph.inputs.len(), 1);
        assert_eq!(snapshot.graph.outputs.len(), 1);
        assert_eq!(snapshot.graph.app_streams.len(), 1);
        assert_eq!(snapshot.graph.app_streams[0].display_name, "Fallback Name");
        assert_eq!(
            snapshot.active_playback_sink.as_deref(),
            Some("alsa_output.usb-speakers")
        );
        assert_eq!(
            snapshot.default_source.as_deref(),
            Some("alsa_input.usb-mic")
        );
        assert_eq!(
            snapshot.default_sink.as_deref(),
            Some("alsa_output.usb-speakers")
        );
        assert_eq!(snapshot.routes.sink_input_routes.len(), 1);
        assert_eq!(
            snapshot
                .sink_levels
                .get("alsa_output.usb-speakers")
                .and_then(|level| level.volume_percent),
            Some(73)
        );
    }

    #[test]
    fn consolidated_audio_snapshot_reports_parallel_queries_in_stable_order() {
        let client = PwClient::new(true);

        let (snapshot, timings) =
            client.audio_state_snapshot_with_effect_availability_timed(None, Vec::new());

        assert!(snapshot.graph.inputs.is_empty());
        assert!(snapshot.graph.outputs.is_empty());
        assert!(snapshot.graph.app_streams.is_empty());
        assert!(timings.iter().all(|timing| timing.succeeded));
        assert_eq!(
            timings
                .iter()
                .map(|timing| timing.label.as_str())
                .collect::<Vec<_>>(),
            [
                "pactl --format=json list sources",
                "pactl --format=json list sinks",
                "pactl --format=json list sink-inputs",
                "pactl --format=json list source-outputs",
                "pactl --format=json list clients",
                "pactl list modules short",
                "pactl --format=json list cards",
                "pactl get-default-source",
                "pactl get-default-sink",
            ]
        );
    }
}
