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
  Link2,
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
  Unlink,
  Volume2,
  VolumeX,
  WandSparkles,
} from "lucide-react";
import { Fragment, useCallback, useEffect, useState } from "react";
import type { CSSProperties, MouseEvent as ReactMouseEvent } from "react";
import { invoke } from "./tauri";
import type {
  AppStateSnapshot,
  AppStream,
  Channel,
  ChannelKind,
  EffectDefinition,
  EffectInstance,
  Mix,
  MixBus,
  MixerSettings,
  Scene,
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

const MAX_SOFTWARE_CHANNELS = 8;
const MAX_HARDWARE_INPUTS = 4;

export default function App() {
  const [state, setState] = useState<AppStateSnapshot | null>(null);
  const [activeView, setActiveView] = useState<View>("mixer");
  const [activeMixId, setActiveMixId] = useState("monitor");
  const [selectedChannelId, setSelectedChannelId] = useState("mic");
  const [busy, setBusy] = useState(false);
  const [toast, setToast] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const next = await invoke<AppStateSnapshot>("get_state");
    setState(next);
    if (!next.config.mixes.some((mix) => mix.id === activeMixId)) {
      setActiveMixId(next.config.mixes[0]?.id ?? "monitor");
    }
    if (!next.config.channels.some((channel) => channel.id === selectedChannelId)) {
      setSelectedChannelId(next.config.channels[0]?.id ?? "mic");
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
      } finally {
        setBusy(false);
      }
    },
    [refresh],
  );

  useEffect(() => {
    refresh().catch((error) => setToast(String(error)));
    const timer = window.setInterval(() => {
      invoke<AppStateSnapshot>("observe_state")
        .then(setState)
        .catch(() => undefined);
    }, 500);
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
            {state?.engine.dry_run ? "Dry run" : "Live graph"} · {state?.config.audio.sample_rate_hz ?? 48000} Hz
          </div>
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <div>
            <h1>{viewTitle(activeView)}</h1>
            <p>{state ? `${state.config.mixes.length} mixes · ${state.config.channels.length} channels` : "Loading"}</p>
          </div>
          <div className="top-actions">
            <button className="icon-button" onClick={() => refresh()} title="Refresh" type="button">
              <RefreshCw size={17} />
            </button>
            <button
              className="primary-button"
              disabled={busy}
              onClick={() => run("repair_audio_graph", undefined, "Graph repair queued")}
              type="button"
            >
              <WandSparkles size={17} />
              Repair
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
            {activeView === "scenes" && <ScenesView run={run} />}
            {activeView === "diagnostics" && <DiagnosticsView state={state} run={run} />}
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
  const inputs = state.graph.inputs.filter((input) => !input.is_virtual);
  const softwareChannelCount = state.config.channels.filter((channel) => !isHardwareChannel(channel)).length;
  const hardwareInputCount = state.config.channels.filter(isHardwareChannel).length;
  const [menu, setMenu] = useState<{ x: number; y: number; channelId: string } | null>(null);
  const menuChannel = menu
    ? state.config.channels.find((channel) => channel.id === menu.channelId)
    : undefined;
  const matrixStyle = { "--mix-count": state.config.mixes.length } as CSSProperties;

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
    <section className="view-stack">
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
            {mix.name}
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

      <div className="mixer-layout">
        <div className="mix-matrix-panel">
          <div className="matrix-toolbar">
            <div>
              <h2>Mix Matrix</h2>
              <span>Inputs down the side, independent mixes across the top</span>
            </div>
            <div className="panel-actions">
              <button
                className="secondary-button"
                disabled={hardwareInputCount >= MAX_HARDWARE_INPUTS || busy}
                onClick={() => {
                  const name = window.prompt("Input name", "Mic 2");
                  if (name) void run("create_channel", { name, kind: "microphone" satisfies ChannelKind }, "Input added");
                }}
                type="button"
                title={`${hardwareInputCount}/${MAX_HARDWARE_INPUTS} hardware inputs`}
              >
                <Mic size={16} />
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

          <div className="matrix-scroller">
            <div className="mix-matrix-grid" style={matrixStyle}>
              <div className="matrix-cell matrix-corner">Input</div>
              {state.config.mixes.map((mix) => (
                <MixHeaderCell
                  active={mix.id === activeMixId}
                  busy={busy}
                  key={mix.id}
                  mix={mix}
                  mixCount={state.config.mixes.length}
                  outputs={outputs}
                  run={run}
                  setActiveMixId={setActiveMixId}
                />
              ))}

              {state.config.channels.map((channel, index) => (
                <Fragment key={channel.id}>
                  <ChannelHeaderCell
                    appCount={state.graph.app_streams.filter((stream) => stream.routed_channel_id === channel.id).length}
                    channel={channel}
                    inputs={inputs}
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
                  {state.config.mixes.map((mix, mixIndex) => {
                    const bus = channel.mix_buses[mix.id] ?? { volume: 1, muted: false };
                    const sourceLevel = meterLevel(state, channel.id, pseudoLevel(channel.id, index));
                    return (
                      <MatrixBusCell
                        bus={bus}
                        channel={channel}
                        key={`${channel.id}-${mix.id}`}
                        mix={mix}
                        run={run}
                        vuLevel={
                          bus.muted || mix.muted
                            ? 0
                            : busVuLevel(sourceLevel * bus.volume * mix.volume, mixIndex)
                        }
                      />
                    );
                  })}
                </Fragment>
              ))}
            </div>
          </div>
        </div>

        <div className="mix-control-panel">
          <div className="panel-header">
            <div>
              <h2>Outputs</h2>
              <span>Master levels and monitor routes</span>
            </div>
          </div>

          <div className="output-stack">
            {state.config.mixes.map((mix, index) => (
              <MasterMixCard
                key={mix.id}
                mix={mix}
                outputs={outputs}
                run={run}
                vuLevel={mix.muted ? 0 : meterLevel(state, mix.id, busVuLevel(0.62, index)) * mix.volume}
              />
            ))}
          </div>

          <div className="master-stats">
            <Stat icon={MonitorSpeaker} label="Output" value={outputs.length.toString()} />
            <Stat icon={Cable} label="Apps" value={state.graph.app_streams.length.toString()} />
            <Stat icon={Cpu} label="DSP" value={availableEffects(state).toString()} />
          </div>
        </div>
      </div>
      {menu && menuChannel && (
        <ChannelContextMenu
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

function MixHeaderCell({
  mix,
  active,
  busy,
  mixCount,
  outputs,
  run,
  setActiveMixId,
}: {
  mix: Mix;
  active: boolean;
  busy: boolean;
  mixCount: number;
  outputs: AppStateSnapshot["graph"]["outputs"];
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setActiveMixId: (mixId: string) => void;
}) {
  return (
    <div className={active ? "matrix-cell mix-header-cell active" : "matrix-cell mix-header-cell"} onClick={() => setActiveMixId(mix.id)}>
      <div className="mix-header-main">
        <GripVertical size={14} />
        <Radio size={15} />
        <strong title={mix.name}>{mix.name}</strong>
      </div>
      <span title={mix.virtual_source_name}>{mix.virtual_source_name}</span>
      <div className="mix-header-actions">
        <button
          className="mini-icon-button"
          onClick={(event) => {
            event.stopPropagation();
            void run(
              "set_mix_mute",
              { mixId: mix.id, muted: !mix.muted },
              mix.muted ? `${mix.name} unmuted` : `${mix.name} muted`,
            );
          }}
          title={`Mute ${mix.name}`}
          type="button"
        >
          {mix.muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
        <button
          className="mini-icon-button"
          onClick={(event) => {
            event.stopPropagation();
            const name = window.prompt("Mix name", mix.name);
            if (name && name !== mix.name) void run("rename_mix", { mixId: mix.id, name }, "Mix renamed");
          }}
          title="Rename mix"
          type="button"
        >
          <Pencil size={14} />
        </button>
        <button
          className="mini-icon-button danger"
          disabled={mixCount <= 1 || busy}
          onClick={(event) => {
            event.stopPropagation();
            if (window.confirm(`Delete ${mix.name}?`)) {
              void run("delete_mix", { mixId: mix.id }, "Mix deleted");
            }
          }}
          title="Delete mix"
          type="button"
        >
          <Trash2 size={14} />
        </button>
      </div>
      <select
        className="matrix-output-select"
        onClick={(event) => event.stopPropagation()}
        onChange={(event) =>
          run("set_mix_monitor_output", {
            mixId: mix.id,
            output: event.currentTarget.value || null,
          })
        }
        value={mix.monitor_output ?? ""}
      >
        <option value="">No monitor route</option>
        {outputs.map((output) => (
          <option key={output.id} value={output.id}>
            {output.description}
          </option>
        ))}
      </select>
    </div>
  );
}

function ChannelHeaderCell({
  channel,
  appCount,
  inputs,
  onFocus,
  onOpenMenu,
  run,
}: {
  channel: Channel;
  appCount: number;
  inputs: AppStateSnapshot["graph"]["inputs"];
  onFocus: () => void;
  onOpenMenu: (event: ReactMouseEvent<HTMLElement>) => void;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const Icon = channel.kind === "microphone" ? Mic : channel.kind === "soundboard" ? Music2 : Headphones;
  const selectedInput =
    channel.source_device === "@DEFAULT_SOURCE@"
      ? "Default input"
      : inputs.find((input) => input.id === channel.source_device)?.description;
  return (
    <div className="matrix-cell channel-header-cell" onClick={onFocus} onContextMenu={onOpenMenu}>
      <div className="channel-header-main">
        <Icon size={17} />
        <div>
          <strong title={channel.name}>{channel.name}</strong>
          <span title={selectedInput ?? "No physical input"}>
            {appCount} apps · {channel.effects.length} FX · {selectedInput ?? "No input"}
          </span>
        </div>
      </div>
      <button
        className={channel.linked ? "mini-icon-button active" : "mini-icon-button"}
        onClick={(event) => {
          event.stopPropagation();
          void run(
            "set_channel_linked",
            { channelId: channel.id, linked: !channel.linked },
            channel.linked ? "Sliders unlinked" : "Sliders linked",
          );
        }}
        title={channel.linked ? "Unlink mix sliders" : "Link mix sliders"}
        type="button"
      >
        {channel.linked ? <Link2 size={14} /> : <Unlink size={14} />}
      </button>
      <select
        className="channel-input-select"
        onChange={(event) => {
          event.stopPropagation();
          void run(
            "set_channel_input",
            {
              channelId: channel.id,
              sourceDevice: event.currentTarget.value || null,
            },
            "Input route updated",
          );
        }}
        onClick={(event) => event.stopPropagation()}
        title="Physical input source"
        value={channel.source_device ?? ""}
      >
        <option value="">No physical input</option>
        <option value="@DEFAULT_SOURCE@">Default input</option>
        {inputs.map((input) => (
          <option key={input.id} value={input.id}>
            {input.description}
          </option>
        ))}
      </select>
    </div>
  );
}

function MatrixBusCell({
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
  const [draft, setDraft] = useState(Math.round(bus.volume * 100));

  useEffect(() => {
    setDraft(Math.round(bus.volume * 100));
  }, [bus.volume]);

  const commit = useCallback(() => {
    void run("set_channel_volume", {
      channelId: channel.id,
      mixId: mix.id,
      volume: draft / 100,
    });
  }, [channel.id, draft, mix.id, run]);

  return (
    <div className={bus.muted || mix.muted ? "matrix-cell matrix-bus-cell muted" : "matrix-cell matrix-bus-cell"}>
      <div className="cell-meter-row">
        <button
          className={bus.muted ? "mini-icon-button danger active" : "mini-icon-button"}
          onClick={() =>
            run("set_channel_mute", {
              channelId: channel.id,
              mixId: mix.id,
              muted: !bus.muted,
            })
          }
          title={`Mute ${channel.name} in ${mix.name}`}
          type="button"
        >
          {bus.muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
        <strong>{draft}</strong>
      </div>
      <div className="cell-vu-slider">
        <div className="cell-vu-fill" style={{ width: `${Math.round(vuLevel * 100)}%` }} />
        <div className="cell-vu-cap" style={{ left: `${Math.round(vuLevel * 100)}%` }} />
        <div className="cell-vu-thumb" style={{ left: thumbPositionHorizontal(draft) }} />
        <input
          aria-label={`${channel.name} to ${mix.name} volume`}
          max={100}
          min={0}
          onChange={(event) => setDraft(Number(event.currentTarget.value))}
          onKeyUp={commit}
          onMouseUp={commit}
          onPointerUp={commit}
          onTouchEnd={commit}
          type="range"
          value={draft}
        />
      </div>
    </div>
  );
}

function MasterMixCard({
  mix,
  outputs,
  run,
  vuLevel,
}: {
  mix: Mix;
  outputs: AppStateSnapshot["graph"]["outputs"];
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  vuLevel: number;
}) {
  const [draft, setDraft] = useState(Math.round(mix.volume * 100));

  useEffect(() => {
    setDraft(Math.round(mix.volume * 100));
  }, [mix.volume]);

  const commit = useCallback(() => {
    void run("set_mix_volume", {
      mixId: mix.id,
      volume: draft / 100,
    });
  }, [draft, mix.id, run]);

  return (
    <article className={mix.muted ? "master-mix-card muted" : "master-mix-card"}>
      <div className="master-mix-title">
        <div>
          <strong>{mix.name}</strong>
          <span>{mix.virtual_source_name}</span>
        </div>
        <button
          className={mix.muted ? "mini-icon-button danger active" : "mini-icon-button"}
          onClick={() =>
            run(
              "set_mix_mute",
              { mixId: mix.id, muted: !mix.muted },
              mix.muted ? `${mix.name} unmuted` : `${mix.name} muted`,
            )
          }
          title={`Mute ${mix.name}`}
          type="button"
        >
          {mix.muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
      </div>
      <div className="cell-vu-slider master">
        <div className="cell-vu-fill" style={{ width: `${Math.round(vuLevel * 100)}%` }} />
        <div className="cell-vu-cap" style={{ left: `${Math.round(vuLevel * 100)}%` }} />
        <div className="cell-vu-thumb" style={{ left: thumbPositionHorizontal(draft) }} />
        <input
          aria-label={`${mix.name} master volume`}
          max={100}
          min={0}
          onChange={(event) => setDraft(Number(event.currentTarget.value))}
          onKeyUp={commit}
          onMouseUp={commit}
          onPointerUp={commit}
          onTouchEnd={commit}
          type="range"
          value={draft}
        />
      </div>
      <div className="master-mix-footer">
        <strong>{draft}</strong>
        <select
          onChange={(event) =>
            run("set_mix_monitor_output", {
              mixId: mix.id,
              output: event.currentTarget.value || null,
            })
          }
          value={mix.monitor_output ?? ""}
        >
          <option value="">No monitor route</option>
          {outputs.map((output) => (
            <option key={output.id} value={output.id}>
              {output.description}
            </option>
          ))}
        </select>
      </div>
    </article>
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
  const Icon = channel.kind === "microphone" ? Mic : channel.kind === "soundboard" ? Music2 : Headphones;
  return (
    <article className="channel-strip" onClick={onFocus} onContextMenu={onOpenMenu}>
      <div className="strip-title">
        <Icon size={17} />
        <span>{channel.name}</span>
      </div>
      <div className="strip-buses">
        {mixes.map((mix, index) => (
          <ChannelBusControl
            bus={channel.mix_buses[mix.id] ?? { volume: 1, muted: false }}
            channel={channel}
            key={mix.id}
            mix={mix}
            run={run}
            vuLevel={busVuLevel(level, index)}
          />
        ))}
      </div>
    </article>
  );
}

function ChannelContextMenu({
  channel,
  mixes,
  onClose,
  run,
  x,
  y,
}: {
  channel: Channel;
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
  const [draft, setDraft] = useState(Math.round(bus.volume * 100));

  useEffect(() => {
    setDraft(Math.round(bus.volume * 100));
  }, [bus.volume]);

  const commit = useCallback(() => {
    void run("set_channel_volume", {
      channelId: channel.id,
      mixId: mix.id,
      volume: draft / 100,
    });
  }, [channel.id, draft, mix.id, run]);

  return (
    <div className="bus-control">
      <div className="bus-label">{compactMixLabel(mix)}</div>
      <div className={bus.muted ? "vu-slider muted" : "vu-slider"}>
        <div className="vu-fill" style={{ height: `${Math.round(vuLevel * 100)}%` }} />
        <div className="vu-cap" style={{ bottom: `${Math.round(vuLevel * 100)}%` }} />
        <div className="vu-thumb" style={{ bottom: thumbPosition(draft) }} />
        <input
          aria-label={`${channel.name} ${mix.name} volume`}
          className="vu-fader"
          max={100}
          min={0}
          onChange={(event) => setDraft(Number(event.currentTarget.value))}
          onKeyUp={commit}
          onMouseUp={commit}
          onPointerUp={commit}
          onTouchEnd={commit}
          type="range"
          value={draft}
        />
      </div>
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
  const [draft, setDraft] = useState(Math.round(mix.volume * 100));

  useEffect(() => {
    setDraft(Math.round(mix.volume * 100));
  }, [mix.volume]);

  const commit = useCallback(() => {
    void run("set_mix_volume", {
      mixId: mix.id,
      volume: draft / 100,
    });
  }, [draft, mix.id, run]);

  return (
    <div className="master-bus-control">
      <div className="master-bus-title">{compactMixLabel(mix)}</div>
      <div className={mix.muted ? "vu-slider master muted" : "vu-slider master"}>
        <div className="vu-fill" style={{ height: `${Math.round(vuLevel * 100)}%` }} />
        <div className="vu-cap" style={{ bottom: `${Math.round(vuLevel * 100)}%` }} />
        <div className="vu-thumb" style={{ bottom: thumbPosition(draft) }} />
        <input
          aria-label={`${mix.name} master volume`}
          className="vu-fader"
          max={100}
          min={0}
          onChange={(event) => setDraft(Number(event.currentTarget.value))}
          onKeyUp={commit}
          onMouseUp={commit}
          onPointerUp={commit}
          onTouchEnd={commit}
          type="range"
          value={draft}
        />
      </div>
      <button
        className={mix.muted ? "mute-button active" : "mute-button"}
        onClick={() =>
          run(
            "set_mix_mute",
            { mixId: mix.id, muted: !mix.muted },
            mix.muted ? `${mix.name} unmuted` : `${mix.name} muted`,
          )
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

function RoutingView({
  state,
  run,
}: {
  state: AppStateSnapshot;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
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
        <div className="rules-grid">
          {state.config.app_routes.map((route, index) => {
            const channel = state.config.channels.find((item) => item.id === route.channel_id);
            return (
              <div className="rule-row" key={`${route.channel_id}-${index}`}>
                <span>{route.matcher.app_id ?? route.matcher.process_name ?? route.matcher.binary ?? "Any"}</span>
                <strong>{channel?.name ?? route.channel_id}</strong>
                <button
                  className="mini-icon-button danger"
                  onClick={() =>
                    run(
                      "remove_app_route",
                      { matcher: route.matcher },
                      "Routing rule removed",
                    )
                  }
                  title="Remove saved routing rule"
                  type="button"
                >
                  <Trash2 size={14} />
                </button>
              </div>
            );
          })}
          {state.config.app_routes.length === 0 && <EmptyState label="No saved routing rules" />}
        </div>
      </div>
    </section>
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
      <button
        className={stream.muted ? "icon-button danger active" : "icon-button"}
        onClick={() => run("set_app_stream_mute", { streamId: stream.id, muted: !stream.muted })}
        title="Mute app"
        type="button"
      >
        {stream.muted ? <VolumeX size={17} /> : <Volume2 size={17} />}
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
              <small>{channel.effects.length}</small>
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
        <div className="effect-chain">
          {selectedEffects.map((effect, index) => {
            const definition = state.catalog.effects.find((item) => item.id === effect.effect_id);
            return (
              <EffectBlock
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
              run("bypass_effect", {
                channelId,
                instanceId: effect.instance_id,
                bypassed: !effect.bypassed,
              })
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
            run("set_effect_param", {
              channelId,
              instanceId: effect.instance_id,
              paramId: param.id,
              value,
            })
          }
        />
      ))}
    </article>
  );
}

function ScenesView({
  run,
}: {
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const [scenes, setScenes] = useState<Scene[]>([]);
  const refreshScenes = useCallback(async () => {
    const next = await invoke<Scene[]>("list_scenes");
    setScenes(Array.isArray(next) ? next : []);
  }, []);

  useEffect(() => {
    refreshScenes().catch(() => setScenes([]));
  }, [refreshScenes]);

  return (
    <section className="panel single-panel">
      <div className="panel-header">
        <h2>Scenes</h2>
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
      <div className="scene-grid">
        {scenes.map((scene) => (
          <button
            className="scene-tile"
            key={scene.id}
            onClick={async () => {
              await run("load_scene", { sceneId: scene.id }, "Scene loaded");
              await refreshScenes();
            }}
            type="button"
          >
            <strong>{scene.name}</strong>
            <span>{scene.config.mixes.length} mixes · {scene.config.channels.length} channels</span>
          </button>
        ))}
        {scenes.length === 0 && <EmptyState label="No saved scenes" />}
      </div>
    </section>
  );
}

function DiagnosticsView({
  state,
  run,
}: {
  state: AppStateSnapshot;
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
}) {
  const [report, setReport] = useState<SoundCheckReport | null>(null);
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
              onClick={() => run("cleanup_audio_graph", undefined, "Managed audio graph removed")}
              type="button"
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
          </div>
        ) : (
          <EmptyState label="No sound check report" />
        )}
      </div>
    </section>
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
        <Stat icon={Cpu} label="Engine" value={state.engine.dry_run ? "Dry" : "Live"} />
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
  const display = normalizedPercent ? Math.round(value * 100) : Math.round(value * 10) / 10;
  const sliderValue = normalizedPercent ? value * 100 : value;
  const sliderMin = normalizedPercent ? 0 : min;
  const sliderMax = normalizedPercent ? 100 : max;
  return (
    <label className={compact ? "fader-row compact" : "fader-row"}>
      <span>{label}</span>
      <input
        max={sliderMax}
        min={sliderMin}
        onChange={(event) => {
          const raw = Number(event.currentTarget.value);
          void onChange(normalizedPercent ? raw / 100 : raw);
        }}
        step={unit === "%" ? 1 : 0.1}
        type="range"
        value={sliderValue}
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

function compactMixLabel(mix: Mix): string {
  if (mix.id === "monitor") return "MON";
  if (mix.id === "stream") return "STR";
  return mix.name.slice(0, 3).toUpperCase();
}

function matcherForStream(stream: AppStream) {
  return {
    app_id: stream.app_id ?? null,
    binary: stream.process_name ?? null,
    process_name: stream.process_name ?? null,
    window_class: null,
  };
}

function availableEffects(state: AppStateSnapshot): number {
  return state.graph.effect_availability.filter((item) => item.available).length;
}

function isHardwareChannel(channel: Pick<Channel, "kind">): boolean {
  return channel.kind === "microphone" || channel.kind === "generic";
}

function pseudoLevel(seed: string, index: number): number {
  const time = Date.now() / 1000;
  const base = seed.split("").reduce((sum, char) => sum + char.charCodeAt(0), 0);
  return Math.max(0.08, Math.min(0.92, 0.45 + Math.sin(time * 1.4 + base + index) * 0.28));
}

function busVuLevel(level: number, busIndex: number): number {
  const offset = busIndex === 0 ? 0.08 : -0.04;
  return Math.max(0.04, Math.min(0.96, level + offset));
}

function meterLevel(state: AppStateSnapshot, nodeId: string, fallback: number): number {
  const meter = state.graph.meters.find((item) => item.node_id === nodeId);
  if (!meter) return fallback;
  const peak = Math.max(meter.peak_left, meter.peak_right);
  return Math.max(0, Math.min(1, Math.sqrt(Math.max(0, peak))));
}

function thumbPosition(percent: number): string {
  const clamped = Math.max(0, Math.min(100, percent));
  return `clamp(0px, calc(${clamped}% - 11px), calc(100% - 22px))`;
}

function thumbPositionHorizontal(percent: number): string {
  const clamped = Math.max(0, Math.min(100, percent));
  return `clamp(0px, calc(${clamped}% - 10px), calc(100% - 20px))`;
}
