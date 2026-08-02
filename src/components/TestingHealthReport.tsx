import { Check, Copy, RefreshCw } from "lucide-react";
import type {
  AppStateSnapshot,
  CommandExecution,
  DeviceInfo,
  Diagnostic,
  ElgatoDeviceSummary,
  GraphDebugReport,
  SoundCheckReport,
  StreamerDeviceSummary,
  UpdateInfo,
} from "../types";

export type AudioActionReport = {
  title: string;
  commands: CommandExecution[];
  plannedCount?: number;
  finishedAt: number;
};

export function TestingHealthReport({
  onCopy,
  onRefresh,
  report,
  status,
}: {
  onCopy: () => void | Promise<unknown>;
  onRefresh: () => void | Promise<unknown>;
  report: string;
  status: string | null;
}) {
  return (
    <div className="testing-health command-report">
      <div className="command-report-header">
        <div>
          <strong>Testing Health Report</strong>
          <span>GitHub issue payload</span>
        </div>
        <div className="panel-actions">
          <button
            className="secondary-button"
            onClick={() => void onRefresh()}
            type="button"
          >
            <RefreshCw size={16} />
            Refresh
          </button>
          <button
            className="secondary-button"
            onClick={() => void onCopy()}
            type="button"
          >
            {status === "Copied" ? <Check size={16} /> : <Copy size={16} />}
            {status ?? "Copy"}
          </button>
        </div>
      </div>
      <textarea
        aria-label="Testing health report"
        className="testing-health-report"
        readOnly
        value={report}
      />
    </div>
  );
}

export function buildTestingHealthReport({
  audioActionReport,
  diagnostics,
  elgatoDeviceError,
  elgatoDevices,
  graphReport,
  report,
  state,
  streamerDeviceError,
  streamerDevices,
  updateInfo,
}: {
  audioActionReport: AudioActionReport | null;
  diagnostics: Diagnostic[];
  elgatoDeviceError: string | null;
  elgatoDevices: ElgatoDeviceSummary[];
  graphReport: GraphDebugReport | null;
  report: SoundCheckReport | null;
  state: AppStateSnapshot;
  streamerDeviceError: string | null;
  streamerDevices: StreamerDeviceSummary[];
  updateInfo: UpdateInfo | null;
}) {
  const settings = state.config.settings;
  const missingEffects =
    report?.missing_effects ??
    state.graph.effect_availability
      .filter((effect) => !effect.available)
      .map((effect) => `${effect.effect_id}: ${effect.detail}`);
  const lines = [
    "# WaveLinux Testing Health Report",
    "",
    `Generated: ${new Date().toISOString()}`,
    `Config version: ${state.config.version}`,
    `Release channel: ${settings.release_channel}`,
    `Auto check updates: ${yesNo(settings.auto_check_updates)}`,
    `Auto install updates: ${yesNo(settings.auto_install_updates)}`,
    `Update status: ${updateInfo?.message ?? "not checked"}`,
    `Update current version: ${updateInfo?.current_version ?? "unknown"}`,
    `Update latest version: ${updateInfo?.version ?? "none"}`,
    `Update install supported: ${updateInfo ? yesNo(updateInfo.install_supported) : "unknown"}`,
    `Update endpoint: ${updateInfo?.endpoint ?? "not checked"}`,
    `Update release URL: ${updateInfo?.release_url ?? "not checked"}`,
    "",
    "## Engine",
    `Healthy: ${yesNo(state.engine.healthy)}`,
    `Audio graph running: ${yesNo(state.engine.audio_graph_running)}`,
    `Dry run: ${yesNo(state.engine.dry_run)}`,
    `Message: ${state.engine.message || "none"}`,
    `Last refresh unix: ${state.engine.last_refresh_unix}`,
    `Refreshes: ${state.engine.refresh.total_refreshes}; last=${state.engine.refresh.last_total_msec}ms; peak=${state.engine.refresh.peak_total_msec}ms; snapshot_commands=${state.engine.refresh.snapshot_commands}; snapshot_failures=${state.engine.refresh.snapshot_failures}`,
    `Refresh phases: ${Object.entries(state.engine.refresh.last_phase_msec)
      .map(([phase, msec]) => `${phase}=${msec}ms`)
      .join(" ") || "none"}`,
    `Route mutations: ${state.engine.refresh.route_mutations}; deferred=${state.engine.refresh.deferred_route_mutations}`,
    `PipeWire health: journal=${state.engine.pipewire_audio_health.monitor_available ? "active" : "unavailable"}; profiler=${state.engine.pipewire_audio_health.profiler_available ? "active" : "unavailable"}; profiler_samples=${state.engine.pipewire_audio_health.profiler_samples}; direct_errors=${state.engine.pipewire_audio_health.direct_errors}; owned_direct_errors=${state.engine.pipewire_audio_health.owned_direct_errors}; warnings=${state.engine.pipewire_audio_health.warning_events}; out_of_buffers=${state.engine.pipewire_audio_health.out_of_buffers}; resyncs=${state.engine.pipewire_audio_health.resyncs}; link_failures=${state.engine.pipewire_audio_health.link_failures}; xruns=${state.engine.pipewire_audio_health.xruns}; owned=${state.engine.pipewire_audio_health.owned_events}`,
    `PipeWire registry: available=${yesNo(state.engine.pipewire_registry.available)}; connected=${yesNo(state.engine.pipewire_registry.connected)}; initialized=${yesNo(state.engine.pipewire_registry.initialized)}; generation=${state.engine.pipewire_registry.generation}; objects=${state.engine.pipewire_registry.object_count}; nodes=${state.engine.pipewire_registry.node_count}; devices=${state.engine.pipewire_registry.device_count}; ports=${state.engine.pipewire_registry.port_count}; links=${state.engine.pipewire_registry.link_count}; metadata=${state.engine.pipewire_registry.metadata_count}; playback_streams=${state.engine.pipewire_registry.playback_stream_count}; capture_streams=${state.engine.pipewire_registry.capture_stream_count}; batches=${state.engine.pipewire_registry.batches_received}; changes=${state.engine.pipewire_registry.objects_changed}; direct_link_failures=${state.engine.pipewire_registry.direct_link_failures}; direct_node_errors=${state.engine.pipewire_registry.direct_node_errors}; reconnects=${state.engine.pipewire_registry.reconnects}${state.engine.pipewire_registry.last_error ? `; last_error=${state.engine.pipewire_registry.last_error}` : ""}`,
    `Meter transport: protocol=${state.engine.meter_transport.protocol_version}; connected=${yesNo(state.engine.meter_transport.connected)}; slots=${state.engine.meter_transport.slot_count}; sequence=${state.engine.meter_transport.last_sequence}; frames=${state.engine.meter_transport.frames_received}; connections=${state.engine.meter_transport.connections}; disconnects=${state.engine.meter_transport.disconnects}; fallback_polls=${state.engine.meter_transport.fallback_polls}; errors=${state.engine.meter_transport.errors}${state.engine.meter_transport.last_error ? `; last_error=${state.engine.meter_transport.last_error}` : ""}`,
    `Peripheral plugins: ${state.engine.peripheral_plugins.map((plugin) => `${plugin.kind}=${plugin.state}(protocol=${plugin.protocol_version},pid=${plugin.pid ?? "none"},restarts=${plugin.restarts}${plugin.last_error ? `,error=${plugin.last_error}` : ""})`).join("; ") || "none active"}`,
    `Accelerator providers: ${state.engine.accelerator_providers.map((provider) => `${provider.provider}=installed:${yesNo(provider.installed)},valid:${yesNo(provider.valid)},qualified:${yesNo(provider.qualified)},active:${yesNo(provider.active)},pack:${provider.pack_version ?? "none"},error:${provider.numerical_max_abs_error ?? "not-tested"},cpu:${provider.cpu_reduction_percent ?? "not-tested"}%,deadlines:${provider.deadline_misses ?? "not-tested"},discontinuities:${provider.discontinuities ?? "not-tested"},fallback:${yesNo(provider.fallback_validated)},live:${yesNo(provider.live_workload_validated)} (${provider.detail})`).join("; ") || "none installed"}`,
    "",
    "## Audio Settings",
    `Sample rate: ${state.config.audio.sample_rate_hz}`,
    `Bit depth: ${state.config.audio.bit_depth}`,
    `Channel layout: ${state.config.audio.channel_layout}`,
    `Mono inputs to stereo: ${yesNo(state.config.audio.mono_inputs_to_stereo)}`,
    `Low-latency monitoring: ${yesNo(settings.low_latency_mic_monitoring)}`,
    `Adaptive latency: ${yesNo(settings.adaptive_latency.enabled)} target=${state.engine.adaptive_latency.target_msec}ms quantum=${state.engine.adaptive_latency.pipewire_quantum_frames || "default"} frames floor=${state.engine.adaptive_latency.pipewire_quantum_floor_frames || "none"} range=${state.engine.adaptive_latency.min_msec}-${state.engine.adaptive_latency.max_msec}ms reason=${state.engine.adaptive_latency.last_reason}`,
    `Hardware direct mic monitoring: ${yesNo(settings.hardware_direct_mic_monitoring)}`,
    `Stream sync delay: ${settings.stream_sync_delay_msec} ms`,
    `Monitor sync delay: ${settings.monitor_sync_delay_msec} ms`,
    "",
    "## Native Audio Core",
    ...state.engine.audio_core.map(
      (core) =>
        `- ${core.channel_id}: online=${yesNo(core.online)} target=${core.target_latency_msec}ms buffer=${core.buffer_fill_msec.toFixed(1)}ms rate=${core.rate_correction.toFixed(6)} worker_running=${yesNo(core.worker_running)} worker_queue=${core.worker_queue_frames}/${core.worker_queue_capacity_frames} worker_blocks=${core.worker_blocks} worker_overruns=${core.worker_overrun_frames} capture_callbacks=${core.capture_callbacks} process=${core.last_process_micros}us max=${core.max_process_micros}us underruns=${core.underrun_frames} delta=${core.underrun_delta} dropped=${core.dropped_frames} non_finite_blocks=${core.non_finite_blocks} non_finite_samples=${core.non_finite_samples} effect_mask=0x${core.non_finite_effect_mask.toString(16)} recoveries=${core.chain_recoveries} swaps=${core.chain_swaps} replacements=${core.chain_swap_replacements} retired_overflows=${core.retired_chain_overflows} accelerator=${core.accelerator_provider ?? "cpu"} accelerator_states=${core.accelerator_active_states} accelerator_pids=${core.accelerator_provider_pids.join(",") || "none"} accelerator_blocks=${core.accelerator_provider_blocks} accelerator_fallbacks=${core.accelerator_fallback_blocks} accelerator_deadlines=${core.accelerator_deadline_misses} accelerator_invalid=${core.accelerator_invalid_results} accelerator_stale=${core.accelerator_stale_results} accelerator_disabled=${core.accelerator_disabled_states}${core.accelerator_startup_failures.length ? ` accelerator_startup_failures=${core.accelerator_startup_failures.join(";")}` : ""}${core.accelerator_last_failure ? ` accelerator_error=${core.accelerator_last_failure}` : ""} submitted_generation=${core.submitted_generation} acknowledged_generation=${core.acknowledged_generation} submitted_route_generation=${core.submitted_route_generation} applied_route_generation=${core.applied_route_generation} targets=${[core.input_target_node_name, ...core.output_target_node_names].filter(Boolean).join(",") || "none"}${core.route_target_error ? ` route_error=${core.route_target_error}` : ""}${core.error ? ` error=${core.error}` : ""}`,
    ),
    "",
    "## Graph Counts",
    `Mixes: ${state.config.mixes.length}`,
    `Channels: ${state.config.channels.length}`,
    `Inputs: ${state.graph.inputs.length}`,
    `Outputs: ${state.graph.outputs.length}`,
    `App streams: ${state.graph.app_streams.length}`,
    `Meters: ${state.graph.meters.length}`,
    `Managed modules: ${graphReport?.managed_modules.length ?? "not loaded"}`,
    `Routes: ${graphReport ? graphReport.sink_input_routes.length + graphReport.source_output_routes.length : "not loaded"}`,
    `Route health issues: ${graphReport?.route_health.length ?? "not loaded"}`,
    `Stale processes: ${graphReport?.stale_processes.length ?? "not loaded"}`,
    "",
    "## Devices",
    "Inputs:",
    ...reportDeviceList(state.graph.inputs),
    "Outputs:",
    ...reportDeviceList(state.graph.outputs),
    "",
    "## Streamer Devices",
    ...(streamerDeviceError
      ? [`Detection error: ${streamerDeviceError}`]
      : reportStreamerDevices(streamerDevices)),
    "",
    "## Elgato Devices",
    ...(elgatoDeviceError
      ? [`Detection error: ${elgatoDeviceError}`]
      : reportElgatoDevices(elgatoDevices)),
    "",
    "## Diagnostics",
    ...reportDiagnostics(diagnostics),
    "",
    "## Effects",
    ...(state.engine.effects.length
      ? state.engine.effects.map(
          (effect) =>
            `- ${effect.channel_id}: state=${effect.state} selected=${effect.selected_effect_count} desired_enabled=${yesNo(effect.desired_enabled)} pending=${yesNo(effect.pending)} core_healthy=${yesNo(effect.core_healthy)} desired_generation=${effect.desired_generation} applied_generation=${effect.applied_generation} in_flight_generation=${effect.in_flight_generation ?? "none"} coalesced_requests=${effect.coalesced_requests} socket=${effect.control_socket}${effect.last_error ? ` error=${effect.last_error}` : ""}`,
        )
      : ["- Runtime status: none"]),
    ...(missingEffects.length
      ? missingEffects.map((effect) => `- Missing: ${effect}`)
      : ["- Missing: none"]),
    "",
    "## Sound Check",
    report
      ? `Active streams: ${report.active_stream_count}; virtual mixes: ${report.virtual_mix_count}; debug log: ${report.debug_log_path || "none"}`
      : "Not run",
    "",
    "## Last Audio Action",
    audioActionReport
      ? `${audioActionReport.title}; commands: ${audioActionReport.commands.length}; planned: ${audioActionReport.plannedCount ?? "unknown"}; finished: ${new Date(audioActionReport.finishedAt).toISOString()}`
      : "None",
    "",
    "## Recent Debug Log",
    ...reportRecentLog(report, graphReport),
  ];
  return lines.join("\n");
}

function reportDeviceList(devices: DeviceInfo[]): string[] {
  if (devices.length === 0) return ["- none"];
  return devices.slice(0, 20).map((device) => {
    const usb =
      device.vendor_id || device.product_id
        ? ` usb=${valueOrNone(device.vendor_id)}:${valueOrNone(device.product_id)}`
        : "";
    const profile = device.matched_profile_id || device.active_profile || "none";
    const defaultState = device.is_default ? " default" : "";
    const virtualState = device.is_virtual ? " virtual" : "";
    const activePort = device.active_port
      ? ` | active_port=${device.active_port}:${device.ports?.find((port) => port.name === device.active_port)?.availability ?? "unknown"}`
      : "";
    return `- ${device.description || device.name} | id=${device.id} | available=${yesNo(device.is_available)}${defaultState}${virtualState} | bus=${valueOrNone(device.bus)}${usb}${activePort} | profile=${profile}`;
  });
}

function reportStreamerDevices(devices: StreamerDeviceSummary[]): string[] {
  if (devices.length === 0) return ["- none detected"];
  return devices.map((device) => {
    const usb =
      device.vendor_id || device.product_id
        ? ` | usb=${valueOrNone(device.vendor_id)}:${valueOrNone(device.product_id)}`
        : "";
    return `- ${device.name} | ${device.family}/${device.transport} | enabled=${yesNo(device.enabled)} | status=${device.permission_status}${usb} | caps=${formatStreamerCaps(device)} | ${device.message || "no message"}`;
  });
}

function reportElgatoDevices(devices: ElgatoDeviceSummary[]): string[] {
  if (devices.length === 0) return ["- none detected"];
  return devices.map((device) => {
    const usb =
      device.vendor_id || device.product_id
        ? ` | usb=${valueOrNone(device.vendor_id)}:${valueOrNone(device.product_id)}`
        : "";
    return `- ${device.name} | ${device.kind} | controls=${yesNo(device.controls_supported)} | bus=${valueOrNone(device.bus)}${usb} | alsa_card=${valueOrNone(device.alsa_card)} | ${device.message || "no message"}`;
  });
}

function reportDiagnostics(diagnostics: Diagnostic[]): string[] {
  if (diagnostics.length === 0) return ["- none"];
  return diagnostics.map(
    (item) =>
      `- [${item.severity}] ${item.code}: ${item.message}${item.action ? ` (${item.action})` : ""}`,
  );
}

function reportRecentLog(
  report: SoundCheckReport | null,
  graphReport: GraphDebugReport | null,
): string[] {
  const lines = graphReport?.recent_log_lines.length
    ? graphReport.recent_log_lines
    : (report?.recent_log_lines ?? []);
  if (lines.length === 0) return ["No recent log lines captured."];
  return ["```text", ...lines.slice(-25), "```"];
}

function formatStreamerCaps(device: StreamerDeviceSummary): string {
  const caps = Object.entries(device.capabilities)
    .filter(([, enabled]) => enabled)
    .map(([key]) => key);
  return caps.length ? caps.join(",") : "none";
}

function yesNo(value: boolean): string {
  return value ? "yes" : "no";
}

function valueOrNone(value: string | null | undefined): string {
  return value && value.trim() ? value : "none";
}
