import {
  Activity,
  AudioLines,
  Cable,
  Check,
  CircleAlert,
  Clipboard,
  Gauge,
  GitBranch,
  Info,
  Mic,
  Radio,
  RefreshCw,
  Sparkles,
  WandSparkles,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";

import { EmptyState, Stat } from "../components/Controls";
import {
  buildTestingHealthReport,
  TestingHealthReport,
  type AudioActionReport,
} from "../components/TestingHealthReport";
import { invoke } from "../tauri";
import type {
  AppStateSnapshot,
  Channel,
  ElgatoDeviceSummary,
  GraphDebugReport,
  RouteHealthIssue,
  SoundCheckReport,
  StreamerDeviceSummary,
  UpdateInfo,
} from "../types";

export function DiagnosticsView({
  audioActionReport,
  onPrune,
  state,
  updateInfo,
  run,
}: {
  audioActionReport: AudioActionReport | null;
  onPrune: () => void | Promise<unknown>;
  state: AppStateSnapshot;
  updateInfo: UpdateInfo | null;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const [report, setReport] = useState<SoundCheckReport | null>(null);
  const [graphReport, setGraphReport] = useState<GraphDebugReport | null>(null);
  const [streamerDevices, setStreamerDevices] = useState<StreamerDeviceSummary[]>([]);
  const [streamerDeviceError, setStreamerDeviceError] = useState<string | null>(null);
  const [elgatoDevices, setElgatoDevices] = useState<ElgatoDeviceSummary[]>([]);
  const [elgatoDeviceError, setElgatoDeviceError] = useState<string | null>(null);
  const [testingReportStatus, setTestingReportStatus] = useState<string | null>(null);
  const diagnostics = report?.diagnostics ?? state.diagnostics;
  const loadTestingDevices = useCallback(async () => {
    try {
      const next = await invoke<StreamerDeviceSummary[]>("list_streamer_devices");
      setStreamerDevices(next);
      setStreamerDeviceError(null);
    } catch (error) {
      setStreamerDevices([]);
      setStreamerDeviceError(String(error));
    }
    try {
      const next = await invoke<ElgatoDeviceSummary[]>("list_elgato_devices");
      setElgatoDevices(next);
      setElgatoDeviceError(null);
    } catch (error) {
      setElgatoDevices([]);
      setElgatoDeviceError(String(error));
    }
  }, []);
  useEffect(() => {
    void loadTestingDevices();
  }, [loadTestingDevices]);
  const testingHealthReport = useMemo(
    () =>
      buildTestingHealthReport({
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
      }),
    [
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
    ],
  );
  const copyTestingHealthReport = async () => {
    try {
      await navigator.clipboard.writeText(testingHealthReport);
      setTestingReportStatus("Copied");
    } catch {
      setTestingReportStatus("Copy failed");
    }
  };
  return (
    <section className="two-column diagnostics-view">
      <div className="panel">
        <div className="panel-header">
          <h2>Checks</h2>
          <div className="panel-actions">
            <button
              className="secondary-button"
              onClick={() =>
                void run<SoundCheckReport>("run_sound_check")
                  .then(setReport)
                  .catch(() => undefined)
              }
              type="button"
            >
              <Activity size={16} />
              Run
            </button>
            <button
              className="secondary-button"
              onClick={() =>
                void run<GraphDebugReport>("get_graph_debug_report")
                  .then(setGraphReport)
                  .catch(() => undefined)
              }
              type="button"
              title="Inspect WaveLinux-managed PipeWire modules, routes, and planned commands"
            >
              <Clipboard size={16} />
              Graph
            </button>
            <button
              className="secondary-button"
              onClick={() => void onPrune()}
              type="button"
              title="Remove old WaveLinux modules without rebuilding the active graph"
            >
              <WandSparkles size={16} />
              Prune
            </button>
          </div>
        </div>
        <div className="diagnostic-list">
          {diagnostics.map((item) => (
            <div className={`diagnostic-row ${item.severity}`} key={item.code}>
              {item.severity === "error" ? <CircleAlert size={17} /> : <Check size={17} />}
              <div>
                <strong>{item.message}</strong>
                {item.action && <span>{item.action}</span>}
              </div>
            </div>
          ))}
        </div>
      </div>
      <div className="panel">
        <div className="panel-header">
          <h2>Sound Check</h2>
          <AudioLines size={18} />
        </div>
        {report ? (
          <div className="sound-check-stack">
            <div className="sound-grid">
              <Stat icon={Cable} label="Streams" value={String(report.active_stream_count)} />
              <Stat icon={Radio} label="Mixes" value={String(report.virtual_mix_count)} />
              <Stat icon={Sparkles} label="Missing FX" value={String(report.missing_effects.length)} />
            </div>
            <div className="debug-log">
              <div className="debug-log-header">
                <strong>Debug Log</strong>
                <code>{report.debug_log_path}</code>
              </div>
              {report.recent_log_lines.length > 0 ? (
                <pre>{report.recent_log_lines.join("\n")}</pre>
              ) : (
                <EmptyState label="No debug log entries yet" />
              )}
            </div>
          </div>
        ) : (
          <EmptyState label="No sound check report" />
        )}
        {graphReport && <GraphDebugSummary report={graphReport} />}
        {audioActionReport && <AudioActionSummary report={audioActionReport} />}
        <LatencySummary state={state} />
        <RuntimeHealthSummary state={state} />
        <EffectAvailabilitySummary state={state} />
        <TestingHealthReport
          onCopy={copyTestingHealthReport}
          onRefresh={loadTestingDevices}
          report={testingHealthReport}
          status={testingReportStatus}
        />
      </div>
    </section>
  );
}

function LatencySummary({ state }: { state: AppStateSnapshot }) {
  const latencySensitiveFx = state.config.channels.flatMap((channel) =>
    channel.effects
      .filter((effect) => !effect.bypassed && ["rnnoise", "convolver"].includes(effect.effect_id))
      .map((effect) => `${channelDisplayName(channel)}: ${effect.effect_id}`),
  );
  const activeMixRoutes = state.config.channels.length * state.config.mixes.length;
  const hardwareInput = state.config.channels.find(isHardwareChannel);
  const hardwareCore = hardwareInput
    ? state.engine.audio_core.find((core) => core.channel_id === hardwareInput.id)
    : undefined;
  const targetLatency = hardwareCore?.target_latency_msec ?? state.engine.adaptive_latency.target_msec;
  const currentBuffer = hardwareCore?.buffer_fill_msec ?? state.engine.adaptive_latency.buffer_fill_msec;
  const streamDelay = state.config.settings.stream_sync_delay_msec;

  return (
    <div className="latency-card command-report">
      <div className="command-report-header">
        <div>
          <strong>Latency</strong>
          <span>PipeWire path estimate for monitoring and virtual mic use</span>
        </div>
        <div className={latencySensitiveFx.length ? "command-pill info" : "command-pill"}>
          {latencySensitiveFx.length ? "DSP Active" : "Low"}
        </div>
      </div>
      <div className="command-stats">
        <Stat icon={Gauge} label="Mic target" value={`${targetLatency} ms`} />
        <Stat icon={Mic} label="Buffer fill" value={currentBuffer == null ? "Waiting" : `${currentBuffer.toFixed(1)} ms`} />
        <Stat icon={GitBranch} label="Stream sync" value={`${streamDelay} ms`} />
      </div>
      <div className="command-stats">
        <Stat icon={Radio} label="Monitor sync" value={`${state.config.settings.monitor_sync_delay_msec} ms`} />
        <Stat icon={Cable} label="Routes" value={String(activeMixRoutes)} />
        <Stat icon={Activity} label="Mode" value={state.config.settings.low_latency_mic_monitoring ? "Low" : "Stable"} />
      </div>
      {latencySensitiveFx.length > 0 && (
        <div className="latency-note info">
          <Info size={15} />
          <span>{latencySensitiveFx.join(", ")}</span>
        </div>
      )}
      {state.engine.audio_core.length > 0 && (
        <div className="command-list">
          {state.engine.audio_core.map((core) => (
            <div className={core.online ? "command-row compact" : "command-row compact error"} key={core.channel_id}>
              <div>
                <strong>{core.channel_id}</strong>
                <span>{core.online ? `${core.last_process_micros} us DSP worker` : core.error ?? "Offline"}</span>
              </div>
              <code className="command-line">
                {core.target_latency_msec} ms / {core.buffer_fill_msec.toFixed(1)} ms buffer / worker queue {core.worker_queue_frames} / {core.worker_queue_capacity_frames} / {(core.rate_correction * 1_000_000 - 1_000_000).toFixed(0)} ppm / {core.underrun_delta} new underruns
              </code>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function RuntimeHealthSummary({ state }: { state: AppStateSnapshot }) {
  const refresh = state.engine.refresh;
  const audioHealth = state.engine.pipewire_audio_health;
  const registry = state.engine.pipewire_registry;
  const phases = Object.entries(refresh.last_phase_msec)
    .map(([phase, msec]) => `${phase}=${msec}ms`)
    .join(" ");
  return (
    <div className="command-report">
      <div className="command-report-header">
        <div>
          <strong>Runtime</strong>
          <span>Current engine counters</span>
        </div>
        <div className={audioHealth.warning_events > 0 ? "command-pill info" : "command-pill"}>
          {audioHealth.warning_events > 0 ? `${audioHealth.warning_events} warnings` : "Clean"}
        </div>
      </div>
      <div className="command-stats">
        <Stat icon={RefreshCw} label="Last refresh" value={`${refresh.last_total_msec} ms`} />
        <Stat icon={Gauge} label="Peak refresh" value={`${refresh.peak_total_msec} ms`} />
        <Stat icon={GitBranch} label="Route repairs" value={String(refresh.route_mutations)} />
      </div>
      <div className="command-stats">
        <Stat icon={Activity} label="Registry" value={registry.connected ? `Gen ${registry.generation}` : "Offline"} />
        <Stat icon={Cable} label="Link failures" value={String(audioHealth.link_failures)} />
        <Stat icon={AudioLines} label="Direct errors" value={String(audioHealth.direct_errors)} />
      </div>
      <code className="command-line">
        {phases || "No completed refresh"} · snapshots={refresh.snapshot_commands} failures={refresh.snapshot_failures} · registry={registry.node_count} nodes/{registry.link_count} links · registry-errors={registry.direct_node_errors + registry.direct_link_failures} · profiler={audioHealth.profiler_available ? "active" : "unavailable"} · journal={audioHealth.monitor_available ? "active" : "unavailable"}
      </code>
      {state.engine.accelerator_providers.length > 0 && (
        <div className="command-list">
          {state.engine.accelerator_providers.map((provider) => (
            <div className="command-row compact" key={provider.provider}>
              <div>
                <strong>{provider.provider}</strong>
                <span>{provider.detail}</span>
              </div>
              <code className="command-line">
                {provider.active
                  ? "active"
                  : provider.qualified
                    ? "qualified / inactive"
                    : provider.valid
                      ? "installed / unqualified"
                      : provider.installed
                        ? "invalid"
                        : "not installed"}
              </code>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function GraphDebugSummary({ report }: { report: GraphDebugReport }) {
  const visibleCommands = report.planned.commands.slice(0, 6);
  const visibleModules = report.managed_modules.slice(0, 6);
  const visibleRouteHealth = report.route_health.slice(0, 6);
  const routeCount = report.sink_input_routes.length + report.source_output_routes.length;
  const healthIssueCount = report.route_health.length + report.stale_processes.length;

  return (
    <div className="graph-debug command-report">
      <div className="command-report-header">
        <div>
          <strong>Graph Debug</strong>
          <span>{report.audio_graph_running ? "Managed graph is present" : "Managed graph is stopped"}</span>
        </div>
        <div className={healthIssueCount ? "command-pill warning" : "command-pill"}>
          {healthIssueCount ? `${healthIssueCount} issue${healthIssueCount === 1 ? "" : "s"}` : "Clean"}
        </div>
      </div>
      <div className="command-stats">
        <Stat icon={WandSparkles} label="Planned" value={String(report.planned.commands.length)} />
        <Stat icon={Cable} label="Modules" value={String(report.managed_modules.length)} />
        <Stat icon={GitBranch} label="Routes" value={String(routeCount)} />
        <Stat icon={Activity} label="Health" value={report.route_health.length ? String(report.route_health.length) : "OK"} />
      </div>
      <div className="graph-debug-grid">
        <div className="graph-debug-section">
          <strong>Managed Modules</strong>
          {visibleModules.map((module) => (
            <code key={module.module_id}>
              {module.module_id} {module.role ?? "module"} {module.node_name ?? module.sink_name ?? module.source_name ?? ""}
            </code>
          ))}
          {visibleModules.length === 0 && <span>No WaveLinux modules visible</span>}
        </div>
        <div className="graph-debug-section">
          <strong>Planned Commands</strong>
          {visibleCommands.map((command, index) => (
            <code key={`${command.description}-${index}`}>{command.description || commandLine(command.program, command.args)}</code>
          ))}
          {visibleCommands.length === 0 && <span>Graph already matches config</span>}
        </div>
        {visibleRouteHealth.length > 0 && (
          <div className="graph-debug-section">
            <strong>Route Health</strong>
            {visibleRouteHealth.map((issue, index) => (
              <code key={`${issue.module_id ?? issue.role}-${index}`}>{routeHealthLabel(issue)}</code>
            ))}
          </div>
        )}
      </div>
      <div className="debug-log">
        <div className="debug-log-header">
          <strong>Engine Log</strong>
          <code>{report.debug_log_path}</code>
        </div>
        {report.recent_log_lines.length > 0 ? (
          <pre>{report.recent_log_lines.join("\n")}</pre>
        ) : (
          <EmptyState label="No debug log entries yet" />
        )}
      </div>
    </div>
  );
}

function routeHealthLabel(issue: RouteHealthIssue) {
  const scope = [issue.channel_id ? `channel ${issue.channel_id}` : "", issue.mix_id ? `mix ${issue.mix_id}` : ""]
    .filter(Boolean)
    .join(" ");
  const module = issue.module_id ? `#${issue.module_id}` : "module";
  return `${module} ${issue.role}${scope ? ` ${scope}` : ""}: ${routeHealthReasonLabel(issue.reason)}`;
}

function routeHealthReasonLabel(reason: RouteHealthIssue["reason"]) {
  switch (reason) {
    case "missing_source":
      return "source missing";
    case "missing_sink":
      return "sink missing";
    case "missing_source_output":
      return "source-output missing";
    case "missing_sink_input":
      return "sink-input missing";
    case "stale_config":
      return "stale config";
    case "duplicate":
      return "duplicate";
    case "level_mismatch":
      return "level mismatch";
    default:
      return reason;
  }
}

function AudioActionSummary({ report }: { report: AudioActionReport }) {
  const failures = report.commands.filter((command) => command.error).length;
  const skipped = report.commands.filter((command) => command.skipped).length;
  const ran = Math.max(0, report.commands.length - skipped);
  const visibleCommands = report.commands.slice(0, 8);

  return (
    <div className="command-report">
      <div className="command-report-header">
        <div>
          <strong>{report.title}</strong>
          <span>{new Date(report.finishedAt).toLocaleTimeString()}</span>
        </div>
        <div className={failures ? "command-pill error" : "command-pill"}>
          {failures ? `${failures} failed` : `${ran} ran`}
        </div>
      </div>
      <div className="command-stats">
        <Stat icon={WandSparkles} label="Planned" value={String(report.plannedCount ?? report.commands.length)} />
        <Stat icon={Check} label="Skipped" value={String(skipped)} />
        <Stat icon={CircleAlert} label="Errors" value={String(failures)} />
      </div>
      <div className="command-list">
        {visibleCommands.map((execution, index) => (
          <div className={execution.error ? "command-row error" : "command-row"} key={`${execution.command.description}-${index}`}>
            <div>
              <strong>{execution.command.description || execution.command.program || "Audio command"}</strong>
              <span>{execution.skipped ? "Skipped" : execution.command.domain}</span>
            </div>
            <code className="command-line">{commandLine(execution.command.program, execution.command.args)}</code>
            {execution.error && <span className="command-error">{execution.error}</span>}
          </div>
        ))}
        {report.commands.length > visibleCommands.length && (
          <div className="command-row compact">
            <span>{report.commands.length - visibleCommands.length} more commands</span>
          </div>
        )}
        {report.commands.length === 0 && <EmptyState label="No host commands were needed" />}
      </div>
    </div>
  );
}

function EffectAvailabilitySummary({ state }: { state: AppStateSnapshot }) {
  const availabilityById = new Map(state.graph.effect_availability.map((item) => [item.effect_id, item]));
  const visibleEffectIds = state.catalog.preferred_order.filter(isVisibleUserCatalogEffect);
  const visibleEffectIdSet = new Set(visibleEffectIds);
  const available = state.graph.effect_availability.filter(
    (item) => item.available && visibleEffectIdSet.has(item.effect_id),
  ).length;
  const total = visibleEffectIds.length;
  const missing = total - available;

  return (
    <div className="fx-availability">
      <div className="command-report-header">
        <div>
          <strong>Effect Availability</strong>
          <span>{available}/{total} bundled DSP processors ready</span>
        </div>
        <div className="panel-actions">
          <div className={available === total ? "command-pill" : "command-pill warning"}>
            {available === total ? "Ready" : `${missing} missing`}
          </div>
        </div>
      </div>
      <div className="fx-availability-list">
        {visibleEffectIds.map((effectId) => {
          const definition = state.catalog.effects.find((effect) => effect.id === effectId);
          if (!definition) return null;
          const availability = availabilityById.get(effectId);
          const isAvailable = availability?.available ?? false;
          return (
            <div className={isAvailable ? "fx-availability-row" : "fx-availability-row missing"} key={effectId}>
              {isAvailable ? <Check size={15} /> : <CircleAlert size={15} />}
              <div>
                <strong>{definition.name}</strong>
                <span>{availability?.detail ?? "Not probed"}</span>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function isHardwareChannel(channel: Pick<Channel, "kind">): boolean {
  return channel.kind === "microphone" || channel.kind === "generic";
}

function channelDisplayName(
  channel: Pick<Channel, "id" | "kind" | "name">,
): string {
  if (
    channel.id === "hardware_in" &&
    isHardwareChannel(channel) &&
    ["hardware in", "hardware input", "input"].includes(
      channel.name.trim().toLowerCase(),
    )
  ) {
    return "Input";
  }
  return channel.name;
}

function isVisibleUserCatalogEffect(effectId: string): boolean {
  return effectId !== "deepfilternet";
}

function commandLine(program: string, args: string[]): string {
  if (!program) return "No command";
  return [program, ...args]
    .map((part) => (part.includes(" ") ? `"${part}"` : part))
    .join(" ");
}
