import { useCallback, useEffect, useRef, useState } from "react";
import type { EffectDefinition, EffectInstance, EffectParamDefinition } from "../types";

export type GraphicEqualizerProps = {
  definition: EffectDefinition;
  effect: EffectInstance;
  onUpdateParam: (instanceId: string, paramId: string, value: number) => void;
};

export function GraphicEqualizer({
  definition,
  effect,
  onUpdateParam,
}: GraphicEqualizerProps) {
  const bands = definition.params.filter((param) => param.id.startsWith("band_"));
  return (
    <div className="graphic-eq" role="group" aria-label="8-band equalizer">
      <div className="graphic-eq-scale" aria-hidden="true">
        <span>+12</span>
        <span>0</span>
        <span>-12</span>
      </div>
      <div className="graphic-eq-bands">
        {bands.map((param) => (
          <EqualizerBandFader
            effect={effect}
            key={param.id}
            onUpdateParam={onUpdateParam}
            param={param}
            value={effect.params[param.id] ?? param.default}
          />
        ))}
      </div>
    </div>
  );
}

function EqualizerBandFader({
  effect,
  onUpdateParam,
  param,
  value,
}: {
  effect: EffectInstance;
  onUpdateParam: GraphicEqualizerProps["onUpdateParam"];
  param: EffectParamDefinition;
  value: number;
}) {
  const [draft, setDraft] = useState(value);
  const lastCommitted = useRef(value);
  const display = Math.round(draft * 10) / 10;

  useEffect(() => {
    setDraft(value);
    lastCommitted.current = value;
  }, [value]);

  const commit = useCallback(
    (raw: number) => {
      const next = Number.isFinite(raw)
        ? Math.max(param.min, Math.min(param.max, raw))
        : value;
      setDraft(next);
      if (lastCommitted.current === next) return;
      lastCommitted.current = next;
      onUpdateParam(effect.instance_id, param.id, next);
    },
    [effect.instance_id, onUpdateParam, param.id, param.max, param.min, value],
  );

  return (
    <label className="graphic-eq-band">
      <strong>{display > 0 ? `+${display}` : display}</strong>
      <div className="graphic-eq-fader">
        <div className="graphic-eq-zero-line" aria-hidden="true" />
        <input
          aria-label={`${param.label} Hz gain`}
          max={param.max}
          min={param.min}
          onBlur={(event) => commit(Number(event.currentTarget.value))}
          onChange={(event) => setDraft(Number(event.currentTarget.value))}
          onKeyUp={(event) => {
            if (event.key === "Enter") commit(Number(event.currentTarget.value));
          }}
          onPointerUp={(event) => commit(Number(event.currentTarget.value))}
          step={0.5}
          type="range"
          value={draft}
        />
      </div>
      <span>{param.label}</span>
    </label>
  );
}
