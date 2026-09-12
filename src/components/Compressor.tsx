import { Pause, Play } from "lucide-react";
import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent,
} from "react";
import { useWaveLinuxMeters } from "../state";
import type {
  CompressorMeter,
  EffectDefinition,
  EffectInstance,
} from "../types";

const HISTORY_LENGTH = 160;
const FLOOR_DB = -60;
const clamp = (value: number, min: number, max: number) =>
  Math.max(min, Math.min(max, Number.isFinite(value) ? value : min));
export const amplitudeDb = (amplitude: number) =>
  amplitude > 0 ? Math.max(FLOOR_DB, 20 * Math.log10(amplitude)) : FLOOR_DB;
const levelFraction = (amplitude: number) =>
  (amplitudeDb(amplitude) - FLOOR_DB) / -FLOOR_DB;
const dbText = (amplitude: number) =>
  amplitude > 0 ? `${amplitudeDb(amplitude).toFixed(0)} dB` : "−∞ dB";

function wavePath(
  history: CompressorMeter[],
  key: "input_peak" | "output_peak",
) {
  if (history.length === 0) return "";
  const offset = HISTORY_LENGTH - history.length;
  const points = history.map((sample, index) => ({
    x: ((offset + index) / (HISTORY_LENGTH - 1)) * 1000,
    height: clamp(levelFraction(sample[key]), 0, 1) * 146,
  }));
  return `M${points[0].x},150 ${points.map((point) => `L${point.x.toFixed(1)},${(150 - point.height).toFixed(1)}`).join(" ")} ${[
    ...points,
  ]
    .reverse()
    .map((point) => `L${point.x.toFixed(1)},${(150 + point.height).toFixed(1)}`)
    .join(" ")} Z`;
}

// Browser development previews are explicitly marked Demo signal. Production
// displays use only measurements delivered by the native compressor.
function demoSample(
  time: number,
  effect: EffectInstance,
  definition: EffectDefinition,
): CompressorMeter {
  const parameter = (id: string) =>
    effect.params[id] ??
    definition.params.find((param) => param.id === id)?.default ??
    0;
  const phase = ((time % 4.8) + 4.8) % 4.8;
  const speaking = phase < 1.55 || (phase > 1.8 && phase < 2.6) || phase > 3;
  const inputDb = speaking
    ? -13 + 5 * Math.sin(time * 2.4) + 2 * Math.sin(time * 19)
    : FLOOR_DB;
  const reduction =
    Math.max(0, inputDb - parameter("threshold_db")) *
    (1 - 1 / Math.max(1, parameter("ratio")));
  return {
    input_peak: speaking ? 10 ** (inputDb / 20) : 0,
    output_peak: speaking
      ? 10 ** ((inputDb - reduction + parameter("makeup_gain_db")) / 20)
      : 0,
    gain_reduction_db: reduction,
  };
}

export function Compressor({
  channelId,
  definition,
  effect,
  enabled = true,
  onUpdateParam,
  demo,
}: {
  channelId?: string;
  definition: EffectDefinition;
  effect: EffectInstance;
  enabled?: boolean;
  onUpdateParam: (instanceId: string, paramId: string, value: number) => void;
  demo?: boolean;
}) {
  const meters = useWaveLinuxMeters();
  const live = meters.find((meter) => meter.node_id === channelId)?.compressor;
  const isDemo =
    demo ??
    (import.meta.env.DEV &&
      !("__TAURI_INTERNALS__" in window || "__TAURI__" in window));
  const active = enabled && !effect.bypassed;
  const parameter = definition.params.find(
    (param) => param.id === "threshold_db",
  );
  const value = effect.params.threshold_db ?? parameter?.default ?? -20;
  const min = parameter?.min ?? -60;
  const max = parameter?.max ?? 0;
  const [threshold, setThreshold] = useState(value);
  const [paused, setPaused] = useState(false);
  const [history, setHistory] = useState<CompressorMeter[]>(() =>
    isDemo && active
      ? Array.from({ length: HISTORY_LENGTH }, (_, i) =>
          demoSample(i / 30, effect, definition),
        )
      : [],
  );
  const [preview, setPreview] = useState<CompressorMeter | null>(null);
  const [expired, setExpired] = useState(false);
  const chart = useRef<HTMLDivElement>(null);
  const gesture = useRef(false);
  const draft = useRef(value);
  const lastCommitted = useRef(value);
  const demoTime = useRef(HISTORY_LENGTH / 30);
  const lineDrag = useRef<number | null>(null);
  const pausedRef = useRef(paused);
  pausedRef.current = paused;
  const current = active
    ? isDemo
      ? (preview ?? history.at(-1))
      : expired
        ? null
        : live
    : null;

  useEffect(() => {
    if (!gesture.current) {
      setThreshold(value);
      draft.current = value;
      lastCommitted.current = value;
    }
  }, [value]);
  useEffect(() => {
    setHistory(
      isDemo && active
        ? Array.from({ length: HISTORY_LENGTH }, (_, i) =>
            demoSample(i / 30, effect, definition),
          )
        : [],
    );
    setPreview(null);
    demoTime.current = HISTORY_LENGTH / 30;
    // Reset only when changing the displayed processor, not on parameter edits.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [channelId, effect.instance_id, active, isDemo]);
  useEffect(() => {
    setExpired(false);
    if (isDemo || !live) return;
    const timer = window.setTimeout(() => {
      setExpired(true);
      if (!pausedRef.current) setHistory([]);
    }, 1000);
    return () => window.clearTimeout(timer);
  }, [meters, live, isDemo, channelId]);
  useEffect(() => {
    if (isDemo || paused || !active || document.hidden) return;
    setHistory((previous) =>
      live && !expired ? [...previous.slice(-(HISTORY_LENGTH - 1)), live] : [],
    );
  }, [meters, live, paused, active, isDemo, expired]);
  useEffect(() => {
    if (!isDemo || !active) return;
    const timer = window.setInterval(() => {
      if (document.hidden) return;
      demoTime.current += 1 / 30;
      const sample = demoSample(
        demoTime.current,
        { ...effect, params: { ...effect.params, threshold_db: threshold } },
        definition,
      );
      setPreview(sample);
      if (!paused)
        setHistory((previous) => [
          ...previous.slice(-(HISTORY_LENGTH - 1)),
          sample,
        ]);
    }, 1000 / 30);
    return () => window.clearInterval(timer);
  }, [isDemo, active, paused, effect, definition, threshold]);

  function update(raw: number) {
    const next = Math.round(clamp(raw, min, max) * 2) / 2;
    gesture.current = true;
    draft.current = next;
    setThreshold(next);
  }
  function commit() {
    if (!gesture.current) return;
    gesture.current = false;
    if (draft.current !== lastCommitted.current) {
      lastCommitted.current = draft.current;
      onUpdateParam(effect.instance_id, "threshold_db", draft.current);
    }
  }
  function cancel() {
    gesture.current = false;
    draft.current = value;
    lastCommitted.current = value;
    setThreshold(value);
  }
  function moveLine(event: PointerEvent<HTMLDivElement>) {
    const bounds = chart.current?.getBoundingClientRect();
    if (lineDrag.current !== event.pointerId || !bounds?.height) return;
    update(((event.clientY - bounds.top) / (bounds.height / 2)) * FLOOR_DB);
  }
  function endLine(event: PointerEvent<HTMLDivElement>, cancelled = false) {
    if (lineDrag.current !== event.pointerId) return;
    lineDrag.current = null;
    if (cancelled) cancel();
    else commit();
    if (event.currentTarget.hasPointerCapture?.(event.pointerId))
      event.currentTarget.releasePointerCapture(event.pointerId);
  }

  return (
    <div
      className="visual-compressor"
      role="group"
      aria-label="Compressor controls"
    >
      <div className="visual-effect-guidance">
        <p>Bring loud and quiet moments closer together.</p>
        <span className="compressor-source-label">
          {!active
            ? "Off"
            : isDemo
              ? "Demo signal"
              : current
                ? "Live"
                : "Waiting for audio"}
        </span>
      </div>
      <div className="compressor-display">
        <SignalMeter
          label="Input"
          value={current ? levelFraction(current.input_peak) : 0}
          readout={current ? dbText(current.input_peak) : "—"}
        />
        <div className="compressor-chart" ref={chart}>
          <svg
            viewBox="0 0 1000 300"
            preserveAspectRatio="none"
            role="img"
            aria-label="Audio before and after compression"
          >
            <line
              className="compressor-center"
              x1="0"
              x2="1000"
              y1="150"
              y2="150"
            />
            <path
              className="compressor-wave-before"
              d={wavePath(history, "input_peak")}
            />
            <path
              className="compressor-wave-after"
              d={wavePath(history, "output_peak")}
            />
          </svg>
          <div
            className="compressor-threshold-line"
            style={{ top: `${(threshold / FLOOR_DB) * 50}%` }}
            aria-hidden="true"
            onPointerDown={(event) => {
              if (event.button !== 0) return;
              event.preventDefault();
              lineDrag.current = event.pointerId;
              gesture.current = true;
              event.currentTarget.setPointerCapture?.(event.pointerId);
            }}
            onPointerMove={moveLine}
            onPointerUp={(event) => endLine(event)}
            onPointerCancel={(event) => endLine(event, true)}
            onLostPointerCapture={(event) => endLine(event, true)}
          >
            <span>{threshold.toFixed(1)} dB</span>
          </div>
          {!active || (!current && history.length === 0) ? (
            <div className="compressor-empty">
              {active ? "Play audio to see it here" : "Compressor is off"}
            </div>
          ) : null}
          <button
            className="compressor-pause"
            type="button"
            onClick={() => setPaused((value) => !value)}
            aria-label={paused ? "Resume display" : "Pause display"}
            aria-pressed={paused}
          >
            {paused ? <Play size={16} /> : <Pause size={16} />}
          </button>
          {paused && (
            <span className="compressor-paused-label">Display paused</span>
          )}
        </div>
        <div className="compressor-threshold-column">
          <output>{threshold.toFixed(1)} dB</output>
          <div className="compressor-threshold-track">
            <input
              aria-label="Threshold"
              aria-orientation="vertical"
              aria-valuetext={`${threshold.toFixed(1)} dB`}
              type="range"
              min={min}
              max={max}
              step="0.5"
              value={threshold}
              style={
                {
                  "--threshold-range": (max - min) / 120,
                  "--threshold-offset": -max / 120,
                } as CSSProperties
              }
              onChange={(event) => update(Number(event.currentTarget.value))}
              onPointerDown={(event) => {
                if (event.button !== 0) return;
                // Let the native thumb own pointer capture. Capturing on the
                // input itself prevents WebKit's thumb from dragging.
                gesture.current = true;
              }}
              onPointerUp={commit}
              onBlur={commit}
              onKeyUp={(event) => {
                if (
                  [
                    "ArrowUp",
                    "ArrowDown",
                    "ArrowLeft",
                    "ArrowRight",
                    "Home",
                    "End",
                    "PageUp",
                    "PageDown",
                    "Enter",
                  ].includes(event.key)
                )
                  commit();
              }}
              onPointerCancel={cancel}
              onLostPointerCapture={() => {
                if (gesture.current) cancel();
              }}
            />
          </div>
          <span>Threshold</span>
        </div>
        <SignalMeter
          label="Reduction"
          value={
            current && current.gain_reduction_db >= 0.05
              ? current.gain_reduction_db / 24
              : 0
          }
          readout={
            current
              ? `${current.gain_reduction_db >= 0.05 ? "−" : ""}${current.gain_reduction_db.toFixed(1)} dB`
              : "—"
          }
          reduction
        />
        <SignalMeter
          label="Output"
          value={current ? levelFraction(current.output_peak) : 0}
          readout={current ? dbText(current.output_peak) : "—"}
        />
      </div>
      <div className="compressor-footer">
        <div className="compressor-legend">
          <span className="before">Before</span>
          <span className="after">After</span>
        </div>
        <p>Lower the threshold to even out more of your audio.</p>
      </div>
    </div>
  );
}

function SignalMeter({
  label,
  value,
  readout,
  reduction = false,
}: {
  label: string;
  value: number;
  readout: string;
  reduction?: boolean;
}) {
  const normalized = clamp(value, 0, 1);
  return (
    <div
      className={
        reduction
          ? "compressor-signal-meter reduction"
          : "compressor-signal-meter"
      }
    >
      <output>{readout}</output>
      <div
        className="compressor-meter-segments"
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(normalized * 100)}
        aria-valuetext={readout}
      >
        {Array.from({ length: 32 }, (_, index) => (
          <span
            key={index}
            className={
              (
                reduction
                  ? index < Math.ceil(normalized * 32)
                  : 31 - index < Math.ceil(normalized * 32)
              )
                ? "lit"
                : undefined
            }
          />
        ))}
      </div>
      <span>{label}</span>
    </div>
  );
}
