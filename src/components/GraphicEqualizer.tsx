import { RotateCcw } from "lucide-react";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent,
} from "react";
import {
  clampGain,
  equalizerBands,
  EQ_SHAPES,
  FREQUENCIES,
  frequencyLabel,
  frequencyPosition,
  gainLabel,
  positionFrequency,
  responsePath,
  responsePoints,
} from "../equalizer-response";
import { useWaveLinuxMeters } from "../state";
import type { EffectDefinition, EffectInstance } from "../types";

export type GraphicEqualizerProps = {
  definition: EffectDefinition;
  effect: EffectInstance;
  channelId?: string;
  enabled?: boolean;
  onUpdateParam: (instanceId: string, paramId: string, value: number) => void;
  onApplyValues?: (instanceId: string, values: Record<string, number>) => void;
  onReset?: () => void;
};

type Band = ReturnType<typeof equalizerBands>[number];
export function GraphicEqualizer({
  definition,
  effect,
  channelId,
  enabled = true,
  onUpdateParam,
  onApplyValues,
  onReset,
}: GraphicEqualizerProps) {
  const [draft, setDraft] = useState<Record<string, number>>({});
  const [selected, setSelected] = useState(0);
  const [hovered, setHovered] = useState<number | null>(null);
  const plot = useRef<HTMLDivElement>(null);
  const drag = useRef<{
    id: string;
    values: Record<string, number>;
    pointerId: number;
    startX: number;
    startY: number;
    frequency: number;
    gain: number;
  } | null>(null);
  const bands = useMemo(
    () =>
      equalizerBands(definition, {
        ...effect,
        params: { ...effect.params, ...draft },
      }),
    [definition, effect, draft],
  );
  const selectedBand = bands[selected] ?? bands[0];
  const tooltipBand = hovered === null ? null : bands[hovered];
  const curve = useMemo(() => responsePath(responsePoints(bands)), [bands]);
  const fills = useMemo(
    () =>
      bands.map(
        (band) => `${responsePath(responsePoints([band]))} L1000,150 L0,150 Z`,
      ),
    [bands],
  );
  const meters = useWaveLinuxMeters();
  const live = meters.find((meter) => meter.node_id === channelId)?.spectrum;
  const [expired, setExpired] = useState(false);
  useEffect(() => {
    setExpired(false);
    const timeout = window.setTimeout(() => setExpired(true), 1000);
    return () => window.clearTimeout(timeout);
  }, [live, channelId]);
  const spectrum = !expired && live?.length === 64 ? live : undefined;
  const spectrumPath = spectrum
    ? `M0,300 ${spectrum.map((value, index) => `L${(index * 1000) / 63},${300 * (1 - clampGain(value, 0, 1))}`).join(" ")} L1000,300 Z`
    : "";

  useEffect(() => {
    // Keep an active gesture intact across a delayed acknowledgement.
    setDraft(drag.current?.values ?? {});
  }, [effect.params]);

  useEffect(() => {
    const element = plot.current;
    const stopScroll = (event: WheelEvent) => {
      if (event.target instanceof Element && event.target.closest(".eq-point"))
        event.preventDefault();
    };
    element?.addEventListener("wheel", stopScroll, { passive: false });
    return () => element?.removeEventListener("wheel", stopScroll);
  }, []);

  function commitValues(values: Record<string, number>) {
    setDraft((current) => ({ ...current, ...values }));
    // A two-dimensional drag changes frequency and gain in one engine update.
    if (onApplyValues && Object.keys(values).length > 1)
      onApplyValues(effect.instance_id, values);
    else
      for (const [id, value] of Object.entries(values))
        onUpdateParam(effect.instance_id, id, value);
  }
  function gain(band: Band, value: number) {
    if (band.shape >= 3) return;
    commitValues({
      [band.id]: Math.round(clampGain(value, band.min, band.max) * 2) / 2,
    });
  }
  function finishDrag(
    event: PointerEvent<HTMLButtonElement>,
    cancelled = false,
  ) {
    const active = drag.current;
    if (!active || active.pointerId !== event.pointerId) return;
    drag.current = null;
    if (cancelled) setDraft({});
    else if (Object.keys(active.values).length) commitValues(active.values);
    if (event.currentTarget.hasPointerCapture?.(event.pointerId))
      event.currentTarget.releasePointerCapture(event.pointerId);
  }

  return (
    <div className="visual-eq" role="group" aria-label="8-band equalizer">
      <div className="visual-effect-guidance">
        <p>Drag dots to shape your sound. Scroll a dot to change its width.</p>
        {onReset && (
          <button
            className="visual-reset"
            type="button"
            onClick={() => {
              drag.current = null;
              setDraft({});
              onReset();
            }}
          >
            <RotateCcw size={14} /> Reset EQ
          </button>
        )}
      </div>
      <div className="eq-chart-row">
        <div className="eq-db-scale" aria-hidden="true">
          {[12, 6, 0, -6, -12].map((db) => (
            <span key={db}>
              {db > 0 ? `+${db}` : db}
              {db === 12 ? " dB" : ""}
            </span>
          ))}
        </div>
        <div className="eq-plot" ref={plot}>
          <svg
            viewBox="0 0 1000 300"
            preserveAspectRatio="none"
            role="img"
            aria-label="Equalizer frequency response"
          >
            {FREQUENCIES.map((freq, index) => {
              const start =
                index === 0
                  ? 0
                  : (frequencyPosition(FREQUENCIES[index - 1]) +
                      frequencyPosition(freq)) *
                    500;
              const end =
                index === FREQUENCIES.length - 1
                  ? 1000
                  : (frequencyPosition(freq) +
                      frequencyPosition(FREQUENCIES[index + 1])) *
                    500;
              return (
                <rect
                  key={freq}
                  x={start}
                  y="0"
                  width={end - start}
                  height="300"
                  fill="currentColor"
                  opacity={index % 2 === 0 ? 0.035 : 0}
                />
              );
            })}
            {[0, 75, 150, 225, 300].map((y) => (
              <line
                className={y === 150 ? "eq-zero" : "eq-grid"}
                key={y}
                x1="0"
                x2="1000"
                y1={y}
                y2={y}
                vectorEffect="non-scaling-stroke"
              />
            ))}
            <path
              className="eq-spectrum"
              data-testid="eq-spectrum"
              d={spectrumPath}
            />
            {bands.map((band, index) => (
              <path
                key={band.id}
                d={fills[index]}
                fill={band.color}
                opacity="0.22"
              />
            ))}
            <path
              className="eq-response"
              d={curve}
              fill="none"
              vectorEffect="non-scaling-stroke"
            />
          </svg>
          {bands.map((band, index) => (
            <button
              key={band.id}
              type="button"
              role="slider"
              aria-label={`${band.name} · ${band.frequencyLabel} ${band.shape >= 3 ? "frequency" : "gain"}`}
              aria-valuemin={band.shape >= 3 ? 20 : band.min}
              aria-valuemax={band.shape >= 3 ? 20000 : band.max}
              aria-valuenow={band.shape >= 3 ? band.frequency : band.gain}
              aria-orientation={band.shape >= 3 ? "horizontal" : "vertical"}
              aria-valuetext={
                band.shape >= 3 ? band.frequencyLabel : gainLabel(band.gain)
              }
              aria-description="Left and right adjust frequency. Up and down adjust gain. Scroll adjusts width."
              className={selected === index ? "eq-point selected" : "eq-point"}
              style={
                {
                  left: `${frequencyPosition(band.frequency) * 100}%`,
                  top: `${((12 - (band.shape >= 3 ? 0 : band.gain)) / 24) * 100}%`,
                  "--band-color": band.color,
                } as CSSProperties
              }
              onFocus={() => {
                setSelected(index);
                setHovered(index);
              }}
              onBlur={() => setHovered(null)}
              onPointerEnter={() => setHovered(index)}
              onPointerLeave={() => {
                if (!drag.current) setHovered(null);
              }}
              onPointerDown={(event) => {
                if (event.button !== 0 || drag.current) return;
                event.preventDefault();
                event.currentTarget.focus();
                setSelected(index);
                setHovered(index);
                drag.current = {
                  id: band.id,
                  values: {},
                  pointerId: event.pointerId,
                  startX: event.clientX,
                  startY: event.clientY,
                  frequency: band.frequency,
                  gain: band.gain,
                };
                event.currentTarget.setPointerCapture?.(event.pointerId);
              }}
              onPointerMove={(event) => {
                if (
                  drag.current?.id !== band.id ||
                  drag.current.pointerId !== event.pointerId
                )
                  return;
                const bounds = plot.current?.getBoundingClientRect();
                if (!bounds?.width || !bounds.height) return;
                const values: Record<string, number> = {
                  [band.frequencyId]: positionFrequency(
                    frequencyPosition(drag.current.frequency) +
                      (event.clientX - drag.current.startX) /
                        Math.max(1, bounds.width - 2),
                  ),
                };
                if (band.shape < 3)
                  values[band.id] =
                    Math.round(
                      clampGain(
                        drag.current.gain -
                          ((event.clientY - drag.current.startY) /
                            Math.max(1, bounds.height - 2)) *
                            24,
                      ) * 2,
                    ) / 2;
                drag.current.values = values;
                setDraft((current) => ({ ...current, ...values }));
              }}
              onPointerUp={(event) => finishDrag(event)}
              onPointerCancel={(event) => finishDrag(event, true)}
              onLostPointerCapture={(event) => finishDrag(event, true)}
              onDoubleClick={() => gain(band, 0)}
              onWheel={(event) => {
                if (!event.deltaY || drag.current) return;
                event.currentTarget.focus();
                commitValues({
                  [band.qId]:
                    Math.round(
                      clampGain(
                        band.q * (event.deltaY < 0 ? 1.1 : 1 / 1.1),
                        0.2,
                        10,
                      ) * 100,
                    ) / 100,
                });
              }}
              onKeyDown={(event) => {
                if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
                  event.preventDefault();
                  commitValues({
                    [band.frequencyId]: Math.round(
                      clampGain(
                        band.frequency *
                          2 **
                            ((event.key === "ArrowRight" ? 1 : -1) /
                              (event.shiftKey ? 3 : 12)),
                        20,
                        20000,
                      ),
                    ),
                  });
                  return;
                }
                const increment = event.shiftKey ? 2 : 0.5;
                const value = (
                  {
                    ArrowUp: band.gain + increment,
                    ArrowDown: band.gain - increment,
                    Home: 0,
                    PageUp: band.gain + 3,
                    PageDown: band.gain - 3,
                  } as Record<string, number>
                )[event.key];
                if (value !== undefined) {
                  event.preventDefault();
                  gain(band, value);
                }
              }}
            >
              <span />
            </button>
          ))}
          {tooltipBand && (
            <div
              className="eq-tooltip"
              role="tooltip"
              style={{
                left: `${clampGain(frequencyPosition(tooltipBand.frequency) * 100, 12, 88)}%`,
                top: `${clampGain(((12 - (tooltipBand.shape >= 3 ? 0 : tooltipBand.gain)) / 24) * 100, 10, 65)}%`,
              }}
            >
              <span>
                Frequency <b>{tooltipBand.frequencyLabel}</b>
              </span>
              {tooltipBand.shape < 3 && (
                <span>
                  Gain <b>{gainLabel(tooltipBand.gain)}</b>
                </span>
              )}
              <span>
                Width (Q) <b>{tooltipBand.q.toFixed(2)}</b>
              </span>
            </div>
          )}
        </div>
      </div>
      <div className="eq-frequency-labels" aria-hidden="true">
        {FREQUENCIES.map((freq, index) => (
          <span
            key={freq}
            style={{ left: `${frequencyPosition(freq) * 100}%` }}
          >
            <strong>
              {
                [
                  "Bass",
                  "Warmth",
                  "Body",
                  "Boxiness",
                  "Midrange",
                  "Clarity",
                  "Presence",
                  "Air",
                ][index]
              }
            </strong>
            <small>{frequencyLabel(freq)}</small>
          </span>
        ))}
      </div>
      {selectedBand && (
        <div
          className="eq-selected-band"
          style={{ "--band-color": selectedBand.color } as CSSProperties}
        >
          <span className="eq-band-swatch" />
          <strong>{selectedBand.name}</strong>
          <label className="eq-parameter">
            Shape
            <select
              aria-label="Filter shape"
              value={selectedBand.shape}
              onChange={(event) =>
                commitValues({
                  [selectedBand.typeId]: Number(event.target.value),
                })
              }
            >
              {EQ_SHAPES.map((name, index) => (
                <option key={name} value={index}>
                  {name}
                </option>
              ))}
            </select>
          </label>
          <EqNumber
            key={`${selectedBand.id}-frequency`}
            label="Frequency (Hz)"
            value={selectedBand.frequency}
            min={20}
            max={20000}
            step={1}
            onCommit={(value) =>
              commitValues({ [selectedBand.frequencyId]: value })
            }
          />
          {selectedBand.shape < 3 && (
            <div className="eq-gain-stepper">
              <button
                type="button"
                aria-label={`Reduce ${selectedBand.name}`}
                onClick={() => gain(selectedBand, selectedBand.gain - 0.5)}
              >
                −
              </button>
              <output aria-live="off">{gainLabel(selectedBand.gain)}</output>
              <button
                type="button"
                aria-label={`Boost ${selectedBand.name}`}
                onClick={() => gain(selectedBand, selectedBand.gain + 0.5)}
              >
                +
              </button>
            </div>
          )}
          <EqNumber
            key={`${selectedBand.id}-q`}
            label="Width (Q)"
            value={selectedBand.q}
            min={0.2}
            max={10}
            step={0.1}
            onCommit={(value) => commitValues({ [selectedBand.qId]: value })}
          />
        </div>
      )}
      <div className="eq-live-caption">
        <span>
          {spectrum ? "Live output · After effects" : "Waiting for this channel’s audio"}
        </span>
        <span>
          {!enabled || effect.bypassed
            ? "EQ bypassed"
            : "Higher Q = narrower curve"}
        </span>
      </div>
    </div>
  );
}

function EqNumber({
  label,
  value,
  min,
  max,
  step,
  onCommit,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onCommit: (value: number) => void;
}) {
  const [text, setText] = useState(String(value));
  useEffect(() => setText(String(value)), [value]);
  return (
    <label className="eq-parameter">
      {label}
      <input
        type="number"
        aria-label={label}
        min={min}
        max={max}
        step={step}
        value={text}
        onChange={(event) => setText(event.target.value)}
        onBlur={() => {
          const next = text.trim() ? clampGain(Number(text), min, max) : value;
          setText(String(next));
          if (next !== value) onCommit(next);
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") event.currentTarget.blur();
          if (event.key === "Escape") {
            setText(String(value));
            event.preventDefault();
          }
        }}
      />
    </label>
  );
}
