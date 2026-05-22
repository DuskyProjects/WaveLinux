import {
  Activity,
  AudioLines,
  ArrowDown,
  ArrowUp,
  BadgeCheck,
  Cable,
  Check,
  CircleAlert,
  CirclePlus,
  Clipboard,
  Copy,
  Cpu,
  Gauge,
  GitBranch,
  GripVertical,
  Headphones,
  Mic,
  MonitorSpeaker,
  Music2,
  Pencil,
  Radio,
  RefreshCw,
  Save,
  Settings,
  SlidersHorizontal,
  Sparkles,
  Trash2,
  Volume2,
  VolumeX,
  WandSparkles,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
} from "react";
import { initialSnapshot, invoke } from "./tauri";
import type {
  AppStateSnapshot,
  AppMatcher,
  AppStream,
  AppVolumePreset,
  Channel,
  ChannelInputMode,
  ChannelKind,
  CommandExecution,
  ConfigBackup,
  EffectDefinition,
  EffectAvailability,
  EffectInstance,
  GraphDebugReport,
  LevelMeter,
  Mix,
  MixBus,
  MixerSettings,
  RepairReport,
  Scene,
  SetupTemplate,
  SoundCheckReport,
} from "./types";

type View = "mixer" | "routing" | "effects" | "scenes" | "diagnostics" | "settings";

const views: Array<{ id: View; label: string; icon: typeof SlidersHorizontal }> = [
  { id: "mixer", label: "Mixer", icon: SlidersHorizontal },
  { id: "routing", label: "Routing", icon: GitBranch },
  { id: "effects", label: "Effects", icon: Sparkles },
  { id: "scenes", label: "Scenes", icon: Save },
  { id: "diagnostics", label: "Health", icon: Activity },
  { id: "settings", label: "Settings", icon: Settings },
];

const inputModeOptions: Array<{ id: ChannelInputMode; label: string }> = [
  { id: "stereo", label: "Stereo" },
  { id: "mono_left", label: "Left to stereo" },
  { id: "mono_right", label: "Right to stereo" },
  { id: "sum_mono", label: "Sum to mono" },
  { id: "swap_lr", label: "Swap L/R" },
];

function initialView(): View {
  if (typeof window === "undefined") return "mixer";
  const params = new URLSearchParams(window.location.search);
  const requested = params.get("view") ?? window.location.hash.replace(/^#\/?/, "");
  return views.some((view) => view.id === requested) ? (requested as View) : "mixer";
}

const MAX_SOFTWARE_CHANNELS = 8;
const MAX_HARDWARE_INPUTS = 4;
const matcherKinds = ["app_id", "process_name", "binary", "window_class", "media_name"] as const;
type MatcherKind = (typeof matcherKinds)[number];

type AudioActionReport = {
  title: string;
  commands: CommandExecution[];
  plannedCount?: number;
  finishedAt: number;
};

type HeldMeterMap = Record<string, { level: number; seenAt: number }>;
type OfflineRoutingEntry = {
  matcher: AppMatcher;
  displayName: string;
  meta: string;
  channel_id?: string;
  volumePreset?: AppVolumePreset;
};

export default function App() {
  const [state, setState] = useState<AppStateSnapshot | null>(() => initialSnapshot());
  const [activeView, setActiveView] = useState<View>(() => initialView());
  const [activeMixId, setActiveMixId] = useState("monitor");
  const [selectedChannelId, setSelectedChannelId] = useState("hardware_in");
  const [busy, setBusy] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [audioActionReport, setAudioActionReport] = useState<AudioActionReport | null>(null);

  const refresh = useCallback(async () => {
    const next = await invoke<AppStateSnapshot>("get_state");
    setState(next);
    if (!next.config.mixes.some((mix) => mix.id === activeMixId)) {
      setActiveMixId(next.config.mixes[0]?.id ?? "monitor");
    }
    if (!next.config.channels.some((channel) => channel.id === selectedChannelId)) {
      setSelectedChannelId(next.config.channels[0]?.id ?? "hardware_in");
    }
  }, [activeMixId, selectedChannelId]);

  const run = useCallback(
    async <T,>(command: string, args?: Record<string, unknown>, message?: string): Promise<T> => {
      setBusy(true);
      try {
        const result = await invoke<T>(command, args);
        await refresh();
        if (message) {
          setToast(message);
        }
        return result;
      } catch (error) {
        setToast(String(error));
        throw error;
      } finally {
        setBusy(false);
      }
    },
    [refresh],
  );

  const recordAudioAction = useCallback((title: string, commands: CommandExecution[], plannedCount?: number) => {
    setAudioActionReport({ title, commands, plannedCount, finishedAt: Date.now() });
    setToast(audioActionToast(title, commands, plannedCount));
  }, []);

  const startOrRepairAudio = useCallback(async () => {
    const title = state?.engine.audio_graph_running ? "Repair Audio" : "Start Audio";
    const report = await run<RepairReport>("repair_audio_graph");
    recordAudioAction(title, report.outputs, report.planned.commands.length);
  }, [recordAudioAction, run, state?.engine.audio_graph_running]);

  const runAudioCommandList = useCallback(
    async (command: string, title: string) => {
      const outputs = await run<CommandExecution[]>(command);
      recordAudioAction(title, outputs);
    },
    [recordAudioAction, run],
  );

  useEffect(() => {
    refresh().catch((error) => setToast(String(error)));
    const timer = window.setInterval(() => {
      invoke<AppStateSnapshot>("observe_state")
        .then(setState)
        .catch(() => undefined);
    }, 1000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    if (!toast) return;
    const timer = window.setTimeout(() => setToast(null), 2400);
    return () => window.clearTimeout(timer);
  }, [toast]);

  const selectedChannel = state?.config.channels.find((channel) => channel.id === selectedChannelId);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">
            <AudioLines size={22} />
          </div>
          <div>
            <strong>WaveLinux</strong>
            <span>4.0</span>
          </div>
        </div>

        <nav className="nav-list">
          {views.map((view) => {
            const Icon = view.icon;
            return (
              <button
                className={activeView === view.id ? "nav-item active" : "nav-item"}
                key={view.id}
                onClick={() => setActiveView(view.id)}
                type="button"
                title={view.label}
              >
                <Icon size={18} />
                <span>{view.label}</span>
              </button>
            );
          })}
        </nav>

        <div className="engine-card">
          <div className="engine-row">
            {state?.engine.healthy ? <BadgeCheck size={18} /> : <CircleAlert size={18} />}
            <span>{state?.engine.message ?? "Starting"}</span>
          </div>
          <div className="engine-meta">
            {state?.engine.dry_run ? "Dry run" : state?.engine.audio_graph_running ? "Graph running" : "Graph stopped"} ·{" "}
            {state?.config.audio.sample_rate_hz ?? 48000} Hz
          </div>
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <div>
            <h1>{viewTitle(activeView)}</h1>
            <p>
              {state
                ? `${state.config.mixes.length} mixes · ${state.config.channels.length} channels${
                    state.config.settings.restore_audio_graph_on_launch ? " · startup restore on" : ""
                  }`
                : "Loading"}
            </p>
          </div>
          <div className="top-actions">
            <button className="icon-button" onClick={() => refresh()} title="Refresh" type="button">
              <RefreshCw size={17} />
            </button>
            {state?.engine.audio_graph_running && (
              <button
                className="secondary-button danger"
                disabled={busy}
                onClick={() => void runAudioCommandList("cleanup_audio_graph", "Stop Audio")}
                type="button"
              >
                <Trash2 size={17} />
                Stop
              </button>
            )}
            <button
              className="primary-button"
              disabled={busy || !state}
              onClick={() => void startOrRepairAudio()}
              type="button"
            >
              <WandSparkles size={17} />
              {state?.engine.audio_graph_running ? "Repair" : "Start Audio"}
            </button>
          </div>
        </header>

        {!state ? (
          <div className="loading-panel">Starting audio engine</div>
        ) : (
          <>
            {activeView === "mixer" && (
              <MixerView
                activeMixId={activeMixId}
                state={state}
                setActiveMixId={setActiveMixId}
                setSelectedChannelId={setSelectedChannelId}
                run={run}
                busy={busy}
              />
            )}
            {activeView === "routing" && <RoutingView state={state} run={run} />}
            {activeView === "effects" && (
              <EffectsView
                state={state}
                selectedChannel={selectedChannel}
                selectedChannelId={selectedChannelId}
                setSelectedChannelId={setSelectedChannelId}
                run={run}
              />
            )}
            {activeView === "scenes" && <ScenesView run={run} state={state} />}
            {activeView === "diagnostics" && (
              <DiagnosticsView
                audioActionReport={audioActionReport}
                onCleanup={() => runAudioCommandList("cleanup_audio_graph", "Cleanup Audio")}
                onPrune={() => runAudioCommandList("cleanup_stale_audio_graph", "Prune Stale Audio")}
                state={state}
                run={run}
              />
            )}
            {activeView === "settings" && <SettingsView state={state} run={run} />}
          </>
        )}
      </main>

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}

function MixerView({
  state,
  activeMixId,
  setActiveMixId,
  setSelectedChannelId,
  run,
  busy,
}: {
  state: AppStateSnapshot;
  activeMixId: string;
  setActiveMixId: (mixId: string) => void;
  setSelectedChannelId: (channelId: string) => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  busy: boolean;
}) {
  const outputs = state.graph.outputs.filter((output) => !output.is_virtual);
  const softwareChannelCount = state.config.channels.filter((channel) => !isHardwareChannel(channel)).length;
  const hardwareInputCount = state.config.channels.filter(isHardwareChannel).length;
  const [menu, setMenu] = useState<{ x: number; y: number; channelId: string } | null>(null);
  const menuChannel = menu
    ? state.config.channels.find((channel) => channel.id === menu.channelId)
    : undefined;
  const menuChannelIndex = menu
    ? state.config.channels.findIndex((channel) => channel.id === menu.channelId)
    : -1;
  const [heldMeters, setHeldMeters] = useState<HeldMeterMap>({});
  const primaryMixes = primaryBusMixes(state.config.mixes);
  const selectedMix =
    state.config.mixes.find((mix) => mix.id === activeMixId) ??
    state.config.mixes[0];
  const metersUnavailable =
    state.engine.audio_graph_running &&
    !state.engine.dry_run &&
    state.graph.meters.length === 0;

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenu(null);
    };
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  useEffect(() => {
    const now = Date.now();
    const activeIds = new Set(state.graph.meters.map((meter) => meter.node_id));
    setHeldMeters((previous) => {
      const next: HeldMeterMap = {};
      for (const [nodeId, held] of Object.entries(previous)) {
        if (activeIds.has(nodeId) || now - held.seenAt < 2500) {
          next[nodeId] = held;
        }
      }
      for (const meter of state.graph.meters) {
        const level = levelFromMeter(meter);
        const previousLevel = next[meter.node_id]?.level ?? 0;
        next[meter.node_id] = {
          level: Math.max(level, previousLevel * 0.6),
          seenAt: now,
        };
      }
      return next;
    });
  }, [state.graph.meters]);

  const meterLevels = useMemo(
    () => meterLevelMap(state, heldMeters),
    [heldMeters, state.engine.audio_graph_running, state.graph.meters],
  );
  const levelFor = useCallback((nodeId: string) => meterLevels[nodeId] ?? 0, [meterLevels]);

  return (
    <section className="view-stack mixer-view-stack">
      <div className="mix-tabs">
        {state.config.mixes.map((mix) => (
          <button
            className={mix.id === activeMixId ? "mix-tab active" : "mix-tab"}
            key={mix.id}
            onClick={() => setActiveMixId(mix.id)}
            onDoubleClick={() => {
              const name = window.prompt("Mix name", mix.name);
              if (name && name !== mix.name) void run("rename_mix", { mixId: mix.id, name }, "Mix renamed");
            }}
            type="button"
            title={`${mix.name} virtual source`}
          >
            <Radio size={16} />
            {mixTabLabel(mix)}
          </button>
        ))}
        <button
          className="mix-tab add"
          disabled={state.config.mixes.length >= 5 || busy}
          onClick={() => {
            const name = window.prompt("Mix name", "MicrophoneFX");
            if (name) void run("create_mix", { name }, "Mix created");
          }}
          type="button"
          title="Create mix"
        >
          <CirclePlus size={16} />
        </button>
      </div>

      <div className="mixer-layout classic">
        <div className="source-strip-panel">
          <div className="source-toolbar">
            <div>
              <h2>Sources</h2>
              <span>
                {metersUnavailable
                  ? "Meters unavailable: no live meter data"
                  : selectedMix
                    ? `${selectedMix.name} mix selected`
                    : "Monitor and Stream"}
              </span>
            </div>
            <div className="panel-actions">
              {metersUnavailable && (
                <span className="meter-warning" title="No live pw-record meter samples are available yet">
                  <Gauge size={14} />
                  Meters unavailable
                </span>
              )}
              <button
                className="secondary-button"
                disabled={hardwareInputCount >= MAX_HARDWARE_INPUTS || busy}
                onClick={() => {
                  const name = window.prompt("Input name", "Hardware Input");
                  if (name) void run("create_channel", { name, kind: "generic" satisfies ChannelKind }, "Input added");
                }}
                type="button"
                title={`${hardwareInputCount}/${MAX_HARDWARE_INPUTS} hardware sources`}
              >
                <Cable size={16} />
                Input
              </button>
              <button
                className="secondary-button"
                disabled={softwareChannelCount >= MAX_SOFTWARE_CHANNELS || busy}
                onClick={() => {
                  const name = window.prompt("Channel name", "Podcast");
                  if (name) void run("create_channel", { name, kind: "application" satisfies ChannelKind }, "Channel created");
                }}
                type="button"
                title={`${softwareChannelCount}/${MAX_SOFTWARE_CHANNELS} software channels`}
              >
                <CirclePlus size={16} />
                App
              </button>
            </div>
          </div>

          <div className="channel-rail">
            {state.config.channels.map((channel) => {
              const sourceLevel = levelFor(channel.id);
              return (
                <ChannelStrip
                  channel={channel}
                  key={channel.id}
                  level={sourceLevel}
                  mixes={primaryMixes}
                  onFocus={() => setSelectedChannelId(channel.id)}
                  onOpenMenu={(event) => {
                    event.preventDefault();
                    setSelectedChannelId(channel.id);
                    setMenu({
                      x: Math.max(12, Math.min(event.clientX, window.innerWidth - 250)),
                      y: Math.max(12, Math.min(event.clientY, window.innerHeight - 360)),
                      channelId: channel.id,
                    });
                  }}
                  run={run}
                />
              );
            })}
            <button
              className="add-channel"
              disabled={softwareChannelCount >= MAX_SOFTWARE_CHANNELS || busy}
              onClick={() => {
                const name = window.prompt("Channel name", "Podcast");
                if (name) void run("create_channel", { name, kind: "application" satisfies ChannelKind }, "Channel created");
              }}
              type="button"
            >
              <CirclePlus size={18} />
              Add
            </button>
          </div>
        </div>

        <div className="master-panel">
          <div className="master-mix-title">
            <div>
              <strong>{selectedMix ? `${selectedMix.name} Mix` : "Mix"}</strong>
              <span>{selectedMix?.virtual_source_name ?? "No virtual source"}</span>
            </div>
            <Radio size={18} />
          </div>

          <div className="master-bus-grid">
            {primaryMixes.map((mix) => (
              <MasterBusControl
                key={mix.id}
                mix={mix}
                run={run}
                vuLevel={mix.muted ? 0 : levelFor(mix.id) * mix.volume}
              />
            ))}
          </div>

          {selectedMix && (
              <>
                <label className="field-label" htmlFor="active-mix-monitor-output">
                  Monitor output
                </label>
                <select
                id="active-mix-monitor-output"
                onChange={(event) =>
                  void run("set_mix_monitor_output", {
                    mixId: selectedMix.id,
                    output: event.currentTarget.value || null,
                  }).catch(() => undefined)
                  }
                  value={selectedMix.monitor_output ?? ""}
                >
                  <option value="">
                    {state.config.settings.monitor_follows_default_output
                      ? "Follow system default output"
                      : "No monitor route"}
                  </option>
                {outputs.map((output) => (
                  <option key={output.id} value={output.id}>
                    {output.description}
                  </option>
                ))}
              </select>
            </>
          )}

          <div className="master-stats">
            <Stat icon={MonitorSpeaker} label="Outputs" value={outputs.length.toString()} />
            <Stat icon={Cable} label="Apps" value={state.graph.app_streams.length.toString()} />
            <Stat icon={Gauge} label="Meters" value={metersUnavailable ? "Unavailable" : state.graph.meters.length.toString()} />
          </div>
        </div>
      </div>
      {menu && menuChannel && (
        <ChannelContextMenu
          canMoveDown={menuChannelIndex >= 0 && menuChannelIndex < state.config.channels.length - 1}
          canMoveUp={menuChannelIndex > 0}
          channel={menuChannel}
          mixes={state.config.mixes}
          onClose={() => setMenu(null)}
          run={run}
          x={menu.x}
          y={menu.y}
        />
      )}
    </section>
  );
}

function ChannelStrip({
  channel,
  mixes,
  level,
  onFocus,
  onOpenMenu,
  run,
}: {
  channel: Channel;
  mixes: Mix[];
  level: number;
  onFocus: () => void;
  onOpenMenu: (event: ReactMouseEvent<HTMLElement>) => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const Icon = channelIcon(channel.kind);
  return (
    <article className="channel-strip" onClick={onFocus} onContextMenu={onOpenMenu}>
      <div className="strip-title">
        <Icon size={17} />
        <span>{channel.name}</span>
      </div>
      <div className="strip-buses">
        {mixes.map((mix) => (
          <ChannelBusControl
            bus={channel.mix_buses[mix.id] ?? { volume: 1, muted: false }}
            channel={channel}
            key={mix.id}
            mix={mix}
            run={run}
            vuLevel={(channel.mix_buses[mix.id]?.muted ?? false) ? 0 : level * (channel.mix_buses[mix.id]?.volume ?? 1)}
          />
        ))}
      </div>
    </article>
  );
}

function ChannelContextMenu({
  channel,
  canMoveDown,
  canMoveUp,
  mixes,
  onClose,
  run,
  x,
  y,
}: {
  channel: Channel;
  canMoveDown: boolean;
  canMoveUp: boolean;
  mixes: Mix[];
  onClose: () => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  x: number;
  y: number;
}) {
  return (
    <div className="context-menu" style={{ left: x, top: y }} onClick={(event) => event.stopPropagation()}>
      <div className="context-menu-title">{channel.name}</div>
      <button
        disabled={!canMoveUp}
        type="button"
        onClick={() =>
          void run("move_channel", { channelId: channel.id, direction: -1 }, "Channel moved")
            .finally(onClose)
        }
      >
        Move Up
      </button>
      <button
        disabled={!canMoveDown}
        type="button"
        onClick={() =>
          void run("move_channel", { channelId: channel.id, direction: 1 }, "Channel moved")
            .finally(onClose)
        }
      >
        Move Down
      </button>
      <div className="context-menu-separator" />
      <button
        type="button"
        onClick={() => {
          const name = window.prompt("Channel name", channel.name);
          if (name && name !== channel.name) {
            void run("rename_channel", { channelId: channel.id, name }, "Channel renamed");
          }
          onClose();
        }}
      >
        Rename Channel
      </button>
      <button
        type="button"
        onClick={() =>
          void run(
            "set_channel_linked",
            { channelId: channel.id, linked: !channel.linked },
            channel.linked ? "Sliders unlinked" : "Sliders linked",
          ).finally(onClose)
        }
      >
        {channel.linked ? "Unlink Mix Sliders" : "Link Mix Sliders"}
      </button>
      <div className="context-menu-separator" />
      {mixes.map((mix) => {
        const bus = channel.mix_buses[mix.id];
        return (
          <button
            key={mix.id}
            type="button"
            onClick={() =>
              void run("set_channel_mute", {
                channelId: channel.id,
                mixId: mix.id,
                muted: !(bus?.muted ?? false),
              }).finally(onClose)
            }
          >
            {bus?.muted ? "Unmute" : "Mute"} {mix.name}
          </button>
        );
      })}
      <button
        className="danger"
        type="button"
        onClick={() => {
          if (window.confirm(`Delete ${channel.name}?`)) {
            void run("delete_channel", { channelId: channel.id }, "Channel deleted");
          }
          onClose();
        }}
      >
        Delete Channel
      </button>
    </div>
  );
}

function ChannelBusControl({
  channel,
  mix,
  bus,
  run,
  vuLevel,
}: {
  channel: Channel;
  mix: Mix;
  bus: MixBus;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  vuLevel: number;
}) {
  const [draft, setDraft] = useState(volumeToPercent(bus.volume));
  const lastCommitted = useRef(draft);

  useEffect(() => {
    const next = volumeToPercent(bus.volume);
    setDraft(next);
    lastCommitted.current = next;
  }, [bus.volume]);

  const commit = useCallback((nextValue = draft) => {
    const next = sliderPercent(nextValue);
    setDraft(next);
    if (lastCommitted.current === next) return;
    lastCommitted.current = next;
    void run("set_channel_volume", {
      channelId: channel.id,
      mixId: mix.id,
      volume: next / 100,
    }).catch(() => undefined);
  }, [channel.id, draft, mix.id, run]);

  return (
    <div className="bus-control">
      <div className="bus-label">{compactMixLabel(mix)}</div>
      <VuSlider
        ariaLabel={`${channel.name} ${mix.name} volume`}
        muted={bus.muted}
        onCommit={commit}
        onDraft={setDraft}
        value={draft}
        vuLevel={vuLevel}
      />
      <button
        className={bus.muted ? "mute-button active" : "mute-button"}
        onClick={(event) => {
          event.stopPropagation();
          void run("set_channel_mute", {
            channelId: channel.id,
            mixId: mix.id,
            muted: !bus.muted,
          });
        }}
        title={`Mute ${mix.name}`}
        type="button"
      >
        {bus.muted ? <VolumeX size={15} /> : <Volume2 size={15} />}
      </button>
      <div className="strip-value">{draft}</div>
    </div>
  );
}

function MasterBusControl({
  mix,
  run,
  vuLevel,
}: {
  mix: Mix;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  vuLevel: number;
}) {
  const [draft, setDraft] = useState(volumeToPercent(mix.volume));
  const lastCommitted = useRef(draft);

  useEffect(() => {
    const next = volumeToPercent(mix.volume);
    setDraft(next);
    lastCommitted.current = next;
  }, [mix.volume]);

  const commit = useCallback((nextValue = draft) => {
    const next = sliderPercent(nextValue);
    setDraft(next);
    if (lastCommitted.current === next) return;
    lastCommitted.current = next;
    void run("set_mix_volume", {
      mixId: mix.id,
      volume: next / 100,
    }).catch(() => undefined);
  }, [draft, mix.id, run]);

  return (
    <div className="master-bus-control">
      <div className="master-bus-title">{compactMixLabel(mix)}</div>
      <VuSlider
        ariaLabel={`${mix.name} master volume`}
        master
        muted={mix.muted}
        onCommit={commit}
        onDraft={setDraft}
        value={draft}
        vuLevel={vuLevel}
      />
      <button
        className={mix.muted ? "mute-button active" : "mute-button"}
        onClick={() =>
          void run(
            "set_mix_mute",
            { mixId: mix.id, muted: !mix.muted },
            mix.muted ? `${mix.name} unmuted` : `${mix.name} muted`,
          ).catch(() => undefined)
        }
        title={`Mute ${mix.name}`}
        type="button"
      >
        {mix.muted ? <VolumeX size={15} /> : <Volume2 size={15} />}
      </button>
      <div className="strip-value">{draft}</div>
    </div>
  );
}

function VuSlider({
  ariaLabel,
  master = false,
  muted = false,
  onCommit,
  onDraft,
  value,
  vuLevel,
}: {
  ariaLabel: string;
  master?: boolean;
  muted?: boolean;
  onCommit: (value: number) => void;
  onDraft: (value: number) => void;
  value: number;
  vuLevel: number;
}) {
  const draggingPointerId = useRef<number | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const className = [
    "vu-slider",
    master ? "master" : "",
    muted ? "muted" : "",
    isDragging ? "dragging" : "",
  ]
    .filter(Boolean)
    .join(" ");

  const valueFromPointer = (event: ReactPointerEvent<HTMLDivElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.height <= 0) return value;
    const ratio = 1 - (event.clientY - rect.top) / rect.height;
    return sliderPercent(ratio * 100);
  };

  const updateFromPointer = (event: ReactPointerEvent<HTMLDivElement>) => {
    const next = valueFromPointer(event);
    onDraft(next);
    return next;
  };

  const finishDrag = (event?: ReactPointerEvent<HTMLDivElement>) => {
    if (event && event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    draggingPointerId.current = null;
    setIsDragging(false);
  };

  const adjustFromKey = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const step = event.shiftKey ? 10 : 1;
    const next = (() => {
      switch (event.key) {
        case "ArrowUp":
        case "ArrowRight":
          return value + step;
        case "ArrowDown":
        case "ArrowLeft":
          return value - step;
        case "PageUp":
          return value + 10;
        case "PageDown":
          return value - 10;
        case "Home":
          return 0;
        case "End":
          return 100;
        default:
          return null;
      }
    })();
    if (next === null) return;
    event.preventDefault();
    const clamped = sliderPercent(next);
    onDraft(clamped);
    onCommit(clamped);
  };

  return (
    <div
      aria-label={ariaLabel}
      aria-valuemax={100}
      aria-valuemin={0}
      aria-valuenow={value}
      className={className}
      onDoubleClick={() => {
        onDraft(100);
        onCommit(100);
      }}
      onKeyDown={adjustFromKey}
      onPointerCancel={(event) => {
        if (draggingPointerId.current === event.pointerId) {
          finishDrag(event);
        }
      }}
      onPointerDown={(event) => {
        event.preventDefault();
        event.currentTarget.focus({ preventScroll: true });
        event.currentTarget.setPointerCapture(event.pointerId);
        draggingPointerId.current = event.pointerId;
        setIsDragging(true);
        updateFromPointer(event);
      }}
      onPointerMove={(event) => {
        if (draggingPointerId.current !== event.pointerId) return;
        updateFromPointer(event);
      }}
      onPointerUp={(event) => {
        if (draggingPointerId.current !== event.pointerId) return;
        const next = updateFromPointer(event);
        onCommit(next);
        finishDrag(event);
      }}
      onLostPointerCapture={() => {
        draggingPointerId.current = null;
        setIsDragging(false);
      }}
      role="slider"
      tabIndex={0}
    >
      <div className="vu-track">
        <div className="vu-fill" style={{ height: trackSize(vuLevel) }} />
        <div className="vu-cap" style={{ bottom: trackPosition(vuLevel) }} />
      </div>
      <div className="vu-thumb" style={{ bottom: thumbPosition(value) }} />
    </div>
  );
}

function RoutingView({
  state,
  run,
}: {
  state: AppStateSnapshot;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const [matcherKind, setMatcherKind] = useState<MatcherKind>("app_id");
  const [matcherValue, setMatcherValue] = useState("");
  const [targetChannelId, setTargetChannelId] = useState(state.config.channels[0]?.id ?? "");

  useEffect(() => {
    if (!state.config.channels.some((channel) => channel.id === targetChannelId)) {
      setTargetChannelId(state.config.channels[0]?.id ?? "");
    }
  }, [state.config.channels, targetChannelId]);

  const addRule = async () => {
    const value = matcherValue.trim();
    if (!value || !targetChannelId) return;
    await run(
      "assign_app_to_channel",
      { channelId: targetChannelId, matcher: matcherFromKind(matcherKind, value) },
      "Routing rule saved",
    );
    setMatcherValue("");
  };
  const offlineEntries = offlineRoutingEntries(state);

  return (
    <section className="two-column">
      <div className="panel">
        <div className="panel-header">
          <h2>Active Apps</h2>
          <Cable size={18} />
        </div>
        <div className="route-list">
          {state.graph.app_streams.map((stream) => (
            <StreamRouteRow key={stream.id} state={state} stream={stream} run={run} />
          ))}
          {state.graph.app_streams.length === 0 && <EmptyState label="No active app streams" />}
        </div>
      </div>
      <div className="panel">
        <div className="panel-header">
          <h2>Offline Rules</h2>
          <GitBranch size={18} />
        </div>
        <div className="rule-editor">
          <select
            aria-label="Rule matcher type"
            onChange={(event) => setMatcherKind(event.currentTarget.value as MatcherKind)}
            value={matcherKind}
          >
            {matcherKinds.map((kind) => (
              <option key={kind} value={kind}>
                {matcherKindLabel(kind)}
              </option>
            ))}
          </select>
          <input
            aria-label="Rule matcher value"
            onChange={(event) => setMatcherValue(event.currentTarget.value)}
            onKeyUp={(event) => {
              if (event.key === "Enter") void addRule();
            }}
            placeholder="com.discordapp.Discord"
            type="text"
            value={matcherValue}
          />
          <select
            aria-label="Rule channel"
            onChange={(event) => setTargetChannelId(event.currentTarget.value)}
            value={targetChannelId}
          >
            {state.config.channels.map((channel) => (
              <option key={channel.id} value={channel.id}>
                {channel.name}
              </option>
            ))}
          </select>
          <button
            className="secondary-button"
            disabled={!matcherValue.trim() || !targetChannelId}
            onClick={() => void addRule()}
            type="button"
          >
            <CirclePlus size={16} />
            Rule
          </button>
        </div>
        <div className="rules-grid">
          {offlineEntries.map((entry, index) => {
            const channel = state.config.channels.find((item) => item.id === entry.channel_id);
            return (
              <div className="rule-row" key={`${routeKey(entry.matcher)}-${index}`}>
                <div>
                  <strong>{entry.displayName}</strong>
                  <span>{entry.meta}</span>
                </div>
                <select
                  aria-label={`Route ${entry.displayName} to channel`}
                  onChange={(event) => {
                    const channelId = event.currentTarget.value;
                    if (channelId) {
                      void run(
                        "assign_app_to_channel",
                        { channelId, matcher: entry.matcher },
                        "Routing rule updated",
                      );
                    } else {
                      void run("remove_app_route", { matcher: entry.matcher }, "Routing rule removed");
                    }
                  }}
                  value={channel?.id ?? ""}
                >
                  <option value="">Unassigned</option>
                  {state.config.channels.map((item) => (
                    <option key={item.id} value={item.id}>
                      {item.name}
                    </option>
                  ))}
                </select>
                <OfflineVolumeControl
                  label={entry.displayName}
                  matcher={entry.matcher}
                  preset={entry.volumePreset}
                  run={run}
                />
                <AppIdentityActions
                  label={entry.displayName}
                  matcher={entry.matcher}
                  run={run}
                  state={state}
                />
                <button
                  className="mini-icon-button danger"
                  onClick={() =>
                    void run(
                      "forget_app",
                      { matcher: entry.matcher },
                      "App forgotten",
                    ).catch(() => undefined)
                  }
                  title="Forget remembered app and clear saved route"
                  type="button"
                >
                  <Trash2 size={14} />
                </button>
              </div>
            );
          })}
          {offlineEntries.length === 0 && <EmptyState label="No saved or remembered apps" />}
        </div>
      </div>
    </section>
  );
}

function OfflineVolumeControl({
  label,
  matcher,
  preset,
  run,
}: {
  label: string;
  matcher: AppMatcher;
  preset?: AppVolumePreset;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const [draft, setDraft] = useState(volumeToPercent(preset?.volume ?? 1));
  const lastCommitted = useRef(draft);

  useEffect(() => {
    const next = volumeToPercent(preset?.volume ?? 1);
    setDraft(next);
    lastCommitted.current = next;
  }, [preset?.volume]);

  const commit = useCallback((nextValue = draft) => {
    const next = sliderPercent(nextValue);
    setDraft(next);
    if (lastCommitted.current === next) return;
    lastCommitted.current = next;
    void run(
      "set_app_volume_preset",
      { matcher, volume: next / 100 },
      "App volume preset saved",
    ).catch(() => undefined);
  }, [draft, matcher, run]);

  return (
    <label className="route-volume-control" title="Offline app volume preset">
      <Volume2 size={14} />
      <input
        aria-label={`${label} saved volume`}
        max={100}
        min={0}
        onBlur={(event) => commit(Number(event.currentTarget.value))}
        onChange={(event) => setDraft(sliderPercent(Number(event.currentTarget.value)))}
        onKeyUp={(event) => {
          if (shouldCommitSliderKey(event)) commit(Number(event.currentTarget.value));
        }}
        onPointerUp={(event) => commit(Number(event.currentTarget.value))}
        type="range"
        value={draft}
      />
      <strong>{draft}</strong>
    </label>
  );
}

function StreamRouteRow({
  state,
  stream,
  run,
}: {
  state: AppStateSnapshot;
  stream: AppStream;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const [draftVolume, setDraftVolume] = useState(volumeToPercent(stream.volume));
  const lastCommitted = useRef(draftVolume);

  useEffect(() => {
    const next = volumeToPercent(stream.volume);
    setDraftVolume(next);
    lastCommitted.current = next;
  }, [stream.volume]);

  const commitVolume = useCallback((nextValue = draftVolume) => {
    const next = sliderPercent(nextValue);
    setDraftVolume(next);
    if (lastCommitted.current === next) return;
    lastCommitted.current = next;
    void (async () => {
      const volume = next / 100;
      await run("set_app_stream_volume", {
        streamId: stream.id,
        volume,
      });
      await run("set_app_volume_preset", {
        matcher: matcherForStream(stream),
        volume,
      });
    })().catch(() => undefined);
  }, [draftVolume, run, stream]);

  const routeStream = async (channelId: string) => {
    if (!channelId) {
      const matcher = matcherForStream(stream);
      await run("remove_app_route", { matcher });
      await run(
        "move_app_stream_to_default",
        { streamId: stream.id },
        "Route cleared",
      );
      return;
    }
    await run("move_app_stream", {
      streamId: stream.id,
      channelId,
    });
    await run(
      "assign_app_to_channel",
      {
        channelId,
        matcher: matcherForStream(stream),
      },
      "Route saved",
    );
  };

  return (
    <div className="route-row">
      <div>
        <strong>{stream.display_name}</strong>
        <span>{stream.media_name ?? stream.process_name ?? stream.id}</span>
      </div>
      <select
        value={stream.routed_channel_id ?? ""}
        onChange={(event) => void routeStream(event.currentTarget.value)}
      >
        <option value="">Unassigned</option>
        {state.config.channels.map((channel) => (
          <option key={channel.id} value={channel.id}>
            {channel.name}
          </option>
        ))}
      </select>
      <label className="route-volume-control" title="App stream volume">
        <Volume2 size={14} />
        <input
          aria-label={`${stream.display_name} volume`}
          max={100}
          min={0}
          onBlur={(event) => commitVolume(Number(event.currentTarget.value))}
          onChange={(event) => setDraftVolume(sliderPercent(Number(event.currentTarget.value)))}
          onKeyUp={(event) => {
            if (shouldCommitSliderKey(event)) commitVolume(Number(event.currentTarget.value));
          }}
          onPointerUp={(event) => commitVolume(Number(event.currentTarget.value))}
          type="range"
          value={draftVolume}
        />
        <strong>{draftVolume}</strong>
      </label>
      <AppIdentityActions
        label={stream.display_name}
        matcher={matcherForStream(stream)}
        run={run}
        state={state}
      />
      <button
        className={stream.muted ? "icon-button danger active" : "icon-button"}
        onClick={() =>
          void run("set_app_stream_mute", {
            streamId: stream.id,
            muted: !stream.muted,
          }).catch(() => undefined)
        }
        title="Mute app"
        type="button"
      >
        {stream.muted ? <VolumeX size={17} /> : <Volume2 size={17} />}
      </button>
    </div>
  );
}

function AppIdentityActions({
  label,
  matcher,
  run,
  state,
}: {
  label: string;
  matcher: AppMatcher;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  state: AppStateSnapshot;
}) {
  const mergeTargets = state.config.app_history.filter(
    (app) => !app.forgotten && routeKey(app.matcher) !== routeKey(matcher),
  );

  return (
    <div className="identity-actions">
      <button
        className="mini-icon-button"
        onClick={() => {
          const next = window.prompt("Pinned app label", label);
          if (next?.trim()) {
            void run("pin_app_identity", { matcher, label: next.trim() }, "App identity pinned");
          }
        }}
        title="Pin or rename app identity"
        type="button"
      >
        <Pencil size={14} />
      </button>
      <button
        className="mini-icon-button"
        disabled={mergeTargets.length === 0}
        onClick={() => {
          const targetName = window.prompt(
            "Merge this app into which remembered app?",
            mergeTargets[0]?.display_name ?? "",
          );
          if (!targetName?.trim()) return;
          const target = mergeTargets.find(
            (app) =>
              app.display_name.toLowerCase() === targetName.trim().toLowerCase() ||
              matcherLabel(app.matcher).toLowerCase() === targetName.trim().toLowerCase(),
          );
          if (!target) {
            window.alert("No remembered app matched that name.");
            return;
          }
          void run(
            "merge_app_identity",
            { source: matcher, target: target.matcher },
            "App identities merged",
          );
        }}
        title="Merge into remembered app"
        type="button"
      >
        <GitBranch size={14} />
      </button>
      <button
        className="mini-icon-button"
        onClick={() => void run("reset_app_identity", { matcher }, "App identity reset").catch(() => undefined)}
        title="Reset app identity"
        type="button"
      >
        <RefreshCw size={14} />
      </button>
    </div>
  );
}

function EffectsView({
  state,
  selectedChannel,
  selectedChannelId,
  setSelectedChannelId,
  run,
}: {
  state: AppStateSnapshot;
  selectedChannel?: Channel;
  selectedChannelId: string;
  setSelectedChannelId: (channelId: string) => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const selectedEffects = selectedChannel?.effects ?? [];
  const microphoneInputs = state.graph.inputs.filter(isMicrophoneSource);
  const [effectClipboard, setEffectClipboard] = useState<EffectInstance | null>(null);
  const updateEffects = (effects: EffectInstance[], message?: string) => {
    if (!selectedChannel) return;
    void run("set_effect_chain", { channelId: selectedChannel.id, effects }, message);
  };
  const addEffect = (effect: EffectDefinition) => {
    if (!selectedChannel) return;
    const instance: EffectInstance = {
      instance_id: crypto.randomUUID(),
      effect_id: effect.id,
      name: null,
      bypassed: false,
      params: Object.fromEntries(effect.params.map((param) => [param.id, param.default])),
    };
    updateEffects([...selectedEffects, instance], "Effect added");
  };
  const applyPreset = (instanceId: string, values: Record<string, number>) => {
    if (!selectedChannel) return;
    const effects = selectedEffects.map((effect) =>
      effect.instance_id === instanceId
        ? { ...effect, params: { ...effect.params, ...values } }
        : effect,
    );
    void run(
      "set_effect_chain",
      { channelId: selectedChannel.id, effects },
      "Preset applied",
    );
  };
  const moveEffect = (index: number, direction: -1 | 1) => {
    const target = index + direction;
    if (!selectedChannel || target < 0 || target >= selectedEffects.length) return;
    const effects = [...selectedEffects];
    [effects[index], effects[target]] = [effects[target], effects[index]];
    updateEffects(effects, "Effect reordered");
  };
  const renameEffect = (instanceId: string) => {
    if (!selectedChannel) return;
    const effect = selectedEffects.find((item) => item.instance_id === instanceId);
    if (!effect) return;
    const definition = state.catalog.effects.find((item) => item.id === effect.effect_id);
    const name = window.prompt("Effect name", effect.name ?? definition?.name ?? effect.effect_id);
    if (!name) return;
    updateEffects(
      selectedEffects.map((item) =>
        item.instance_id === instanceId ? { ...item, name: name.trim() || null } : item,
      ),
      "Effect renamed",
    );
  };
  const copyEffect = (effect: EffectInstance) => {
    setEffectClipboard(structuredClone(effect));
  };
  const pasteEffect = () => {
    if (!selectedChannel || !effectClipboard) return;
    updateEffects(
      [
        ...selectedEffects,
        {
          ...structuredClone(effectClipboard),
          instance_id: crypto.randomUUID(),
          name: effectClipboard.name ? `${effectClipboard.name} Copy` : null,
        },
      ],
      "Effect pasted",
    );
  };
  const deleteEffect = (instanceId: string) => {
    if (!selectedChannel) return;
    updateEffects(
      selectedEffects.filter((effect) => effect.instance_id !== instanceId),
      "Effect removed",
    );
  };
  return (
    <section className="two-column effects-view">
      <div className="panel">
        <div className="panel-header">
          <h2>Channel</h2>
          <SlidersHorizontal size={18} />
        </div>
        <div className="channel-picker">
          {state.config.channels.map((channel) => (
            <button
              className={channel.id === selectedChannelId ? "picker-row active" : "picker-row"}
              key={channel.id}
              onClick={() => setSelectedChannelId(channel.id)}
              type="button"
            >
              <span>{channel.name}</span>
              <small>
                {isHardwareChannel(channel)
                  ? channelInputLabel(channel, microphoneInputs)
                  : `${channel.effects.length} FX`}
              </small>
            </button>
          ))}
        </div>
      </div>
      <div className="panel">
        <div className="panel-header">
          <h2>{selectedChannel?.name ?? "Effects"}</h2>
          <div className="panel-actions">
            <button
              className="secondary-button"
              disabled={!selectedChannel}
              onClick={() => {
                if (!selectedChannel) return;
                addEffect(state.catalog.effects[0]);
              }}
              type="button"
            >
              <CirclePlus size={16} />
              Add
            </button>
            <button
              className="secondary-button"
              disabled={!selectedChannel || !effectClipboard}
              onClick={pasteEffect}
              type="button"
              title="Paste copied effect"
            >
              <Clipboard size={16} />
              Paste
            </button>
          </div>
        </div>
        {selectedChannel && isHardwareChannel(selectedChannel) && (
          <div className="hardware-source-card">
            <label className="field-label" htmlFor="effects-microphone-source">
              Microphone
            </label>
            <select
              id="effects-microphone-source"
              onChange={(event) =>
                void run(
                  "set_channel_input",
                  {
                    channelId: selectedChannel.id,
                    sourceDevice: event.currentTarget.value || null,
                    source_device: event.currentTarget.value || null,
                  },
                  "Microphone updated",
                ).catch(() => undefined)
              }
              value={selectedChannel.source_device ?? ""}
            >
              <option value="">Default mic</option>
              {selectedChannel.source_device === "@DEFAULT_SOURCE@" && (
                <option value="@DEFAULT_SOURCE@">Default mic</option>
              )}
              {microphoneInputs.map((input) => (
                <option key={input.id} value={input.id}>
                  {input.description}
                </option>
              ))}
            </select>
            <label className="field-label" htmlFor="effects-input-mode">
              Input mode
            </label>
            <select
              id="effects-input-mode"
              onChange={(event) =>
                void run(
                  "set_channel_input_mode",
                  {
                    channelId: selectedChannel.id,
                    inputMode: event.currentTarget.value,
                    input_mode: event.currentTarget.value,
                  },
                  "Input mode updated",
                ).catch(() => undefined)
              }
              value={selectedChannel.input_mode}
            >
              {inputModeOptions.map((mode) => (
                <option key={mode.id} value={mode.id}>
                  {mode.label}
                </option>
              ))}
            </select>
          </div>
        )}
        <div className="effect-chain">
          {selectedEffects.map((effect, index) => {
            const definition = state.catalog.effects.find((item) => item.id === effect.effect_id);
            return (
              <EffectBlock
                availability={state.graph.effect_availability.find((item) => item.effect_id === effect.effect_id)}
                channelId={selectedChannel?.id ?? ""}
                definition={definition}
                effect={effect}
                index={index}
                key={effect.instance_id}
                onCopy={copyEffect}
                onDelete={deleteEffect}
                onMove={moveEffect}
                onRename={renameEffect}
                onApplyPreset={applyPreset}
                run={run}
                total={selectedEffects.length}
              />
            );
          })}
          {selectedEffects.length === 0 && <EmptyState label="No effects on this channel" />}
        </div>
      </div>
      <div className="panel catalog-panel">
        <div className="panel-header">
          <h2>Catalog</h2>
          <Sparkles size={18} />
        </div>
        <div className="catalog-grid">
          {state.catalog.effects.map((effect) => {
            const availability = state.graph.effect_availability.find((item) => item.effect_id === effect.id);
            return (
              <button
                className="catalog-item"
                disabled={!selectedChannel}
                key={effect.id}
                onClick={() => {
                  addEffect(effect);
                }}
                type="button"
              >
                <span>{effect.name}</span>
                {availability?.available ? <Check size={15} /> : <CircleAlert size={15} />}
              </button>
            );
          })}
        </div>
      </div>
    </section>
  );
}

function EffectBlock({
  availability,
  channelId,
  effect,
  definition,
  index,
  total,
  onApplyPreset,
  onCopy,
  onDelete,
  onMove,
  onRename,
  run,
}: {
  availability?: EffectAvailability;
  channelId: string;
  effect: EffectInstance;
  definition?: EffectDefinition;
  index: number;
  total: number;
  onApplyPreset: (instanceId: string, values: Record<string, number>) => void;
  onCopy: (effect: EffectInstance) => void;
  onDelete: (instanceId: string) => void;
  onMove: (index: number, direction: -1 | 1) => void;
  onRename: (instanceId: string) => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  return (
    <article className={effect.bypassed ? "effect-block bypassed" : "effect-block"}>
      <div className="effect-title">
        <div className="effect-name">
          <GripVertical size={15} />
          <div>
            <strong>{effect.name || definition?.name || effect.effect_id}</strong>
            <span>{definition?.description ?? effect.effect_id}</span>
          </div>
        </div>
        <div className="effect-actions">
          <button
            className="mini-icon-button"
            disabled={index === 0}
            onClick={() => onMove(index, -1)}
            type="button"
            title="Move effect up"
          >
            <ArrowUp size={14} />
          </button>
          <button
            className="mini-icon-button"
            disabled={index >= total - 1}
            onClick={() => onMove(index, 1)}
            type="button"
            title="Move effect down"
          >
            <ArrowDown size={14} />
          </button>
          <button
            className="mini-icon-button"
            onClick={() => onRename(effect.instance_id)}
            type="button"
            title="Rename effect"
          >
            <Pencil size={14} />
          </button>
          <button
            className="mini-icon-button"
            onClick={() => onCopy(effect)}
            type="button"
            title="Copy effect"
          >
            <Copy size={14} />
          </button>
          <button
            className={effect.bypassed ? "mini-icon-button active" : "mini-icon-button"}
            onClick={() =>
              void run("bypass_effect", {
                channelId,
                instanceId: effect.instance_id,
                bypassed: !effect.bypassed,
              }).catch(() => undefined)
            }
            type="button"
            title="Bypass effect"
          >
            <Gauge size={14} />
          </button>
          <button
            className="mini-icon-button danger"
            onClick={() => onDelete(effect.instance_id)}
            type="button"
            title="Delete effect"
          >
            <Trash2 size={14} />
          </button>
        </div>
      </div>
      {availability && !availability.available && (
        <div className="effect-warning">
          <CircleAlert size={15} />
          <span>{availability.detail}</span>
        </div>
      )}
      {definition && definition.presets.length > 0 && (
        <div className="preset-row">
          {definition.presets.map((preset) => (
            <button
              key={preset.name}
              onClick={() => onApplyPreset(effect.instance_id, preset.values)}
              type="button"
            >
              {preset.name}
            </button>
          ))}
        </div>
      )}
      {definition?.params.map((param) => (
        <VolumeFader
          compact
          key={param.id}
          label={param.label}
          max={param.max}
          min={param.min}
          unit={param.unit}
          value={effect.params[param.id] ?? param.default}
          onChange={(value) =>
            void run("set_effect_param", {
              channelId,
              instanceId: effect.instance_id,
              paramId: param.id,
              value,
            }).catch(() => undefined)
          }
        />
      ))}
    </article>
  );
}

function ScenesView({
  run,
  state,
}: {
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  state: AppStateSnapshot;
}) {
  const [scenes, setScenes] = useState<Scene[]>([]);
  const [templates, setTemplates] = useState<SetupTemplate[]>([]);
  const importInput = useRef<HTMLInputElement | null>(null);
  const refreshScenes = useCallback(async () => {
    const next = await invoke<Scene[]>("list_scenes");
    setScenes(Array.isArray(next) ? next : []);
  }, []);
  const refreshTemplates = useCallback(async () => {
    const next = await invoke<SetupTemplate[]>("list_setup_templates");
    setTemplates(Array.isArray(next) ? next : []);
  }, []);

  useEffect(() => {
    refreshScenes().catch(() => setScenes([]));
    refreshTemplates().catch(() => setTemplates([]));
  }, [refreshScenes, refreshTemplates]);

  const importSceneFile = async (file: File) => {
    try {
      const text = await file.text();
      const parsed = JSON.parse(text) as unknown;
      if (isBackupExport(parsed)) {
        if (!window.confirm("Import this WaveLinux backup and replace the current setup and saved scenes?")) return;
        await run<ConfigBackup>("import_backup", { backup: parsed }, "Backup imported");
        await refreshScenes();
        return;
      }
      if (!isSceneExport(parsed)) {
        window.alert("That file is not a WaveLinux scene or backup export.");
        return;
      }
      await run<Scene>("import_scene", { scene: parsed }, "Scene imported");
      await refreshScenes();
    } catch (error) {
      window.alert(`Import failed: ${String(error)}`);
    }
  };

  const exportBackup = async () => {
    const backup = await run<ConfigBackup>("export_backup", undefined, "Backup exported");
    const blob = new Blob([JSON.stringify(backup, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = backupFileName(backup);
    link.click();
    URL.revokeObjectURL(url);
  };

  const exportScene = (scene: Scene) => {
    const blob = new Blob([JSON.stringify(scene, null, 2)], {
      type: "application/json",
    });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = sceneFileName(scene);
    link.click();
    URL.revokeObjectURL(url);
  };

  return (
    <section className="panel single-panel">
      <div className="panel-header">
        <h2>Scenes</h2>
        <div className="panel-actions">
          <input
            ref={importInput}
            accept="application/json,.json"
            hidden
            onChange={(event) => {
              const file = event.currentTarget.files?.[0];
              event.currentTarget.value = "";
              if (file) void importSceneFile(file);
            }}
            type="file"
          />
          <button
            className="secondary-button"
            onClick={() => importInput.current?.click()}
            type="button"
          >
            <ArrowUp size={16} />
            Import
          </button>
          <button
            className="secondary-button"
            onClick={() => void exportBackup()}
            type="button"
            title="Export the full mixer setup and saved scene library"
          >
            <ArrowDown size={16} />
            Backup
          </button>
          <button
            className="primary-button"
            onClick={async () => {
              const name = window.prompt("Scene name", "Streaming");
              if (!name) return;
              await run("save_scene", { name }, "Scene saved");
              await refreshScenes();
            }}
            type="button"
          >
            <Save size={16} />
            Save
          </button>
        </div>
      </div>
      <div className="template-section">
        <div className="subsection-header">
          <strong>Quick Starts</strong>
          <span>
            3.1-style setup snapshots for common workflows · current setup has{" "}
            {state.config.mixes.length} mixes and {state.config.channels.length} channels
          </span>
        </div>
        <div className="template-grid">
          {templates.map((template) => (
            <article className="template-card" key={template.id}>
              <div>
                <strong>{template.name}</strong>
                <span>{template.description}</span>
              </div>
              <ul>
                {template.details.slice(0, 4).map((detail) => (
                  <li key={detail}>{detail}</li>
                ))}
              </ul>
              <button
                className="secondary-button"
                onClick={async () => {
                  if (!window.confirm(`Replace the current mixer layout with "${template.name}"?`)) return;
                  await run("apply_setup_template", { templateId: template.id, template_id: template.id }, "Template applied");
                  await refreshScenes();
                }}
                type="button"
              >
                <WandSparkles size={16} />
                Apply
              </button>
            </article>
          ))}
          {templates.length === 0 && <EmptyState label="No setup templates available" />}
        </div>
      </div>
      <div className="scene-grid">
        {scenes.map((scene) => (
          <article className="scene-tile" key={scene.id}>
            <button
              className="scene-load-button"
              onClick={async () => {
                await run("load_scene", sceneIdArgs(scene.id), "Scene loaded");
                await refreshScenes();
              }}
              type="button"
            >
              <strong>{scene.name}</strong>
              <span>{scene.config.mixes.length} mixes · {scene.config.channels.length} channels</span>
            </button>
            <div className="scene-actions">
              <button
                className="mini-icon-button"
                onClick={() => exportScene(scene)}
                title="Export scene"
                type="button"
              >
                <ArrowDown size={14} />
              </button>
              <button
                className="mini-icon-button danger"
                onClick={async () => {
                  if (!window.confirm(`Delete scene "${scene.name}"?`)) return;
                  await run("delete_scene", sceneIdArgs(scene.id), "Scene deleted");
                  await refreshScenes();
                }}
                title="Delete scene"
                type="button"
              >
                <Trash2 size={14} />
              </button>
            </div>
          </article>
        ))}
        {scenes.length === 0 && <EmptyState label="No saved scenes" />}
      </div>
    </section>
  );
}

function DiagnosticsView({
  audioActionReport,
  onCleanup,
  onPrune,
  state,
  run,
}: {
  audioActionReport: AudioActionReport | null;
  onCleanup: () => void | Promise<unknown>;
  onPrune: () => void | Promise<unknown>;
  state: AppStateSnapshot;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const [report, setReport] = useState<SoundCheckReport | null>(null);
  const [graphReport, setGraphReport] = useState<GraphDebugReport | null>(null);
  const diagnostics = report?.diagnostics ?? state.diagnostics;
  return (
    <section className="two-column">
      <div className="panel">
        <div className="panel-header">
          <h2>Checks</h2>
          <div className="panel-actions">
            <button
              className="secondary-button"
              onClick={async () => setReport(await run<SoundCheckReport>("run_sound_check"))}
              type="button"
            >
              <Activity size={16} />
              Run
            </button>
            <button
              className="secondary-button"
              onClick={async () => setGraphReport(await run<GraphDebugReport>("get_graph_debug_report"))}
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
            <button
              className="secondary-button danger"
              onClick={() => void onCleanup()}
              type="button"
              title="Disruptive: unload every WaveLinux-managed audio module"
            >
              <Trash2 size={16} />
              Cleanup
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
        <EffectAvailabilitySummary state={state} />
      </div>
    </section>
  );
}

function LatencySummary({ state }: { state: AppStateSnapshot }) {
  const heavyFx = state.config.channels.flatMap((channel) =>
    channel.effects
      .filter((effect) => !effect.bypassed && ["deepfilternet", "rnnoise", "convolver"].includes(effect.effect_id))
      .map((effect) => `${channel.name}: ${effect.effect_id}`),
  );
  const activeMixRoutes = state.config.channels.length * state.config.mixes.length;
  const estimatedMicPath = heavyFx.length > 0 ? "30 ms+" : "20-30 ms";

  return (
    <div className="latency-card command-report">
      <div className="command-report-header">
        <div>
          <strong>Latency</strong>
          <span>PipeWire path estimate for monitoring and virtual mic use</span>
        </div>
        <div className={heavyFx.length ? "command-pill warning" : "command-pill"}>
          {heavyFx.length ? "Review FX" : "Low"}
        </div>
      </div>
      <div className="command-stats">
        <Stat icon={Gauge} label="Loopbacks" value="10 ms" />
        <Stat icon={Mic} label="Mic path" value={estimatedMicPath} />
        <Stat icon={GitBranch} label="Routes" value={String(activeMixRoutes)} />
      </div>
      {heavyFx.length > 0 && (
        <div className="latency-note">
          <CircleAlert size={15} />
          <span>{heavyFx.join(", ")}</span>
        </div>
      )}
    </div>
  );
}

function GraphDebugSummary({ report }: { report: GraphDebugReport }) {
  const visibleCommands = report.planned.commands.slice(0, 6);
  const visibleModules = report.managed_modules.slice(0, 6);
  const routeCount = report.sink_input_routes.length + report.source_output_routes.length;

  return (
    <div className="graph-debug command-report">
      <div className="command-report-header">
        <div>
          <strong>Graph Debug</strong>
          <span>{report.audio_graph_running ? "Managed graph is present" : "Managed graph is stopped"}</span>
        </div>
        <div className={report.stale_processes.length ? "command-pill warning" : "command-pill"}>
          {report.stale_processes.length ? `${report.stale_processes.length} stale` : "Clean"}
        </div>
      </div>
      <div className="command-stats">
        <Stat icon={WandSparkles} label="Planned" value={String(report.planned.commands.length)} />
        <Stat icon={Cable} label="Modules" value={String(report.managed_modules.length)} />
        <Stat icon={GitBranch} label="Routes" value={String(routeCount)} />
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
  const available = state.graph.effect_availability.filter((item) => item.available).length;
  const total = state.catalog.effects.length;

  return (
    <div className="fx-availability">
      <div className="command-report-header">
        <div>
          <strong>Effect Availability</strong>
          <span>{available}/{total} open DSP replacements detected</span>
        </div>
        <div className={available === total ? "command-pill" : "command-pill warning"}>
          {available === total ? "Ready" : `${total - available} missing`}
        </div>
      </div>
      <div className="fx-availability-list">
        {state.catalog.preferred_order.map((effectId) => {
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

function SettingsView({
  state,
  run,
}: {
  state: AppStateSnapshot;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const updateSettings = (settings: MixerSettings) =>
    run("set_settings", { settings }, "Settings updated");

  return (
    <section className="panel single-panel">
      <div className="panel-header">
        <h2>Settings</h2>
        <Settings size={18} />
      </div>
      <div className="settings-grid">
        <Toggle
          label="Start at login"
          onChange={(value) =>
            updateSettings({ ...state.config.settings, start_at_login: value })
          }
          value={state.config.settings.start_at_login}
        />
        <Toggle
          label="Restore audio graph on launch"
          onChange={(value) =>
            updateSettings({ ...state.config.settings, restore_audio_graph_on_launch: value })
          }
          value={state.config.settings.restore_audio_graph_on_launch}
        />
        <Toggle
          label="Monitor follows default output"
          onChange={(value) =>
            updateSettings({ ...state.config.settings, monitor_follows_default_output: value })
          }
          value={state.config.settings.monitor_follows_default_output}
        />
        <Toggle
          label="Lock default input"
          onChange={(value) =>
            updateSettings({ ...state.config.settings, lock_default_input: value })
          }
          value={state.config.settings.lock_default_input}
        />
        <Toggle
          label="Lock default output"
          onChange={(value) =>
            updateSettings({ ...state.config.settings, lock_default_output: value })
          }
          value={state.config.settings.lock_default_output}
        />
        <Toggle
          label="Auto-check updates"
          onChange={(value) =>
            updateSettings({ ...state.config.settings, auto_check_updates: value })
          }
          value={state.config.settings.auto_check_updates}
        />
      </div>
      <div className="system-grid">
        <Stat icon={Cpu} label="Engine" value={state.engine.audio_graph_running ? "Running" : "Stopped"} />
        <Stat icon={Radio} label="Rate" value={`${state.config.audio.sample_rate_hz / 1000} kHz`} />
        <Stat icon={AudioLines} label="Format" value={`${state.config.audio.bit_depth}-bit`} />
      </div>
    </section>
  );
}

function VolumeFader({
  label,
  value,
  min = 0,
  max = 1,
  unit = "%",
  compact = false,
  onChange,
}: {
  label: string;
  value: number;
  min?: number;
  max?: number;
  unit?: string;
  compact?: boolean;
  onChange: (value: number) => void | Promise<unknown>;
}) {
  const normalizedPercent = unit === "%" && min === 0 && max === 1;
  const sliderMin = normalizedPercent ? 0 : min;
  const sliderMax = normalizedPercent ? 100 : max;
  const incomingSliderValue = normalizedPercent ? value * 100 : value;
  const [draft, setDraft] = useState(incomingSliderValue);
  const lastCommitted = useRef(incomingSliderValue);
  const display = normalizedPercent ? Math.round(draft) : Math.round(draft * 10) / 10;

  useEffect(() => {
    const next = normalizedPercent ? value * 100 : value;
    setDraft(next);
    lastCommitted.current = next;
  }, [normalizedPercent, value]);

  const commit = useCallback((raw: number) => {
    const next = Number.isFinite(raw) ? Math.max(sliderMin, Math.min(sliderMax, raw)) : incomingSliderValue;
    setDraft(next);
    if (lastCommitted.current === next) return;
    lastCommitted.current = next;
    void onChange(normalizedPercent ? next / 100 : next);
  }, [incomingSliderValue, normalizedPercent, onChange, sliderMax, sliderMin]);

  return (
    <label className={compact ? "fader-row compact" : "fader-row"}>
      <span>{label}</span>
      <input
        max={sliderMax}
        min={sliderMin}
        onBlur={(event) => commit(Number(event.currentTarget.value))}
        onChange={(event) => setDraft(Number(event.currentTarget.value))}
        onKeyUp={(event) => {
          if (shouldCommitSliderKey(event)) commit(Number(event.currentTarget.value));
        }}
        onPointerUp={(event) => commit(Number(event.currentTarget.value))}
        step={unit === "%" ? 1 : 0.1}
        type="range"
        value={draft}
      />
      <strong>{display}{unit}</strong>
    </label>
  );
}

function Toggle({
  label,
  value,
  onChange,
}: {
  label: string;
  value: boolean;
  onChange: (value: boolean) => void | Promise<unknown>;
}) {
  return (
    <button className="toggle-row" onClick={() => onChange(!value)} type="button">
      <span>{label}</span>
      <span className={value ? "toggle on" : "toggle"} />
    </button>
  );
}

function Stat({ icon: Icon, label, value }: { icon: typeof Activity; label: string; value: string }) {
  return (
    <div className="stat">
      <Icon size={17} />
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function EmptyState({ label }: { label: string }) {
  return <div className="empty-state">{label}</div>;
}

function viewTitle(view: View): string {
  return {
    mixer: "Mixer",
    routing: "Routing",
    effects: "Effects",
    scenes: "Scenes",
    diagnostics: "Health",
    settings: "Settings",
  }[view];
}

function primaryBusMixes(mixes: Mix[]): Mix[] {
  const monitor = mixes.find((mix) => mix.id === "monitor");
  const stream = mixes.find((mix) => mix.id === "stream");
  const primary = [monitor, stream].filter(Boolean) as Mix[];
  return primary.length === 2 ? primary : mixes.slice(0, 2);
}

function mixTabLabel(mix: Mix): string {
  if (mix.id === "monitor") return "Monitor Mix";
  if (mix.id === "stream") return "Stream Mix";
  return mix.name;
}

function compactMixLabel(mix: Mix): string {
  if (mix.id === "monitor") return "MON";
  if (mix.id === "stream") return "STR";
  return mix.name.slice(0, 3).toUpperCase();
}

function matcherForStream(stream: AppStream): AppMatcher {
  const matcher = {
    app_id: stream.app_id ?? null,
    binary: stream.binary ?? stream.process_name ?? null,
    process_name: stream.process_name ?? null,
    window_class: stream.window_class ?? null,
    media_name: stream.media_name ?? null,
  };
  if (!matcherIsEmpty(matcher)) return matcher;

  return {
    app_id: fallbackMatcherValueForStream(stream),
    binary: null,
    process_name: null,
    window_class: null,
  };
}

function matcherIsEmpty(matcher: AppMatcher): boolean {
  return matcherKinds.every((kind) => !matcher[kind]?.trim());
}

function fallbackMatcherValueForStream(stream: AppStream): string {
  const candidates = [
    stream.media_name,
    stream.display_name && !/^Stream\s+\d+$/i.test(stream.display_name) ? stream.display_name : null,
    stream.id ? `stream-${stream.id}` : null,
  ];
  const value = candidates
    .map((candidate) => candidate?.trim())
    .find((candidate): candidate is string => Boolean(candidate));
  return `stream:${value ?? "unknown"}`;
}

function matcherFromKind(kind: MatcherKind, value: string): AppMatcher {
  const cleaned = value.trim();
  return {
    app_id: kind === "app_id" ? cleaned : null,
    binary: kind === "binary" ? cleaned : null,
    process_name: kind === "process_name" ? cleaned : null,
    window_class: kind === "window_class" ? cleaned : null,
    media_name: kind === "media_name" ? cleaned : null,
  };
}

function matcherKindLabel(kind: MatcherKind): string {
  return {
    app_id: "App ID",
    process_name: "Process",
    binary: "Binary",
    window_class: "Window Class",
    media_name: "Media Name",
  }[kind];
}

function matcherEntries(matcher: AppMatcher): Array<[MatcherKind, string]> {
  return matcherKinds
    .map((kind) => [kind, matcher[kind]?.trim() ?? ""] as [MatcherKind, string])
    .filter(([, value]) => value.length > 0);
}

function matcherLabel(matcher: AppMatcher): string {
  return matcherEntries(matcher)[0]?.[1] ?? "Unknown app";
}

function matcherTypeLabel(matcher: AppMatcher): string {
  const entries = matcherEntries(matcher);
  if (entries.length === 0) return "No matcher";
  return entries.map(([kind]) => matcherKindLabel(kind)).join(" + ");
}

function routeKey(matcher: AppMatcher): string {
  const entries = matcherEntries(matcher);
  if (entries.length === 0) return "empty";
  return entries.map(([kind, value]) => `${kind}:${value}`).join("|");
}

function volumePresetForMatcher(
  presets: AppVolumePreset[] | undefined,
  matcher: AppMatcher,
): AppVolumePreset | undefined {
  const key = routeKey(matcher);
  return presets?.find((preset) => routeKey(preset.matcher) === key);
}

function offlineRoutingEntries(state: AppStateSnapshot): OfflineRoutingEntry[] {
  const entries = new Map<string, OfflineRoutingEntry>();
  for (const app of state.config.app_history ?? []) {
    if (app.forgotten) continue;
    const key = routeKey(app.matcher);
    entries.set(key, {
      matcher: app.matcher,
      displayName: app.display_name || matcherLabel(app.matcher),
      meta: [matcherTypeLabel(app.matcher), formatLastSeen(app.last_seen_unix)].filter(Boolean).join(" · "),
      channel_id: undefined,
      volumePreset: volumePresetForMatcher(state.config.app_volume_presets, app.matcher),
    });
  }

  for (const route of state.config.app_routes) {
    const key = routeKey(route.matcher);
    const existing = entries.get(key);
    entries.set(key, {
      matcher: route.matcher,
      displayName: existing?.displayName ?? matcherLabel(route.matcher),
      meta: existing?.meta ?? matcherTypeLabel(route.matcher),
      channel_id: route.channel_id,
      volumePreset: existing?.volumePreset ?? volumePresetForMatcher(state.config.app_volume_presets, route.matcher),
    });
  }

  return [...entries.values()].sort((left, right) => {
    const leftRouted = left.channel_id ? 0 : 1;
    const rightRouted = right.channel_id ? 0 : 1;
    return leftRouted - rightRouted || left.displayName.localeCompare(right.displayName);
  });
}

function formatLastSeen(lastSeenUnix: number): string {
  if (!Number.isFinite(lastSeenUnix) || lastSeenUnix <= 0) return "";
  const elapsedSeconds = Math.max(0, Math.round(Date.now() / 1000 - lastSeenUnix));
  if (elapsedSeconds < 120) return "Seen now";
  const minutes = Math.round(elapsedSeconds / 60);
  if (minutes < 60) return `Seen ${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `Seen ${hours}h ago`;
  return `Seen ${Math.round(hours / 24)}d ago`;
}

function isSceneExport(value: unknown): value is Scene {
  if (!value || typeof value !== "object") return false;
  const scene = value as Partial<Scene>;
  return (
    typeof scene.name === "string" &&
    Boolean(scene.config) &&
    typeof scene.config === "object" &&
    Array.isArray(scene.config.mixes) &&
    Array.isArray(scene.config.channels)
  );
}

function isBackupExport(value: unknown): value is ConfigBackup {
  if (!value || typeof value !== "object") return false;
  const backup = value as Partial<ConfigBackup>;
  return (
    typeof backup.backup_version === "number" &&
    Boolean(backup.config) &&
    typeof backup.config === "object" &&
    Array.isArray(backup.config.mixes) &&
    Array.isArray(backup.config.channels) &&
    (backup.scenes === undefined || Array.isArray(backup.scenes))
  );
}

function sceneFileName(scene: Scene): string {
  return `${slugForFile(scene.name) || "wavelinux-scene"}.wavelinux-scene.json`;
}

function backupFileName(backup: ConfigBackup): string {
  const date = new Date((backup.exported_unix || Math.floor(Date.now() / 1000)) * 1000)
    .toISOString()
    .slice(0, 10);
  return `wavelinux-backup-${date}.json`;
}

function sceneIdArgs(sceneId: string): Record<string, string> {
  return { sceneId, scene_id: sceneId };
}

function slugForFile(value: string): string {
  return value
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}

function availableEffects(state: AppStateSnapshot): number {
  return state.graph.effect_availability.filter((item) => item.available).length;
}

function isHardwareChannel(channel: Pick<Channel, "kind">): boolean {
  return channel.kind === "microphone" || channel.kind === "generic";
}

function isWaveLinuxManagedDevice(
  device: Pick<AppStateSnapshot["graph"]["inputs"][number], "id" | "name" | "is_virtual">,
): boolean {
  return (
    device.is_virtual &&
    (looksLikeWaveLinuxNode(device.id) || looksLikeWaveLinuxNode(device.name))
  );
}

function looksLikeWaveLinuxNode(value: string): boolean {
  return value.toLowerCase().includes("wavelinux");
}

function isMicrophoneSource(device: AppStateSnapshot["graph"]["inputs"][number]): boolean {
  const name = device.name.toLowerCase();
  const description = device.description.toLowerCase();
  return (
    !isWaveLinuxManagedDevice(device) &&
    !name.endsWith(".monitor") &&
    !description.startsWith("monitor of ") &&
    !description.includes(" monitor")
  );
}

function channelInputLabel(
  channel: Pick<Channel, "source_device">,
  inputs: AppStateSnapshot["graph"]["inputs"],
): string {
  if (channel.source_device === "@DEFAULT_SOURCE@") return "Default mic";
  if (!channel.source_device) return "Default mic";
  return (
    inputs.find((input) => input.id === channel.source_device)?.description ??
    channel.source_device
  );
}

function channelIcon(kind: ChannelKind): typeof Headphones {
  if (kind === "microphone") return Mic;
  if (kind === "soundboard") return Music2;
  if (kind === "generic") return Cable;
  if (kind === "system") return MonitorSpeaker;
  return Headphones;
}

function levelFromMeter(meter: LevelMeter): number {
  const peak = Math.max(meter.peak_left, meter.peak_right);
  return Math.max(0, Math.min(1, Math.sqrt(Math.max(0, peak))));
}

function meterLevelMap(
  state: AppStateSnapshot,
  heldMeters?: HeldMeterMap,
): Record<string, number> {
  const levels: Record<string, number> = {};
  for (const meter of state.graph.meters) {
    levels[meter.node_id] = levelFromMeter(meter);
  }
  if (!state.engine.audio_graph_running || !heldMeters) return levels;

  const now = Date.now();
  for (const [nodeId, held] of Object.entries(heldMeters)) {
    if (nodeId in levels) continue;
    const ageSeconds = Math.max(0, (now - held.seenAt) / 1000);
    if (ageSeconds < 2.5) {
      levels[nodeId] = Math.max(0, Math.min(1, held.level * Math.exp(-ageSeconds * 1.6)));
    }
  }
  return levels;
}

function audioActionToast(title: string, commands: CommandExecution[], plannedCount?: number): string {
  const failures = commands.filter((command) => command.error).length;
  const skipped = commands.filter((command) => command.skipped).length;
  const ran = Math.max(0, commands.length - skipped);
  if (failures > 0) return `${title}: ${failures} command${failures === 1 ? "" : "s"} failed`;
  if (ran === 0 && skipped > 0) return `${title}: no live graph changes needed`;
  if (plannedCount !== undefined && plannedCount === 0) return `${title}: graph already matched config`;
  return `${title}: ${ran} command${ran === 1 ? "" : "s"} applied`;
}

function commandLine(program: string, args: string[]): string {
  if (!program) return "No command";
  return [program, ...args].map((part) => (part.includes(" ") ? `"${part}"` : part)).join(" ");
}

function sliderPercent(value: number): number {
  return Math.max(0, Math.min(100, Math.round(value)));
}

function volumeToPercent(volume: number): number {
  return sliderPercent(volume * 100);
}

function shouldCommitSliderKey(event: ReactKeyboardEvent<HTMLInputElement>): boolean {
  return event.key === "Enter";
}

function thumbPosition(percent: number): string {
  const clamped = Math.max(0, Math.min(100, percent));
  return `calc(13px + (100% - 26px) * ${clamped / 100})`;
}

function trackPosition(level: number): string {
  const clamped = Math.max(0, Math.min(1, level));
  return `calc((100% - 2px) * ${clamped})`;
}

function trackSize(level: number): string {
  const clamped = Math.max(0, Math.min(1, level));
  return `${clamped * 100}%`;
}
