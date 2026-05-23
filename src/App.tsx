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
  Download,
  ExternalLink,
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
import { createPortal } from "react-dom";
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
  ChannelKind,
  CommandExecution,
  ConfigBackup,
  EffectDefinition,
  EffectAvailability,
  EffectInstance,
  FallbackHardwareProfile,
  GraphDebugReport,
  HardwareProfileSummary,
  HardwareProfileUiState,
  DeviceInfo,
  LevelMeter,
  Mix,
  MixBus,
  MixerSettings,
  RepairReport,
  Scene,
  SetupTemplate,
  SoundCheckReport,
  UpdateInfo,
  UpdateInstallResult,
} from "./types";

type View = "mixer" | "routing" | "effects" | "scenes" | "settings";

const views: Array<{ id: View; label: string; icon: typeof SlidersHorizontal }> = [
  { id: "mixer", label: "Mixer", icon: SlidersHorizontal },
  { id: "routing", label: "Routing", icon: GitBranch },
  { id: "effects", label: "Effects", icon: Sparkles },
  { id: "scenes", label: "Scenes", icon: Save },
  { id: "settings", label: "Settings", icon: Settings },
];

const singleInstanceEffectIds = new Set([
  "deepfilternet",
  "rnnoise",
  "highpass",
  "eq",
  "compressor",
  "gate",
  "limiter",
]);

function initialView(): View {
  if (typeof window === "undefined") return "mixer";
  const params = new URLSearchParams(window.location.search);
  const requested = params.get("view") ?? window.location.hash.replace(/^#\/?/, "");
  return views.some((view) => view.id === requested) ? (requested as View) : "mixer";
}

const MAX_SOFTWARE_CHANNELS = 8;
const AUTO_MONITOR_OUTPUT_VALUE = "__auto_monitor_output__";
const SELECT_VISIBLE_OPTION_LIMIT = 80;
const UI_METER_ATTACK_SECONDS = 0.018;
const UI_METER_RELEASE_SECONDS = 0.34;
const UI_METER_FLOOR = 0.003;
const matcherKinds = ["app_id", "process_name", "binary", "window_class", "media_name"] as const;
type MatcherKind = (typeof matcherKinds)[number];

type AudioActionReport = {
  title: string;
  commands: CommandExecution[];
  plannedCount?: number;
  finishedAt: number;
};

type OfflineRoutingEntry = {
  matcher: AppMatcher;
  displayName: string;
  meta: string;
  channel_id?: string;
  volumePreset?: AppVolumePreset;
};

type MergeTarget = {
  matcher: AppMatcher;
  displayName: string;
  meta: string;
};

type SelectOption = {
  value: string;
  label: string;
  disabled?: boolean;
};

type SettingsTab = "general" | "profiles" | "health";

export default function App() {
  const [state, setState] = useState<AppStateSnapshot | null>(() => initialSnapshot());
  const [activeView, setActiveView] = useState<View>(() => initialView());
  const [selectedChannelId, setSelectedChannelId] = useState("hardware_in");
  const [busy, setBusy] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [audioActionReport, setAudioActionReport] = useState<AudioActionReport | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const autoUpdateCheckStarted = useRef(false);
  const refreshTimer = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const refreshInFlight = useRef(false);
  const refreshQueued = useRef(false);

  const applySnapshot = useCallback((next: AppStateSnapshot) => {
    setState(next);
    setSelectedChannelId((current) =>
      next.config.channels.some((channel) => channel.id === current)
        ? current
        : next.config.channels[0]?.id ?? "hardware_in",
    );
  }, []);

  const refresh = useCallback(async () => {
    const next = await invoke<AppStateSnapshot>("get_state");
    applySnapshot(next);
  }, [applySnapshot]);

  const scheduleRefresh = useCallback((delayMs = 120) => {
    if (refreshTimer.current !== null) {
      window.clearTimeout(refreshTimer.current);
    }
    refreshTimer.current = window.setTimeout(() => {
      refreshTimer.current = null;
      if (refreshInFlight.current) {
        refreshQueued.current = true;
        return;
      }
      refreshInFlight.current = true;
      invoke<AppStateSnapshot>("observe_state")
        .then(applySnapshot)
        .catch(() => undefined)
        .finally(() => {
          refreshInFlight.current = false;
          if (refreshQueued.current) {
            refreshQueued.current = false;
            scheduleRefresh(delayMs);
          }
        });
    }, delayMs);
  }, [applySnapshot]);

  // UI actions should not wait on a full state refresh. This helper invokes the
  // backend command, then coalesces a lightweight refresh in the background.
  const run = useCallback(
    async <T,>(command: string, args?: Record<string, unknown>, message?: string): Promise<T> => {
      try {
        const result = await invoke<T>(command, args);
        scheduleRefresh();
        if (message) {
          setToast(message);
        }
        return result;
      } catch (error) {
        setToast(String(error));
        throw error;
      }
    },
    [scheduleRefresh],
  );

  const patchMixVolume = useCallback((mixId: string, volume: number) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          mixes: current.config.mixes.map((mix) =>
            mix.id === mixId ? { ...mix, volume } : mix,
          ),
        },
      };
    });
  }, []);

  const patchMix = useCallback((mixId: string, patch: Partial<Mix>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          mixes: current.config.mixes.map((mix) =>
            mix.id === mixId ? { ...mix, ...patch } : mix,
          ),
        },
      };
    });
  }, []);

  const patchChannelBusVolume = useCallback((channelId: string, mixId: string, volume: number) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          channels: current.config.channels.map((channel) => {
            if (channel.id !== channelId) return channel;
            const mixBuses = Object.fromEntries(
              Object.entries(channel.mix_buses).map(([busMixId, bus]) => [
                busMixId,
                channel.linked || busMixId === mixId ? { ...bus, volume } : bus,
              ]),
            );
            return { ...channel, mix_buses: mixBuses };
          }),
        },
      };
    });
  }, []);

  const patchChannelBus = useCallback((channelId: string, mixId: string, patch: Partial<MixBus>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          channels: current.config.channels.map((channel) => {
            if (channel.id !== channelId) return channel;
            const bus = channel.mix_buses[mixId] ?? { volume: 1, muted: false };
            return {
              ...channel,
              mix_buses: {
                ...channel.mix_buses,
                [mixId]: { ...bus, ...patch },
              },
            };
          }),
        },
      };
    });
  }, []);

  const patchChannel = useCallback((channelId: string, patch: Partial<Channel>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          channels: current.config.channels.map((channel) =>
            channel.id === channelId ? { ...channel, ...patch } : channel,
          ),
        },
      };
    });
  }, []);

  const patchAppStream = useCallback((streamId: string, patch: Partial<AppStream>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        graph: {
          ...current.graph,
          app_streams: current.graph.app_streams.map((stream) =>
            stream.id === streamId ? { ...stream, ...patch } : stream,
          ),
        },
      };
    });
  }, []);

  const setMixVolumeFast = useCallback(
    async (mixId: string, volume: number) => {
      patchMixVolume(mixId, volume);
      try {
        const mix = await invoke<Mix>("set_mix_volume", { mixId, volume });
        patchMixVolume(mix.id, mix.volume);
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMixVolume, refresh],
  );

  const setChannelBusVolumeFast = useCallback(
    async (channelId: string, mixId: string, volume: number) => {
      patchChannelBusVolume(channelId, mixId, volume);
      try {
        const bus = await invoke<MixBus>("set_channel_volume", { channelId, mixId, volume });
        patchChannelBusVolume(channelId, mixId, bus.volume);
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannelBusVolume, refresh],
  );

  const setMixMuteFast = useCallback(
    async (mixId: string, muted: boolean) => {
      patchMix(mixId, { muted });
      try {
        const mix = await invoke<Mix>("set_mix_mute", { mixId, muted });
        patchMix(mix.id, { muted: mix.muted });
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, refresh],
  );

  const setChannelBusMuteFast = useCallback(
    async (channelId: string, mixId: string, muted: boolean) => {
      patchChannelBus(channelId, mixId, { muted });
      try {
        const bus = await invoke<MixBus>("set_channel_mute", { channelId, mixId, muted });
        patchChannelBus(channelId, mixId, { muted: bus.muted });
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannelBus, refresh],
  );

  const setChannelInputFast = useCallback(
    async (channelId: string, sourceDevice: string | null) => {
      patchChannel(channelId, { source_device: sourceDevice });
      try {
        const channel = await invoke<Channel>("set_channel_input", {
          channelId,
          channel_id: channelId,
          sourceDevice,
          source_device: sourceDevice,
        });
        patchChannel(channel.id, { source_device: channel.source_device ?? null });
        scheduleRefresh();
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannel, refresh, scheduleRefresh],
  );

  const patchSettings = useCallback((settings: MixerSettings) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          settings,
        },
      };
    });
  }, []);

  const patchSettingsFromPartial = useCallback((patch: Partial<MixerSettings>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          settings: {
            ...current.config.settings,
            ...patch,
          },
        },
      };
    });
  }, []);

  const setMixMonitorOutputFast = useCallback(
    async (mixId: string, output: string | null) => {
      patchMix(mixId, { monitor_output: output });
      patchSettingsFromPartial({ monitor_follows_default_output: false });
      try {
        const mix = await invoke<Mix>("set_mix_monitor_output", { mixId, output });
        patchMix(mix.id, { monitor_output: mix.monitor_output ?? null });
        scheduleRefresh();
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, patchSettingsFromPartial, refresh, scheduleRefresh],
  );

  const setSettingsFast = useCallback(
    async (settings: MixerSettings) => {
      patchSettings(settings);
      try {
        const next = await invoke<MixerSettings>("set_settings", { settings });
        patchSettings(next);
        scheduleRefresh();
        setToast("Settings updated");
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchSettings, refresh, scheduleRefresh],
  );

  const setAppStreamMuteFast = useCallback(
    async (streamId: string, muted: boolean) => {
      patchAppStream(streamId, { muted });
      try {
        await invoke("set_app_stream_mute", { streamId, muted });
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchAppStream, refresh],
  );

  const recordAudioAction = useCallback((title: string, commands: CommandExecution[], plannedCount?: number) => {
    setAudioActionReport({ title, commands, plannedCount, finishedAt: Date.now() });
    setToast(audioActionToast(title, commands, plannedCount));
  }, []);

  const startOrRepairAudio = useCallback(async () => {
    const title = state?.engine.audio_graph_running ? "Repair Audio" : "Start Audio";
    setBusy(true);
    try {
      const report = await invoke<RepairReport>("repair_audio_graph");
      scheduleRefresh(0);
      recordAudioAction(title, report.outputs, report.planned.commands.length);
    } catch (error) {
      setToast(String(error));
    } finally {
      setBusy(false);
    }
  }, [recordAudioAction, scheduleRefresh, state?.engine.audio_graph_running]);

  const runAudioCommandList = useCallback(
    async (command: string, title: string) => {
      setBusy(true);
      try {
        const outputs = await invoke<CommandExecution[]>(command);
        scheduleRefresh(0);
        recordAudioAction(title, outputs);
      } catch (error) {
        setToast(String(error));
      } finally {
        setBusy(false);
      }
    },
    [recordAudioAction, scheduleRefresh],
  );

  const checkUpdates = useCallback(async (showToast = true) => {
    setUpdateBusy(true);
    try {
      const next = await invoke<UpdateInfo>("check_for_updates");
      setUpdateInfo(next);
      if (showToast || next.available) setToast(next.message);
      if (next.available && state?.config.settings.auto_install_updates && next.install_supported) {
        const result = await invoke<UpdateInstallResult>("install_update");
        setToast(result.message);
      }
      return next;
    } catch (error) {
      if (showToast) setToast(String(error));
      throw error;
    } finally {
      setUpdateBusy(false);
    }
  }, [state?.config.settings.auto_install_updates]);

  useEffect(() => {
    refresh().catch((error) => setToast(String(error)));
    const intervalMs = state?.engine.audio_graph_running ? 750 : 1000;
    const timer = window.setInterval(() => {
      invoke<AppStateSnapshot>("observe_state")
        .then(applySnapshot)
        .catch(() => undefined);
    }, intervalMs);
    return () => window.clearInterval(timer);
  }, [applySnapshot, refresh, state?.engine.audio_graph_running]);

  useEffect(() => {
    return () => {
      if (refreshTimer.current !== null) {
        window.clearTimeout(refreshTimer.current);
      }
    };
  }, []);

  useEffect(() => {
    if (!toast) return;
    const timer = window.setTimeout(() => setToast(null), 2400);
    return () => window.clearTimeout(timer);
  }, [toast]);

  useEffect(() => {
    if (!state?.config.settings.auto_check_updates || autoUpdateCheckStarted.current) return;
    autoUpdateCheckStarted.current = true;
    const timer = window.setTimeout(() => {
      checkUpdates(false).catch(() => undefined);
    }, 2500);
    return () => window.clearTimeout(timer);
  }, [checkUpdates, state?.config.settings.auto_check_updates]);

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
            <span>4.1</span>
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

        <div
          aria-label={state?.engine.message ?? "Starting"}
          className="engine-card"
          title={state?.engine.message ?? "Starting"}
        >
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
                state={state}
                setSelectedChannelId={setSelectedChannelId}
                run={run}
                setChannelBusVolume={setChannelBusVolumeFast}
                setChannelBusMute={setChannelBusMuteFast}
                setChannelInput={setChannelInputFast}
                setMixMonitorOutput={setMixMonitorOutputFast}
                setMixMute={setMixMuteFast}
                setMixVolume={setMixVolumeFast}
                setSettings={setSettingsFast}
                busy={busy}
              />
            )}
            {activeView === "routing" && (
              <RoutingView
                state={state}
                run={run}
                setAppStreamMute={setAppStreamMuteFast}
              />
            )}
            {activeView === "effects" && (
              <EffectsView
                state={state}
                selectedChannel={selectedChannel}
                selectedChannelId={selectedChannelId}
                setSelectedChannelId={setSelectedChannelId}
                setChannelInput={setChannelInputFast}
              />
            )}
            {activeView === "scenes" && <ScenesView run={run} state={state} />}
            {activeView === "settings" && (
              <SettingsView
                audioActionReport={audioActionReport}
                state={state}
                run={run}
                setSettings={setSettingsFast}
                updateBusy={updateBusy}
                updateInfo={updateInfo}
                onCleanup={() => runAudioCommandList("cleanup_audio_graph", "Cleanup Audio")}
                onCheckUpdates={() => void checkUpdates(true).catch(() => undefined)}
                onInstallUpdate={() => {
                  setUpdateBusy(true);
                  invoke<UpdateInstallResult>("install_update")
                    .then((result) => setToast(result.message))
                    .catch((error) => setToast(String(error)))
                    .finally(() => setUpdateBusy(false));
                }}
                onOpenReleases={() => {
                  void invoke("open_release_page").catch((error) => setToast(String(error)));
                }}
                onPrune={() => runAudioCommandList("cleanup_stale_audio_graph", "Prune Stale Audio")}
              />
            )}
          </>
        )}
      </main>

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}

function MixerView({
  state,
  setSelectedChannelId,
  run,
  setChannelBusVolume,
  setChannelBusMute,
  setChannelInput,
  setMixMonitorOutput,
  setMixMute,
  setMixVolume,
  setSettings,
  busy,
}: {
  state: AppStateSnapshot;
  setSelectedChannelId: (channelId: string) => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  setMixMonitorOutput: (mixId: string, output: string | null) => Promise<void>;
  setMixMute: (mixId: string, muted: boolean) => Promise<void>;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
  setSettings: (settings: MixerSettings) => Promise<void>;
  busy: boolean;
}) {
  const outputs = state.graph.outputs.filter((output) => !output.is_virtual);
  const softwareChannelCount = state.config.channels.filter((channel) => !isHardwareChannel(channel)).length;
  const microphoneInputs = useMemo(
    () => sortedMicrophoneInputs(state.graph.inputs),
    [state.graph.inputs],
  );
  const [menu, setMenu] = useState<{ x: number; y: number; channelId: string } | null>(null);
  const menuChannel = menu
    ? state.config.channels.find((channel) => channel.id === menu.channelId)
    : undefined;
  const menuChannelIndex = menu
    ? state.config.channels.findIndex((channel) => channel.id === menu.channelId)
    : -1;
  const primaryMixes = primaryBusMixes(state.config.mixes);
  const monitorMix =
    state.config.mixes.find((mix) => mix.id === "monitor") ??
    state.config.mixes[0];
  const [liveMeters, setLiveMeters] = useState<LevelMeter[]>(state.graph.meters);
  const metersUnavailable =
    state.engine.audio_graph_running &&
    !state.engine.dry_run &&
    liveMeters.length === 0;

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
    setLiveMeters(state.graph.meters);
  }, [state.graph.meters]);

  useEffect(() => {
    if (!state.engine.audio_graph_running) {
      setLiveMeters([]);
      return;
    }

    let stopped = false;
    let timer = 0;
    const tick = () => {
      invoke<LevelMeter[]>("observe_meters")
        .then((meters) => {
          if (!stopped) setLiveMeters(meters);
        })
        .catch(() => undefined)
        .finally(() => {
          if (!stopped) timer = window.setTimeout(tick, 16);
        });
    };

    timer = window.setTimeout(tick, 0);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [state.engine.audio_graph_running]);

  const rawMeterLevels = useMemo(() => meterLevelMap(liveMeters), [liveMeters]);
  const meterLevels = useSmoothMeterLevels(rawMeterLevels, state.engine.audio_graph_running);
  const levelFor = useCallback((nodeId: string) => meterLevels[nodeId] ?? 0, [meterLevels]);

  return (
    <section className="view-stack mixer-view-stack no-mix-tabs">
      <div className="mixer-layout classic">
        <div className="source-strip-panel">
          <div className="source-toolbar">
            <div>
              <h2>Sources</h2>
            </div>
            <div className="panel-actions">
              {metersUnavailable && (
                <span className="meter-warning" title="No live PipeWire meter samples are available yet">
                  <Gauge size={14} />
                  Meters unavailable
                </span>
              )}
              <button
                className="secondary-button"
                disabled={softwareChannelCount >= MAX_SOFTWARE_CHANNELS || busy}
                onClick={() => {
                  const name = window.prompt("Route name", "Podcast");
                  if (name) void run("create_channel", { name, kind: "application" satisfies ChannelKind }, "Route added");
                }}
                type="button"
                title={`${softwareChannelCount}/${MAX_SOFTWARE_CHANNELS} source fader routes`}
              >
                <CirclePlus size={16} />
                Route
              </button>
            </div>
          </div>

          <div className="channel-rail">
            {state.config.channels.map((channel) => {
              return (
                <ChannelStrip
                  channel={channel}
                  key={channel.id}
                  levelFor={levelFor}
                  mixes={primaryMixes}
                  microphoneInputs={microphoneInputs}
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
                  setChannelBusMute={setChannelBusMute}
                  setChannelBusVolume={setChannelBusVolume}
                  setChannelInput={setChannelInput}
                />
              );
            })}
            <button
              className="add-channel"
              disabled={softwareChannelCount >= MAX_SOFTWARE_CHANNELS || busy}
              onClick={() => {
                const name = window.prompt("Route name", "Podcast");
                if (name) void run("create_channel", { name, kind: "application" satisfies ChannelKind }, "Route added");
              }}
              title="Add a source fader route"
              type="button"
            >
              <CirclePlus size={18} />
              Route
            </button>
          </div>
        </div>

        <div className="master-panel">
          <div className="master-mix-title">
            <div>
              <strong>Monitor Mix</strong>
            </div>
            <Radio size={18} />
          </div>

          <div className="master-bus-grid">
            {primaryMixes.map((mix) => (
              <MasterBusControl
                key={mix.id}
                mix={mix}
                setMixMute={setMixMute}
                setMixVolume={setMixVolume}
                vuLevel={levelFor(mix.id)}
              />
            ))}
          </div>

          {monitorMix && (
            <>
              <label className="field-label" htmlFor="active-mix-monitor-output">
                Monitor output
              </label>
              <AppSelect
                ariaLabel="Monitor output"
                id="active-mix-monitor-output"
                onChange={(value) => {
                  if (value === AUTO_MONITOR_OUTPUT_VALUE) {
                    void setSettings({
                      ...state.config.settings,
                      monitor_follows_default_output: true,
                    }).catch(() => undefined);
                    return;
                  }
                  void setMixMonitorOutput(monitorMix.id, value || null).catch(() => undefined);
                }}
                options={[
                  {
                    value: AUTO_MONITOR_OUTPUT_VALUE,
                    label: "Auto: Bluetooth, USB, jack, speakers",
                  },
                  { value: "", label: "No monitor route" },
                  ...outputs.map((output) => ({
                    value: output.id,
                    label: output.description,
                  })),
                ]}
                value={
                  state.config.settings.monitor_follows_default_output
                    ? AUTO_MONITOR_OUTPUT_VALUE
                    : monitorMix.monitor_output ?? ""
                }
              />
            </>
          )}

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
          setChannelBusMute={setChannelBusMute}
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
  microphoneInputs,
  levelFor,
  onFocus,
  onOpenMenu,
  setChannelBusMute,
  setChannelBusVolume,
  setChannelInput,
}: {
  channel: Channel;
  mixes: Mix[];
  microphoneInputs: AppStateSnapshot["graph"]["inputs"];
  levelFor: (nodeId: string) => number;
  onFocus: () => void;
  onOpenMenu: (event: ReactMouseEvent<HTMLElement>) => void;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
}) {
  const Icon = channelIcon(channel.kind);
  const isHardware = isHardwareChannel(channel);
  const displayName = channelDisplayName(channel);
  const selectedInputMissing =
    isHardware &&
    channel.source_device &&
    !microphoneInputs.some((input) => input.id === channel.source_device);

  return (
    <article
      className={isHardware ? "channel-strip hardware" : "channel-strip"}
      onClick={onFocus}
      onContextMenu={onOpenMenu}
    >
      <div className="strip-title">
        <Icon size={17} />
        <span>{displayName}</span>
      </div>
      <div className="strip-buses">
        {mixes.map((mix) => (
          <ChannelBusControl
            bus={channel.mix_buses[mix.id] ?? { volume: 1, muted: false }}
            channel={channel}
            key={mix.id}
            mix={mix}
            setChannelBusMute={setChannelBusMute}
            setChannelBusVolume={setChannelBusVolume}
            vuLevel={channelBusVuLevel(
              channel,
              mix,
              channel.mix_buses[mix.id] ?? { volume: 1, muted: false },
              levelFor,
            )}
          />
        ))}
      </div>
      {isHardware && (
        <AppSelect
          className="strip-device-select"
          ariaLabel={`${displayName} microphone`}
          onChange={(nextValue) => {
            const value = nextValue || null;
            void setChannelInput(channel.id, value).catch(() => undefined);
          }}
          options={[
            { value: "", label: autoMicrophoneLabel(microphoneInputs, "Auto mic") },
            ...(selectedInputMissing
              ? [{
                  value: channel.source_device ?? "",
                  label: channel.source_device ?? "",
                }]
              : []),
            ...microphoneInputs.map((input) => ({
              value: input.id,
              label: input.description,
            })),
          ]}
          value={channel.source_device ?? ""}
        />
      )}
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
  setChannelBusMute,
  x,
  y,
}: {
  channel: Channel;
  canMoveDown: boolean;
  canMoveUp: boolean;
  mixes: Mix[];
  onClose: () => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  x: number;
  y: number;
}) {
  const displayName = channelDisplayName(channel);
  return (
    <div className="context-menu" style={{ left: x, top: y }} onClick={(event) => event.stopPropagation()}>
      <div className="context-menu-title">{displayName}</div>
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
          const name = window.prompt("Channel name", displayName);
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
              void setChannelBusMute(channel.id, mix.id, !(bus?.muted ?? false)).finally(onClose)
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
          if (window.confirm(`Delete ${displayName}?`)) {
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

function AppSelect({
  ariaLabel,
  className = "",
  disabled = false,
  id,
  onChange,
  options,
  value,
}: {
  ariaLabel: string;
  className?: string;
  disabled?: boolean;
  id?: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  value: string;
}) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const selectSearchOnFocusRef = useRef(true);
  const [open, setOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const [position, setPosition] = useState({ left: 0, top: 0, width: 360, maxHeight: 260 });
  const selectedIndex = options.findIndex((option) => option.value === value);
  const selectedOption = selectedIndex >= 0 ? options[selectedIndex] : options[0];
  const filteredOptions = useMemo(
    () => filterSelectOptions(options, searchQuery),
    [options, searchQuery],
  );
  const visibleOptions = useMemo(
    () => visibleSelectOptions(filteredOptions, value),
    [filteredOptions, value],
  );

  const positionMenu = useCallback(() => {
    const button = buttonRef.current;
    if (!button) return;
    const rect = button.getBoundingClientRect();
    const viewportMargin = 12;
    const left = Math.max(viewportMargin, Math.min(rect.left, window.innerWidth - viewportMargin - 240));
    const availableRight = Math.max(240, window.innerWidth - left - viewportMargin);
    const readableWidth = Math.min(520, availableRight, Math.max(360, rect.width));
    const maxHeight = Math.min(320, Math.max(140, window.innerHeight - viewportMargin * 2));
    const height = Math.min(maxHeight, Math.max(140, window.innerHeight - rect.top - viewportMargin));
    setPosition({
      left,
      top: Math.max(viewportMargin, Math.min(rect.top, window.innerHeight - viewportMargin - height)),
      width: readableWidth,
      maxHeight: height,
    });
  }, []);

  const openMenu = useCallback((initialSearch = "") => {
    if (disabled || options.length === 0) return;
    const nextQuery = initialSearch;
    const nextOptions = visibleSelectOptions(filterSelectOptions(options, nextQuery), value);
    const nextSelectedIndex = nextOptions.findIndex((option) => option.value === value);
    selectSearchOnFocusRef.current = nextQuery.length === 0;
    setSearchQuery(nextQuery);
    setActiveIndex(nextSelectedIndex >= 0 ? nextSelectedIndex : firstEnabledOptionIndex(nextOptions));
    positionMenu();
    setOpen(true);
  }, [disabled, options, positionMenu, value]);

  const closeMenu = useCallback(() => {
    setOpen(false);
    setSearchQuery("");
  }, []);

  useEffect(() => {
    if (!open) return;
    positionMenu();
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (
        target &&
        (rootRef.current?.contains(target) || menuRef.current?.contains(target))
      ) {
        return;
      }
      closeMenu();
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeMenu();
    };
    const onScroll = (event: Event) => {
      const target = event.target as Node | null;
      if (target && menuRef.current?.contains(target)) return;
      closeMenu();
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("resize", positionMenu);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("resize", positionMenu);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [closeMenu, open, positionMenu]);

  useEffect(() => {
    if (!open) return;
    const frame = window.requestAnimationFrame(() => {
      searchRef.current?.focus({ preventScroll: true });
      if (selectSearchOnFocusRef.current) {
        searchRef.current?.select();
      } else {
        const length = searchRef.current?.value.length ?? 0;
        searchRef.current?.setSelectionRange(length, length);
      }
    });
    return () => window.cancelAnimationFrame(frame);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    setActiveIndex((current) => {
      if (visibleOptions[current] && !visibleOptions[current].disabled) return current;
      return firstEnabledOptionIndex(visibleOptions);
    });
  }, [open, visibleOptions]);

  const choose = useCallback((option: SelectOption) => {
    if (option.disabled) return;
    closeMenu();
    if (option.value !== value) {
      onChange(option.value);
    }
    buttonRef.current?.focus({ preventScroll: true });
  }, [closeMenu, onChange, value]);

  const moveActive = useCallback((direction: 1 | -1) => {
    setActiveIndex((current) => nextEnabledOptionIndex(visibleOptions, current, direction));
  }, [visibleOptions]);

  const chooseActive = useCallback(() => {
    const option = visibleOptions[activeIndex];
    if (option) choose(option);
  }, [activeIndex, choose, visibleOptions]);

  const handleSearchKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      moveActive(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      moveActive(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      chooseActive();
    } else if (event.key === "Escape") {
      event.preventDefault();
      closeMenu();
      buttonRef.current?.focus({ preventScroll: true });
    }
  };

  return (
    <div
      className={["app-select", className].filter(Boolean).join(" ")}
      onClick={(event) => event.stopPropagation()}
      onPointerDown={(event) => event.stopPropagation()}
      ref={rootRef}
    >
      <button
        aria-expanded={open}
        aria-haspopup="listbox"
        aria-label={ariaLabel}
        className="app-select-button"
        disabled={disabled}
        id={id}
        ref={buttonRef}
        onClick={() => (open ? closeMenu() : openMenu())}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown") {
            event.preventDefault();
            if (!open) openMenu();
            else moveActive(1);
          } else if (event.key === "ArrowUp") {
            event.preventDefault();
            if (!open) openMenu();
            else moveActive(-1);
          } else if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            if (open) {
              chooseActive();
            } else {
              openMenu();
            }
          } else if (!open && isPrintableSelectSearchKey(event)) {
            event.preventDefault();
            openMenu(event.key);
          }
        }}
        type="button"
      >
        <span>{selectedOption?.label ?? "Select"}</span>
        <ArrowDown size={15} />
      </button>
      {open && typeof document !== "undefined" && createPortal(
        <div
          className="app-select-menu"
          ref={menuRef}
          style={{
            left: position.left,
            maxHeight: position.maxHeight,
            top: position.top,
            width: position.width,
          }}
        >
          <input
            aria-label={`${ariaLabel} search`}
            className="app-select-search"
            onChange={(event) => setSearchQuery(event.currentTarget.value)}
            onKeyDown={handleSearchKeyDown}
            placeholder="Search"
            ref={searchRef}
            value={searchQuery}
          />
          <div className="app-select-options" role="listbox">
            {visibleOptions.map((option, index) => (
              <button
                aria-selected={option.value === value}
                className={[
                  "app-select-option",
                  option.value === value ? "selected" : "",
                  index === activeIndex ? "active" : "",
                ].filter(Boolean).join(" ")}
                disabled={option.disabled}
                key={`${option.value}-${index}`}
                onClick={() => choose(option)}
                role="option"
                title={option.label}
                type="button"
              >
                <span>{option.label}</span>
                {option.value === value && <Check size={14} />}
              </button>
            ))}
            {filteredOptions.length === 0 && (
              <div className="app-select-empty">No matching options</div>
            )}
            {filteredOptions.length > visibleOptions.length && (
              <div className="app-select-empty">
                Showing {visibleOptions.length} of {filteredOptions.length}
              </div>
            )}
          </div>
        </div>,
        document.body,
      )}
    </div>
  );
}

function firstEnabledOptionIndex(options: SelectOption[]) {
  const index = options.findIndex((option) => !option.disabled);
  return index >= 0 ? index : 0;
}

function nextEnabledOptionIndex(options: SelectOption[], current: number, direction: 1 | -1) {
  if (options.length === 0) return 0;
  let next = current;
  for (let attempt = 0; attempt < options.length; attempt += 1) {
    next = (next + direction + options.length) % options.length;
    if (!options[next]?.disabled) return next;
  }
  return current;
}

function filterSelectOptions(options: SelectOption[], query: string): SelectOption[] {
  const needles = normalizeSelectSearch(query)
    .split(" ")
    .filter(Boolean);
  if (needles.length === 0) return options;
  return options.filter((option) => {
    const haystack = normalizeSelectSearch(`${option.label} ${option.value}`);
    return needles.every((needle) => haystack.includes(needle));
  });
}

function visibleSelectOptions(options: SelectOption[], selectedValue: string): SelectOption[] {
  if (options.length <= SELECT_VISIBLE_OPTION_LIMIT) return options;
  const visible = options.slice(0, SELECT_VISIBLE_OPTION_LIMIT);
  if (!selectedValue || visible.some((option) => option.value === selectedValue)) {
    return visible;
  }
  const selectedOption = options.find((option) => option.value === selectedValue);
  return selectedOption ? [selectedOption, ...visible.slice(0, SELECT_VISIBLE_OPTION_LIMIT - 1)] : visible;
}

function normalizeSelectSearch(value: string): string {
  return value.trim().toLowerCase();
}

function isPrintableSelectSearchKey(event: ReactKeyboardEvent<HTMLElement>): boolean {
  return event.key.length === 1 && !event.altKey && !event.ctrlKey && !event.metaKey;
}

function ChannelBusControl({
  channel,
  mix,
  bus,
  setChannelBusMute,
  setChannelBusVolume,
  vuLevel,
}: {
  channel: Channel;
  mix: Mix;
  bus: MixBus;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
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
    void setChannelBusVolume(channel.id, mix.id, next / 100).catch(() => undefined);
  }, [channel.id, draft, mix.id, setChannelBusVolume]);

  return (
    <div className="bus-control">
      <div className="bus-label">{compactMixLabel(mix)}</div>
      <VuSlider
        ariaLabel={`${channelDisplayName(channel)} ${mix.name} volume`}
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
          void setChannelBusMute(channel.id, mix.id, !bus.muted).catch(() => undefined);
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
  setMixMute,
  setMixVolume,
  vuLevel,
}: {
  mix: Mix;
  setMixMute: (mixId: string, muted: boolean) => Promise<void>;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
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
    void setMixVolume(mix.id, next / 100).catch(() => undefined);
  }, [draft, mix.id, setMixVolume]);

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
          void setMixMute(mix.id, !mix.muted).catch(() => undefined)
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
  const trackRef = useRef<HTMLDivElement | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const visibleVuLevel = vuLevel >= 0.001 ? vuLevel : 0;
  const className = [
    "vu-slider",
    master ? "master" : "",
    muted ? "muted" : "",
    isDragging ? "dragging" : "",
  ]
    .filter(Boolean)
    .join(" ");

  const valueFromPointer = (event: ReactPointerEvent<HTMLDivElement>) => {
    const rect = trackRef.current?.getBoundingClientRect() ?? event.currentTarget.getBoundingClientRect();
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
      <div className="vu-track" ref={trackRef}>
        {visibleVuLevel > 0 && (
          <>
            <div className="vu-fill" style={{ height: trackSize(visibleVuLevel) }} />
            <div className="vu-cap" style={{ bottom: trackPosition(visibleVuLevel) }} />
          </>
        )}
      </div>
      <div className="vu-thumb" style={{ bottom: thumbPosition(value) }} />
    </div>
  );
}

function RoutingView({
  state,
  run,
  setAppStreamMute,
}: {
  state: AppStateSnapshot;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setAppStreamMute: (streamId: string, muted: boolean) => Promise<void>;
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
    <section className="two-column routing-view">
      <div className="panel">
        <div className="panel-header">
          <h2>Active Apps</h2>
          <Cable size={18} />
        </div>
        <div className="route-list">
          {state.graph.app_streams.map((stream) => (
            <StreamRouteRow
              key={stream.id}
              state={state}
              stream={stream}
              run={run}
              setAppStreamMute={setAppStreamMute}
            />
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
          <AppSelect
            ariaLabel="Rule matcher type"
            onChange={(value) => setMatcherKind(value as MatcherKind)}
            options={matcherKinds.map((kind) => ({
              value: kind,
              label: matcherKindLabel(kind),
            }))}
            value={matcherKind}
          />
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
          <AppSelect
            ariaLabel="Rule channel"
            onChange={setTargetChannelId}
            options={state.config.channels.map((channel) => ({
              value: channel.id,
              label: channelDisplayName(channel),
            }))}
            value={targetChannelId}
          />
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
                <AppSelect
                  ariaLabel={`Route ${entry.displayName} to channel`}
                  onChange={(channelId) => {
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
                  options={[
                    { value: "", label: "Unassigned" },
                    ...state.config.channels.map((item) => ({
                      value: item.id,
                      label: channelDisplayName(item),
                    })),
                  ]}
                  value={channel?.id ?? ""}
                />
                <OfflineVolumeControl
                  label={entry.displayName}
                  matcher={entry.matcher}
                  preset={entry.volumePreset}
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
}: {
  label: string;
  matcher: AppMatcher;
  preset?: AppVolumePreset;
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
    void invoke("set_app_volume_preset", { matcher, volume: next / 100 }).catch(() => undefined);
  }, [draft, matcher]);

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
  setAppStreamMute,
}: {
  state: AppStateSnapshot;
  stream: AppStream;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setAppStreamMute: (streamId: string, muted: boolean) => Promise<void>;
}) {
  const [draftVolume, setDraftVolume] = useState(appVolumeToPercent(stream.volume));
  const [draftRoute, setDraftRoute] = useState(stream.routed_channel_id ?? "");
  const lastCommitted = useRef(draftVolume);
  const volumeApplyInFlight = useRef(false);
  const queuedVolume = useRef<number | null>(null);
  const presetSaveTimer = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const optimisticVolumeUntil = useRef(0);
  const optimisticRouteUntil = useRef(0);

  useEffect(() => {
    if (
      volumeApplyInFlight.current ||
      queuedVolume.current !== null ||
      Date.now() < optimisticVolumeUntil.current
    ) {
      return;
    }
    const next = appVolumeToPercent(stream.volume);
    setDraftVolume(next);
    lastCommitted.current = next;
  }, [stream.volume]);

  useEffect(() => {
    if (Date.now() < optimisticRouteUntil.current) return;
    setDraftRoute(stream.routed_channel_id ?? "");
  }, [stream.routed_channel_id]);

  useEffect(() => {
    return () => {
      if (presetSaveTimer.current !== null) {
        window.clearTimeout(presetSaveTimer.current);
      }
    };
  }, []);

  const flushQueuedVolume = useCallback(() => {
    if (volumeApplyInFlight.current) return;
    const next = queuedVolume.current;
    if (next === null) return;
    queuedVolume.current = null;
    volumeApplyInFlight.current = true;
    optimisticVolumeUntil.current = Date.now() + 1500;
    void invoke("set_app_stream_volume", {
      streamId: stream.id,
      volume: next / 100,
    })
      .catch(() => undefined)
      .finally(() => {
        volumeApplyInFlight.current = false;
        if (queuedVolume.current !== null) {
          flushQueuedVolume();
        } else {
          optimisticVolumeUntil.current = Date.now() + 750;
        }
      });
  }, [stream.id]);

  const commitVolume = useCallback((nextValue = draftVolume) => {
    const next = appVolumePercent(nextValue);
    setDraftVolume(next);
    if (lastCommitted.current === next) return;
    lastCommitted.current = next;
    optimisticVolumeUntil.current = Date.now() + 1500;
    queuedVolume.current = next;
    flushQueuedVolume();
    const volume = next / 100;
    if (presetSaveTimer.current !== null) {
      window.clearTimeout(presetSaveTimer.current);
    }
    presetSaveTimer.current = window.setTimeout(() => {
      void invoke("set_app_volume_preset", {
        matcher: matcherForStream(stream),
        volume,
      }).catch(() => undefined);
    }, 250);
  }, [draftVolume, flushQueuedVolume, stream]);

  const routeStream = async (channelId: string) => {
    setDraftRoute(channelId);
    optimisticRouteUntil.current = Date.now() + 1500;
    if (!channelId) {
      const matcher = matcherForStream(stream);
      await invoke("remove_app_route", { matcher });
      await invoke("move_app_stream_to_default", { streamId: stream.id });
      optimisticRouteUntil.current = Date.now() + 750;
      return;
    }
    await invoke("move_app_stream", {
      streamId: stream.id,
      channelId,
    });
    await invoke("assign_app_to_channel", {
      channelId,
      matcher: matcherForStream(stream),
    });
    optimisticRouteUntil.current = Date.now() + 750;
  };

  return (
    <div className="route-row">
      <div>
        <strong>{stream.display_name}</strong>
        <span>{stream.media_name ?? stream.process_name ?? stream.id}</span>
      </div>
      <AppSelect
        ariaLabel={`Route ${stream.display_name} to channel`}
        onChange={(value) => void routeStream(value).catch(() => setDraftRoute(stream.routed_channel_id ?? ""))}
        options={[
          { value: "", label: "Unassigned" },
          ...state.config.channels.map((channel) => ({
            value: channel.id,
            label: channelDisplayName(channel),
          })),
        ]}
        value={draftRoute}
      />
      <label className="route-volume-control" title="App stream volume">
        <Volume2 size={14} />
        <input
          aria-label={`${stream.display_name} volume`}
          max={100}
          min={1}
          onBlur={(event) => commitVolume(Number(event.currentTarget.value))}
          onChange={(event) => setDraftVolume(appVolumePercent(Number(event.currentTarget.value)))}
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
          void setAppStreamMute(stream.id, !stream.muted).catch(() => undefined)
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
  const [mergeOpen, setMergeOpen] = useState(false);
  const mergeButtonRef = useRef<HTMLButtonElement | null>(null);
  const [mergePosition, setMergePosition] = useState<{ left: number; top: number } | null>(null);
  const mergeTargets = mergeTargetsForState(state, matcher);

  const updateMergePosition = useCallback(() => {
    const rect = mergeButtonRef.current?.getBoundingClientRect();
    if (!rect) return;
    const width = Math.min(360, Math.max(260, window.innerWidth - 24));
    const maxLeft = Math.max(12, window.innerWidth - width - 12);
    const left = Math.min(maxLeft, Math.max(12, rect.right - width));
    const below = rect.bottom + 8;
    const maxTop = Math.max(12, window.innerHeight - 332);
    const top = Math.min(maxTop, Math.max(12, below));
    setMergePosition({ left, top });
  }, []);

  useEffect(() => {
    if (!mergeOpen) return;
    const close = () => setMergeOpen(false);
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMergeOpen(false);
    };
    const onLayout = () => updateMergePosition();
    updateMergePosition();
    window.addEventListener("click", close);
    window.addEventListener("keydown", onKey);
    window.addEventListener("resize", onLayout);
    window.addEventListener("scroll", onLayout, true);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("resize", onLayout);
      window.removeEventListener("scroll", onLayout, true);
    };
  }, [mergeOpen, updateMergePosition]);

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
        ref={mergeButtonRef}
        onClick={(event) => {
          event.stopPropagation();
          setMergeOpen((open) => {
            if (open) return false;
            updateMergePosition();
            return true;
          });
        }}
        title="Merge into remembered app"
        type="button"
      >
        <GitBranch size={14} />
      </button>
      {mergeOpen && mergePosition && createPortal(
        <div
          className="identity-merge-popover"
          onClick={(event) => event.stopPropagation()}
          style={{ left: mergePosition.left, top: mergePosition.top }}
        >
          <strong>Merge Into</strong>
          <div className="identity-merge-list">
            {mergeTargets.map((target) => (
              <button
                key={routeKey(target.matcher)}
                onClick={() => {
                  setMergeOpen(false);
                  void run(
                    "merge_app_identity",
                    { source: matcher, target: target.matcher },
                    "App identities merged",
                  ).catch(() => undefined);
                }}
                type="button"
              >
                <span>{target.displayName}</span>
                <small>{target.meta}</small>
              </button>
            ))}
          </div>
        </div>,
        document.body,
      )}
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
  setChannelInput,
}: {
  state: AppStateSnapshot;
  selectedChannel?: Channel;
  selectedChannelId: string;
  setSelectedChannelId: (channelId: string) => void;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
}) {
  const microphoneInputs = useMemo(
    () => sortedMicrophoneInputs(state.graph.inputs),
    [state.graph.inputs],
  );
  const [effectClipboard, setEffectClipboard] = useState<EffectInstance | null>(null);
  const [draftEffectsByChannel, setDraftEffectsByChannel] = useState<Record<string, EffectInstance[]>>({});
  const [pendingEffectWrites, setPendingEffectWrites] = useState<Record<string, number>>({});
  const [effectError, setEffectError] = useState<string | null>(null);
  const effectWriteGeneration = useRef<Record<string, number>>({});
  const selectedEffects = selectedChannel
    ? draftEffectsByChannel[selectedChannel.id] ?? selectedChannel.effects
    : [];

  useEffect(() => {
    setDraftEffectsByChannel((current) => {
      let changed = false;
      const next = { ...current };
      for (const channel of state.config.channels) {
        const draft = next[channel.id];
        if (draft && effectChainsEqual(draft, channel.effects)) {
          delete next[channel.id];
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [state.config.channels]);

  const updateEffects = (effects: EffectInstance[], message?: string, preferredInstanceId?: string) => {
    if (!selectedChannel) return;
    const channelId = selectedChannel.id;
    const optimisticEffects = normalizeSourceEffects(effects, preferredInstanceId);
    const writeGeneration = (effectWriteGeneration.current[channelId] ?? 0) + 1;
    effectWriteGeneration.current[channelId] = writeGeneration;
    setEffectError(null);
    setDraftEffectsByChannel((current) => ({
      ...current,
      [channelId]: optimisticEffects,
    }));
    setPendingEffectWrites((current) => ({
      ...current,
      [channelId]: (current[channelId] ?? 0) + 1,
    }));
    void invoke<Channel>("set_effect_chain", { channelId, effects: optimisticEffects })
      .then((channel) => {
        if (effectWriteGeneration.current[channelId] !== writeGeneration) return;
        setDraftEffectsByChannel((current) => ({
          ...current,
          [channelId]: channel.effects,
        }));
        if (message) {
          // Keep this local to EffectsView so effect edits never wait on a full state refresh.
          setEffectError(null);
        }
      })
      .catch((error) => {
        if (effectWriteGeneration.current[channelId] !== writeGeneration) return;
        setEffectError(String(error));
        setDraftEffectsByChannel((current) => {
          const next = { ...current };
          delete next[channelId];
          return next;
        });
      })
      .finally(() => {
        setPendingEffectWrites((current) => {
          const count = Math.max(0, (current[channelId] ?? 1) - 1);
          const next = { ...current };
          if (count === 0) {
            delete next[channelId];
          } else {
            next[channelId] = count;
          }
          return next;
        });
      });
  };
  const addEffect = (effect: EffectDefinition) => {
    if (!selectedChannel) return;
    const existing = selectedEffects.find((item) => item.effect_id === effect.id);
    if (existing && isSingleInstanceEffect(effect.id)) {
      if (!existing.bypassed) return;
      updateEffects(
        selectedEffects.map((item) =>
          item.instance_id === existing.instance_id ? { ...item, bypassed: false } : item,
        ),
        "Effect enabled",
        existing.instance_id,
      );
      return;
    }
    const instance: EffectInstance = {
      instance_id: crypto.randomUUID(),
      effect_id: effect.id,
      name: null,
      bypassed: false,
      params: Object.fromEntries(effect.params.map((param) => [param.id, param.default])),
    };
    updateEffects([...selectedEffects, instance], "Effect added", instance.instance_id);
  };
  const applyPreset = (instanceId: string, values: Record<string, number>) => {
    if (!selectedChannel) return;
    const effects = selectedEffects.map((effect) =>
      effect.instance_id === instanceId
        ? { ...effect, params: { ...effect.params, ...values } }
        : effect,
    );
    updateEffects(effects, "Preset applied");
  };
  const updateEffectParam = (instanceId: string, paramId: string, value: number) => {
    if (!selectedChannel) return;
    updateEffects(
      selectedEffects.map((effect) =>
        effect.instance_id === instanceId
          ? { ...effect, params: { ...effect.params, [paramId]: value } }
          : effect,
      ),
      "Effect updated",
    );
  };
  const toggleEffectBypass = (instanceId: string, bypassed: boolean) => {
    if (!selectedChannel) return;
    updateEffects(
      selectedEffects.map((effect) =>
        effect.instance_id === instanceId ? { ...effect, bypassed } : effect,
      ),
      bypassed ? "Effect bypassed" : "Effect enabled",
      bypassed ? undefined : instanceId,
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
    const pastedEffect = {
      ...structuredClone(effectClipboard),
      instance_id: crypto.randomUUID(),
      name: effectClipboard.name ? `${effectClipboard.name} Copy` : null,
    };
    const existing = selectedEffects.find((effect) => effect.effect_id === pastedEffect.effect_id);
    if (existing && isSingleInstanceEffect(pastedEffect.effect_id)) {
      updateEffects(
        selectedEffects.map((effect) =>
          effect.instance_id === existing.instance_id
            ? { ...pastedEffect, instance_id: existing.instance_id }
            : effect,
        ),
        "Effect replaced",
        existing.instance_id,
      );
      return;
    }
    updateEffects(
      [...selectedEffects, pastedEffect],
      "Effect pasted",
      pastedEffect.instance_id,
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
              <span>{channelDisplayName(channel)}</span>
              <small>
                {isHardwareChannel(channel)
                  ? channelInputLabel(channel, microphoneInputs)
                  : `${(draftEffectsByChannel[channel.id] ?? channel.effects).length} FX`}
              </small>
            </button>
          ))}
        </div>
      </div>
      <div className="panel">
        <div className="panel-header">
          <h2>{selectedChannel ? channelDisplayName(selectedChannel) : "Effects"}</h2>
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
        {(pendingEffectWrites[selectedChannel?.id ?? ""] ?? 0) > 0 && (
          <div className="effect-sync-status">Syncing effect chain...</div>
        )}
        {effectError && (
          <div className="effect-warning">
            <CircleAlert size={15} />
            <span>{effectError}</span>
          </div>
        )}
        {selectedChannel && isHardwareChannel(selectedChannel) && (
          <div className="hardware-source-card">
            <label className="field-label" htmlFor="effects-microphone-source">
              Microphone
            </label>
            <AppSelect
              ariaLabel="Microphone"
              id="effects-microphone-source"
              onChange={(value) =>
                void setChannelInput(selectedChannel.id, value || null).catch(() => undefined)
              }
              options={[
                { value: "", label: autoMicrophoneLabel(microphoneInputs, "Auto hardware input") },
                ...microphoneInputs.map((input) => ({
                  value: input.id,
                  label: input.description,
                })),
              ]}
              value={selectedChannel.source_device ?? ""}
            />
            <div className="field-label">Input mode</div>
            <div className="static-field">Mono</div>
          </div>
        )}
        <div className="effect-chain">
          {selectedEffects.map((effect, index) => {
            const definition = state.catalog.effects.find((item) => item.id === effect.effect_id);
            return (
              <EffectBlock
                availability={state.graph.effect_availability.find((item) => item.effect_id === effect.effect_id)}
                definition={definition}
                effect={effect}
                index={index}
                key={effect.instance_id}
                onCopy={copyEffect}
                onDelete={deleteEffect}
                onMove={moveEffect}
                onRename={renameEffect}
                onApplyPreset={applyPreset}
                onToggleBypass={toggleEffectBypass}
                onUpdateParam={updateEffectParam}
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
            const isEnabled = selectedEffects.some((item) => item.effect_id === effect.id && !item.bypassed);
            const isPresent = selectedEffects.some((item) => item.effect_id === effect.id);
            const isUnavailable = availability?.available === false;
            const className = [
              "catalog-item",
              isEnabled ? "enabled" : "",
              isPresent && !isEnabled ? "bypassed" : "",
            ]
              .filter(Boolean)
              .join(" ");
            return (
              <button
                className={className}
                disabled={!selectedChannel}
                key={effect.id}
                onClick={() => {
                  addEffect(effect);
                }}
                title={
                  isEnabled
                    ? "Enabled on this source"
                    : isUnavailable
                      ? availability.detail
                      : isPresent
                        ? "Bypassed on this source"
                        : "Add to this source"
                }
                type="button"
              >
                <span>{effect.name}</span>
                {isUnavailable ? (
                  <CircleAlert size={15} />
                ) : isEnabled ? (
                  <Check size={15} />
                ) : (
                  <CirclePlus size={15} />
                )}
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
  effect,
  definition,
  index,
  total,
  onApplyPreset,
  onCopy,
  onDelete,
  onMove,
  onRename,
  onToggleBypass,
  onUpdateParam,
}: {
  availability?: EffectAvailability;
  effect: EffectInstance;
  definition?: EffectDefinition;
  index: number;
  total: number;
  onApplyPreset: (instanceId: string, values: Record<string, number>) => void;
  onCopy: (effect: EffectInstance) => void;
  onDelete: (instanceId: string) => void;
  onMove: (index: number, direction: -1 | 1) => void;
  onRename: (instanceId: string) => void;
  onToggleBypass: (instanceId: string, bypassed: boolean) => void;
  onUpdateParam: (instanceId: string, paramId: string, value: number) => void;
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
            onClick={() => onToggleBypass(effect.instance_id, !effect.bypassed)}
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
          onChange={(value) => onUpdateParam(effect.instance_id, param.id, value)}
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
  const runSceneAction = (action: () => Promise<void>) => {
    void action().catch(() => undefined);
  };

  return (
    <section className="panel single-panel scene-panel">
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
            onClick={() => runSceneAction(exportBackup)}
            type="button"
            title="Export the full mixer setup and saved scene library"
          >
            <ArrowDown size={16} />
            Backup
          </button>
          <button
            className="primary-button"
            onClick={() =>
              runSceneAction(async () => {
                const name = window.prompt("Scene name", "Streaming");
                if (!name) return;
                await run("save_scene", { name }, "Scene saved");
                await refreshScenes();
              })
            }
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
                onClick={() =>
                  runSceneAction(async () => {
                    if (!window.confirm(`Replace the current mixer layout with "${template.name}"?`)) return;
                    await run("apply_setup_template", { templateId: template.id, template_id: template.id }, "Template applied");
                    await refreshScenes();
                  })
                }
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
              onClick={() =>
                runSceneAction(async () => {
                  await run("load_scene", sceneIdArgs(scene.id), "Scene loaded");
                  await refreshScenes();
                })
              }
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
                onClick={() =>
                  runSceneAction(async () => {
                    if (!window.confirm(`Delete scene "${scene.name}"?`)) return;
                    await run("delete_scene", sceneIdArgs(scene.id), "Scene deleted");
                    await refreshScenes();
                  })
                }
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
      .map((effect) => `${channelDisplayName(channel)}: ${effect.effect_id}`),
  );
  const activeMixRoutes = state.config.channels.length * state.config.mixes.length;
  const baseLatency = state.config.settings.low_latency_mic_monitoring ? 12 : 25;
  const hardwareInput = state.config.channels.find(isHardwareChannel);
  const hardwareFx = hardwareInput?.effects.some((effect) => !effect.bypassed) ?? false;
  const micHops = hardwareFx ? 4 : 3;
  const estimatedMicPath = `${baseLatency * micHops} ms+`;
  const streamDelay = state.config.settings.stream_sync_delay_msec;

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
        <Stat icon={Gauge} label="Mic hop" value={`${baseLatency} ms`} />
        <Stat icon={Mic} label="Mic path" value={estimatedMicPath} />
        <Stat icon={GitBranch} label="Stream sync" value={`${streamDelay} ms`} />
      </div>
      <div className="command-stats">
        <Stat icon={Radio} label="Monitor sync" value={`${state.config.settings.monitor_sync_delay_msec} ms`} />
        <Stat icon={Cable} label="Routes" value={String(activeMixRoutes)} />
        <Stat icon={Activity} label="Mode" value={state.config.settings.low_latency_mic_monitoring ? "Low" : "Stable"} />
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
  audioActionReport,
  state,
  run,
  setSettings,
  updateBusy,
  updateInfo,
  onCleanup,
  onCheckUpdates,
  onInstallUpdate,
  onOpenReleases,
  onPrune,
}: {
  audioActionReport: AudioActionReport | null;
  state: AppStateSnapshot;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setSettings: (settings: MixerSettings) => Promise<void>;
  updateBusy: boolean;
  updateInfo: UpdateInfo | null;
  onCleanup: () => void | Promise<unknown>;
  onCheckUpdates: () => void;
  onInstallUpdate: () => void;
  onOpenReleases: () => void;
  onPrune: () => void | Promise<unknown>;
}) {
  const updateSettings = (settings: MixerSettings) => void setSettings(settings);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");

  return (
    <section className={settingsTab === "health" ? "settings-view wide" : "settings-view"}>
      <div className="panel settings-tabs-panel">
        <div className="panel-header">
          <h2>Settings</h2>
          <Settings size={18} />
        </div>
        <div className="settings-tabs" role="tablist" aria-label="Settings sections">
          <button
            className={settingsTab === "general" ? "settings-tab active" : "settings-tab"}
            onClick={() => setSettingsTab("general")}
            role="tab"
            type="button"
          >
            <Settings size={16} />
            General
          </button>
          <button
            className={settingsTab === "profiles" ? "settings-tab active" : "settings-tab"}
            onClick={() => setSettingsTab("profiles")}
            role="tab"
            type="button"
          >
            <Cable size={16} />
            Profiles
          </button>
          <button
            className={settingsTab === "health" ? "settings-tab active" : "settings-tab"}
            onClick={() => setSettingsTab("health")}
            role="tab"
            type="button"
          >
            <Activity size={16} />
            Health
          </button>
        </div>
      </div>

      {settingsTab === "general" && (
        <section className="panel single-panel settings-content-panel">
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
              label="Auto monitor output"
              onChange={(value) =>
                updateSettings({ ...state.config.settings, monitor_follows_default_output: value })
              }
              value={state.config.settings.monitor_follows_default_output}
            />
            <Toggle
              label="Control default microphone"
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
            <Toggle
              label="Auto-install AppImage updates"
              onChange={(value) =>
                updateSettings({ ...state.config.settings, auto_install_updates: value })
              }
              value={state.config.settings.auto_install_updates}
            />
            <div className="settings-control">
              <span>Release channel</span>
              <AppSelect
                ariaLabel="Release channel"
                onChange={(value) =>
                  void updateSettings({
                    ...state.config.settings,
                    release_channel: value === "beta" ? "beta" : "stable",
                  })
                }
                options={[
                  { value: "stable", label: "Stable" },
                  { value: "beta", label: "Pre-release" },
                ]}
                value={state.config.settings.release_channel}
              />
            </div>
          </div>
          <div className="settings-section">
            <div className="panel-header compact">
              <h2>Updates</h2>
              <Download size={18} />
            </div>
            <div className="update-card">
              <div>
                <strong>{updateInfo?.message ?? "Update status has not been checked"}</strong>
                <span>
                  {updateInfo
                    ? `${updateInfo.channel} · current ${updateInfo.current_version}${updateInfo.version ? ` · latest ${updateInfo.version}` : ""}`
                    : "Signed AppImage updates, plus deb/rpm/AUR package releases"}
                </span>
              </div>
              <div className="panel-actions">
                <button className="secondary-button" disabled={updateBusy} onClick={onCheckUpdates} type="button">
                  <RefreshCw size={16} />
                  Check
                </button>
                <button
                  className="secondary-button"
                  disabled={updateBusy || !updateInfo?.available || !updateInfo.install_supported}
                  onClick={onInstallUpdate}
                  title={
                    updateInfo?.install_supported === false
                      ? "Install through your package manager or use the AppImage"
                      : "Download, verify, install, and restart"
                  }
                  type="button"
                >
                  <Download size={16} />
                  Install
                </button>
                <button className="secondary-button" onClick={onOpenReleases} type="button">
                  <ExternalLink size={16} />
                  Releases
                </button>
              </div>
            </div>
          </div>
          <div className="settings-section">
            <div className="panel-header compact">
              <h2>Sync</h2>
              <Gauge size={18} />
            </div>
            <div className="settings-grid">
              <Toggle
                label="Low-latency mic monitoring"
                onChange={(value) =>
                  updateSettings({ ...state.config.settings, low_latency_mic_monitoring: value })
                }
                value={state.config.settings.low_latency_mic_monitoring}
              />
              <VolumeFader
                label="Stream source delay"
                max={250}
                min={0}
                unit="ms"
                value={state.config.settings.stream_sync_delay_msec}
                onChange={(value) =>
                  updateSettings({
                    ...state.config.settings,
                    stream_sync_delay_msec: Math.round(value),
                  })
                }
              />
              <VolumeFader
                label="Monitor source delay"
                max={250}
                min={0}
                unit="ms"
                value={state.config.settings.monitor_sync_delay_msec}
                onChange={(value) =>
                  updateSettings({
                    ...state.config.settings,
                    monitor_sync_delay_msec: Math.round(value),
                  })
                }
              />
            </div>
          </div>
          <div className="system-grid">
            <Stat icon={Cpu} label="Engine" value={state.engine.audio_graph_running ? "Running" : "Stopped"} />
            <Stat icon={Radio} label="Rate" value={`${state.config.audio.sample_rate_hz / 1000} kHz`} />
            <Stat icon={AudioLines} label="Format" value={`${state.config.audio.bit_depth}-bit`} />
          </div>
        </section>
      )}

      {settingsTab === "profiles" && <HardwareProfilesView state={state} />}

      {settingsTab === "health" && (
        <DiagnosticsView
          audioActionReport={audioActionReport}
          onCleanup={onCleanup}
          onPrune={onPrune}
          state={state}
          run={run}
        />
      )}
    </section>
  );
}

function HardwareProfilesView({
  state,
}: {
  state: AppStateSnapshot;
}) {
  const [hardwareProfiles, setHardwareProfiles] = useState<HardwareProfileUiState | null>(null);
  const [hardwareProfileError, setHardwareProfileError] = useState<string | null>(null);
  const fallbackProfile = hardwareProfiles?.fallback_profile ?? state.config.device_policy.fallback_hardware_profile;
  const fallbackSummary = useMemo(() => hardwareProfileSummaryFromFallback(fallbackProfile), [fallbackProfile]);
  const [selectedDeviceId, setSelectedDeviceId] = useState<string | null>(null);
  const [profileNameDraft, setProfileNameDraft] = useState(fallbackSummary.name);
  const hardwareDevices = useMemo(
    () => [
      ...state.graph.inputs
        .filter(isHardwareProfileDevice)
        .map((device) => ({ device, kind: "Input" })),
      ...state.graph.outputs
        .filter(isHardwareProfileDevice)
        .map((device) => ({ device, kind: "Output" })),
    ],
    [state.graph.inputs, state.graph.outputs],
  );
  const profileSummaries = useMemo(() => {
    const profilesById = new Map<string, HardwareProfileSummary>();
    for (const profile of hardwareProfiles?.profiles ?? []) {
      profilesById.set(profile.id, profile);
    }
    profilesById.set(fallbackSummary.id, fallbackSummary);
    return Array.from(profilesById.values());
  }, [fallbackSummary, hardwareProfiles?.profiles]);
  const profileById = useMemo(
    () => new Map(profileSummaries.map((profile) => [profile.id, profile])),
    [profileSummaries],
  );
  const profileOptions = useMemo(() => {
    const options: SelectOption[] = [{ value: "", label: "Auto match" }];
    for (const profile of profileSummaries) {
      options.push({ value: profile.id, label: hardwareProfileOptionLabel(profile) });
    }
    const missingAssignments = new Set(
      Object.values(hardwareProfiles?.assignments ?? {}).filter((profileId) => !profileById.has(profileId)),
    );
    for (const { device } of hardwareDevices) {
      if (device.matched_profile_id && !profileById.has(device.matched_profile_id)) {
        missingAssignments.add(device.matched_profile_id);
      }
    }
    for (const profileId of missingAssignments) {
      options.push({ value: profileId, label: `Missing profile: ${profileId}`, disabled: true });
    }
    return options;
  }, [hardwareDevices, hardwareProfiles?.assignments, profileById, profileSummaries]);
  const resolvedProfileIdForDevice = useCallback(
    (device: DeviceInfo) =>
      hardwareProfiles?.assignments[device.id] || device.matched_profile_id || fallbackProfile.id,
    [fallbackProfile.id, hardwareProfiles?.assignments],
  );
  const selectedDeviceEntry = useMemo(
    () => hardwareDevices.find(({ device }) => device.id === selectedDeviceId) ?? hardwareDevices[0] ?? null,
    [hardwareDevices, selectedDeviceId],
  );
  const currentProfileId = selectedDeviceEntry
    ? resolvedProfileIdForDevice(selectedDeviceEntry.device)
    : fallbackProfile.id;
  const currentProfile = profileById.get(currentProfileId) ?? fallbackSummary;

  const loadHardwareProfiles = useCallback(async () => {
    try {
      const next = await invoke<HardwareProfileUiState>("list_hardware_profiles");
      setHardwareProfiles(next);
      setHardwareProfileError(null);
    } catch (error) {
      setHardwareProfileError(String(error));
    }
  }, []);

  useEffect(() => {
    void loadHardwareProfiles();
  }, [loadHardwareProfiles]);

  useEffect(() => {
    if (hardwareDevices.length === 0) {
      setSelectedDeviceId(null);
      return;
    }
    if (!selectedDeviceId || !hardwareDevices.some(({ device }) => device.id === selectedDeviceId)) {
      setSelectedDeviceId(hardwareDevices[0].device.id);
    }
  }, [hardwareDevices, selectedDeviceId]);

  useEffect(() => {
    setProfileNameDraft(currentProfile.name);
  }, [currentProfile.id, currentProfile.name]);

  const assignHardwareProfile = async (deviceId: string, profileId: string) => {
    try {
      const next = await invoke<HardwareProfileUiState>(
        "set_device_hardware_profile",
        {
          deviceId,
          device_id: deviceId,
          profileId: profileId || null,
          profile_id: profileId || null,
        },
      );
      setHardwareProfiles(next);
      setHardwareProfileError(null);
    } catch (error) {
      setHardwareProfileError(String(error));
    }
  };

  const updateCurrentProfile = async (profile: HardwareProfileSummary) => {
    try {
      const next = await invoke<HardwareProfileUiState>(
        "set_hardware_profile_policy",
        {
          profileId: profile.id,
          profile_id: profile.id,
          name: profile.name,
          latencyPolicy: profile.latency_policy,
          latency_policy: profile.latency_policy,
          routingPolicy: profile.routing_policy,
          routing_policy: profile.routing_policy,
        },
      );
      setHardwareProfiles(next);
      setHardwareProfileError(null);
    } catch (error) {
      setHardwareProfileError(String(error));
    }
  };

  const updateCurrentLatency = (key: keyof HardwareProfileSummary["latency_policy"], value: number) => {
    void updateCurrentProfile({
      ...currentProfile,
      latency_policy: {
        ...currentProfile.latency_policy,
        [key]: Math.round(value),
      },
    }).catch(() => undefined);
  };

  const updateCurrentRouting = (key: keyof HardwareProfileSummary["routing_policy"], value: number | boolean) => {
    void updateCurrentProfile({
      ...currentProfile,
      routing_policy: {
        ...currentProfile.routing_policy,
        [key]: typeof value === "number" ? Math.round(value) : value,
      },
    }).catch(() => undefined);
  };

  const commitCurrentProfileName = () => {
    const name = profileNameDraft.trim();
    if (!name || name === currentProfile.name) {
      setProfileNameDraft(currentProfile.name);
      return;
    }
    void updateCurrentProfile({ ...currentProfile, name }).catch(() => undefined);
  };

  return (
    <section className="panel single-panel">
      <div className="panel-header">
        <h2>Hardware Profiles</h2>
        <Cable size={18} />
      </div>
      {hardwareProfileError && (
        <div className="effect-warning">
          <CircleAlert size={15} />
          <span>{hardwareProfileError}</span>
        </div>
      )}
      <div className="hardware-profile-grid">
        <div className="profile-editor">
          <label className="field-label" htmlFor="profiles-current-profile-name">
            Current profile
          </label>
          <input
            className="text-field"
            id="profiles-current-profile-name"
            onBlur={commitCurrentProfileName}
            onChange={(event) => setProfileNameDraft(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.currentTarget.blur();
              }
            }}
            value={profileNameDraft}
          />
          <div className="profile-editor-meta">
            <strong>{currentProfile.source}</strong>
            <span>{currentProfile.confidence}</span>
          </div>
          {selectedDeviceEntry && (
            <div className="profile-editor-meta">
              <strong>{selectedDeviceEntry.kind}</strong>
              <span>{selectedDeviceEntry.device.description || selectedDeviceEntry.device.name}</span>
            </div>
          )}
          <VolumeFader
            compact
            label="Stable"
            max={500}
            min={5}
            unit=" ms"
            value={currentProfile.latency_policy.stable_msec ?? 35}
            onChange={(value) => updateCurrentLatency("stable_msec", value)}
          />
          <VolumeFader
            compact
            label="Low latency"
            max={500}
            min={5}
            unit=" ms"
            value={currentProfile.latency_policy.low_latency_msec ?? 20}
            onChange={(value) => updateCurrentLatency("low_latency_msec", value)}
          />
          <VolumeFader
            compact
            label="Bluetooth floor"
            max={500}
            min={50}
            unit=" ms"
            value={currentProfile.latency_policy.bluetooth_floor_msec ?? 120}
            onChange={(value) => updateCurrentLatency("bluetooth_floor_msec", value)}
          />
          <VolumeFader
            compact
            label="Input priority"
            max={100}
            min={0}
            unit=""
            value={currentProfile.routing_policy.input_priority ?? 35}
            onChange={(value) => updateCurrentRouting("input_priority", value)}
          />
          <VolumeFader
            compact
            label="Output priority"
            max={100}
            min={0}
            unit=""
            value={currentProfile.routing_policy.output_priority ?? 30}
            onChange={(value) => updateCurrentRouting("output_priority", value)}
          />
          <Toggle
            label="Auto-select input"
            onChange={(value) => updateCurrentRouting("allow_auto_select_input", value)}
            value={currentProfile.routing_policy.allow_auto_select_input}
          />
          <Toggle
            label="Auto-select output"
            onChange={(value) => updateCurrentRouting("allow_auto_select_output", value)}
            value={currentProfile.routing_policy.allow_auto_select_output}
          />
          <Toggle
            label="Prefer wired input"
            onChange={(value) => updateCurrentRouting("prefer_non_bluetooth_input", value)}
            value={currentProfile.routing_policy.prefer_non_bluetooth_input}
          />
        </div>
        <div className="profile-device-list">
          {hardwareDevices.map(({ device, kind }) => {
            const assignment = hardwareProfiles?.assignments[device.id] ?? "";
            const resolvedProfileId = resolvedProfileIdForDevice(device);
            const activeProfile = profileById.get(resolvedProfileId);
            const selected = selectedDeviceEntry?.device.id === device.id;
            return (
              <div
                className={selected ? "profile-device-row selected" : "profile-device-row"}
                key={`${kind}-${device.id}`}
                onClick={() => setSelectedDeviceId(device.id)}
                onFocus={() => setSelectedDeviceId(device.id)}
              >
                <div>
                  <strong>{device.description || device.name}</strong>
                  <span>
                    {kind} · {device.bus ?? "unknown"} · {activeProfile?.name ?? device.matched_profile_id ?? "Auto"}
                  </span>
                </div>
                <AppSelect
                  ariaLabel={`${device.description || device.name} profile`}
                  disabled={!hardwareProfiles}
                  onChange={(value) => void assignHardwareProfile(device.id, value).catch(() => undefined)}
                  options={profileOptions}
                  value={assignment || resolvedProfileId}
                />
              </div>
            );
          })}
          {hardwareDevices.length === 0 && <EmptyState label="No hardware devices detected" />}
        </div>
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
    settings: "Settings",
  }[view];
}

function hardwareProfileOptionLabel(profile: HardwareProfileSummary): string {
  return `${profile.name} · ${profile.source}`;
}

function hardwareProfileSummaryFromFallback(profile: FallbackHardwareProfile): HardwareProfileSummary {
  return {
    id: profile.id,
    name: profile.name,
    source: "default",
    confidence: profile.confidence,
    latency_policy: profile.latency_policy,
    routing_policy: profile.routing_policy,
    bluetooth_mic_policy: profile.bluetooth_mic_policy,
  };
}

function isHardwareProfileDevice(device: DeviceInfo): boolean {
  if (device.is_virtual || device.bus === "virtual") return false;
  const text = [device.id, device.name, device.description].join(" ").toLowerCase();
  if (text.includes("wavelinux")) return false;
  if (text.includes(".monitor") || text.includes("monitor of")) return false;
  return true;
}

function primaryBusMixes(mixes: Mix[]): Mix[] {
  const monitor = mixes.find((mix) => mix.id === "monitor");
  const stream = mixes.find((mix) => mix.id === "stream");
  const primary = [monitor, stream].filter(Boolean) as Mix[];
  return primary.length === 2 ? primary : mixes.slice(0, 2);
}

function compactMixLabel(mix: Mix): string {
  if (mix.id === "monitor") return "MON";
  if (mix.id === "stream") return "STR";
  return mix.name.slice(0, 3).toUpperCase();
}

function matcherForStream(stream: AppStream): AppMatcher {
  const keepMediaName = shouldKeepStreamMediaName(stream);
  const matcher = {
    app_id: stream.app_id ?? null,
    binary: stream.binary ?? stream.process_name ?? null,
    process_name: stream.process_name ?? null,
    window_class: stream.window_class ?? null,
    media_name: keepMediaName ? (stream.media_name ?? null) : null,
  };
  if (!matcherIsEmpty(matcher)) return matcher;

  return {
    app_id: fallbackMatcherValueForStream(stream),
    binary: null,
    process_name: null,
    window_class: null,
  };
}

function shouldKeepStreamMediaName(stream: AppStream): boolean {
  const mediaName = stream.media_name?.trim();
  if (!mediaName || isGenericMediaName(mediaName)) return false;

  const identityValues = [
    stream.app_id,
    stream.binary,
    stream.process_name,
    stream.window_class,
  ]
    .map((value) => value?.trim().toLowerCase())
    .filter((value): value is string => Boolean(value));

  if (identityValues.length === 0) return true;

  const wrapperNeedles = ["ferdium", "electron", "chromium", "chrome", "brave", "vivaldi", "webapp", "web-app"];
  return identityValues.some((value) => wrapperNeedles.some((needle) => value.includes(needle)));
}

function isGenericMediaName(value: string): boolean {
  return ["audio-src", "audio src", "audio", "playback", "output", "input"].includes(value.trim().toLowerCase());
}

function matcherIsEmpty(matcher: AppMatcher): boolean {
  return matcherKinds.every((kind) => !matcher[kind]?.trim());
}

function fallbackMatcherValueForStream(stream: AppStream): string {
  const candidates = [
    stream.display_name && !/^Stream\s+\d+$/i.test(stream.display_name) ? stream.display_name : null,
    stream.media_name && !isGenericMediaName(stream.media_name) ? stream.media_name : null,
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

function normalizedMatcherValue(value: string): string {
  return value.trim().toLowerCase();
}

function matchersOverlap(left: AppMatcher, right: AppMatcher): boolean {
  if (routeKey(left) === routeKey(right)) return true;
  const rightEntries = new Map(
    matcherEntries(right).map(([kind, value]) => [kind, normalizedMatcherValue(value)]),
  );
  return matcherEntries(left).some(([kind, value]) => {
    const rightValue = rightEntries.get(kind);
    return Boolean(rightValue && rightValue === normalizedMatcherValue(value));
  });
}

function activeMatchersForState(state: AppStateSnapshot): AppMatcher[] {
  return state.graph.app_streams.map((stream) => matcherForStream(stream));
}

function matcherIsActive(matcher: AppMatcher, activeMatchers: AppMatcher[]): boolean {
  return activeMatchers.some((activeMatcher) => matchersOverlap(activeMatcher, matcher));
}

function volumePresetForMatcher(
  presets: AppVolumePreset[] | undefined,
  matcher: AppMatcher,
): AppVolumePreset | undefined {
  const key = routeKey(matcher);
  return presets?.find((preset) => routeKey(preset.matcher) === key);
}

function mergeTargetsForState(state: AppStateSnapshot, source: AppMatcher): MergeTarget[] {
  const sourceKey = routeKey(source);
  const targets = new Map<string, MergeTarget>();

  for (const stream of state.graph.app_streams) {
    const matcher = matcherForStream(stream);
    const key = routeKey(matcher);
    if (key === sourceKey || !isMergeableAppTarget(stream.display_name, matcher)) continue;
    targets.set(key, {
      matcher,
      displayName: stream.display_name || matcherLabel(matcher),
      meta: "Active app",
    });
  }

  for (const entry of offlineRoutingEntries(state)) {
    const key = routeKey(entry.matcher);
    if (key === sourceKey || targets.has(key) || !isMergeableAppTarget(entry.displayName, entry.matcher)) {
      continue;
    }
    targets.set(key, {
      matcher: entry.matcher,
      displayName: entry.displayName,
      meta: entry.meta || "Offline app",
    });
  }

  return [...targets.values()].sort((left, right) => left.displayName.localeCompare(right.displayName));
}

function isMergeableAppTarget(displayName: string, matcher: AppMatcher): boolean {
  if (routeKey(matcher) === "empty") return false;
  const haystack = [displayName, ...matcherEntries(matcher).map(([, value]) => value)]
    .join("\n")
    .toLowerCase();
  const blocked = [
    "wavelinux",
    "pipewire",
    "wireplumber",
    "libcanberra",
    "pw-play",
    "pw-cat",
    "paplay",
    "wavelinux-route-test",
  ];
  return !blocked.some((needle) => haystack.includes(needle));
}

function offlineRoutingEntries(state: AppStateSnapshot): OfflineRoutingEntry[] {
  const entries = new Map<string, OfflineRoutingEntry>();
  const activeMatchers = activeMatchersForState(state);
  for (const app of state.config.app_history ?? []) {
    if (app.forgotten) continue;
    if (matcherIsActive(app.matcher, activeMatchers)) continue;
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
    if (matcherIsActive(route.matcher, activeMatchers)) continue;
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

function isHardwareChannel(channel: Pick<Channel, "kind">): boolean {
  return channel.kind === "microphone" || channel.kind === "generic";
}

function channelDisplayName(channel: Pick<Channel, "id" | "kind" | "name">): string {
  if (
    channel.id === "hardware_in" &&
    isHardwareChannel(channel) &&
    ["hardware in", "hardware input", "input"].includes(channel.name.trim().toLowerCase())
  ) {
    return "Input";
  }
  return channel.name;
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
    device.is_available !== false &&
    !isWaveLinuxManagedDevice(device) &&
    !name.endsWith(".monitor") &&
    !description.startsWith("monitor of ") &&
    !description.includes(" monitor")
  );
}

function autoMicrophoneLabel(
  inputs: AppStateSnapshot["graph"]["inputs"],
  fallback: string,
): string {
  const input = inputs[0];
  return input ? `Auto: ${input.description}` : fallback;
}

function sortedMicrophoneInputs(
  inputs: AppStateSnapshot["graph"]["inputs"],
): AppStateSnapshot["graph"]["inputs"] {
  return inputs
    .filter(isMicrophoneSource)
    .slice()
    .sort((left, right) => {
      const priority = microphoneInputPriority(right) - microphoneInputPriority(left);
      if (priority !== 0) return priority;
      if (left.is_default !== right.is_default) return left.is_default ? -1 : 1;
      return left.description.localeCompare(right.description);
    });
}

function microphoneInputPriority(device: AppStateSnapshot["graph"]["inputs"][number]): number {
  const text = `${device.id} ${device.name} ${device.description}`.toLowerCase();
  if (text.includes("usb")) return 60;
  if (text.includes("bluez") || text.includes("bluetooth")) return 30;
  if (
    text.includes("jack") ||
    text.includes("headset") ||
    text.includes("headphone") ||
    text.includes("linein") ||
    text.includes("line-in") ||
    text.includes("front mic") ||
    text.includes("rear mic")
  ) {
    return 50;
  }
  if (
    text.includes("built-in") ||
    text.includes("built in") ||
    text.includes("internal") ||
    text.includes("digital microphone") ||
    text.includes("dmic") ||
    text.includes("hda") ||
    text.includes("pci")
  ) {
    return 40;
  }
  if (text.includes("mic") || text.includes("microphone") || text.includes("analog")) return 35;
  return 1;
}

function channelInputLabel(
  channel: Pick<Channel, "source_device">,
  inputs: AppStateSnapshot["graph"]["inputs"],
): string {
  if (!channel.source_device) return "Auto input";
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
  if (peak < 0.01) return 0;
  return Math.max(0, Math.min(1, peak));
}

function meterLevelMap(meters: LevelMeter[]): Record<string, number> {
  const levels: Record<string, number> = {};
  for (const meter of meters) {
    levels[meter.node_id] = levelFromMeter(meter);
  }
  return levels;
}

function useSmoothMeterLevels(rawLevels: Record<string, number>, graphRunning: boolean): Record<string, number> {
  const rawRef = useRef(rawLevels);
  const smoothRef = useRef<Record<string, number>>({});
  const displayRef = useRef<Record<string, number>>({});
  const [displayLevels, setDisplayLevels] = useState<Record<string, number>>({});

  useEffect(() => {
    rawRef.current = rawLevels;
  }, [rawLevels]);

  useEffect(() => {
    let frame = 0;
    let lastTick = performance.now();

    const tick = (now: number) => {
      const elapsedSeconds = Math.min(0.25, Math.max(0.001, (now - lastTick) / 1000));
      lastTick = now;
      const raw = graphRunning ? rawRef.current : {};
      const smooth = smoothRef.current;
      const nextDisplay: Record<string, number> = {};
      const keys = new Set([...Object.keys(smooth), ...Object.keys(raw)]);

      for (const key of keys) {
        const incoming = raw[key] ?? 0;
        const current = smooth[key] ?? 0;
        const timeConstant = incoming > current ? UI_METER_ATTACK_SECONDS : UI_METER_RELEASE_SECONDS;
        const blend = 1 - Math.exp(-elapsedSeconds / timeConstant);
        const nextLevel = current + (incoming - current) * blend;

        if (nextLevel < UI_METER_FLOOR && incoming < UI_METER_FLOOR) {
          delete smooth[key];
          continue;
        }

        const clamped = Math.max(0, Math.min(1, nextLevel));
        smooth[key] = clamped;
        nextDisplay[key] = clamped;
      }

      if (!meterLevelMapsEqual(displayRef.current, nextDisplay)) {
        displayRef.current = nextDisplay;
        setDisplayLevels(nextDisplay);
      }
      frame = window.requestAnimationFrame(tick);
    };

    frame = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(frame);
  }, [graphRunning]);

  return displayLevels;
}

function meterLevelMapsEqual(left: Record<string, number>, right: Record<string, number>): boolean {
  const leftKeys = Object.keys(left);
  const rightKeys = Object.keys(right);
  if (leftKeys.length !== rightKeys.length) return false;
  return leftKeys.every((key) => Math.abs((left[key] ?? 0) - (right[key] ?? 0)) < 0.001);
}

function channelBusMeterId(channelId: string, mixId: string): string {
  return `channel:${channelId}:mix:${mixId}`;
}

function channelBusVuLevel(
  channel: Channel,
  mix: Mix,
  _bus: MixBus,
  levelFor: (nodeId: string) => number,
): number {
  return levelFor(channelBusMeterId(channel.id, mix.id));
}

function normalizeSourceEffects(effects: EffectInstance[], preferredInstanceId?: string): EffectInstance[] {
  const singleInstanceIndexes = new Map<string, number[]>();
  for (const [index, effect] of effects.entries()) {
    if (!isSingleInstanceEffect(effect.effect_id)) continue;
    const indexes = singleInstanceIndexes.get(effect.effect_id) ?? [];
    indexes.push(index);
    singleInstanceIndexes.set(effect.effect_id, indexes);
  }

  if (singleInstanceIndexes.size === 0) {
    return structuredClone(effects);
  }

  const keepIndexes = new Set<number>();
  for (const indexes of singleInstanceIndexes.values()) {
    const preferred = indexes.find((index) => effects[index]?.instance_id === preferredInstanceId);
    const active = [...indexes].reverse().find((index) => effects[index] && !effects[index].bypassed);
    const keepIndex = preferred ?? active ?? indexes.at(-1);
    if (keepIndex !== undefined) keepIndexes.add(keepIndex);
  }

  return effects
    .filter((effect, index) => !isSingleInstanceEffect(effect.effect_id) || keepIndexes.has(index))
    .map((effect) => structuredClone(effect));
}

function isSingleInstanceEffect(effectId: string): boolean {
  return singleInstanceEffectIds.has(effectId);
}

function effectChainsEqual(left: EffectInstance[], right: EffectInstance[]): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
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

function appVolumePercent(value: number): number {
  return Math.max(1, Math.min(100, Math.round(value)));
}

function appVolumeToPercent(volume: number): number {
  return appVolumePercent(volume * 100);
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
