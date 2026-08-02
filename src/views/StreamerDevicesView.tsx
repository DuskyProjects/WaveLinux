import {
  Activity,
  CircleAlert,
  CirclePlus,
  Info,
  Keyboard,
  RefreshCw,
  Trash2,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { AppSelect, type SelectOption } from "../components/AppSelect";
import { EmptyState, Toggle } from "../components/Controls";
import { invoke } from "../tauri";
import type {
  AppStateSnapshot,
  Channel,
  Mix,
  StreamerAction,
  StreamerActionResult,
  StreamerBinding,
  StreamerBindingProfile,
  StreamerDeviceSummary,
  StreamerDevicesConfig,
  StreamerLearnResult,
} from "../types";

export function StreamerDevicesView({ state }: { state: AppStateSnapshot }) {
  const [devices, setDevices] = useState<StreamerDeviceSummary[]>([]);
  const [deviceError, setDeviceError] = useState<string | null>(null);
  const [bindings, setBindings] = useState<StreamerDevicesConfig | null>(
    state.config.streamer_devices,
  );
  const [selectedDeviceId, setSelectedDeviceId] = useState(devices[0]?.id ?? "");
  const [selectedBindingIndex, setSelectedBindingIndex] = useState(0);
  const [streamerError, setStreamerError] = useState<string | null>(null);
  const [streamerMessage, setStreamerMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const actionOptions = useMemo(() => streamerActionOptions(state), [state]);

  const loadDevices = useCallback(async () => {
    try {
      const next = await invoke<StreamerDeviceSummary[]>("list_streamer_devices");
      setDevices(next);
      setDeviceError(null);
    } catch (error) {
      setDevices([]);
      setDeviceError(String(error));
    }
  }, []);

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
  }, [loadBindings]);

  useEffect(() => {
    void loadDevices();
  }, [loadDevices]);

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
  const selectedDeviceBindable = selectedDevice
    ? streamerDeviceBindingsAvailable(selectedDevice)
    : false;

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
      setDevices(
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
    const nextBindings = profile.bindings.map((binding, bindingIndex) =>
      bindingIndex === index ? { ...binding, ...patch } : binding,
    );
    void saveProfile({ ...profile, safe_preset: false, bindings: nextBindings });
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
            onClick={() => void loadDevices()}
            type="button"
          >
            <RefreshCw size={16} />
            Refresh
          </button>
        </div>
      </div>
      {(deviceError || streamerError || streamerMessage) && (
        <div
          className={streamerError || deviceError ? "effect-warning" : "latency-note info"}
        >
          {streamerError || deviceError ? <CircleAlert size={15} /> : <Info size={15} />}
          <span>{streamerError ?? deviceError ?? streamerMessage}</span>
        </div>
      )}
      <div className="streamer-grid">
        <div className="streamer-device-list">
          {devices.map((device) => (
            <button
              className={
                device.id === selectedDevice?.id
                  ? "streamer-device-row active"
                  : "streamer-device-row"
              }
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
                <button
                  className="secondary-button"
                  disabled={busy || !selectedDeviceBindable}
                  onClick={addBinding}
                  type="button"
                >
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
                    className={
                      index === selectedBindingIndex
                        ? "streamer-binding-row active"
                        : "streamer-binding-row"
                    }
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
                      onBlur={(event) =>
                        updateBinding(index, { control_id: event.currentTarget.value })
                      }
                      onChange={() => undefined}
                      defaultValue={binding.control_id}
                    />
                    <AppSelect
                      ariaLabel="Binding action"
                      disabled={busy || !selectedDeviceBindable}
                      onChange={(value) =>
                        updateBinding(index, { action: parseStreamerAction(value) })
                      }
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
        action: {
          kind: "channel_bus_enabled_toggle",
          channel_id: channel.id,
          mix_id: mix.id,
        },
      });
      actions.push({
        label: `${channelDisplayName(channel)} ${mix.name}: +10`,
        action: {
          kind: "channel_volume_adjust",
          channel_id: channel.id,
          mix_id: mix.id,
          delta: 0.1,
        },
      });
      actions.push({
        label: `${channelDisplayName(channel)} ${mix.name}: -10`,
        action: {
          kind: "channel_volume_adjust",
          channel_id: channel.id,
          mix_id: mix.id,
          delta: -0.1,
        },
      });
      actions.push({
        label: `${channelDisplayName(channel)} ${mix.name}: from control`,
        action: {
          kind: "channel_volume_set_from_control",
          channel_id: channel.id,
          mix_id: mix.id,
        },
      });
    }
    for (const effect of channel.effects) {
      actions.push({
        label: `${channelDisplayName(channel)}: ${effect.name || effect.effect_id}`,
        action: {
          kind: "effect_bypass_toggle",
          channel_id: channel.id,
          instance_id: effect.instance_id,
        },
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
    return parsed && typeof parsed === "object" && "kind" in parsed
      ? parsed
      : { kind: "noop" };
  } catch {
    return { kind: "noop" };
  }
}

function primaryBusMixes(mixes: Mix[]): Mix[] {
  const monitor = mixes.find((mix) => mix.id === "monitor");
  const stream = mixes.find((mix) => mix.id === "stream");
  const primary = [monitor, stream].filter(Boolean) as Mix[];
  return primary.length === 2 ? primary : mixes.slice(0, 2);
}

function channelDisplayName(channel: Pick<Channel, "id" | "kind" | "name">): string {
  if (
    channel.id === "hardware_in" &&
    (channel.kind === "microphone" || channel.kind === "generic") &&
    ["hardware in", "hardware input", "input"].includes(channel.name.trim().toLowerCase())
  ) {
    return "Input";
  }
  return channel.name;
}
