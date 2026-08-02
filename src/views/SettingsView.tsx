import { useState } from "react";
import {
  Activity,
  AudioLines,
  Cable,
  Cpu,
  Download,
  ExternalLink,
  Gauge,
  Keyboard,
  Mic,
  Radio,
  RefreshCw,
  Settings,
} from "lucide-react";
import { AppSelect } from "../components/AppSelect";
import { Stat, Toggle, VolumeFader } from "../components/Controls";
import type { AudioActionReport } from "../components/TestingHealthReport";
import type { UiThemeDefinition } from "../themes";
import type { AppStateSnapshot, MixerSettings, UpdateInfo } from "../types";
import { DiagnosticsView } from "./DiagnosticsView";
import { ElgatoDevicesView } from "./ElgatoDevicesView";
import { HardwareProfilesView } from "./HardwareProfilesView";
import { StreamerDevicesView } from "./StreamerDevicesView";

type SettingsTab = "general" | "profiles" | "streamers" | "elgato" | "health";

export function SettingsView({
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
  onOpenReleases,
  onPrune,
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
  onOpenReleases: () => void;
  onPrune: () => void | Promise<unknown>;
}) {
  const updateSettings = (settings: MixerSettings) => void setSettings(settings);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");
  const visibleUpdateInfo =
    updateInfo?.channel === state.config.settings.release_channel ? updateInfo : null;
  const betaUpdatesEnabled = state.config.settings.release_channel === "beta";
  const updateChannelLabel = betaUpdatesEnabled ? "Testing" : "Stable";

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
            className={settingsTab === "streamers" ? "settings-tab active" : "settings-tab"}
            onClick={() => setSettingsTab("streamers")}
            role="tab"
            type="button"
          >
            <Keyboard size={16} />
            Streamers
          </button>
          <button
            className={settingsTab === "elgato" ? "settings-tab active" : "settings-tab"}
            onClick={() => setSettingsTab("elgato")}
            role="tab"
            type="button"
          >
            <Mic size={16} />
            Elgato
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
                updateSettings({
                  ...state.config.settings,
                  restore_audio_graph_on_launch: value,
                })
              }
              value={state.config.settings.restore_audio_graph_on_launch}
            />
            <Toggle
              label="Auto monitor output"
              onChange={(value) =>
                updateSettings({
                  ...state.config.settings,
                  monitor_follows_default_output: value,
                })
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
                <button
                  className="secondary-button"
                  disabled={updateBusy}
                  onClick={onCheckUpdates}
                  type="button"
                >
                  <RefreshCw size={16} />
                  Check
                </button>
                <button
                  className="secondary-button"
                  disabled={
                    updateBusy ||
                    !visibleUpdateInfo?.available ||
                    !visibleUpdateInfo.install_supported
                  }
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
                  updateSettings({
                    ...state.config.settings,
                    low_latency_mic_monitoring: value,
                  })
                }
                value={state.config.settings.low_latency_mic_monitoring}
              />
              <Toggle
                label="Hardware direct mic monitor"
                onChange={(value) =>
                  updateSettings({
                    ...state.config.settings,
                    hardware_direct_mic_monitoring: value,
                  })
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
            <Stat
              icon={Cpu}
              label="Engine"
              value={state.engine.audio_graph_running ? "Running" : "Inactive"}
            />
            <Stat
              icon={Radio}
              label="Rate"
              value={`${state.config.audio.sample_rate_hz / 1000} kHz`}
            />
            <Stat
              icon={AudioLines}
              label="Format"
              value={`${state.config.audio.bit_depth}-bit`}
            />
          </div>
        </section>
      )}

      {settingsTab === "profiles" && <HardwareProfilesView state={state} />}
      {settingsTab === "streamers" && <StreamerDevicesView state={state} />}
      {settingsTab === "elgato" && <ElgatoDevicesView />}
      {settingsTab === "health" && (
        <DiagnosticsView
          audioActionReport={audioActionReport}
          onPrune={onPrune}
          state={state}
          updateInfo={updateInfo}
          run={run}
        />
      )}
    </section>
  );
}
