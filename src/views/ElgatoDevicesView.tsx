import {
  CircleAlert,
  Gauge,
  Headphones,
  Mic,
  RefreshCw,
  SlidersHorizontal,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { EmptyState, Stat, Toggle, VolumeFader } from "../components/Controls";
import { invoke } from "../tauri";
import type { ElgatoDeviceSummary, ElgatoWaveXlrState } from "../types";

const ELGATO_POLL_MS = 1500;

export function ElgatoDevicesView() {
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
    void loadElgato(false);
  }, [loadElgato]);

  const controlsPresent = devices.some((device) => device.controls_supported);
  useEffect(() => {
    if (!controlsPresent) return undefined;
    let cancelled = false;
    const tick = () => {
      if (cancelled || document.visibilityState !== "visible") return;
      void loadElgato(false);
    };
    const interval = window.setInterval(tick, ELGATO_POLL_MS);
    const handleVisibility = () => {
      if (document.visibilityState === "visible") tick();
    };
    document.addEventListener("visibilitychange", handleVisibility);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
      document.removeEventListener("visibilitychange", handleVisibility);
    };
  }, [controlsPresent, loadElgato]);

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
              className={
                device.controls_supported ? "elgato-device-row active" : "elgato-device-row"
              }
              key={device.id}
            >
              <div>
                <strong>{device.name}</strong>
                <span>{device.description}</span>
              </div>
              <div className="elgato-device-meta">
                <span>{elgatoKindLabel(device.kind)}</span>
                <span>{device.bus ?? "unknown"}</span>
                {device.product_id && (
                  <span>
                    {device.vendor_id ?? "usb"}:{device.product_id}
                  </span>
                )}
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
                <Stat
                  icon={Headphones}
                  label="Headphones"
                  value={`${waveXlr.hp_volume_db.toFixed(1)} dB`}
                />
                <Stat
                  icon={SlidersHorizontal}
                  label="Knob"
                  value={waveXlr.volume_select === "headphones" ? "Headphones" : "Gain"}
                />
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
            <EmptyState
              label={
                elgatoError ? "Wave XLR controls unavailable" : "Reading Wave XLR controls"
              }
            />
          ) : (
            <EmptyState label="No controllable Wave XLR found" />
          )}
        </div>
      </div>
    </section>
  );
}

function elgatoKindLabel(kind: ElgatoDeviceSummary["kind"]): string {
  return {
    wave_xlr: "Wave XLR",
    wave_microphone: "Wave microphone",
    capture_audio: "Capture audio",
    audio_endpoint: "Audio endpoint",
  }[kind];
}

function formatHexGain(value: number): string {
  return `0x${Math.max(0, Math.round(value)).toString(16).toUpperCase().padStart(4, "0")}`;
}
