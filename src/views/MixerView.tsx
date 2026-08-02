import {
  ArrowDown,
  ArrowUp,
  AudioLines,
  Cable,
  Chrome,
  CircleMinus,
  CirclePlus,
  Clapperboard,
  ExternalLink,
  Gamepad2,
  Gauge,
  GitBranch,
  Headphones,
  Maximize2,
  MessageCircle,
  Mic,
  Minimize2,
  Monitor,
  MonitorSpeaker,
  Music2,
  Pencil,
  Radio,
  SlidersHorizontal,
  Sparkles,
  Trash2,
  Volume2,
  VolumeX,
  X,
} from "lucide-react";
import { createPortal } from "react-dom";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  KeyboardEvent as ReactKeyboardEvent,
  MouseEvent as ReactMouseEvent,
  PointerEvent as ReactPointerEvent,
} from "react";
import {
  autoMicrophoneLabel,
  channelDisplayName,
  isHardwareChannel,
  resolvedAutoInput,
  sortedMicrophoneInputs,
} from "../audio-ui";
import { AppSelect } from "../components/AppSelect";
import { EmptyState, shouldCommitSliderKey } from "../components/Controls";
import { EffectStateButton } from "../components/EffectStateButton";
import { effectRuntimeForChannel } from "../effect-runtime";
import { defaultMixBus, mixOutputDevices } from "../mixer-ui";
import {
  matcherForStream,
  offlineRoutingEntries,
  routeKey,
  type OfflineRoutingEntry,
} from "../routing";
import { useWaveLinuxMetersAvailable } from "../state";
import { invoke } from "../tauri";
import type {
  AppStateSnapshot,
  AppStream,
  Channel,
  ChannelKind,
  DeviceInfo,
  EffectRuntimeStatus,
  Mix,
  MixBus,
  MixerSettings,
} from "../types";
import {
  appVolumePercent,
  appVolumeToPercent,
  sliderPercent,
  thumbPosition,
  volumeToPercent,
} from "../volume";
import { OfflineVolumeControl } from "./RoutingView";
import {
  WaveLinkEffectsEditor,
  type SetEffectChain,
} from "./EffectsView";

type View = "mixer" | "routing" | "effects" | "settings";
const MAX_SOFTWARE_CHANNELS = 8;
const MAX_MIXES = 5;
const AUTO_MONITOR_OUTPUT_VALUE = "__auto_monitor_output__";
const CLEAR_MIX_OUTPUTS_VALUE = "__clear_mix_outputs__";
const MIX_TEMPLATE_NAMES = ["Personal", "Chat", "Stream"];

type IconOption = {
  id: string;
  label: string;
  icon: typeof SlidersHorizontal;
};

const MIX_ICON_OPTIONS: IconOption[] = [
  { id: "headphones", label: "Personal", icon: Headphones },
  { id: "radio", label: "Stream", icon: Radio },
  { id: "chat", label: "Chat", icon: Cable },
  { id: "music", label: "Music", icon: Music2 },
  { id: "monitor", label: "Monitor", icon: MonitorSpeaker },
  { id: "mic", label: "Mic", icon: Mic },
  { id: "sparkles", label: "FX", icon: Sparkles },
  { id: "audio", label: "Audio", icon: AudioLines },
];

const SOURCE_ICON_OPTIONS: IconOption[] = [
  { id: "mic", label: "Microphone", icon: Mic },
  { id: "system", label: "System", icon: Monitor },
  { id: "game", label: "Game", icon: Gamepad2 },
  { id: "chat", label: "Chat", icon: MessageCircle },
  { id: "music", label: "Music", icon: Music2 },
  { id: "browser", label: "Browser", icon: Chrome },
  { id: "sfx", label: "SFX", icon: Sparkles },
  { id: "media", label: "Media", icon: Clapperboard },
  { id: "headphones", label: "Monitor", icon: Headphones },
  { id: "audio", label: "Audio", icon: AudioLines },
];

type AutoDevices = AppStateSnapshot["graph"]["auto_devices"];

type SourceCandidate = {
  id: string;
  label: string;
  meta: string;
  kind: ChannelKind;
  sourceDevice?: string;
  streamId?: string;
};

type MixerDrawer =
  | { type: "routing" }
  | { type: "effects"; channelId: string }
  | { type: "mix"; mixId: string }
  | { type: "source"; channelId: string };

export function MixerView({
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
  const metersAvailable = useWaveLinuxMetersAvailable();
  const metersUnavailable =
    state.engine.audio_graph_running &&
    !state.engine.dry_run &&
    !metersAvailable;

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
                  autoDevices={state.graph.auto_devices}
                  channel={channel}
                  key={channel.id}
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

export function WaveLinkMixerView({
  busy,
  run,
  selectedChannelId,
  setActiveView,
  setAppStreamMute,
  setChannelBusEnabled,
  setChannelBusMute,
  setChannelBusVolume,
  setChannelEffectsEnabled,
  setChannelInput,
  setMixIcon,
  setMixOutputs,
  setMixMute,
  setMixVolume,
  setEffectChain,
  setChannelIcon,
  setSelectedChannelId,
  setSettings,
  state,
}: {
  busy: boolean;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  selectedChannelId: string;
  setActiveView: (view: View) => void;
  setAppStreamMute: (streamId: string, muted: boolean) => Promise<void>;
  setChannelBusEnabled: (channelId: string, mixId: string, enabled: boolean) => Promise<void>;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
  setChannelEffectsEnabled: (channelId: string, enabled: boolean) => Promise<void>;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  setMixIcon: (mixId: string, icon: string | null) => Promise<void>;
  setMixOutputs: (mixId: string, outputs: string[]) => Promise<void>;
  setMixMute: (mixId: string, muted: boolean) => Promise<void>;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
  setEffectChain: SetEffectChain;
  setChannelIcon: (channelId: string, icon: string | null) => Promise<void>;
  setSelectedChannelId: (channelId: string) => void;
  setSettings: (settings: MixerSettings) => Promise<void>;
  state: AppStateSnapshot;
}) {
  const outputs = state.graph.outputs.filter((output) => !output.is_virtual);
  const microphoneInputs = useMemo(() => sortedMicrophoneInputs(state.graph.inputs), [state.graph.inputs]);
  const softwareChannelCount = state.config.channels.filter((channel) => !isHardwareChannel(channel)).length;
  const [sourceCreatorOpen, setSourceCreatorOpen] = useState(false);
  const [mixCreatorOpen, setMixCreatorOpen] = useState(false);
  const mixerDensityTouched = useRef(false);
  const drawerLayerRef = useRef<HTMLDivElement>(null);
  const [matrixCollapsed, setMatrixCollapsed] = useState(prefersCompactWaveLinkMixer);
  const [drawer, setDrawer] = useState<MixerDrawer | null>(null);

  useEffect(() => {
    const syncDensity = () => {
      if (!mixerDensityTouched.current) {
        setMatrixCollapsed(prefersCompactWaveLinkMixer());
      }
    };
    window.addEventListener("resize", syncDensity);
    syncDensity();
    return () => window.removeEventListener("resize", syncDensity);
  }, []);

  const streamsByChannelId = useMemo(() => {
    const groups = new Map<string, AppStream[]>();
    for (const stream of state.graph.app_streams) {
      if (!stream.routed_channel_id) continue;
      const current = groups.get(stream.routed_channel_id) ?? [];
      current.push(stream);
      groups.set(stream.routed_channel_id, current);
    }
    return groups;
  }, [state.graph.app_streams]);
  const offlineEntries = useMemo(() => offlineRoutingEntries(state), [state]);

  const selectedChannel = state.config.channels.find((channel) => channel.id === selectedChannelId);
  const drawerOpen = drawer !== null;
  const selectMixerChannel = useCallback((channelId: string) => {
    setSelectedChannelId(channelId);
    setDrawer((current) => {
      if (current?.type === "effects") return { type: "effects", channelId };
      if (current?.type === "source") return { type: "source", channelId };
      return current;
    });
  }, [setSelectedChannelId]);

  useEffect(() => {
    if (!drawer) return;
    if (drawer.type === "mix" && !state.config.mixes.some((mix) => mix.id === drawer.mixId)) {
      setDrawer(null);
    }
    if (
      (drawer.type === "source" || drawer.type === "effects") &&
      !state.config.channels.some((channel) => channel.id === drawer.channelId)
    ) {
      setDrawer(null);
    }
  }, [drawer, state.config.channels, state.config.mixes]);

  useEffect(() => {
    if (!drawerOpen) return;
    const previouslyFocused = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    const layer = drawerLayerRef.current;
    const focusableSelector = [
      "button:not([disabled]):not([tabindex='-1'])",
      "input:not([disabled])",
      "select:not([disabled])",
      "textarea:not([disabled])",
      "[href]",
      "[tabindex]:not([tabindex='-1'])",
    ].join(",");
    const focusFirst = () => layer?.querySelector<HTMLElement>(focusableSelector)?.focus();
    const frame = window.requestAnimationFrame(focusFirst);
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setDrawer(null);
        return;
      }
      if (event.key !== "Tab" || !layer) return;
      const focusable = Array.from(layer.querySelectorAll<HTMLElement>(focusableSelector))
        .filter((element) => element.offsetParent !== null);
      if (focusable.length === 0) {
        event.preventDefault();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      window.cancelAnimationFrame(frame);
      document.removeEventListener("keydown", handleKeyDown);
      previouslyFocused?.focus();
    };
  }, [drawerOpen]);

  return (
    <section className={matrixCollapsed ? "wl-mixer compact" : "wl-mixer"}>
      <div className="wl-mixer-commandbar">
        <div>
          <strong>Matrix Mixer</strong>
          <span>
            {state.config.channels.length} sources · {state.config.mixes.length} mixes · {state.config.audio.sample_rate_hz / 1000} kHz
          </span>
        </div>
        <div className="wl-mixer-actions">
          <button
            className="secondary-button"
            disabled={softwareChannelCount >= MAX_SOFTWARE_CHANNELS || busy}
            onClick={() => setSourceCreatorOpen(true)}
            title={`${softwareChannelCount}/${MAX_SOFTWARE_CHANNELS} source fader routes`}
            type="button"
          >
            <CirclePlus size={16} />
            Source
          </button>
          <button
            className="secondary-button"
            onClick={() => {
              mixerDensityTouched.current = true;
              setMatrixCollapsed((current) => !current);
            }}
            title={matrixCollapsed ? "Expand mixes view" : "Shrink mixes view"}
            type="button"
          >
            {matrixCollapsed ? <Maximize2 size={16} /> : <Minimize2 size={16} />}
            {matrixCollapsed ? "Expand" : "Shrink"}
          </button>
          <button
            className="secondary-button"
            disabled={state.config.mixes.length >= MAX_MIXES || busy}
            onClick={() => setMixCreatorOpen(true)}
            title={`${state.config.mixes.length}/${MAX_MIXES} virtual mixes`}
            type="button"
          >
            <CirclePlus size={16} />
            Mix
          </button>
          <button
            aria-pressed={drawer?.type === "routing"}
            className={drawer?.type === "routing" ? "secondary-button active" : "secondary-button"}
            onClick={() => setDrawer((current) => current?.type === "routing" ? null : { type: "routing" })}
            title={drawer?.type === "routing" ? "Hide app routing drawer" : "Show app routing drawer"}
            type="button"
          >
            <Cable size={16} />
            Apps
          </button>
          <button
            aria-pressed={drawer?.type === "effects" && drawer.channelId === selectedChannel?.id}
            className={drawer?.type === "effects" && drawer.channelId === selectedChannel?.id ? "secondary-button active" : "secondary-button"}
            disabled={!selectedChannel}
            onClick={() => {
              if (!selectedChannel) return;
              setSelectedChannelId(selectedChannel.id);
              setDrawer((current) =>
                current?.type === "effects" && current.channelId === selectedChannel.id
                  ? null
                  : { type: "effects", channelId: selectedChannel.id },
              );
            }}
            type="button"
          >
            <Sparkles size={16} />
            FX
          </button>
        </div>
      </div>

      <div className={drawerOpen ? "wl-mixer-grid drawer-open" : "wl-mixer-grid"}>
        <div className="wl-matrix-panel">
          <div className="wl-matrix-scroll">
            <div
              className="wl-matrix"
              style={{
                gridTemplateColumns: `minmax(220px, 250px) repeat(${state.config.mixes.length}, minmax(176px, 1fr))`,
              }}
            >
              <div className="wl-matrix-corner">
                <strong>Inputs</strong>
                <span>Route each source into every output mix</span>
              </div>
              {state.config.mixes.map((mix) => (
                <WaveLinkMixHeader
                  autoDevices={state.graph.auto_devices}
                  key={mix.id}
                  mix={mix}
                  outputs={outputs}
                  onOpenSettings={() => setDrawer({ type: "mix", mixId: mix.id })}
                  setMixMute={setMixMute}
                  setMixVolume={setMixVolume}
                  settings={state.config.settings}
                />
              ))}

              {state.config.channels.map((channel) => (
                <WaveLinkSourceRow
                  channel={channel}
                  appStreams={streamsByChannelId.get(channel.id) ?? []}
                  autoDevices={state.graph.auto_devices}
                  effectRuntime={effectRuntimeForChannel(state.engine.effects, channel.id)}
                  isSelected={channel.id === selectedChannelId}
                  key={channel.id}
                  microphoneInputs={microphoneInputs}
                  mixes={state.config.mixes}
                  onOpenSettings={() => setDrawer({ type: "source", channelId: channel.id })}
                  openEffects={() => {
                    setSelectedChannelId(channel.id);
                    setDrawer({ type: "effects", channelId: channel.id });
                  }}
                  setChannelBusMute={setChannelBusMute}
                  setChannelBusEnabled={setChannelBusEnabled}
                  setChannelBusVolume={setChannelBusVolume}
                  setChannelEffectsEnabled={setChannelEffectsEnabled}
                  setSelectedChannelId={selectMixerChannel}
                />
              ))}
            </div>
          </div>
        </div>

        {drawer && (
          <div
            aria-label="Mixer controls"
            aria-modal="true"
            className="wl-drawer-layer"
            ref={drawerLayerRef}
            role="dialog"
          >
          <button
            aria-label="Close mixer drawer"
            className="wl-drawer-scrim"
            onClick={() => setDrawer(null)}
            tabIndex={-1}
            type="button"
          />
          {drawer.type === "routing" && (
          <aside className="wl-routing-drawer">
            <div className="wl-drawer-header">
              <div>
                <strong>App Routing</strong>
                <span>{state.graph.app_streams.length} active streams</span>
              </div>
              <div className="wl-inline-actions">
                <button className="mini-icon-button" onClick={() => setActiveView("routing")} title="Open routing" type="button">
                  <ExternalLink size={14} />
                </button>
                <button className="mini-icon-button" onClick={() => setDrawer(null)} title="Close app routing" type="button">
                  <X size={14} />
                </button>
              </div>
            </div>
            <div className="wl-app-route-list">
              <div className="wl-drawer-section-title">
                <span>Active Apps</span>
                <strong>{state.graph.app_streams.length}</strong>
              </div>
              {state.graph.app_streams.map((stream) => (
                <WaveLinkAppRouteCard
                  channels={state.config.channels}
                  key={stream.id}
                  run={run}
                  setAppStreamMute={setAppStreamMute}
                  stream={stream}
                />
              ))}
              {state.graph.app_streams.length === 0 && <EmptyState label="No active app streams" />}
              <div className="wl-drawer-section-title">
                <span>Saved Rules</span>
                <strong>{offlineEntries.length}</strong>
              </div>
              {offlineEntries.slice(0, 5).map((entry) => (
                <WaveLinkOfflineRuleCard
                  channels={state.config.channels}
                  entry={entry}
                  key={routeKey(entry.matcher)}
                  run={run}
                />
              ))}
              {offlineEntries.length === 0 && <EmptyState label="No saved routing rules" />}
              {offlineEntries.length > 5 && (
                <button className="secondary-button" onClick={() => setActiveView("routing")} type="button">
                  <ExternalLink size={16} />
                  More Rules
                </button>
              )}
            </div>
          </aside>
          )}
          {drawer.type === "effects" && (() => {
            const channel = state.config.channels.find((item) => item.id === drawer.channelId);
            if (!channel) return null;
            return (
              <aside className="wl-routing-drawer wl-effects-drawer">
                <div className="wl-drawer-header">
                  <div>
                    <strong>FX</strong>
                    <span>{channelDisplayName(channel)}</span>
                  </div>
                  <div className="wl-inline-actions">
                    <button className="mini-icon-button" onClick={() => setActiveView("effects")} title="Open effects workspace" type="button">
                      <ExternalLink size={14} />
                    </button>
                    <button className="mini-icon-button" onClick={() => setDrawer(null)} title="Close FX" type="button">
                      <X size={14} />
                    </button>
                  </div>
                </div>
                <div className="wl-effects-drawer-scroll">
                  <WaveLinkEffectsEditor
                    channel={channel}
                    setChannelInput={setChannelInput}
                    setEffectChain={setEffectChain}
                    state={state}
                  />
                </div>
              </aside>
            );
          })()}
          {drawer.type === "mix" && (() => {
            const mixIndex = state.config.mixes.findIndex((item) => item.id === drawer.mixId);
            const mix = state.config.mixes[mixIndex];
            if (!mix) return null;
            return (
              <WaveLinkMixSettingsDrawer
                autoDevices={state.graph.auto_devices}
                canDelete={state.config.mixes.length > 1}
                canMoveDown={mixIndex < state.config.mixes.length - 1}
                canMoveUp={mixIndex > 0}
                mix={mix}
                onClose={() => setDrawer(null)}
                outputs={outputs}
                run={run}
                setMixIcon={setMixIcon}
                setMixMute={setMixMute}
                setMixOutputs={setMixOutputs}
                setMixVolume={setMixVolume}
                setSettings={setSettings}
                settings={state.config.settings}
              />
            );
          })()}
          {drawer.type === "source" && (() => {
            const channelIndex = state.config.channels.findIndex((item) => item.id === drawer.channelId);
            const channel = state.config.channels[channelIndex];
            if (!channel) return null;
            return (
              <WaveLinkSourceSettingsDrawer
                autoDevices={state.graph.auto_devices}
                canMoveDown={channelIndex < state.config.channels.length - 1}
                canMoveUp={channelIndex > 0}
                channel={channel}
                microphoneInputs={microphoneInputs}
                onClose={() => setDrawer(null)}
                run={run}
                setChannelInput={setChannelInput}
                setChannelIcon={setChannelIcon}
              />
            );
          })()}
          </div>
        )}
      </div>
      {sourceCreatorOpen && (
        <WaveLinkCreateSourceDialog
          appStreams={state.graph.app_streams}
          microphoneInputs={microphoneInputs}
          onClose={() => setSourceCreatorOpen(false)}
          run={run}
          setSelectedChannelId={setSelectedChannelId}
        />
      )}
      {mixCreatorOpen && (
        <WaveLinkCreateMixDialog
          onClose={() => setMixCreatorOpen(false)}
          run={run}
          setMixIcon={setMixIcon}
        />
      )}
    </section>
  );
}

function WaveLinkCreateSourceDialog({
  appStreams,
  microphoneInputs,
  onClose,
  run,
  setSelectedChannelId,
}: {
  appStreams: AppStream[];
  microphoneInputs: AppStateSnapshot["graph"]["inputs"];
  onClose: () => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setSelectedChannelId: (channelId: string) => void;
}) {
  const [name, setName] = useState("Podcast");
  const [kind, setKind] = useState<ChannelKind>("application");
  const [sourceDevice, setSourceDevice] = useState("");
  const [streamId, setStreamId] = useState("");
  const [selectedCandidateId, setSelectedCandidateId] = useState("virtual");
  const [busy, setBusy] = useState(false);
  const isHardware = kind === "microphone" || kind === "generic";
  const candidates = useMemo<SourceCandidate[]>(() => [
    ...microphoneInputs.map((input) => ({
      id: `input:${input.id}`,
      label: input.description,
      meta: input.bus ? `${input.bus} input` : "Hardware input",
      kind: "microphone" as ChannelKind,
      sourceDevice: input.id,
    })),
    ...appStreams.map((stream) => ({
      id: `app:${stream.id}`,
      label: stream.display_name || stream.process_name || stream.binary || stream.id,
      meta: stream.media_name || stream.app_id || stream.process_name || "Active app",
      kind: "application" as ChannelKind,
      streamId: stream.id,
    })),
    {
      id: "virtual",
      label: "Virtual Channel",
      meta: "Appears as an app output route",
      kind: "application" as ChannelKind,
    },
    {
      id: "system",
      label: "System",
      meta: "Desktop audio channel",
      kind: "system" as ChannelKind,
    },
    {
      id: "sfx",
      label: "Soundboard / SFX",
      meta: "Sound effects channel",
      kind: "soundboard" as ChannelKind,
    },
  ], [appStreams, microphoneInputs]);

  const selectCandidate = useCallback((candidate: SourceCandidate) => {
    setSelectedCandidateId(candidate.id);
    setKind(candidate.kind);
    setSourceDevice(candidate.sourceDevice ?? "");
    setStreamId(candidate.streamId ?? "");
    setName(candidate.label);
  }, []);

  const body = (
    <div className="wl-modal-backdrop" onMouseDown={onClose}>
      <form
        className="wl-dialog"
        onMouseDown={(event) => event.stopPropagation()}
        onSubmit={(event) => {
          event.preventDefault();
          const cleanName = name.trim();
          if (!cleanName || busy) return;
          setBusy(true);
          void (async () => {
            const channel = await run<Channel>("create_channel", { name: cleanName, kind }, "Source added");
            setSelectedChannelId(channel.id);
            if (isHardware && sourceDevice) {
              await run<Channel>(
                "set_channel_input",
                { channelId: channel.id, sourceDevice },
              );
            }
            const stream = appStreams.find((item) => item.id === streamId);
            if (stream) {
              await run("move_app_stream", { streamId: stream.id, channelId: channel.id });
              await run("assign_app_to_channel", {
                channelId: channel.id,
                matcher: matcherForStream(stream),
              });
            }
            onClose();
          })()
            .catch(() => undefined)
            .finally(() => setBusy(false));
        }}
      >
        <div className="wl-dialog-header">
          <strong>New Source</strong>
          <button className="mini-icon-button" onClick={onClose} type="button">x</button>
        </div>
        <div className="wl-source-candidate-list" role="listbox" aria-label="Source type">
          {candidates.map((candidate, index) => {
            const showHeader =
              index === 0 ||
              (candidate.id.startsWith("app:") && !candidates[index - 1]?.id.startsWith("app:")) ||
              (!candidate.id.startsWith("input:") &&
                !candidate.id.startsWith("app:") &&
                (candidates[index - 1]?.id.startsWith("input:") || candidates[index - 1]?.id.startsWith("app:")));
            const header = candidate.id.startsWith("input:")
              ? "Input Devices"
              : candidate.id.startsWith("app:")
                ? "Apps"
                : "Channels";
            return (
              <div className="wl-source-candidate-group" key={candidate.id}>
                {showHeader && <span>{header}</span>}
                <button
                  aria-selected={selectedCandidateId === candidate.id}
                  className={selectedCandidateId === candidate.id ? "active" : ""}
                  onClick={() => selectCandidate(candidate)}
                  role="option"
                  type="button"
                >
                  <strong>{candidate.label}</strong>
                  <small>{candidate.meta}</small>
                </button>
              </div>
            );
          })}
        </div>
        <label className="wl-dialog-field">
          <span>Name</span>
          <input autoFocus value={name} onChange={(event) => setName(event.currentTarget.value)} />
        </label>
        {isHardware && (
          <AppSelect
            ariaLabel="Hardware input"
            onChange={setSourceDevice}
            options={[
              { value: "", label: autoMicrophoneLabel(microphoneInputs, "Auto mic") },
              ...microphoneInputs.map((input) => ({ value: input.id, label: input.description })),
            ]}
            value={sourceDevice}
          />
        )}
        {kind === "application" && appStreams.length > 0 && selectedCandidateId !== "virtual" && (
          <AppSelect
            ariaLabel="Active app"
            onChange={setStreamId}
            options={[
              { value: "", label: "No active app" },
              ...appStreams.map((stream) => ({
                value: stream.id,
                label: stream.display_name || stream.process_name || stream.binary || stream.id,
              })),
            ]}
            value={streamId}
          />
        )}
        <div className="wl-dialog-actions">
          <button className="secondary-button" onClick={onClose} type="button">Cancel</button>
          <button className="primary-button" disabled={busy || !name.trim()} type="submit">
            <CirclePlus size={16} />
            Add Source
          </button>
        </div>
      </form>
    </div>
  );
  return createPortal(body, document.body);
}

function WaveLinkCreateMixDialog({
  onClose,
  run,
  setMixIcon,
}: {
  onClose: () => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setMixIcon: (mixId: string, icon: string | null) => Promise<void>;
}) {
  const [name, setName] = useState("Podcast");
  const [icon, setIcon] = useState("headphones");
  const [busy, setBusy] = useState(false);
  const body = (
    <div className="wl-modal-backdrop" onMouseDown={onClose}>
      <form
        className="wl-dialog"
        onMouseDown={(event) => event.stopPropagation()}
        onSubmit={(event) => {
          event.preventDefault();
          const cleanName = name.trim();
          if (!cleanName || busy) return;
          setBusy(true);
          void (async () => {
            const mix = await run<Mix>("create_mix", { name: cleanName }, "Mix added");
            await setMixIcon(mix.id, icon);
            onClose();
          })()
            .catch(() => undefined)
            .finally(() => setBusy(false));
        }}
      >
        <div className="wl-dialog-header">
          <strong>New Mix</strong>
          <button className="mini-icon-button" onClick={onClose} type="button">x</button>
        </div>
        <div className="wl-template-grid" aria-label="Mix templates">
          {MIX_TEMPLATE_NAMES.map((templateName) => (
            <button
              className={name === templateName ? "active" : ""}
              key={templateName}
              onClick={() => {
                setName(templateName);
                setIcon(defaultMixIconForName(templateName));
              }}
              type="button"
            >
              {templateName}
            </button>
          ))}
        </div>
        <WaveLinkMixIconPicker
          mixId="new"
          selectedIcon={icon}
          setMixIcon={(_mixId, nextIcon) => {
            setIcon(nextIcon ?? "audio");
            return Promise.resolve();
          }}
        />
        <label className="wl-dialog-field">
          <span>Name</span>
          <input autoFocus value={name} onChange={(event) => setName(event.currentTarget.value)} />
        </label>
        <div className="wl-dialog-actions">
          <button className="secondary-button" onClick={onClose} type="button">Cancel</button>
          <button className="primary-button" disabled={busy || !name.trim()} type="submit">
            <CirclePlus size={16} />
            Add Mix
          </button>
        </div>
      </form>
    </div>
  );
  return createPortal(body, document.body);
}

function WaveLinkMixHeader({
  autoDevices,
  mix,
  onOpenSettings,
  outputs,
  setMixMute,
  setMixVolume,
  settings,
}: {
  autoDevices: AutoDevices;
  mix: Mix;
  onOpenSettings: () => void;
  outputs: DeviceInfo[];
  setMixMute: (mixId: string, muted: boolean) => Promise<void>;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
  settings: MixerSettings;
}) {
  const MixIcon = mixIconComponent(mixIconId(mix));
  const selectedOutputs = mixOutputDevices(mix);
  const outputSummary = mixOutputSummary(mix, outputs, settings, autoDevices);
  return (
    <div className="wl-mix-header">
      <div className="wl-mix-title">
        <MixIcon size={18} />
        <div>
          <strong>{mix.name}</strong>
          <span>{mix.virtual_source_name}</span>
        </div>
        <button
          className={mix.muted ? "mini-icon-button danger active" : "mini-icon-button"}
          onClick={() => void setMixMute(mix.id, !mix.muted).catch(() => undefined)}
          title={`${mix.muted ? "Unmute" : "Mute"} ${mix.name}`}
          type="button"
        >
          {mix.muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
      </div>
      <WaveLinkMasterControl
        mix={mix}
        setMixVolume={setMixVolume}
      />
      <div className="wl-mix-output-summary" title={outputSummary}>
        {mix.id === "monitor" && settings.monitor_follows_default_output ? (
          <span className="wl-output-chip">Auto output</span>
        ) : selectedOutputs.length > 0 ? (
          <span className="wl-output-chip">{outputSummary}</span>
        ) : (
          <span className="wl-output-chip muted">No direct output</span>
        )}
      </div>
      <div className="wl-inline-actions">
        <button
          className="mini-icon-button"
          onClick={onOpenSettings}
          title={`${mix.name} settings`}
          type="button"
        >
          <SlidersHorizontal size={14} />
        </button>
      </div>
    </div>
  );
}

function WaveLinkMixSettingsDrawer({
  autoDevices,
  canDelete,
  canMoveDown,
  canMoveUp,
  mix,
  onClose,
  outputs,
  run,
  setMixIcon,
  setMixMute,
  setMixOutputs,
  setMixVolume,
  setSettings,
  settings,
}: {
  autoDevices: AutoDevices;
  canDelete: boolean;
  canMoveDown: boolean;
  canMoveUp: boolean;
  mix: Mix;
  onClose: () => void;
  outputs: DeviceInfo[];
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setMixIcon: (mixId: string, icon: string | null) => Promise<void>;
  setMixMute: (mixId: string, muted: boolean) => Promise<void>;
  setMixOutputs: (mixId: string, outputs: string[]) => Promise<void>;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
  setSettings: (settings: MixerSettings) => Promise<void>;
  settings: MixerSettings;
}) {
  const [name, setName] = useState(mix.name);
  const [busy, setBusy] = useState(false);
  const cleanName = name.trim();
  const MixIcon = mixIconComponent(mixIconId(mix));

  useEffect(() => {
    setName(mix.name);
  }, [mix.id, mix.name]);

  return (
    <aside className="wl-routing-drawer wl-settings-drawer">
      <form
        className="wl-drawer-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!cleanName || cleanName === mix.name || busy) return;
          setBusy(true);
          void run("rename_mix", { mixId: mix.id, name: cleanName }, "Mix renamed")
            .catch(() => undefined)
            .finally(() => setBusy(false));
        }}
      >
        <div className="wl-drawer-header">
          <div>
            <strong>Output Settings</strong>
            <span>{mix.virtual_source_name}</span>
          </div>
          <button className="mini-icon-button" onClick={onClose} title="Close output settings" type="button">
            <X size={14} />
          </button>
        </div>
        <div className="wl-drawer-body">
          <div className="wl-editor-summary">
            <MixIcon size={20} />
            <div>
              <strong>{mix.name}</strong>
              <span>{mixOutputSummary(mix, outputs, settings, autoDevices)}</span>
            </div>
          </div>
          <label className="wl-dialog-field">
            <span>Name</span>
            <input value={name} onChange={(event) => setName(event.currentTarget.value)} />
          </label>
          <button className="secondary-button" disabled={busy || !cleanName || cleanName === mix.name} type="submit">
            <Pencil size={16} />
            Save Name
          </button>
          <div className="wl-drawer-section-title">
            <span>Icon</span>
            <strong>{mixIconLabel(mixIconId(mix))}</strong>
          </div>
          <WaveLinkMixIconPicker
            mixId={mix.id}
            selectedIcon={mixIconId(mix)}
            setMixIcon={setMixIcon}
          />
          <div className="wl-drawer-section-title">
            <span>Output Routes</span>
            <strong>{mixOutputDevices(mix).length}</strong>
          </div>
          <WaveLinkMixOutputs
            mix={mix}
            outputs={outputs}
            setMixOutputs={setMixOutputs}
            setSettings={setSettings}
            settings={settings}
          />
          <div className="wl-drawer-section-title">
            <span>Master</span>
            <strong>{volumeToPercent(mix.volume)}</strong>
          </div>
          <WaveLinkMasterControl mix={mix} setMixVolume={setMixVolume} />
          <button
            className={mix.muted ? "secondary-button danger active" : "secondary-button"}
            onClick={() => void setMixMute(mix.id, !mix.muted).catch(() => undefined)}
            type="button"
          >
            {mix.muted ? <VolumeX size={16} /> : <Volume2 size={16} />}
            {mix.muted ? "Unmute Output" : "Mute Output"}
          </button>
          <div className="wl-drawer-section-title">
            <span>Order</span>
            <strong>Matrix</strong>
          </div>
          <div className="wl-drawer-action-grid">
            <button
              className="secondary-button"
              disabled={!canMoveUp}
              onClick={() => void run("move_mix", { mixId: mix.id, direction: -1 }, "Mix moved")}
              type="button"
            >
              <ArrowUp size={16} />
              Left
            </button>
            <button
              className="secondary-button"
              disabled={!canMoveDown}
              onClick={() => void run("move_mix", { mixId: mix.id, direction: 1 }, "Mix moved")}
              type="button"
            >
              <ArrowDown size={16} />
              Right
            </button>
          </div>
          <button
            className="secondary-button danger"
            disabled={!canDelete}
            onClick={() => {
              if (window.confirm(`Delete ${mix.name}?`)) {
                onClose();
                void run("delete_mix", { mixId: mix.id }, "Mix deleted");
              }
            }}
            type="button"
          >
            <Trash2 size={16} />
            Delete Output
          </button>
        </div>
      </form>
    </aside>
  );
}

function WaveLinkMixIconPicker({
  mixId,
  selectedIcon,
  setMixIcon,
}: {
  mixId: string;
  selectedIcon: string;
  setMixIcon: (mixId: string, icon: string | null) => Promise<void>;
}) {
  return (
    <div className="wl-mix-icon-picker" aria-label="Mix icon">
      {MIX_ICON_OPTIONS.map((option) => {
        const Icon = option.icon;
        return (
          <button
            className={selectedIcon === option.id ? "active" : ""}
            aria-pressed={selectedIcon === option.id}
            key={option.id}
            onClick={() => void setMixIcon(mixId, option.id).catch(() => undefined)}
            title={option.label}
            type="button"
          >
            <Icon size={14} />
          </button>
        );
      })}
    </div>
  );
}

function WaveLinkChannelIconPicker({
  channelId,
  selectedIcon,
  setChannelIcon,
}: {
  channelId: string;
  selectedIcon: string;
  setChannelIcon: (channelId: string, icon: string | null) => Promise<void>;
}) {
  return (
    <div className="wl-mix-icon-picker" aria-label="Source icon">
      {SOURCE_ICON_OPTIONS.map((option) => {
        const Icon = option.icon;
        return (
          <button
            className={selectedIcon === option.id ? "active" : ""}
            aria-pressed={selectedIcon === option.id}
            key={option.id}
            onClick={() => void setChannelIcon(channelId, option.id).catch(() => undefined)}
            title={option.label}
            type="button"
          >
            <Icon size={14} />
          </button>
        );
      })}
    </div>
  );
}

function WaveLinkMixOutputs({
  mix,
  outputs,
  setMixOutputs,
  setSettings,
  settings,
}: {
  mix: Mix;
  outputs: DeviceInfo[];
  setMixOutputs: (mixId: string, outputs: string[]) => Promise<void>;
  setSettings: (settings: MixerSettings) => Promise<void>;
  settings: MixerSettings;
}) {
  const selectedOutputs = mixOutputDevices(mix);
  const isAutoMonitor = mix.id === "monitor" && settings.monitor_follows_default_output;
  const outputLabel = useCallback((outputId: string) => {
    return outputs.find((output) => output.id === outputId)?.description ?? outputId;
  }, [outputs]);
  const availableOutputs = outputs.filter((output) => !selectedOutputs.includes(output.id));

  return (
    <div className="wl-mix-outputs">
      <div className="wl-output-chips">
        {isAutoMonitor ? (
          <span className="wl-output-chip">Auto output</span>
        ) : selectedOutputs.length > 0 ? (
          selectedOutputs.map((outputId) => (
            <span className="wl-output-chip" key={outputId}>
              <span>{outputLabel(outputId)}</span>
              <button
                aria-label={`Remove ${outputLabel(outputId)}`}
                onClick={() => void setMixOutputs(
                  mix.id,
                  selectedOutputs.filter((current) => current !== outputId),
                ).catch(() => undefined)}
                type="button"
              >
                x
              </button>
            </span>
          ))
        ) : (
          <span className="wl-output-chip muted">No direct output</span>
        )}
      </div>
      <AppSelect
        ariaLabel={`${mix.name} output routes`}
        className="wl-monitor-select"
        onChange={(value) => {
          if (value === AUTO_MONITOR_OUTPUT_VALUE) {
            void setSettings({ ...settings, monitor_follows_default_output: true }).catch(() => undefined);
            return;
          }
          if (mix.id === "monitor" && settings.monitor_follows_default_output) {
            void setSettings({ ...settings, monitor_follows_default_output: false }).catch(() => undefined);
          }
          if (value === CLEAR_MIX_OUTPUTS_VALUE) {
            void setMixOutputs(mix.id, []).catch(() => undefined);
            return;
          }
          void setMixOutputs(mix.id, [...selectedOutputs, value]).catch(() => undefined);
        }}
        options={[
          ...(mix.id === "monitor"
            ? [{ value: AUTO_MONITOR_OUTPUT_VALUE, label: "Auto output" }]
            : []),
          { value: "", label: availableOutputs.length > 0 ? "Add output" : "All outputs added", disabled: true },
          { value: CLEAR_MIX_OUTPUTS_VALUE, label: "No direct output" },
          ...availableOutputs.map((output) => ({
            value: output.id,
            label: output.description,
          })),
        ]}
        value=""
      />
    </div>
  );
}

function WaveLinkMasterControl({
  mix,
  setMixVolume,
}: {
  mix: Mix;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
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
    <label className="wl-master-control">
      <span>Master</span>
      <div className="wl-horizontal-meter">
        <LiveHorizontalMeterFill className="wl-horizontal-meter-fill" meterId={mix.id} />
      </div>
      <input
        aria-label={`${mix.name} master volume`}
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

function WaveLinkSourceRow({
  appStreams,
  autoDevices,
  channel,
  effectRuntime,
  isSelected,
  microphoneInputs,
  mixes,
  onOpenSettings,
  openEffects,
  setChannelBusEnabled,
  setChannelBusMute,
  setChannelBusVolume,
  setChannelEffectsEnabled,
  setSelectedChannelId,
}: {
  appStreams: AppStream[];
  autoDevices: AutoDevices;
  channel: Channel;
  effectRuntime?: EffectRuntimeStatus;
  isSelected: boolean;
  microphoneInputs: AppStateSnapshot["graph"]["inputs"];
  mixes: Mix[];
  onOpenSettings: () => void;
  openEffects: () => void;
  setChannelBusEnabled: (channelId: string, mixId: string, enabled: boolean) => Promise<void>;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
  setChannelEffectsEnabled: (channelId: string, enabled: boolean) => Promise<void>;
  setSelectedChannelId: (channelId: string) => void;
}) {
  const Icon = channelIconComponent(channel);
  const isHardware = isHardwareChannel(channel);
  const displayName = channelDisplayName(channel);
  return (
    <>
      <div
        className={isSelected ? "wl-source-cell selected" : "wl-source-cell"}
        onClick={() => setSelectedChannelId(channel.id)}
      >
        <div className="wl-source-title">
          <Icon size={18} />
          <div>
            <strong>{displayName}</strong>
            <span>{isHardware ? channelInputLabel(channel, microphoneInputs, autoDevices) : channel.virtual_sink_name}</span>
          </div>
        </div>
        <div className="wl-source-meter" aria-hidden="true">
          <LiveHorizontalMeterFill className="wl-source-meter-fill" meterId={channel.id} />
        </div>
        {appStreams.length > 0 && (
          <div className="wl-source-app-chips" aria-label={`${displayName} active apps`}>
            {appStreams.slice(0, 3).map((stream) => (
              <span className="wl-source-app-chip" key={stream.id}>
                {stream.display_name || stream.process_name || stream.binary || "App"}
              </span>
            ))}
            {appStreams.length > 3 && <span className="wl-source-app-chip">+{appStreams.length - 3}</span>}
          </div>
        )}
        <div className="wl-source-actions">
          <button
            className="mini-icon-button"
            onClick={(event) => {
              event.stopPropagation();
              openEffects();
            }}
            title="Open effects"
            type="button"
          >
            <Sparkles size={14} />
          </button>
          <EffectStateButton
            channel={channel}
            onToggle={(enabled) => setChannelEffectsEnabled(channel.id, enabled)}
            runtime={effectRuntime}
            stopPropagation
          />
          <button
            className="mini-icon-button"
            onClick={(event) => {
              event.stopPropagation();
              onOpenSettings();
            }}
            title={`${displayName} settings`}
            type="button"
          >
            <SlidersHorizontal size={14} />
          </button>
        </div>
      </div>
      {mixes.map((mix) => {
        const bus = channel.mix_buses[mix.id] ?? defaultMixBus(false);
        return (
          <WaveLinkSendCell
            bus={bus}
            channel={channel}
            key={`${channel.id}-${mix.id}`}
            mix={mix}
            setChannelBusEnabled={setChannelBusEnabled}
            setChannelBusMute={setChannelBusMute}
            setChannelBusVolume={setChannelBusVolume}
          />
        );
      })}
    </>
  );
}

function WaveLinkSourceSettingsDrawer({
  autoDevices,
  canMoveDown,
  canMoveUp,
  channel,
  microphoneInputs,
  onClose,
  run,
  setChannelIcon,
  setChannelInput,
}: {
  canMoveDown: boolean;
  canMoveUp: boolean;
  channel: Channel;
  microphoneInputs: AppStateSnapshot["graph"]["inputs"];
  onClose: () => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setChannelIcon: (channelId: string, icon: string | null) => Promise<void>;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  autoDevices: AutoDevices;
}) {
  const [name, setName] = useState(channelDisplayName(channel));
  const [busy, setBusy] = useState(false);
  const displayName = channelDisplayName(channel);
  const cleanName = name.trim();
  const Icon = channelIconComponent(channel);
  const isHardware = isHardwareChannel(channel);
  const selectedInputMissing =
    isHardware &&
    channel.source_device &&
    !microphoneInputs.some((input) => input.id === channel.source_device);

  useEffect(() => {
    setName(channelDisplayName(channel));
  }, [channel.id, channel.name]);

  return (
    <aside className="wl-routing-drawer wl-settings-drawer">
      <form
        className="wl-drawer-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!cleanName || cleanName === displayName || busy) return;
          setBusy(true);
          void run("rename_channel", { channelId: channel.id, name: cleanName }, "Source renamed")
            .catch(() => undefined)
            .finally(() => setBusy(false));
        }}
      >
        <div className="wl-drawer-header">
          <div>
            <strong>Source Settings</strong>
            <span>{displayName}</span>
          </div>
          <button className="mini-icon-button" onClick={onClose} title="Close source settings" type="button">
            <X size={14} />
          </button>
        </div>
        <div className="wl-drawer-body">
          <div className="wl-editor-summary">
            <Icon size={20} />
            <div>
              <strong>{displayName}</strong>
              <span>{isHardware ? channelInputLabel(channel, microphoneInputs, autoDevices) : channel.virtual_sink_name}</span>
            </div>
          </div>
          <label className="wl-dialog-field">
            <span>Name</span>
            <input value={name} onChange={(event) => setName(event.currentTarget.value)} />
          </label>
          <button className="secondary-button" disabled={busy || !cleanName || cleanName === displayName} type="submit">
            <Pencil size={16} />
            Save Name
          </button>
          <div className="wl-drawer-section-title">
            <span>Icon</span>
            <strong>{sourceIconLabel(channelIconId(channel))}</strong>
          </div>
          <WaveLinkChannelIconPicker
            channelId={channel.id}
            selectedIcon={channelIconId(channel)}
            setChannelIcon={setChannelIcon}
          />
          {isHardware && (
            <>
              <div className="wl-drawer-section-title">
                <span>Hardware Input</span>
                <strong>Mono</strong>
              </div>
              <AppSelect
                ariaLabel={`${displayName} microphone`}
                className="wl-source-select"
                onChange={(nextValue) => void setChannelInput(channel.id, nextValue || null).catch(() => undefined)}
                options={[
                  { value: "", label: autoMicrophoneLabel(microphoneInputs, "Auto mic", autoDevices, channel.id) },
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
            </>
          )}
          <div className="wl-drawer-section-title">
            <span>Send Control</span>
            <strong>{channel.linked ? "Linked" : "Split"}</strong>
          </div>
          <button
            className={channel.linked ? "secondary-button active" : "secondary-button"}
            onClick={() =>
              void run(
                "set_channel_linked",
                { channelId: channel.id, linked: !channel.linked },
                channel.linked ? "Sliders unlinked" : "Sliders linked",
              )
            }
            type="button"
          >
            <GitBranch size={16} />
            {channel.linked ? "Unlink Sends" : "Link Sends"}
          </button>
          <div className="wl-drawer-section-title">
            <span>Order</span>
            <strong>Sources</strong>
          </div>
          <div className="wl-drawer-action-grid">
            <button
              className="secondary-button"
              disabled={!canMoveUp}
              onClick={() => void run("move_channel", { channelId: channel.id, direction: -1 }, "Source moved")}
              type="button"
            >
              <ArrowUp size={16} />
              Up
            </button>
            <button
              className="secondary-button"
              disabled={!canMoveDown}
              onClick={() => void run("move_channel", { channelId: channel.id, direction: 1 }, "Source moved")}
              type="button"
            >
              <ArrowDown size={16} />
              Down
            </button>
          </div>
          <button
            className="secondary-button danger"
            onClick={() => {
              if (window.confirm(`Delete ${displayName}?`)) {
                onClose();
                void run("delete_channel", { channelId: channel.id }, "Source deleted");
              }
            }}
            type="button"
          >
            <Trash2 size={16} />
            Delete Source
          </button>
        </div>
      </form>
    </aside>
  );
}

function WaveLinkSendCell({
  bus,
  channel,
  mix,
  setChannelBusEnabled,
  setChannelBusMute,
  setChannelBusVolume,
}: {
  bus: MixBus;
  channel: Channel;
  mix: Mix;
  setChannelBusEnabled: (channelId: string, mixId: string, enabled: boolean) => Promise<void>;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
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

  if (!bus.enabled) {
    return (
      <div className="wl-send-cell disabled">
        <button
          className="wl-send-enable"
          onClick={() => void setChannelBusEnabled(channel.id, mix.id, true).catch(() => undefined)}
          title={`Add ${channelDisplayName(channel)} to ${mix.name}`}
          type="button"
        >
          <CirclePlus size={17} />
          Add
        </button>
      </div>
    );
  }

  return (
    <div className={bus.muted ? "wl-send-cell muted" : "wl-send-cell"}>
      <div className="wl-send-meter" aria-hidden="true">
        <LiveHorizontalMeterFill
          className="wl-send-meter-fill"
          meterId={channelBusMeterId(channel.id, mix.id)}
        />
      </div>
      <input
        aria-label={`${channelDisplayName(channel)} ${mix.name} volume`}
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
      <div className="wl-send-footer">
        <strong>{draft}</strong>
        <button
          className="mini-icon-button"
          onClick={() => void setChannelBusEnabled(channel.id, mix.id, false).catch(() => undefined)}
          title={`Remove ${channelDisplayName(channel)} from ${mix.name}`}
          type="button"
        >
          <CircleMinus size={14} />
        </button>
        <button
          className={bus.muted ? "mini-icon-button danger active" : "mini-icon-button"}
          onClick={() => void setChannelBusMute(channel.id, mix.id, !bus.muted).catch(() => undefined)}
          title={`${bus.muted ? "Unmute" : "Mute"} ${channelDisplayName(channel)} in ${mix.name}`}
          type="button"
        >
          {bus.muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
      </div>
    </div>
  );
}

function WaveLinkAppRouteCard({
  channels,
  run,
  setAppStreamMute,
  stream,
}: {
  channels: Channel[];
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setAppStreamMute: (streamId: string, muted: boolean) => Promise<void>;
  stream: AppStream;
}) {
  const [draftRoute, setDraftRoute] = useState(stream.routed_channel_id ?? "");
  const [draftVolume, setDraftVolume] = useState(appVolumeToPercent(stream.volume));
  const lastCommitted = useRef(draftVolume);

  useEffect(() => {
    setDraftRoute(stream.routed_channel_id ?? "");
  }, [stream.routed_channel_id]);

  useEffect(() => {
    const next = appVolumeToPercent(stream.volume);
    setDraftVolume(next);
    lastCommitted.current = next;
  }, [stream.volume]);

  const routeStream = async (channelId: string) => {
    setDraftRoute(channelId);
    if (!channelId) {
      const matcher = matcherForStream(stream);
      await invoke("remove_app_route", { matcher });
      await invoke("move_app_stream_to_default", { streamId: stream.id });
      return;
    }
    await invoke("move_app_stream", { streamId: stream.id, channelId });
    await run("assign_app_to_channel", {
      channelId,
      matcher: matcherForStream(stream),
    }, "App route saved");
  };

  const commitVolume = useCallback((nextValue = draftVolume) => {
    const next = appVolumePercent(nextValue);
    setDraftVolume(next);
    if (lastCommitted.current === next) return;
    lastCommitted.current = next;
    const volume = next / 100;
    void invoke("set_app_stream_volume", {
      streamId: stream.id,
      volume,
    }).catch(() => undefined);
    void invoke("set_app_volume_preset", {
      matcher: matcherForStream(stream),
      volume,
    }).catch(() => undefined);
  }, [draftVolume, stream]);

  const routedChannel = channels.find((channel) => channel.id === draftRoute);

  return (
    <article className="wl-app-route-card">
      <div className="wl-app-route-title">
        <MonitorSpeaker size={16} />
        <div>
          <strong>{stream.display_name}</strong>
          <span>{stream.media_name ?? stream.process_name ?? stream.id}</span>
        </div>
        <button
          className={stream.muted ? "mini-icon-button danger active" : "mini-icon-button"}
          onClick={() => void setAppStreamMute(stream.id, !stream.muted).catch(() => undefined)}
          title="Mute app"
          type="button"
        >
          {stream.muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
      </div>
      <AppSelect
        ariaLabel={`Route ${stream.display_name} to source`}
        onChange={(value) => void routeStream(value).catch(() => setDraftRoute(stream.routed_channel_id ?? ""))}
        options={[
          { value: "", label: "Unassigned" },
          ...channels.map((channel) => ({
            value: channel.id,
            label: channelDisplayName(channel),
          })),
        ]}
        value={draftRoute}
      />
      <div className="wl-app-route-status">
        <span>Input</span>
        <strong>{routedChannel ? channelDisplayName(routedChannel) : "Unassigned"}</strong>
      </div>
      <label className="wl-app-volume-control">
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
    </article>
  );
}

function WaveLinkOfflineRuleCard({
  channels,
  entry,
  run,
}: {
  channels: Channel[];
  entry: OfflineRoutingEntry;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const [draftRoute, setDraftRoute] = useState(entry.channel_id ?? "");

  useEffect(() => {
    setDraftRoute(entry.channel_id ?? "");
  }, [entry.channel_id]);

  const routeRule = async (channelId: string) => {
    setDraftRoute(channelId);
    if (channelId) {
      await run(
        "assign_app_to_channel",
        { channelId, matcher: entry.matcher },
        "Routing rule updated",
      );
    } else {
      await run("remove_app_route", { matcher: entry.matcher }, "Routing rule removed");
    }
  };

  return (
    <article className="wl-app-route-card saved">
      <div className="wl-app-route-title">
        <GitBranch size={16} />
        <div>
          <strong>{entry.displayName}</strong>
          <span>{entry.meta}</span>
        </div>
        <button
          className="mini-icon-button danger"
          onClick={() => void run("forget_app", { matcher: entry.matcher }, "App forgotten").catch(() => undefined)}
          title="Forget saved rule"
          type="button"
        >
          <Trash2 size={14} />
        </button>
      </div>
      <AppSelect
        ariaLabel={`Route ${entry.displayName} to source`}
        onChange={(value) => void routeRule(value).catch(() => setDraftRoute(entry.channel_id ?? ""))}
        options={[
          { value: "", label: "Unassigned" },
          ...channels.map((channel) => ({
            value: channel.id,
            label: channelDisplayName(channel),
          })),
        ]}
        value={draftRoute}
      />
      <OfflineVolumeControl
        label={entry.displayName}
        matcher={entry.matcher}
        preset={entry.volumePreset}
      />
    </article>
  );
}

function ChannelStrip({
  autoDevices,
  channel,
  mixes,
  microphoneInputs,
  onFocus,
  onOpenMenu,
  setChannelBusMute,
  setChannelBusVolume,
  setChannelInput,
}: {
  autoDevices: AutoDevices;
  channel: Channel;
  mixes: Mix[];
  microphoneInputs: AppStateSnapshot["graph"]["inputs"];
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
            bus={channel.mix_buses[mix.id] ?? defaultMixBus()}
            channel={channel}
            key={mix.id}
            mix={mix}
            setChannelBusMute={setChannelBusMute}
            setChannelBusVolume={setChannelBusVolume}
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
            { value: "", label: autoMicrophoneLabel(microphoneInputs, "Auto mic", autoDevices, channel.id) },
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

function ChannelBusControl({
  channel,
  mix,
  bus,
  setChannelBusMute,
  setChannelBusVolume,
}: {
  channel: Channel;
  mix: Mix;
  bus: MixBus;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
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
        meterId={channelBusMeterId(channel.id, mix.id)}
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
}: {
  mix: Mix;
  setMixMute: (mixId: string, muted: boolean) => Promise<void>;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
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
        meterId={mix.id}
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

function LiveHorizontalMeterFill({
  className,
  meterId,
}: {
  className: string;
  meterId: string;
}) {
  return <div className={className} data-meter-id={meterId} />;
}

function LiveVerticalMeter({ meterId }: { meterId: string }) {
  return (
    <>
      <div className="vu-fill" data-meter-id={meterId} />
      <div className="vu-cap" data-meter-id={meterId} />
    </>
  );
}

function VuSlider({
  ariaLabel,
  master = false,
  meterId,
  muted = false,
  onCommit,
  onDraft,
  value,
}: {
  ariaLabel: string;
  master?: boolean;
  meterId: string;
  muted?: boolean;
  onCommit: (value: number) => void;
  onDraft: (value: number) => void;
  value: number;
}) {
  const draggingPointerId = useRef<number | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
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
        <LiveVerticalMeter meterId={meterId} />
      </div>
      <div className="vu-thumb" style={{ bottom: thumbPosition(value) }} />
    </div>
  );
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

function prefersCompactWaveLinkMixer(): boolean {
  if (typeof window === "undefined") return false;
  return window.innerWidth < 1180 || window.innerHeight < 760;
}

function mixIconId(mix: Mix): string {
  return mix.icon || defaultMixIconForName(mix.name, mix.id);
}

function defaultMixIconForName(name: string, id = ""): string {
  const value = `${id} ${name}`.toLowerCase();
  if (value.includes("monitor") || value.includes("personal")) return "headphones";
  if (value.includes("stream") || value.includes("record")) return "radio";
  if (value.includes("chat") || value.includes("discord") || value.includes("voice")) return "chat";
  if (value.includes("music")) return "music";
  if (value.includes("mic")) return "mic";
  if (value.includes("fx")) return "sparkles";
  return "audio";
}

function mixIconComponent(iconId: string): typeof SlidersHorizontal {
  return MIX_ICON_OPTIONS.find((option) => option.id === iconId)?.icon ?? AudioLines;
}

function mixIconLabel(iconId: string): string {
  return MIX_ICON_OPTIONS.find((option) => option.id === iconId)?.label ?? "Audio";
}

function sourceIconLabel(iconId: string): string {
  return SOURCE_ICON_OPTIONS.find((option) => option.id === iconId)?.label ?? "Audio";
}

function channelIconId(channel: Channel): string {
  return channel.icon || defaultChannelIconForChannel(channel);
}

function channelIconComponent(channel: Channel): typeof Headphones {
  const iconId = channelIconId(channel);
  return (
    SOURCE_ICON_OPTIONS.find((option) => option.id === iconId)?.icon ??
    MIX_ICON_OPTIONS.find((option) => option.id === iconId)?.icon ??
    AudioLines
  ) as typeof Headphones;
}

function defaultChannelIconForChannel(channel: Channel): string {
  const value = `${channel.id} ${channel.name}`.toLowerCase();
  if (channel.kind === "microphone" || channel.kind === "generic" || value.includes("input") || value.includes("mic")) {
    return "mic";
  }
  if (channel.kind === "soundboard" || value.includes("sfx") || value.includes("sound")) return "sfx";
  if (channel.kind === "system" || value.includes("system") || value.includes("desktop")) return "system";
  if (value.includes("game")) return "game";
  if (value.includes("browser") || value.includes("web") || value.includes("chrome") || value.includes("firefox")) return "browser";
  if (value.includes("chat") || value.includes("discord") || value.includes("voice")) return "chat";
  if (value.includes("music") || value.includes("spotify")) return "music";
  if (value.includes("media") || value.includes("video")) return "media";
  return "audio";
}

function mixOutputSummary(mix: Mix, outputs: DeviceInfo[], settings: MixerSettings, autoDevices: AutoDevices = []): string {
  if (mix.id === "monitor" && settings.monitor_follows_default_output) {
    const resolved = resolvedAutoOutput(autoDevices, mix.id);
    return resolved?.device_description || resolved?.device_id
      ? `Auto: ${resolved.device_description ?? resolved.device_id}`
      : "Auto output";
  }
  const selectedOutputs = mixOutputDevices(mix);
  if (selectedOutputs.length === 0) return "No direct output";
  const labels = selectedOutputs.map((outputId) =>
    outputs.find((output) => output.id === outputId)?.description ?? outputId,
  );
  if (labels.length <= 2) return labels.join(", ");
  return `${labels.slice(0, 2).join(", ")} +${labels.length - 2}`;
}

function resolvedAutoOutput(autoDevices: AutoDevices, mixId?: string) {
  return autoDevices.find((device) =>
    device.kind === "output" && (!mixId || device.mix_id === mixId)
  );
}

function channelInputLabel(
  channel: Pick<Channel, "id" | "source_device">,
  inputs: AppStateSnapshot["graph"]["inputs"],
  autoDevices: AutoDevices = [],
): string {
  if (!channel.source_device) {
    const resolved = resolvedAutoInput(autoDevices, channel.id);
    return resolved?.device_description || resolved?.device_id
      ? `Auto: ${resolved.device_description ?? resolved.device_id}`
      : "Auto input";
  }
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

function channelBusMeterId(channelId: string, mixId: string): string {
  return `channel:${channelId}:mix:${mixId}`;
}
