import {
  AudioLines,
  BadgeCheck,
  CircleAlert,
  GitBranch,
  RefreshCw,
  Settings,
  SlidersHorizontal,
  Sparkles,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { initialSnapshot, invoke } from "./tauri";
import type { AudioActionReport } from "./components/TestingHealthReport";
import { SettingsView } from "./views/SettingsView";
import { RoutingView } from "./views/RoutingView";
import { MixerView, WaveLinkMixerView } from "./views/MixerView";
import {
  EffectsView,
  type SetEffectChain,
} from "./views/EffectsView";
import {
  initializeWaveLinuxState,
  replaceWaveLinuxState,
  useWaveLinuxSelector,
  waveLinuxRevisions,
} from "./state";
import { useMixerMutations } from "./hooks/useMixerMutations";
import { useWaveLinuxRuntime } from "./hooks/useWaveLinuxRuntime";
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
  CommandExecution,
  MixerSettings,
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

function initialView(): View {
  if (typeof window === "undefined") return "mixer";
  const params = new URLSearchParams(window.location.search);
  const requested = params.get("view") ?? window.location.hash.replace(/^#\/?/, "");
  return views.some((view) => view.id === requested) ? (requested as View) : "mixer";
}

const APP_DISPLAY_NAME = "WaveLinux 6";

type UiThemePreference = {
  theme_id: string;
};

type RunMutation = <T>(
  command: string,
  args?: Record<string, unknown>,
  message?: string,
) => Promise<T>;

export default function App() {
  initializeWaveLinuxState(initialSnapshot());
  const stateReady = useWaveLinuxSelector((current) => current !== null);
  const engineHealthy = useWaveLinuxSelector((current) => current?.engine.healthy ?? false);
  const audioGraphRunning = useWaveLinuxSelector(
    (current) => current?.engine.audio_graph_running ?? false,
  );
  const engineDryRun = useWaveLinuxSelector((current) => current?.engine.dry_run ?? false);
  const engineMessage = useWaveLinuxSelector((current) => current?.engine.message ?? "Starting");
  const sampleRateHz = useWaveLinuxSelector(
    (current) => current?.config.audio.sample_rate_hz ?? 48_000,
  );
  const releaseChannel = useWaveLinuxSelector(
    (current) => current?.config.settings.release_channel ?? "stable",
  );
  const autoInstallUpdates = useWaveLinuxSelector(
    (current) => current?.config.settings.auto_install_updates ?? false,
  );
  const autoCheckUpdates = useWaveLinuxSelector(
    (current) => current?.config.settings.auto_check_updates ?? false,
  );
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
  const themeChangedByUser = useRef(false);
  const activeThemeTokenKeys = useRef<string[]>([]);
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
    replaceWaveLinuxState(next);
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
    const scheduledRevision = waveLinuxRevisions().state;
    refreshTimer.current = window.setTimeout(() => {
      refreshTimer.current = null;
      if (waveLinuxRevisions().state > scheduledRevision) return;
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
    }, Math.max(delayMs, 350));
  }, [applySnapshot]);

  const {
    run,
    setAppStreamMuteFast,
    setChannelBusEnabledFast,
    setChannelBusMuteFast,
    setChannelBusVolumeFast,
    setChannelEffectsEnabledFast,
    setChannelIconFast,
    setChannelInputFast,
    setEffectChainFast,
    setMixIconFast,
    setMixMonitorOutputFast,
    setMixMuteFast,
    setMixOutputsFast,
    setMixVolumeFast,
    setSettingsFast,
  } = useMixerMutations({ refresh, reportError: setToast });

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
      const next = await invoke<UpdateInfo>("check_for_updates", { releaseChannel });
      setUpdateInfo(next);
      if (showToast || next.available) setToast(next.message);
      if (next.available && autoInstallUpdates && next.install_supported) {
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
  }, [autoInstallUpdates, releaseChannel]);

  useWaveLinuxRuntime({
    meterActive: activeView === "mixer",
    refresh,
    reportError: setToast,
  });

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
    if (!autoCheckUpdates || autoUpdateCheckStarted.current) return;
    autoUpdateCheckStarted.current = true;
    const timer = window.setTimeout(() => {
      checkUpdates(false).catch(() => undefined);
    }, 2500);
    return () => window.clearTimeout(timer);
  }, [autoCheckUpdates, checkUpdates]);

  const isWaveLinkSurface = activeTheme.surface === "wavelink3";
  const workspace = stateReady ? (
    <AppWorkspace
      activeThemeId={activeTheme.id}
      activeView={activeView}
      audioActionReport={audioActionReport}
      busy={busy}
      isWaveLinkSurface={isWaveLinkSurface}
      onCheckUpdates={() => void checkUpdates(true).catch(() => undefined)}
      onInstallUpdate={() => {
        setUpdateBusy(true);
        invoke<UpdateInstallResult>("install_update", { releaseChannel })
          .then((result) => setToast(result.message))
          .catch((error) => setToast(String(error)))
          .finally(() => setUpdateBusy(false));
      }}
      onOpenReleases={() => {
        void invoke("open_release_page", { releaseChannel }).catch((error) => setToast(String(error)));
      }}
      onOpenThemeFolder={() => void openThemeFolder().catch((error) => setToast(String(error)))}
      onPrune={() => runAudioCommandList("cleanup_stale_audio_graph", "Prune Stale Audio")}
      onReloadThemes={() => void reloadUiThemes().catch((error) => setToast(String(error)))}
      onThemeChange={setUiTheme}
      run={run}
      selectedChannelId={selectedChannelId}
      setActiveView={setActiveView}
      setAppStreamMute={setAppStreamMuteFast}
      setChannelBusEnabled={setChannelBusEnabledFast}
      setChannelBusMute={setChannelBusMuteFast}
      setChannelBusVolume={setChannelBusVolumeFast}
      setChannelEffectsEnabled={setChannelEffectsEnabledFast}
      setChannelIcon={setChannelIconFast}
      setChannelInput={setChannelInputFast}
      setEffectChain={setEffectChainFast}
      setMixIcon={setMixIconFast}
      setMixMonitorOutput={setMixMonitorOutputFast}
      setMixMute={setMixMuteFast}
      setMixOutputs={setMixOutputsFast}
      setMixVolume={setMixVolumeFast}
      setSelectedChannelId={setSelectedChannelId}
      setSettings={setSettingsFast}
      themes={uiThemes}
      updateBusy={updateBusy}
      updateInfo={updateInfo}
    />
  ) : (
    <div className="loading-panel">Starting audio engine</div>
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
          <div className="wl-brand" title={APP_DISPLAY_NAME}>
            <AudioLines size={22} />
            <span>WL</span>
          </div>
          <nav className="wl-nav" aria-label={`${APP_DISPLAY_NAME} sections`}>
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
            className={audioGraphRunning ? "wl-engine-pill running" : "wl-engine-pill"}
            title={engineMessage}
          >
            {engineHealthy ? <BadgeCheck size={16} /> : <CircleAlert size={16} />}
          </div>
        </aside>
        <main className="wl-main">
          <header className="wl-topbar">
            <div>
              <p>{APP_DISPLAY_NAME}</p>
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
            <strong>{APP_DISPLAY_NAME}</strong>
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
          aria-label={engineMessage}
          className="engine-card"
          title={engineMessage}
        >
          <div className="engine-row">
            {engineHealthy ? <BadgeCheck size={18} /> : <CircleAlert size={18} />}
            <span>{engineMessage}</span>
          </div>
          <div className="engine-meta">
            {engineDryRun ? "Dry run" : audioGraphRunning ? "Graph running" : "Graph stopped"} ·{" "}
            {sampleRateHz} Hz
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

function AppWorkspace({
  activeThemeId,
  activeView,
  audioActionReport,
  busy,
  isWaveLinkSurface,
  onCheckUpdates,
  onInstallUpdate,
  onOpenReleases,
  onOpenThemeFolder,
  onPrune,
  onReloadThemes,
  onThemeChange,
  run,
  selectedChannelId,
  setActiveView,
  setAppStreamMute,
  setChannelBusEnabled,
  setChannelBusMute,
  setChannelBusVolume,
  setChannelEffectsEnabled,
  setChannelIcon,
  setChannelInput,
  setEffectChain,
  setMixIcon,
  setMixMonitorOutput,
  setMixMute,
  setMixOutputs,
  setMixVolume,
  setSelectedChannelId,
  setSettings,
  themes,
  updateBusy,
  updateInfo,
}: {
  activeThemeId: string;
  activeView: View;
  audioActionReport: AudioActionReport | null;
  busy: boolean;
  isWaveLinkSurface: boolean;
  onCheckUpdates: () => void;
  onInstallUpdate: () => void;
  onOpenReleases: () => void;
  onOpenThemeFolder: () => void;
  onPrune: () => void | Promise<unknown>;
  onReloadThemes: () => void;
  onThemeChange: (themeId: string) => void;
  run: RunMutation;
  selectedChannelId: string;
  setActiveView: (view: View) => void;
  setAppStreamMute: (streamId: string, muted: boolean) => Promise<void>;
  setChannelBusEnabled: (channelId: string, mixId: string, enabled: boolean) => Promise<void>;
  setChannelBusMute: (channelId: string, mixId: string, muted: boolean) => Promise<void>;
  setChannelBusVolume: (channelId: string, mixId: string, volume: number) => Promise<void>;
  setChannelEffectsEnabled: (channelId: string, enabled: boolean) => Promise<void>;
  setChannelIcon: (channelId: string, icon: string | null) => Promise<void>;
  setChannelInput: (channelId: string, sourceDevice: string | null) => Promise<void>;
  setEffectChain: SetEffectChain;
  setMixIcon: (mixId: string, icon: string | null) => Promise<void>;
  setMixMonitorOutput: (mixId: string, output: string | null) => Promise<void>;
  setMixMute: (mixId: string, muted: boolean) => Promise<void>;
  setMixOutputs: (mixId: string, outputs: string[]) => Promise<void>;
  setMixVolume: (mixId: string, volume: number) => Promise<void>;
  setSelectedChannelId: (channelId: string) => void;
  setSettings: (settings: MixerSettings) => Promise<void>;
  themes: UiThemeDefinition[];
  updateBusy: boolean;
  updateInfo: UpdateInfo | null;
}) {
  const state = useWaveLinuxSelector((current) => current);
  if (!state) return <div className="loading-panel">Starting audio engine</div>;
  const selectedChannel = state.config.channels.find(
    (channel) => channel.id === selectedChannelId,
  );

  if (activeView === "mixer") {
    return isWaveLinkSurface ? (
      <WaveLinkMixerView
        busy={busy}
        run={run}
        selectedChannelId={selectedChannelId}
        setActiveView={setActiveView}
        setAppStreamMute={setAppStreamMute}
        setChannelBusEnabled={setChannelBusEnabled}
        setChannelBusMute={setChannelBusMute}
        setChannelBusVolume={setChannelBusVolume}
        setChannelEffectsEnabled={setChannelEffectsEnabled}
        setChannelIcon={setChannelIcon}
        setChannelInput={setChannelInput}
        setEffectChain={setEffectChain}
        setMixIcon={setMixIcon}
        setMixMute={setMixMute}
        setMixOutputs={setMixOutputs}
        setMixVolume={setMixVolume}
        setSelectedChannelId={setSelectedChannelId}
        setSettings={setSettings}
        state={state}
      />
    ) : (
      <MixerView
        busy={busy}
        run={run}
        setChannelBusMute={setChannelBusMute}
        setChannelBusVolume={setChannelBusVolume}
        setChannelInput={setChannelInput}
        setMixMonitorOutput={setMixMonitorOutput}
        setMixMute={setMixMute}
        setMixVolume={setMixVolume}
        setSelectedChannelId={setSelectedChannelId}
        setSettings={setSettings}
        state={state}
      />
    );
  }

  if (activeView === "routing") {
    return <RoutingView run={run} setAppStreamMute={setAppStreamMute} state={state} />;
  }

  if (activeView === "effects") {
    return (
      <EffectsView
        selectedChannel={selectedChannel}
        selectedChannelId={selectedChannelId}
        setChannelEffectsEnabled={setChannelEffectsEnabled}
        setChannelInput={setChannelInput}
        setEffectChain={setEffectChain}
        setSelectedChannelId={setSelectedChannelId}
        state={state}
      />
    );
  }

  return (
    <SettingsView
      activeThemeId={activeThemeId}
      audioActionReport={audioActionReport}
      onCheckUpdates={onCheckUpdates}
      onInstallUpdate={onInstallUpdate}
      onOpenReleases={onOpenReleases}
      onOpenThemeFolder={onOpenThemeFolder}
      onPrune={onPrune}
      onReloadThemes={onReloadThemes}
      onThemeChange={onThemeChange}
      run={run}
      setSettings={setSettings}
      state={state}
      themes={themes}
      updateBusy={updateBusy}
      updateInfo={updateInfo}
    />
  );
}

function viewTitle(view: View): string {
  return {
    mixer: "Mixer",
    routing: "Routing",
    effects: "Effects",
    settings: "Settings",
  }[view];
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
