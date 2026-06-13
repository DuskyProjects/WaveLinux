import {
  Activity,
  AudioLines,
  ArrowDown,
  ArrowUp,
  BadgeCheck,
  Cable,
  Check,
  Chrome,
  CircleAlert,
  CircleMinus,
  CirclePlus,
  Clapperboard,
  Clipboard,
  Copy,
  Cpu,
  Download,
  ExternalLink,
  Gauge,
  Gamepad2,
  GitBranch,
  GripVertical,
  Headphones,
  Info,
  Keyboard,
  Maximize2,
  MessageCircle,
  Mic,
  Minimize2,
  Monitor,
  MonitorSpeaker,
  Music2,
  Pencil,
  Radio,
  RefreshCw,
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
import {
  allUiThemes,
  loadStoredThemeId,
  normalizeFileUiThemes,
  resolveUiTheme,
  saveStoredThemeId,
  themeToStyle,
  type UiThemeDefinition,
} from "./themes";
import type {
  AppStateSnapshot,
  AppMatcher,
  AppStream,
  AppVolumePreset,
  Channel,
  ChannelKind,
  CommandExecution,
  Diagnostic,
  ElgatoDeviceSummary,
  ElgatoWaveXlrState,
  EffectPluginInstallResult,
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
  RouteHealthIssue,
  SoundCheckReport,
  StreamerAction,
  StreamerActionResult,
  StreamerBinding,
  StreamerBindingProfile,
  StreamerDeviceSummary,
  StreamerDevicesConfig,
  StreamerLearnResult,
  UpdateInfo,
  UpdateInstallResult,
} from "./types";

type View = "mixer" | "routing" | "effects" | "settings";

const views: Array<{ id: View; label: string; icon: typeof SlidersHorizontal }> = [
  { id: "mixer", label: "Mixer", icon: SlidersHorizontal },
  { id: "routing", label: "Routing", icon: GitBranch },
  { id: "effects", label: "Effects", icon: Sparkles },
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
const MAX_MIXES = 5;
const AUTO_MONITOR_OUTPUT_VALUE = "__auto_monitor_output__";
const CLEAR_MIX_OUTPUTS_VALUE = "__clear_mix_outputs__";
const MIX_TEMPLATE_NAMES = ["Personal", "Chat", "Stream"];
type IconOption = { id: string; label: string; icon: typeof SlidersHorizontal };
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
const SELECT_VISIBLE_OPTION_LIMIT = 80;
const ELGATO_POLL_MS = 1500;
const STREAMER_DEVICE_POLL_MS = 1500;
const LIVE_METER_POLL_MS = 16;
const IDLE_METER_POLL_MS = 250;
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

type AutoDevices = AppStateSnapshot["graph"]["auto_devices"];

type LatestNumberQueue = {
  inFlight: boolean;
  latest: number | null;
};

type UiThemePreference = {
  theme_id: string;
};

type MergeTarget = {
  matcher: AppMatcher;
  displayName: string;
  meta: string;
};

type SourceCandidate = {
  id: string;
  label: string;
  meta: string;
  kind: ChannelKind;
  sourceDevice?: string;
  streamId?: string;
};

type SelectOption = {
  value: string;
  label: string;
  disabled?: boolean;
};

type SettingsTab = "general" | "profiles" | "streamers" | "elgato" | "health";

type MixerDrawer =
  | { type: "routing" }
  | { type: "effects"; channelId: string }
  | { type: "mix"; mixId: string }
  | { type: "source"; channelId: string };

type SetEffectChain = (channelId: string, effects: EffectInstance[]) => Promise<Channel>;

function defaultMixBus(enabled = true): MixBus {
  return { volume: 1, muted: false, enabled };
}

export default function App() {
  const [state, setState] = useState<AppStateSnapshot | null>(() => initialSnapshot());
  const [activeView, setActiveView] = useState<View>(() => initialView());
  const [selectedChannelId, setSelectedChannelId] = useState("hardware_in");
  const [busy, setBusy] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [audioActionReport, setAudioActionReport] = useState<AudioActionReport | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [pluginInstallBusy, setPluginInstallBusy] = useState(false);
  const autoUpdateCheckStarted = useRef(false);
  const refreshTimer = useRef<ReturnType<typeof window.setTimeout> | null>(null);
  const refreshInFlight = useRef(false);
  const refreshQueued = useRef(false);
  const themeChangedByUser = useRef(false);
  const mixVolumeQueues = useRef<Record<string, LatestNumberQueue>>({});
  const channelVolumeQueues = useRef<Record<string, LatestNumberQueue>>({});
  const activeThemeTokenKeys = useRef<string[]>([]);
  const settingsQueue = useRef<{ inFlight: boolean; latest: MixerSettings | null }>({
    inFlight: false,
    latest: null,
  });
  const [customThemes, setCustomThemes] = useState<UiThemeDefinition[]>([]);
  const [activeThemeId, setActiveThemeId] = useState(() => loadStoredThemeId());
  const uiThemes = useMemo(() => allUiThemes(customThemes), [customThemes]);
  const activeTheme = useMemo(
    () => resolveUiTheme(activeThemeId, customThemes),
    [activeThemeId, customThemes],
  );

  const persistUiThemePreference = useCallback((themeId: string) => {
    saveStoredThemeId(themeId);
    void invoke<UiThemePreference>("set_ui_theme_preference", { themeId, theme_id: themeId }).catch(() => undefined);
  }, []);

  const setUiTheme = useCallback((themeId: string) => {
    themeChangedByUser.current = true;
    setActiveThemeId(themeId);
    persistUiThemePreference(themeId);
  }, [persistUiThemePreference]);

  useEffect(() => {
    if (typeof document === "undefined") return;
    const root = document.documentElement;
    for (const key of activeThemeTokenKeys.current) {
      root.style.removeProperty(key);
    }
    const style = themeToStyle(activeTheme) as Record<string, string>;
    const keys = Object.keys(style);
    for (const key of keys) {
      root.style.setProperty(key, style[key]);
    }
    activeThemeTokenKeys.current = keys;
    root.dataset.wlSurface = activeTheme.surface;
    root.dataset.wlThemeVariant = activeTheme.variant;
  }, [activeTheme]);

  useEffect(() => {
    let cancelled = false;
    invoke<UiThemePreference | null>("get_ui_theme_preference")
      .then((preference) => {
        if (cancelled || themeChangedByUser.current || !preference?.theme_id) return;
        setActiveThemeId(preference.theme_id);
        saveStoredThemeId(preference.theme_id);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  const reloadUiThemes = useCallback(async () => {
    const themes = await invoke<unknown>("list_ui_themes");
    setCustomThemes(normalizeFileUiThemes(themes));
  }, []);

  const openThemeFolder = useCallback(async () => {
    await invoke("open_ui_theme_folder");
  }, []);

  useEffect(() => {
    void reloadUiThemes().catch(() => undefined);
  }, [reloadUiThemes]);

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

  // Run quick backend actions while coalescing the follow-up refresh.
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
            const bus = channel.mix_buses[mixId] ?? defaultMixBus(false);
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

  const flushMixVolumeQueue = useCallback(
    (mixId: string) => {
      const queue = mixVolumeQueues.current[mixId];
      if (!queue || queue.inFlight || queue.latest === null) return;
      const volume = queue.latest;
      queue.latest = null;
      queue.inFlight = true;
      void invoke<Mix>("set_mix_volume", { mixId, volume })
        .then((mix) => {
          if (queue.latest === null) {
            patchMixVolume(mix.id, mix.volume);
          }
        })
        .catch((error) => {
          setToast(String(error));
          void refresh().catch(() => undefined);
        })
        .finally(() => {
          queue.inFlight = false;
          if (queue.latest !== null) {
            flushMixVolumeQueue(mixId);
          }
        });
    },
    [patchMixVolume, refresh],
  );

  const setMixVolumeFast = useCallback(
    async (mixId: string, volume: number) => {
      patchMixVolume(mixId, volume);
      const queue = mixVolumeQueues.current[mixId] ?? { inFlight: false, latest: null };
      mixVolumeQueues.current[mixId] = queue;
      queue.latest = volume;
      flushMixVolumeQueue(mixId);
    },
    [flushMixVolumeQueue, patchMixVolume],
  );

  const flushChannelVolumeQueue = useCallback(
    (channelId: string, mixId: string) => {
      const key = `${channelId}\u0000${mixId}`;
      const queue = channelVolumeQueues.current[key];
      if (!queue || queue.inFlight || queue.latest === null) return;
      const volume = queue.latest;
      queue.latest = null;
      queue.inFlight = true;
      void invoke<MixBus>("set_channel_volume", { channelId, mixId, volume })
        .then((bus) => {
          if (queue.latest === null) {
            patchChannelBusVolume(channelId, mixId, bus.volume);
          }
        })
        .catch((error) => {
          setToast(String(error));
          void refresh().catch(() => undefined);
        })
        .finally(() => {
          queue.inFlight = false;
          if (queue.latest !== null) {
            flushChannelVolumeQueue(channelId, mixId);
          }
        });
    },
    [patchChannelBusVolume, refresh],
  );

  const setChannelBusVolumeFast = useCallback(
    async (channelId: string, mixId: string, volume: number) => {
      patchChannelBusVolume(channelId, mixId, volume);
      const key = `${channelId}\u0000${mixId}`;
      const queue = channelVolumeQueues.current[key] ?? { inFlight: false, latest: null };
      channelVolumeQueues.current[key] = queue;
      queue.latest = volume;
      flushChannelVolumeQueue(channelId, mixId);
    },
    [flushChannelVolumeQueue, patchChannelBusVolume],
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

  const setMixIconFast = useCallback(
    async (mixId: string, icon: string | null) => {
      patchMix(mixId, { icon });
      try {
        const mix = await invoke<Mix>("set_mix_icon", {
          mixId,
          mix_id: mixId,
          icon,
        });
        patchMix(mix.id, { icon: mix.icon ?? null });
        scheduleRefresh();
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, refresh, scheduleRefresh],
  );

  const setChannelIconFast = useCallback(
    async (channelId: string, icon: string | null) => {
      patchChannel(channelId, { icon });
      try {
        const channel = await invoke<Channel>("set_channel_icon", {
          channelId,
          channel_id: channelId,
          icon,
        });
        patchChannel(channel.id, { icon: channel.icon ?? null });
        scheduleRefresh();
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannel, refresh, scheduleRefresh],
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

  const setChannelBusEnabledFast = useCallback(
    async (channelId: string, mixId: string, enabled: boolean) => {
      patchChannelBus(channelId, mixId, { enabled });
      try {
        const bus = await invoke<MixBus>("set_channel_bus_enabled", {
          channelId,
          channel_id: channelId,
          mixId,
          mix_id: mixId,
          enabled,
        });
        patchChannelBus(channelId, mixId, { enabled: bus.enabled, muted: bus.muted, volume: bus.volume });
        scheduleRefresh();
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannelBus, refresh, scheduleRefresh],
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

  const setEffectChainFast = useCallback<SetEffectChain>(
    async (channelId, effects) => {
      patchChannel(channelId, { effects });
      try {
        const channel = await invoke<Channel>("set_effect_chain", { channelId, effects });
        patchChannel(channel.id, { effects: channel.effects });
        scheduleRefresh();
        return channel;
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
        throw error;
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
      patchMix(mixId, { monitor_output: output, output_devices: output ? [output] : [] });
      patchSettingsFromPartial({ monitor_follows_default_output: false });
      try {
        const mix = await invoke<Mix>("set_mix_monitor_output", { mixId, output });
        patchMix(mix.id, {
          monitor_output: mix.monitor_output ?? null,
          output_devices: mixOutputDevices(mix),
        });
        scheduleRefresh();
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, patchSettingsFromPartial, refresh, scheduleRefresh],
  );

  const setMixOutputsFast = useCallback(
    async (mixId: string, outputs: string[]) => {
      const cleanOutputs = Array.from(new Set(outputs.map((output) => output.trim()).filter(Boolean)));
      patchMix(mixId, {
        monitor_output: cleanOutputs[0] ?? null,
        output_devices: cleanOutputs,
      });
      if (mixId === "monitor") {
        patchSettingsFromPartial({ monitor_follows_default_output: false });
      }
      try {
        const mix = await invoke<Mix>("set_mix_outputs", {
          mixId,
          mix_id: mixId,
          outputs: cleanOutputs,
        });
        patchMix(mix.id, {
          monitor_output: mix.monitor_output ?? null,
          output_devices: mixOutputDevices(mix),
        });
        scheduleRefresh();
      } catch (error) {
        setToast(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, patchSettingsFromPartial, refresh, scheduleRefresh],
  );

  const flushSettingsQueue = useCallback(() => {
    const queue = settingsQueue.current;
    if (queue.inFlight || queue.latest === null) return;
    const settings = queue.latest;
    queue.latest = null;
    queue.inFlight = true;
    void invoke<MixerSettings>("set_settings", { settings })
      .then((next) => {
        if (queue.latest === null) {
          patchSettings(next);
          scheduleRefresh();
          setToast("Settings updated");
        }
      })
      .catch((error) => {
        setToast(String(error));
        void refresh().catch(() => undefined);
      })
      .finally(() => {
        queue.inFlight = false;
        if (queue.latest !== null) {
          flushSettingsQueue();
        }
      });
  }, [patchSettings, refresh, scheduleRefresh]);

  const setSettingsFast = useCallback(
    async (settings: MixerSettings) => {
      patchSettings(settings);
      settingsQueue.current.latest = settings;
      flushSettingsQueue();
    },
    [flushSettingsQueue, patchSettings],
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
      const releaseChannel = state?.config.settings.release_channel ?? "stable";
      const next = await invoke<UpdateInfo>("check_for_updates", { releaseChannel });
      setUpdateInfo(next);
      if (showToast || next.available) setToast(next.message);
      if (next.available && state?.config.settings.auto_install_updates && next.install_supported) {
        const result = await invoke<UpdateInstallResult>("install_update", { releaseChannel });
        setToast(result.message);
      }
      return next;
    } catch (error) {
      if (showToast) setToast(String(error));
      throw error;
    } finally {
      setUpdateBusy(false);
    }
  }, [state?.config.settings.auto_install_updates, state?.config.settings.release_channel]);

  const installEffectPlugins = useCallback(async () => {
    setPluginInstallBusy(true);
    try {
      const result = await invoke<EffectPluginInstallResult>("install_effect_plugins");
      setToast(result.message);
      await refresh().catch(() => undefined);
    } catch (error) {
      setToast(String(error));
    } finally {
      setPluginInstallBusy(false);
    }
  }, [refresh]);

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
  const isWaveLinkSurface = activeTheme.surface === "wavelink3";
  const workspace = !state ? (
    <div className="loading-panel">Starting audio engine</div>
  ) : (
    <>
      {activeView === "mixer" && (
        isWaveLinkSurface ? (
          <WaveLinkMixerView
            busy={busy}
            run={run}
            selectedChannelId={selectedChannelId}
            setActiveView={setActiveView}
            setChannelBusMute={setChannelBusMuteFast}
            setChannelBusEnabled={setChannelBusEnabledFast}
            setChannelBusVolume={setChannelBusVolumeFast}
            setChannelInput={setChannelInputFast}
            setMixOutputs={setMixOutputsFast}
            setMixIcon={setMixIconFast}
            setMixMute={setMixMuteFast}
            setMixVolume={setMixVolumeFast}
            setEffectChain={setEffectChainFast}
            setChannelIcon={setChannelIconFast}
            setAppStreamMute={setAppStreamMuteFast}
            setSelectedChannelId={setSelectedChannelId}
            setSettings={setSettingsFast}
            state={state}
          />
        ) : (
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
        )
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
          setEffectChain={setEffectChainFast}
        />
      )}
      {activeView === "settings" && (
        <SettingsView
          audioActionReport={audioActionReport}
          state={state}
          run={run}
          setSettings={setSettingsFast}
          updateBusy={updateBusy}
          updateInfo={updateInfo}
          onCheckUpdates={() => void checkUpdates(true).catch(() => undefined)}
          onInstallUpdate={() => {
            setUpdateBusy(true);
            const releaseChannel = state.config.settings.release_channel;
            invoke<UpdateInstallResult>("install_update", { releaseChannel })
              .then((result) => setToast(result.message))
              .catch((error) => setToast(String(error)))
              .finally(() => setUpdateBusy(false));
          }}
          onOpenReleases={() => {
            const releaseChannel = state.config.settings.release_channel;
            void invoke("open_release_page", { releaseChannel }).catch((error) => setToast(String(error)));
          }}
          onPrune={() => runAudioCommandList("cleanup_stale_audio_graph", "Prune Stale Audio")}
          onInstallEffectPlugins={() => void installEffectPlugins()}
          pluginInstallBusy={pluginInstallBusy}
          activeThemeId={activeTheme.id}
          onOpenThemeFolder={() => void openThemeFolder().catch((error) => setToast(String(error)))}
          onReloadThemes={() => void reloadUiThemes().catch((error) => setToast(String(error)))}
          onThemeChange={setUiTheme}
          themes={uiThemes}
        />
      )}
    </>
  );
  const topActions = (
    <div className="top-actions">
      <button className="icon-button" onClick={() => refresh()} title="Refresh" type="button">
        <RefreshCw size={17} />
      </button>
    </div>
  );

  if (isWaveLinkSurface) {
    return (
      <div
        className={activeTheme.variant === "dark" ? "wl-shell dark" : "wl-shell"}
        style={themeToStyle(activeTheme)}
      >
        <aside className="wl-rail">
          <div className="wl-brand" title="WaveLinux">
            <AudioLines size={22} />
            <span>WL</span>
          </div>
          <nav className="wl-nav" aria-label="WaveLinux sections">
            {views.map((view) => {
              const Icon = view.icon;
              return (
                <button
                  className={activeView === view.id ? "wl-nav-item active" : "wl-nav-item"}
                  key={view.id}
                  onClick={() => setActiveView(view.id)}
                  title={view.label}
                  type="button"
                >
                  <Icon size={19} />
                  <span>{view.label}</span>
                </button>
              );
            })}
          </nav>
          <div
            className={state?.engine.audio_graph_running ? "wl-engine-pill running" : "wl-engine-pill"}
            title={state?.engine.message ?? "Starting"}
          >
            {state?.engine.healthy ? <BadgeCheck size={16} /> : <CircleAlert size={16} />}
          </div>
        </aside>
        <main className="wl-main">
          <header className="wl-topbar">
            <div>
              <p>WaveLinux</p>
              <h1>{viewTitle(activeView)}</h1>
            </div>
            {topActions}
          </header>
          <div className={activeView === "mixer" ? "wl-workspace mixer" : "wl-workspace"}>
            {workspace}
          </div>
        </main>
        {toast && <div className="toast">{toast}</div>}
      </div>
    );
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">
            <AudioLines size={22} />
          </div>
          <div>
            <strong>WaveLinux</strong>
            <span>4.2</span>
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
          {topActions}
        </header>

        {workspace}
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
      if (!documentHasActiveFocus()) {
        setLiveMeters([]);
        timer = window.setTimeout(tick, IDLE_METER_POLL_MS);
        return;
      }
      invoke<LevelMeter[]>("observe_meters")
        .then((meters) => {
          if (!stopped) setLiveMeters(meters);
        })
        .catch(() => undefined)
        .finally(() => {
          if (!stopped) timer = window.setTimeout(tick, LIVE_METER_POLL_MS);
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
                  autoDevices={state.graph.auto_devices}
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

function WaveLinkMixerView({
  busy,
  run,
  selectedChannelId,
  setActiveView,
  setAppStreamMute,
  setChannelBusEnabled,
  setChannelBusMute,
  setChannelBusVolume,
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
  const [liveMeters, setLiveMeters] = useState<LevelMeter[]>(state.graph.meters);
  const [sourceCreatorOpen, setSourceCreatorOpen] = useState(false);
  const [mixCreatorOpen, setMixCreatorOpen] = useState(false);
  const mixerDensityTouched = useRef(false);
  const [matrixCollapsed, setMatrixCollapsed] = useState(prefersCompactWaveLinkMixer);
  const [drawer, setDrawer] = useState<MixerDrawer | null>(null);

  useEffect(() => {
    setLiveMeters(state.graph.meters);
  }, [state.graph.meters]);

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

  useEffect(() => {
    if (!state.engine.audio_graph_running) {
      setLiveMeters([]);
      return;
    }

    let stopped = false;
    let timer = 0;
    const tick = () => {
      if (!documentHasActiveFocus()) {
        setLiveMeters([]);
        timer = window.setTimeout(tick, IDLE_METER_POLL_MS);
        return;
      }
      invoke<LevelMeter[]>("observe_meters")
        .then((meters) => {
          if (!stopped) setLiveMeters(meters);
        })
        .catch(() => undefined)
        .finally(() => {
          if (!stopped) timer = window.setTimeout(tick, LIVE_METER_POLL_MS);
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
                  vuLevel={levelFor(mix.id)}
                />
              ))}

              {state.config.channels.map((channel) => (
                <WaveLinkSourceRow
                  channel={channel}
                  appStreams={streamsByChannelId.get(channel.id) ?? []}
                  autoDevices={state.graph.auto_devices}
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
                  setSelectedChannelId={selectMixerChannel}
                  sourceVuLevel={levelFor(channel.id)}
                  vuForBus={(mix, bus) => channelBusVuLevel(channel, mix, bus, levelFor)}
                />
              ))}
            </div>
          </div>
        </div>

        {drawer && (
          <>
          <button
            aria-label="Close mixer drawer"
            className="wl-drawer-scrim"
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
                  x
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
                      x
                    </button>
                  </div>
                </div>
                <WaveLinkEffectsEditor
                  channel={channel}
                  className="wl-drawer-body"
                  setChannelInput={setChannelInput}
                  setEffectChain={setEffectChain}
                  state={state}
                />
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
                vuLevel={levelFor(mix.id)}
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
          </>
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
  vuLevel,
}: {
  autoDevices: AutoDevices;
  mix: Mix;
  onOpenSettings: () => void;
  outputs: DeviceInfo[];
  setMixMute: (mixId: string, muted: boolean) => Promise<void>;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
  settings: MixerSettings;
  vuLevel: number;
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
          title={`Mute ${mix.name}`}
          type="button"
        >
          {mix.muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
        </button>
      </div>
      <WaveLinkMasterControl
        mix={mix}
        setMixVolume={setMixVolume}
        vuLevel={vuLevel}
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
  vuLevel,
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
  vuLevel: number;
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
            x
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
          <WaveLinkMasterControl mix={mix} setMixVolume={setMixVolume} vuLevel={vuLevel} />
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
  vuLevel,
}: {
  mix: Mix;
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
    <label className="wl-master-control">
      <span>Master</span>
      <div className="wl-horizontal-meter">
        <div className="wl-horizontal-meter-fill" style={{ width: trackSize(vuLevel) }} />
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
  isSelected,
  microphoneInputs,
  mixes,
  onOpenSettings,
  openEffects,
  setChannelBusEnabled,
  setChannelBusMute,
  setChannelBusVolume,
  setSelectedChannelId,
  sourceVuLevel,
  vuForBus,
}: {
  appStreams: AppStream[];
  autoDevices: AutoDevices;
  channel: Channel;
  isSelected: boolean;
  microphoneInputs: AppStateSnapshot["graph"]["inputs"];
  mixes: Mix[];
  onOpenSettings: () => void;
  openEffects: () => void;
  setChannelBusEnabled: (channelId: string, mixId: string, enabled: boolean) => Promise<void>;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
  setSelectedChannelId: (channelId: string) => void;
  sourceVuLevel: number;
  vuForBus: (mix: Mix, bus: MixBus) => number;
}) {
  const Icon = channelIconComponent(channel);
  const isHardware = isHardwareChannel(channel);
  const displayName = channelDisplayName(channel);
  const activeEffectCount = channel.effects.filter((effect) => !effect.bypassed).length;

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
          <div className="wl-source-meter-fill" style={{ width: trackSize(sourceVuLevel) }} />
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
            className="mini-icon-button fx-led-button"
            onClick={(event) => {
              event.stopPropagation();
              openEffects();
            }}
            title={activeEffectCount > 0 ? `${activeEffectCount} active effects` : "No active effects"}
            type="button"
          >
            <Sparkles size={14} />
            <span className={activeEffectCount > 0 ? "fx-led active" : "fx-led"} aria-hidden="true" />
          </button>
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
            vuLevel={vuForBus(mix, bus)}
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
            x
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
  vuLevel,
}: {
  bus: MixBus;
  channel: Channel;
  mix: Mix;
  setChannelBusEnabled: (channelId: string, mixId: string, enabled: boolean) => Promise<void>;
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
        <div className="wl-send-meter-fill" style={{ width: trackSize(vuLevel) }} />
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
          title={`Mute ${channelDisplayName(channel)} in ${mix.name}`}
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
  levelFor,
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
            bus={channel.mix_buses[mix.id] ?? defaultMixBus()}
            channel={channel}
            key={mix.id}
            mix={mix}
            setChannelBusMute={setChannelBusMute}
            setChannelBusVolume={setChannelBusVolume}
            vuLevel={channelBusVuLevel(
              channel,
              mix,
              channel.mix_buses[mix.id] ?? defaultMixBus(),
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

function WaveLinkEffectsEditor({
  channel,
  className,
  setChannelInput,
  setEffectChain,
  state,
}: {
  channel?: Channel;
  className?: string;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  setEffectChain: SetEffectChain;
  state: AppStateSnapshot;
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
  const selectedEffects = channel
    ? draftEffectsByChannel[channel.id] ?? channel.effects
    : [];

  useEffect(() => {
    setDraftEffectsByChannel((current) => {
      let changed = false;
      const next = { ...current };
      for (const source of state.config.channels) {
        const draft = next[source.id];
        if (draft && effectChainsEqual(draft, source.effects)) {
          delete next[source.id];
          changed = true;
        }
      }
      return changed ? next : current;
    });
  }, [state.config.channels]);

  const updateEffects = (effects: EffectInstance[], message?: string, preferredInstanceId?: string) => {
    if (!channel) return;
    const channelId = channel.id;
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
    void setEffectChain(channelId, optimisticEffects)
      .then((nextChannel) => {
        if (effectWriteGeneration.current[channelId] !== writeGeneration) return;
        setDraftEffectsByChannel((current) => ({
          ...current,
          [channelId]: nextChannel.effects,
        }));
        if (message) setEffectError(null);
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
    if (!channel) return;
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
    if (!channel) return;
    updateEffects(
      selectedEffects.map((effect) =>
        effect.instance_id === instanceId
          ? { ...effect, params: { ...effect.params, ...values } }
          : effect,
      ),
      "Preset applied",
    );
  };

  const updateEffectParam = (instanceId: string, paramId: string, value: number) => {
    if (!channel) return;
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
    if (!channel) return;
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
    if (!channel || target < 0 || target >= selectedEffects.length) return;
    const effects = [...selectedEffects];
    [effects[index], effects[target]] = [effects[target], effects[index]];
    updateEffects(effects, "Effect reordered");
  };

  const renameEffect = (instanceId: string) => {
    if (!channel) return;
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
    if (!channel || !effectClipboard) return;
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
    updateEffects([...selectedEffects, pastedEffect], "Effect pasted", pastedEffect.instance_id);
  };

  const deleteEffect = (instanceId: string) => {
    if (!channel) return;
    updateEffects(
      selectedEffects.filter((effect) => effect.instance_id !== instanceId),
      "Effect removed",
    );
  };

  return (
    <div className={["wl-effects-editor-stack", className].filter(Boolean).join(" ")}>
      <div className="panel wl-effect-chain-panel">
        <div className="panel-header">
          <h2>{channel ? channelDisplayName(channel) : "Effects"}</h2>
          <div className="panel-actions">
            <button
              className="secondary-button"
              disabled={!channel || state.catalog.effects.length === 0}
              onClick={() => {
                if (!state.catalog.effects[0]) return;
                addEffect(state.catalog.effects[0]);
              }}
              type="button"
            >
              <CirclePlus size={16} />
              Add
            </button>
            <button
              className="secondary-button"
              disabled={!channel || !effectClipboard}
              onClick={pasteEffect}
              type="button"
              title="Paste copied effect"
            >
              <Clipboard size={16} />
              Paste
            </button>
          </div>
        </div>
        {(pendingEffectWrites[channel?.id ?? ""] ?? 0) > 0 && (
          <div className="effect-sync-status">Syncing effect chain...</div>
        )}
        {effectError && (
          <div className="effect-warning">
            <CircleAlert size={15} />
            <span>{effectError}</span>
          </div>
        )}
        {channel && isHardwareChannel(channel) && (
          <div className="hardware-source-card">
            <label className="field-label" htmlFor={`effects-microphone-source-${channel.id}`}>
              Microphone
            </label>
            <AppSelect
              ariaLabel="Microphone"
              id={`effects-microphone-source-${channel.id}`}
              onChange={(value) =>
                void setChannelInput(channel.id, value || null).catch(() => undefined)
              }
              options={[
                { value: "", label: autoMicrophoneLabel(microphoneInputs, "Auto hardware input", state.graph.auto_devices, channel.id) },
                ...microphoneInputs.map((input) => ({
                  value: input.id,
                  label: input.description,
                })),
              ]}
              value={channel.source_device ?? ""}
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
      <div className="panel catalog-panel wl-effects-catalog-panel">
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
            const itemClassName = [
              "catalog-item",
              isEnabled ? "enabled" : "",
              isPresent && !isEnabled ? "bypassed" : "",
            ]
              .filter(Boolean)
              .join(" ");
            return (
              <button
                className={itemClassName}
                disabled={!channel}
                key={effect.id}
                onClick={() => addEffect(effect)}
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
    </div>
  );
}

function EffectsView({
  state,
  selectedChannel,
  selectedChannelId,
  setSelectedChannelId,
  setChannelInput,
  setEffectChain,
}: {
  state: AppStateSnapshot;
  selectedChannel?: Channel;
  selectedChannelId: string;
  setSelectedChannelId: (channelId: string) => void;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  setEffectChain: SetEffectChain;
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
    void setEffectChain(channelId, optimisticEffects)
      .then((channel) => {
        if (effectWriteGeneration.current[channelId] !== writeGeneration) return;
        setDraftEffectsByChannel((current) => ({
          ...current,
          [channelId]: channel.effects,
        }));
        if (message) {
          // Keep effect edits local so they do not block on global refresh.
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
          {state.config.channels.map((channel) => {
            const effects = draftEffectsByChannel[channel.id] ?? channel.effects;
            const activeEffectCount = effects.filter((effect) => !effect.bypassed).length;
            const effectTitle =
              activeEffectCount > 0
                ? `${activeEffectCount} active effect${activeEffectCount === 1 ? "" : "s"}`
                : "No active effects";
            return (
              <button
                className={channel.id === selectedChannelId ? "picker-row active" : "picker-row"}
                key={channel.id}
                onClick={() => setSelectedChannelId(channel.id)}
                title={`${channelDisplayName(channel)} · ${effectTitle}`}
                type="button"
              >
                <span>{channelDisplayName(channel)}</span>
                <span
                  aria-hidden="true"
                  className={activeEffectCount > 0 ? "fx-led active" : "fx-led"}
                />
              </button>
            );
          })}
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
                { value: "", label: autoMicrophoneLabel(microphoneInputs, "Auto hardware input", state.graph.auto_devices, selectedChannel.id) },
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

function DiagnosticsView({
  audioActionReport,
  onInstallEffectPlugins,
  onPrune,
  pluginInstallBusy,
  state,
  updateInfo,
  run,
}: {
  audioActionReport: AudioActionReport | null;
  onInstallEffectPlugins: () => void;
  onPrune: () => void | Promise<unknown>;
  pluginInstallBusy: boolean;
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
        <EffectAvailabilitySummary
          installBusy={pluginInstallBusy}
          onInstallMissing={onInstallEffectPlugins}
          state={state}
        />
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

function TestingHealthReport({
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

function buildTestingHealthReport({
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
    "",
    "## Audio Settings",
    `Sample rate: ${state.config.audio.sample_rate_hz}`,
    `Bit depth: ${state.config.audio.bit_depth}`,
    `Channel layout: ${state.config.audio.channel_layout}`,
    `Mono inputs to stereo: ${yesNo(state.config.audio.mono_inputs_to_stereo)}`,
    `Low-latency monitoring: ${yesNo(settings.low_latency_mic_monitoring)}`,
    `Hardware direct mic monitoring: ${yesNo(settings.hardware_direct_mic_monitoring)}`,
    `Stream sync delay: ${settings.stream_sync_delay_msec} ms`,
    `Monitor sync delay: ${settings.monitor_sync_delay_msec} ms`,
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
    ...(streamerDeviceError ? [`Detection error: ${streamerDeviceError}`] : reportStreamerDevices(streamerDevices)),
    "",
    "## Elgato Devices",
    ...(elgatoDeviceError ? [`Detection error: ${elgatoDeviceError}`] : reportElgatoDevices(elgatoDevices)),
    "",
    "## Diagnostics",
    ...reportDiagnostics(diagnostics),
    "",
    "## Effects",
    ...(missingEffects.length ? missingEffects.map((effect) => `- Missing: ${effect}`) : ["- Missing: none"]),
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
    const usb = device.vendor_id || device.product_id ? ` usb=${valueOrNone(device.vendor_id)}:${valueOrNone(device.product_id)}` : "";
    const profile = device.matched_profile_id || device.active_profile || "none";
    const defaultState = device.is_default ? " default" : "";
    const virtualState = device.is_virtual ? " virtual" : "";
    return `- ${device.description || device.name} | id=${device.id} | available=${yesNo(device.is_available)}${defaultState}${virtualState} | bus=${valueOrNone(device.bus)}${usb} | profile=${profile}`;
  });
}

function reportStreamerDevices(devices: StreamerDeviceSummary[]): string[] {
  if (devices.length === 0) return ["- none detected"];
  return devices.map((device) => {
    const usb = device.vendor_id || device.product_id ? ` | usb=${valueOrNone(device.vendor_id)}:${valueOrNone(device.product_id)}` : "";
    return `- ${device.name} | ${device.family}/${device.transport} | enabled=${yesNo(device.enabled)} | status=${device.permission_status}${usb} | caps=${formatStreamerCaps(device)} | ${device.message || "no message"}`;
  });
}

function reportElgatoDevices(devices: ElgatoDeviceSummary[]): string[] {
  if (devices.length === 0) return ["- none detected"];
  return devices.map((device) => {
    const usb = device.vendor_id || device.product_id ? ` | usb=${valueOrNone(device.vendor_id)}:${valueOrNone(device.product_id)}` : "";
    return `- ${device.name} | ${device.kind} | controls=${yesNo(device.controls_supported)} | bus=${valueOrNone(device.bus)}${usb} | alsa_card=${valueOrNone(device.alsa_card)} | ${device.message || "no message"}`;
  });
}

function reportDiagnostics(diagnostics: Diagnostic[]): string[] {
  if (diagnostics.length === 0) return ["- none"];
  return diagnostics.map((item) => `- [${item.severity}] ${item.code}: ${item.message}${item.action ? ` (${item.action})` : ""}`);
}

function reportRecentLog(report: SoundCheckReport | null, graphReport: GraphDebugReport | null): string[] {
  const lines = graphReport?.recent_log_lines.length
    ? graphReport.recent_log_lines
    : report?.recent_log_lines ?? [];
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

function LatencySummary({ state }: { state: AppStateSnapshot }) {
  const latencySensitiveFx = state.config.channels.flatMap((channel) =>
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
        <div className={latencySensitiveFx.length ? "command-pill info" : "command-pill"}>
          {latencySensitiveFx.length ? "DSP Active" : "Low"}
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
      {latencySensitiveFx.length > 0 && (
        <div className="latency-note info">
          <Info size={15} />
          <span>{latencySensitiveFx.join(", ")}</span>
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

function EffectAvailabilitySummary({
  installBusy,
  onInstallMissing,
  state,
}: {
  installBusy: boolean;
  onInstallMissing: () => void;
  state: AppStateSnapshot;
}) {
  const availabilityById = new Map(state.graph.effect_availability.map((item) => [item.effect_id, item]));
  const available = state.graph.effect_availability.filter((item) => item.available).length;
  const total = state.catalog.effects.length;
  const missing = total - available;

  return (
    <div className="fx-availability">
      <div className="command-report-header">
        <div>
          <strong>Effect Availability</strong>
          <span>{available}/{total} open DSP replacements detected</span>
        </div>
        <div className="panel-actions">
          {missing > 0 && (
            <button
              className="secondary-button"
              disabled={installBusy}
              onClick={onInstallMissing}
              title="Install missing LADSPA effect plugins with the system package manager"
              type="button"
            >
              <Download size={16} />
              {installBusy ? "Installing" : "Install FX"}
            </button>
          )}
          <div className={available === total ? "command-pill" : "command-pill warning"}>
            {available === total ? "Ready" : `${missing} missing`}
          </div>
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
  activeThemeId,
  audioActionReport,
  onOpenThemeFolder,
  onReloadThemes,
  onThemeChange,
  state,
  themes,
  run,
  setSettings,
  updateBusy,
  updateInfo,
  onCheckUpdates,
  onInstallUpdate,
  onInstallEffectPlugins,
  onOpenReleases,
  onPrune,
  pluginInstallBusy,
}: {
  activeThemeId: string;
  audioActionReport: AudioActionReport | null;
  onOpenThemeFolder: () => void;
  onReloadThemes: () => void;
  onThemeChange: (themeId: string) => void;
  state: AppStateSnapshot;
  themes: UiThemeDefinition[];
  run: <T>(command: string, args?: Record<string, unknown>, message?: string) => Promise<T>;
  setSettings: (settings: MixerSettings) => Promise<void>;
  updateBusy: boolean;
  updateInfo: UpdateInfo | null;
  onCheckUpdates: () => void;
  onInstallUpdate: () => void;
  onInstallEffectPlugins: () => void;
  onOpenReleases: () => void;
  onPrune: () => void | Promise<unknown>;
  pluginInstallBusy: boolean;
}) {
  const updateSettings = (settings: MixerSettings) => void setSettings(settings);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");
  const [streamerDevices, setStreamerDevices] = useState<StreamerDeviceSummary[]>([]);
  const [streamerDeviceError, setStreamerDeviceError] = useState<string | null>(null);
  const visibleUpdateInfo =
    updateInfo?.channel === state.config.settings.release_channel ? updateInfo : null;
  const betaUpdatesEnabled = state.config.settings.release_channel === "beta";
  const updateChannelLabel = betaUpdatesEnabled ? "Testing" : "Stable";
  const hasStreamerDevices = streamerDevices.length > 0;
  const hasElgatoDevices = useMemo(
    () => [...state.graph.inputs, ...state.graph.outputs].some(isElgatoAudioDevice),
    [state.graph.inputs, state.graph.outputs],
  );

  const loadStreamerDevices = useCallback(async () => {
    try {
      const devices = await invoke<StreamerDeviceSummary[]>("list_streamer_devices");
      setStreamerDevices(devices);
      setStreamerDeviceError(null);
    } catch (error) {
      setStreamerDevices([]);
      setStreamerDeviceError(String(error));
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    const tick = () => {
      if (cancelled) return;
      void loadStreamerDevices();
    };
    tick();
    const interval = window.setInterval(tick, STREAMER_DEVICE_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [loadStreamerDevices]);

  useEffect(() => {
    if (!hasElgatoDevices && settingsTab === "elgato") {
      setSettingsTab("general");
    }
    if (!hasStreamerDevices && settingsTab === "streamers") {
      setSettingsTab("general");
    }
  }, [hasElgatoDevices, hasStreamerDevices, settingsTab]);

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
          {hasStreamerDevices && (
            <button
              className={settingsTab === "streamers" ? "settings-tab active" : "settings-tab"}
              onClick={() => setSettingsTab("streamers")}
              role="tab"
              type="button"
            >
              <Keyboard size={16} />
              Streamers
            </button>
          )}
          {hasElgatoDevices && (
            <button
              className={settingsTab === "elgato" ? "settings-tab active" : "settings-tab"}
              onClick={() => setSettingsTab("elgato")}
              role="tab"
              type="button"
            >
              <Mic size={16} />
              Elgato
            </button>
          )}
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
            <div className="settings-control theme-file-control">
              <span>Interface</span>
              <AppSelect
                ariaLabel="Interface"
                onChange={onThemeChange}
                options={themes.map((theme) => ({
                  value: theme.id,
                  label: theme.builtin ? theme.name : `${theme.name} (custom)`,
                }))}
                value={activeThemeId}
              />
              <div className="theme-file-actions">
                <button className="secondary-button" onClick={onReloadThemes} type="button">
                  <RefreshCw size={16} />
                  Refresh
                </button>
                <button className="secondary-button" onClick={onOpenThemeFolder} type="button">
                  <ExternalLink size={16} />
                  Folder
                </button>
              </div>
            </div>
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
          </div>
          <div className="settings-section">
            <div className="panel-header compact">
              <h2>Updates</h2>
              <Download size={18} />
            </div>
            <div className="update-card">
              <div>
                <strong>{visibleUpdateInfo?.message ?? "Update status has not been checked"}</strong>
                <span>
                  {visibleUpdateInfo
                    ? `${updateChannelLabel} · current ${visibleUpdateInfo.current_version}${visibleUpdateInfo.version ? ` · latest ${visibleUpdateInfo.version}` : ""}`
                    : betaUpdatesEnabled
                      ? "Testing updates use the moving prerelease feed"
                      : "Signed AppImage updates, plus deb/rpm/AUR package releases"}
                </span>
                <label className="updater-checkbox" title="Use the WaveLinux Testing prerelease feed">
                  <input
                    checked={betaUpdatesEnabled}
                    onChange={(event) =>
                      updateSettings({
                        ...state.config.settings,
                        release_channel: event.currentTarget.checked ? "beta" : "stable",
                      })
                    }
                    type="checkbox"
                  />
                  <span>Beta updates</span>
                </label>
              </div>
              <div className="panel-actions">
                <button className="secondary-button" disabled={updateBusy} onClick={onCheckUpdates} type="button">
                  <RefreshCw size={16} />
                  Check
                </button>
                <button
                  className="secondary-button"
                  disabled={updateBusy || !visibleUpdateInfo?.available || !visibleUpdateInfo.install_supported}
                  onClick={onInstallUpdate}
                  title={
                    visibleUpdateInfo?.install_supported === false
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
              <Toggle
                label="Hardware direct mic monitor"
                onChange={(value) =>
                  updateSettings({ ...state.config.settings, hardware_direct_mic_monitoring: value })
                }
                value={state.config.settings.hardware_direct_mic_monitoring}
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
            <Stat icon={Cpu} label="Engine" value={state.engine.audio_graph_running ? "Running" : "Inactive"} />
            <Stat icon={Radio} label="Rate" value={`${state.config.audio.sample_rate_hz / 1000} kHz`} />
            <Stat icon={AudioLines} label="Format" value={`${state.config.audio.bit_depth}-bit`} />
          </div>
        </section>
      )}

      {settingsTab === "profiles" && <HardwareProfilesView state={state} />}

      {settingsTab === "streamers" && hasStreamerDevices && (
        <StreamerDevicesView
          devices={streamerDevices}
          deviceError={streamerDeviceError}
          onDevicesChange={setStreamerDevices}
          onRefresh={loadStreamerDevices}
          state={state}
        />
      )}

      {settingsTab === "elgato" && hasElgatoDevices && <ElgatoDevicesView />}

      {settingsTab === "health" && (
        <DiagnosticsView
          audioActionReport={audioActionReport}
          onInstallEffectPlugins={onInstallEffectPlugins}
          onPrune={onPrune}
          pluginInstallBusy={pluginInstallBusy}
          state={state}
          updateInfo={updateInfo}
          run={run}
        />
      )}
    </section>
  );
}

function StreamerDevicesView({
  devices,
  deviceError,
  onDevicesChange,
  onRefresh,
  state,
}: {
  devices: StreamerDeviceSummary[];
  deviceError: string | null;
  onDevicesChange: (devices: StreamerDeviceSummary[]) => void;
  onRefresh: () => Promise<void>;
  state: AppStateSnapshot;
}) {
  const [bindings, setBindings] = useState<StreamerDevicesConfig | null>(state.config.streamer_devices);
  const [selectedDeviceId, setSelectedDeviceId] = useState(devices[0]?.id ?? "");
  const [selectedBindingIndex, setSelectedBindingIndex] = useState(0);
  const [streamerError, setStreamerError] = useState<string | null>(null);
  const [streamerMessage, setStreamerMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const actionOptions = useMemo(() => streamerActionOptions(state), [state]);

  const loadBindings = useCallback(async () => {
    try {
      const next = await invoke<StreamerDevicesConfig>("get_streamer_bindings");
      setBindings(next);
      setStreamerError(null);
    } catch (error) {
      setStreamerError(String(error));
    }
  }, []);

  useEffect(() => {
    void loadBindings();
  }, [loadBindings, devices]);

  useEffect(() => {
    if (devices.length === 0) {
      setSelectedDeviceId("");
      return;
    }
    if (!selectedDeviceId || !devices.some((device) => device.id === selectedDeviceId)) {
      setSelectedDeviceId(devices[0].id);
    }
  }, [devices, selectedDeviceId]);

  const selectedDevice =
    devices.find((device) => device.id === selectedDeviceId) ?? devices[0] ?? null;
  const profile = selectedDevice
    ? bindings?.profiles[selectedDevice.id] ?? emptyStreamerProfile(selectedDevice)
    : null;
  const selectedBinding = profile?.bindings[selectedBindingIndex] ?? profile?.bindings[0] ?? null;
  const selectedDeviceBindable = selectedDevice ? streamerDeviceBindingsAvailable(selectedDevice) : false;

  useEffect(() => {
    if (!profile) return;
    if (selectedBindingIndex >= profile.bindings.length) {
      setSelectedBindingIndex(Math.max(0, profile.bindings.length - 1));
    }
  }, [profile, selectedBindingIndex]);

  const saveProfile = async (nextProfile: StreamerBindingProfile) => {
    setBusy(true);
    try {
      const saved = await invoke<StreamerBindingProfile>("set_streamer_binding_profile", {
        profile: nextProfile,
      });
      setBindings((current) => ({
        version: current?.version ?? 1,
        profiles: {
          ...(current?.profiles ?? {}),
          [saved.device_id]: saved,
        },
      }));
      setStreamerError(null);
    } catch (error) {
      setStreamerError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const setDeviceEnabled = async (device: StreamerDeviceSummary, enabled: boolean) => {
    setBusy(true);
    try {
      const next = await invoke<StreamerDevicesConfig>("set_streamer_device_enabled", {
        deviceId: device.id,
        device_id: device.id,
        enabled,
      });
      setBindings(next);
      onDevicesChange(
        devices.map((item) => (item.id === device.id ? { ...item, enabled } : item)),
      );
      setStreamerError(null);
    } catch (error) {
      setStreamerError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const updateBinding = (index: number, patch: Partial<StreamerBinding>) => {
    if (!profile) return;
    const bindings = profile.bindings.map((binding, bindingIndex) =>
      bindingIndex === index ? { ...binding, ...patch } : binding,
    );
    void saveProfile({ ...profile, safe_preset: false, bindings });
  };

  const addBinding = () => {
    if (!profile) return;
    const nextBinding: StreamerBinding = {
      control_id: `manual:${profile.bindings.length + 1}`,
      label: "New binding",
      control_kind: "button",
      action: { kind: "noop" },
    };
    setSelectedBindingIndex(profile.bindings.length);
    void saveProfile({
      ...profile,
      safe_preset: false,
      bindings: [...profile.bindings, nextBinding],
    });
  };

  const removeBinding = (index: number) => {
    if (!profile) return;
    const nextBindings = profile.bindings.filter((_, bindingIndex) => bindingIndex !== index);
    setSelectedBindingIndex(Math.max(0, Math.min(index, nextBindings.length - 1)));
    void saveProfile({ ...profile, safe_preset: false, bindings: nextBindings });
  };

  const learnBinding = async (index: number) => {
    if (!selectedDevice || !profile) return;
    setBusy(true);
    try {
      const result = await invoke<StreamerLearnResult>("learn_streamer_control", {
        deviceId: selectedDevice.id,
        device_id: selectedDevice.id,
      });
      setStreamerMessage(result.message);
      if (result.control_id) {
        updateBinding(index, {
          control_id: result.control_id,
          control_kind: result.control_kind,
        });
      }
    } catch (error) {
      setStreamerError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const testBinding = async (binding: StreamerBinding) => {
    setBusy(true);
    try {
      const result = await invoke<StreamerActionResult>("run_streamer_action_test", {
        action: binding.action,
      });
      setStreamerMessage(result.message);
      setStreamerError(null);
    } catch (error) {
      setStreamerError(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="panel single-panel">
      <div className="panel-header">
        <h2>Streamer Devices</h2>
        <div className="panel-actions">
          <button
            className="secondary-button"
            disabled={busy}
            onClick={() => void onRefresh()}
            type="button"
          >
            <RefreshCw size={16} />
            Refresh
          </button>
        </div>
      </div>
      {(deviceError || streamerError || streamerMessage) && (
        <div className={streamerError || deviceError ? "effect-warning" : "latency-note info"}>
          {streamerError || deviceError ? <CircleAlert size={15} /> : <Info size={15} />}
          <span>{streamerError ?? deviceError ?? streamerMessage}</span>
        </div>
      )}
      <div className="streamer-grid">
        <div className="streamer-device-list">
          {devices.map((device) => (
            <button
              className={device.id === selectedDevice?.id ? "streamer-device-row active" : "streamer-device-row"}
              key={device.id}
              onClick={() => setSelectedDeviceId(device.id)}
              type="button"
            >
              <div>
                <strong>{device.name}</strong>
                <span>{device.description}</span>
              </div>
              <div className="elgato-device-meta">
                <span>{streamerFamilyLabel(device.family)}</span>
                <span>{device.transport}</span>
                <span>{streamerPermissionLabel(device.permission_status)}</span>
              </div>
              <small>{device.message}</small>
            </button>
          ))}
        </div>

        <div className="streamer-binding-panel">
          {selectedDevice && profile ? (
            <>
              <div className="panel-header compact">
                <div>
                  <h2>{profile.name}</h2>
                  <span>{profile.safe_preset ? "Safe preset" : "Custom bindings"}</span>
                </div>
                <Keyboard size={18} />
              </div>
              <Toggle
                disabled={busy || !selectedDeviceBindable}
                label="Enabled"
                onChange={(enabled) => void setDeviceEnabled(selectedDevice, enabled)}
                value={selectedDevice.enabled}
              />
              {!selectedDeviceBindable && (
                <div className="latency-note info">
                  <Info size={15} />
                  <span>{streamerBindingUnavailableMessage(selectedDevice)}</span>
                </div>
              )}
              <div className="streamer-binding-actions">
                <button className="secondary-button" disabled={busy || !selectedDeviceBindable} onClick={addBinding} type="button">
                  <CirclePlus size={16} />
                  Binding
                </button>
                <button
                  className="secondary-button"
                  disabled={busy || !selectedDeviceBindable || !selectedBinding}
                  onClick={() => selectedBinding && void testBinding(selectedBinding)}
                  type="button"
                >
                  <Activity size={16} />
                  Test
                </button>
              </div>
              <div className="streamer-binding-list">
                {profile.bindings.map((binding, index) => (
                  <div
                    className={index === selectedBindingIndex ? "streamer-binding-row active" : "streamer-binding-row"}
                    key={`${binding.control_id}-${index}`}
                    onClick={() => setSelectedBindingIndex(index)}
                  >
                    <input
                      aria-label="Binding label"
                      className="text-field"
                      disabled={busy || !selectedDeviceBindable}
                      onBlur={(event) => updateBinding(index, { label: event.currentTarget.value })}
                      onChange={() => undefined}
                      defaultValue={binding.label}
                    />
                    <input
                      aria-label="Control id"
                      className="text-field"
                      disabled={busy || !selectedDeviceBindable}
                      onBlur={(event) => updateBinding(index, { control_id: event.currentTarget.value })}
                      onChange={() => undefined}
                      defaultValue={binding.control_id}
                    />
                    <AppSelect
                      ariaLabel="Binding action"
                      disabled={busy || !selectedDeviceBindable}
                      onChange={(value) => updateBinding(index, { action: parseStreamerAction(value) })}
                      options={actionOptions}
                      value={streamerActionKey(binding.action)}
                    />
                    <div className="streamer-binding-buttons">
                      <button
                        className="mini-icon-button"
                        disabled={busy || !selectedDeviceBindable}
                        onClick={(event) => {
                          event.stopPropagation();
                          setSelectedBindingIndex(index);
                          void learnBinding(index);
                        }}
                        title="Learn control"
                        type="button"
                      >
                        <Keyboard size={14} />
                      </button>
                      <button
                        className="mini-icon-button"
                        disabled={busy || !selectedDeviceBindable}
                        onClick={(event) => {
                          event.stopPropagation();
                          void testBinding(binding);
                        }}
                        title="Test action"
                        type="button"
                      >
                        <Activity size={14} />
                      </button>
                      <button
                        className="mini-icon-button danger"
                        disabled={busy || !selectedDeviceBindable}
                        onClick={(event) => {
                          event.stopPropagation();
                          removeBinding(index);
                        }}
                        title="Remove binding"
                        type="button"
                      >
                        <Trash2 size={14} />
                      </button>
                    </div>
                  </div>
                ))}
                {profile.bindings.length === 0 && <EmptyState label="No bindings" />}
              </div>
            </>
          ) : (
            <EmptyState label="No streamer device selected" />
          )}
        </div>
      </div>
    </section>
  );
}

function ElgatoDevicesView() {
  const [devices, setDevices] = useState<ElgatoDeviceSummary[]>([]);
  const [waveXlr, setWaveXlr] = useState<ElgatoWaveXlrState | null>(null);
  const [elgatoError, setElgatoError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const commandBusy = useRef(false);
  const loadBusy = useRef(false);

  const loadElgato = useCallback(async (showBusy = false) => {
    if (commandBusy.current || loadBusy.current) return;
    loadBusy.current = true;
    if (showBusy) setBusy(true);
    try {
      const nextDevices = await invoke<ElgatoDeviceSummary[]>("list_elgato_devices");
      setDevices(nextDevices);
      if (nextDevices.some((device) => device.controls_supported)) {
        try {
          const nextState = await invoke<ElgatoWaveXlrState>("read_elgato_wave_xlr");
          setWaveXlr(nextState);
          setElgatoError(null);
        } catch (error) {
          setWaveXlr(null);
          setElgatoError(String(error));
        }
      } else {
        setWaveXlr(null);
        setElgatoError(null);
      }
    } catch (error) {
      setDevices([]);
      setWaveXlr(null);
      setElgatoError(String(error));
    } finally {
      loadBusy.current = false;
      if (showBusy) setBusy(false);
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    const tick = () => {
      if (cancelled) return;
      void loadElgato(false);
    };
    tick();
    const interval = window.setInterval(tick, ELGATO_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [loadElgato]);

  const waitForElgatoRefresh = async () => {
    while (loadBusy.current) {
      await new Promise((resolve) => window.setTimeout(resolve, 25));
    }
  };

  const runWaveCommand = async (command: string, args: Record<string, unknown>) => {
    if (commandBusy.current) return;
    commandBusy.current = true;
    setBusy(true);
    try {
      await waitForElgatoRefresh();
      const nextState = await invoke<ElgatoWaveXlrState>(command, args);
      setWaveXlr(nextState);
      setElgatoError(null);
    } catch (error) {
      setWaveXlr(null);
      setElgatoError(String(error));
    } finally {
      commandBusy.current = false;
      setBusy(false);
    }
  };

  const controllableDevice = devices.find((device) => device.controls_supported);

  return (
    <section className="panel single-panel">
      <div className="panel-header">
        <h2>Elgato Devices</h2>
        <div className="panel-actions">
          <button
            className="secondary-button"
            disabled={busy}
            onClick={() => void loadElgato(true)}
            type="button"
          >
            <RefreshCw size={16} />
            Refresh
          </button>
        </div>
      </div>
      {elgatoError && (
        <div className="effect-warning">
          <CircleAlert size={15} />
          <span>{elgatoError}</span>
        </div>
      )}
      <div className="elgato-grid">
        <div className="elgato-device-list">
          {devices.map((device) => (
            <div
              className={device.controls_supported ? "elgato-device-row active" : "elgato-device-row"}
              key={device.id}
            >
              <div>
                <strong>{device.name}</strong>
                <span>{device.description}</span>
              </div>
              <div className="elgato-device-meta">
                <span>{elgatoKindLabel(device.kind)}</span>
                <span>{device.bus ?? "unknown"}</span>
                {device.product_id && <span>{device.vendor_id ?? "usb"}:{device.product_id}</span>}
              </div>
              <small>{device.message}</small>
            </div>
          ))}
          {devices.length === 0 && <EmptyState label="No Elgato audio devices detected" />}
        </div>

        <div className="elgato-control-card">
          <div className="panel-header compact">
            <h2>{controllableDevice?.name ?? "Wave XLR"}</h2>
            <Mic size={18} />
          </div>
          {waveXlr ? (
            <>
              <div className="elgato-state-grid">
                <Stat icon={Gauge} label="Gain" value={formatHexGain(waveXlr.gain_raw)} />
                <Stat icon={Headphones} label="Headphones" value={`${waveXlr.hp_volume_db.toFixed(1)} dB`} />
                <Stat icon={SlidersHorizontal} label="Knob" value={waveXlr.volume_select === "headphones" ? "Headphones" : "Gain"} />
              </div>
              <div className="elgato-info-grid">
                <span>Firmware</span>
                <strong>{waveXlr.firmware_version ?? "Unknown"}</strong>
                <span>API</span>
                <strong>{waveXlr.api_version ?? "Unknown"}</strong>
                <span>Serial</span>
                <strong>{waveXlr.serial ?? "Unknown"}</strong>
              </div>
              <Toggle
                disabled={busy}
                label="Mute microphone"
                onChange={(muted) =>
                  void runWaveCommand("set_elgato_wave_xlr_mute", { muted })
                }
                value={waveXlr.muted}
              />
              <VolumeFader
                compact
                disabled={busy}
                formatValue={(value) => formatHexGain(Math.round(value))}
                label="Gain"
                max={waveXlr.gain_max_raw}
                min={0}
                step={64}
                unit=""
                value={waveXlr.gain_raw}
                onChange={(value) => {
                  const gainRaw = Math.round(value);
                  void runWaveCommand("set_elgato_wave_xlr_gain", {
                    gainRaw,
                    gain_raw: gainRaw,
                  });
                }}
              />
              <VolumeFader
                compact
                disabled={busy}
                label="Headphones"
                max={waveXlr.hp_max_db}
                min={waveXlr.hp_min_db}
                step={0.5}
                unit=" dB"
                value={waveXlr.hp_volume_db}
                onChange={(db) =>
                  void runWaveCommand("set_elgato_wave_xlr_hp_volume_db", { db })
                }
              />
              <Toggle
                disabled={busy}
                label="Low impedance"
                onChange={(enabled) =>
                  void runWaveCommand("set_elgato_wave_xlr_low_impedance", { enabled })
                }
                value={waveXlr.low_impedance}
              />
            </>
          ) : controllableDevice ? (
            <EmptyState label={elgatoError ? "Wave XLR controls unavailable" : "Reading Wave XLR controls"} />
          ) : (
            <EmptyState label="No controllable Wave XLR found" />
          )}
        </div>
      </div>
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
  disabled = false,
  step,
  formatValue,
  onChange,
}: {
  label: string;
  value: number;
  min?: number;
  max?: number;
  unit?: string;
  compact?: boolean;
  disabled?: boolean;
  step?: number;
  formatValue?: (value: number) => string;
  onChange: (value: number) => void | Promise<unknown>;
}) {
  const normalizedPercent = unit === "%" && min === 0 && max === 1;
  const sliderMin = normalizedPercent ? 0 : min;
  const sliderMax = normalizedPercent ? 100 : max;
  const incomingSliderValue = normalizedPercent ? value * 100 : value;
  const [draft, setDraft] = useState(incomingSliderValue);
  const lastCommitted = useRef(incomingSliderValue);
  const display = normalizedPercent ? Math.round(draft) : Math.round(draft * 10) / 10;
  const displayText = formatValue ? formatValue(draft) : `${display}${unit}`;

  useEffect(() => {
    const next = normalizedPercent ? value * 100 : value;
    setDraft(next);
    lastCommitted.current = next;
  }, [normalizedPercent, value]);

  const commit = useCallback((raw: number) => {
    if (disabled) return;
    const next = Number.isFinite(raw) ? Math.max(sliderMin, Math.min(sliderMax, raw)) : incomingSliderValue;
    setDraft(next);
    if (lastCommitted.current === next) return;
    lastCommitted.current = next;
    void onChange(normalizedPercent ? next / 100 : next);
  }, [disabled, incomingSliderValue, normalizedPercent, onChange, sliderMax, sliderMin]);

  return (
    <label
      aria-disabled={disabled}
      className={`${compact ? "fader-row compact" : "fader-row"}${disabled ? " disabled" : ""}`}
    >
      <span>{label}</span>
      <input
        disabled={disabled}
        max={sliderMax}
        min={sliderMin}
        onBlur={(event) => commit(Number(event.currentTarget.value))}
        onChange={(event) => setDraft(Number(event.currentTarget.value))}
        onKeyUp={(event) => {
          if (shouldCommitSliderKey(event)) commit(Number(event.currentTarget.value));
        }}
        onPointerUp={(event) => commit(Number(event.currentTarget.value))}
        step={step ?? (unit === "%" ? 1 : 0.1)}
        type="range"
        value={draft}
      />
      <strong>{displayText}</strong>
    </label>
  );
}

function Toggle({
  label,
  value,
  disabled = false,
  onChange,
}: {
  label: string;
  value: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void | Promise<unknown>;
}) {
  return (
    <button
      className="toggle-row"
      disabled={disabled}
      onClick={() => onChange(!value)}
      type="button"
    >
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
    settings: "Settings",
  }[view];
}

function hardwareProfileOptionLabel(profile: HardwareProfileSummary): string {
  return `${profile.name} · ${profile.source}`;
}

function elgatoKindLabel(kind: ElgatoDeviceSummary["kind"]): string {
  return {
    wave_xlr: "Wave XLR",
    wave_microphone: "Wave microphone",
    capture_audio: "Capture audio",
    audio_endpoint: "Audio endpoint",
  }[kind];
}

function emptyStreamerProfile(device: StreamerDeviceSummary): StreamerBindingProfile {
  return {
    device_id: device.id,
    family: device.family,
    name: device.name,
    enabled: device.enabled,
    safe_preset: true,
    bindings: [],
  };
}

function streamerFamilyLabel(family: StreamerDeviceSummary["family"]): string {
  return {
    stream_deck: "Stream Deck",
    rode: "RODE",
    go_xlr: "GoXLR",
    midi_surface: "MIDI surface",
    loupedeck: "Loupedeck",
    x_keys: "X-keys",
    unknown_supported: "Hardware",
  }[family];
}

function streamerPermissionLabel(status: StreamerDeviceSummary["permission_status"]): string {
  return {
    ready: "Ready",
    permission_denied: "Permission",
    busy: "Busy",
    missing_runtime: "Missing runtime",
    unsupported_protocol: "Unsupported",
  }[status];
}

function streamerDeviceBindingsAvailable(device: StreamerDeviceSummary): boolean {
  return (
    device.permission_status === "ready" &&
    (device.transport === "hid" || device.transport === "midi")
  );
}

function streamerBindingUnavailableMessage(device: StreamerDeviceSummary): string {
  if (device.transport === "audio_profile" || device.permission_status === "unsupported_protocol") {
    return "WaveLinux detected this hardware, but no native control protocol is available for bindings yet.";
  }
  if (device.permission_status === "permission_denied") {
    return "WaveLinux needs hidraw or MIDI permissions before bindings can run.";
  }
  if (device.permission_status === "busy") {
    return "Another app appears to own this device, so WaveLinux will not retry aggressively.";
  }
  if (device.permission_status === "missing_runtime") {
    return "A required runtime tool is missing before bindings can run.";
  }
  return "Bindings become available when the device status is Ready.";
}

function streamerActionOptions(state: AppStateSnapshot): SelectOption[] {
  const actions: Array<{ label: string; action: StreamerAction }> = [
    { label: "No action", action: { kind: "noop" } },
    { label: "Prune stale audio", action: { kind: "cleanup_stale_audio_graph" } },
  ];
  for (const mix of state.config.mixes) {
    actions.push({
      label: `${mix.name}: mute`,
      action: { kind: "mix_mute_toggle", mix_id: mix.id },
    });
    actions.push({
      label: `${mix.name}: volume +10`,
      action: { kind: "mix_volume_adjust", mix_id: mix.id, delta: 0.1 },
    });
    actions.push({
      label: `${mix.name}: volume -10`,
      action: { kind: "mix_volume_adjust", mix_id: mix.id, delta: -0.1 },
    });
    actions.push({
      label: `${mix.name}: volume from control`,
      action: { kind: "mix_volume_set_from_control", mix_id: mix.id },
    });
  }
  for (const channel of state.config.channels) {
    for (const mix of primaryBusMixes(state.config.mixes)) {
      actions.push({
        label: `${channelDisplayName(channel)} ${mix.name}: mute`,
        action: { kind: "channel_mute_toggle", channel_id: channel.id, mix_id: mix.id },
      });
      actions.push({
        label: `${channelDisplayName(channel)} ${mix.name}: enable`,
        action: { kind: "channel_bus_enabled_toggle", channel_id: channel.id, mix_id: mix.id },
      });
      actions.push({
        label: `${channelDisplayName(channel)} ${mix.name}: +10`,
        action: { kind: "channel_volume_adjust", channel_id: channel.id, mix_id: mix.id, delta: 0.1 },
      });
      actions.push({
        label: `${channelDisplayName(channel)} ${mix.name}: -10`,
        action: { kind: "channel_volume_adjust", channel_id: channel.id, mix_id: mix.id, delta: -0.1 },
      });
      actions.push({
        label: `${channelDisplayName(channel)} ${mix.name}: from control`,
        action: { kind: "channel_volume_set_from_control", channel_id: channel.id, mix_id: mix.id },
      });
    }
    for (const effect of channel.effects) {
      actions.push({
        label: `${channelDisplayName(channel)}: ${effect.name || effect.effect_id}`,
        action: { kind: "effect_bypass_toggle", channel_id: channel.id, instance_id: effect.instance_id },
      });
    }
  }
  return actions.map(({ label, action }) => ({ label, value: streamerActionKey(action) }));
}

function streamerActionKey(action: StreamerAction): string {
  return JSON.stringify(action);
}

function parseStreamerAction(value: string): StreamerAction {
  try {
    const parsed = JSON.parse(value) as StreamerAction;
    return parsed && typeof parsed === "object" && "kind" in parsed ? parsed : { kind: "noop" };
  } catch {
    return { kind: "noop" };
  }
}

function formatHexGain(value: number): string {
  return `0x${Math.max(0, Math.round(value)).toString(16).toUpperCase().padStart(4, "0")}`;
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

function isElgatoAudioDevice(device: DeviceInfo): boolean {
  if (device.is_virtual || device.bus === "virtual") return false;
  const vendorId = normalizeUsbId(device.vendor_id);
  const profileId = device.matched_profile_id?.toLowerCase() ?? "";
  const text = [device.id, device.name, device.description].join(" ").toLowerCase();
  return (
    vendorId === "0fd9" ||
    profileId.startsWith("elgato.") ||
    text.includes("elgato") ||
    text.includes("wave xlr") ||
    text.includes("wave:3")
  );
}

function normalizeUsbId(value?: string | null): string {
  const normalized = (value ?? "")
    .trim()
    .replace(/^0x/i, "")
    .toLowerCase()
    .replace(/[^0-9a-f]/g, "");
  return normalized ? normalized.padStart(4, "0") : "";
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

function mixOutputDevices(mix: Mix): string[] {
  const outputs = mix.output_devices?.filter(Boolean) ?? [];
  return outputs.length > 0 ? outputs : mix.monitor_output ? [mix.monitor_output] : [];
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

function matcherForStream(stream: AppStream): AppMatcher {
  const keepMediaName = shouldKeepStreamMediaName(stream);
  const matcher = {
    app_id: stream.app_id ?? null,
    binary: stream.binary ?? null,
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
  return entries.map(([kind, value]) => `${kind}:${normalizedMatcherValue(value)}`).join("|");
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
  autoDevices: AutoDevices = [],
  channelId?: string,
): string {
  const resolved = resolvedAutoInput(autoDevices, channelId);
  if (resolved?.device_description || resolved?.device_id) {
    return `Auto: ${resolved.device_description ?? resolved.device_id}`;
  }
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

function resolvedAutoInput(autoDevices: AutoDevices, channelId?: string) {
  return autoDevices.find((device) =>
    device.kind === "input" && (!channelId || device.channel_id === channelId)
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

function documentHasActiveFocus(): boolean {
  return document.visibilityState === "visible" && document.hasFocus();
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
