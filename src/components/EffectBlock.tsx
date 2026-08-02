import {
  ArrowDown,
  ArrowUp,
  CircleAlert,
  GripVertical,
  Pencil,
  Settings2,
  Trash2,
} from "lucide-react";
import { useState } from "react";
import {
  simpleEffectParams,
  simpleEffectStrength,
  simpleEffectStrengthLabel,
  simplePresetEffectIds,
  simpleStrengthEffectIds,
} from "../effect-strength";
import type {
  EffectAvailability,
  EffectDefinition,
  EffectInstance,
} from "../types";
import { AppSelect } from "./AppSelect";
import { VolumeFader } from "./Controls";
import { GraphicEqualizer } from "./GraphicEqualizer";

export function EffectBlock({
  availability,
  effect,
  definition,
  index,
  total,
  onApplyPreset,
  onDelete,
  onMove,
  onRename,
  onUpdateParam,
}: {
  availability?: EffectAvailability;
  effect: EffectInstance;
  definition?: EffectDefinition;
  index: number;
  total: number;
  onApplyPreset: (instanceId: string, values: Record<string, number>) => void;
  onDelete: (instanceId: string) => void;
  onMove: (index: number, direction: -1 | 1) => void;
  onRename: (instanceId: string) => void;
  onUpdateParam: (instanceId: string, paramId: string, value: number) => void;
}) {
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const isVoiceStyle = definition?.id === "karaoke_stage";
  const isGraphicEq = definition?.id === "eq";
  const hasSimpleStrength = definition
    ? simpleStrengthEffectIds.has(definition.id)
    : false;
  const showsSimplePresets = definition
    ? simplePresetEffectIds.has(definition.id)
    : false;
  const selectedPreset = definition ? matchingPresetName(definition, effect) : null;
  const advancedId = `effect-advanced-${effect.instance_id}`;

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
          {hasSimpleStrength && (
            <button
              aria-controls={advancedId}
              aria-expanded={advancedOpen}
              className={advancedOpen ? "effect-advanced-button active" : "effect-advanced-button"}
              onClick={() => setAdvancedOpen((open) => !open)}
              type="button"
            >
              <Settings2 size={14} />
              Advanced
            </button>
          )}
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
      {definition && definition.presets.length > 0 && (!hasSimpleStrength || showsSimplePresets) &&
        (isVoiceStyle ? (
          <EffectStyleSelect
            definition={definition}
            effect={effect}
            onApplyPreset={onApplyPreset}
          />
        ) : (
          <div className="preset-row">
            {definition.presets.map((preset) => (
              <button
                aria-pressed={selectedPreset === preset.name}
                className={selectedPreset === preset.name ? "active" : undefined}
                key={preset.name}
                onClick={() => onApplyPreset(effect.instance_id, preset.values)}
                type="button"
              >
                {preset.name}
              </button>
            ))}
          </div>
        ))}
      {definition && isGraphicEq ? (
        <GraphicEqualizer
          definition={definition}
          effect={effect}
          onUpdateParam={onUpdateParam}
        />
      ) : definition && hasSimpleStrength ? (
        <VolumeFader
          compact
          formatValue={(value) => simpleEffectStrengthLabel(effect.effect_id, value)}
          label="Strength"
          max={100}
          min={0}
          step={0.1}
          unit="%"
          value={simpleEffectStrength(effect, definition)}
          onChange={(value) =>
            onApplyPreset(effect.instance_id, simpleEffectParams(effect.effect_id, value))
          }
        />
      ) : (
        definition?.params.map((param) => (
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
        ))
      )}
      {definition && hasSimpleStrength && advancedOpen && (
        <div className="effect-advanced-controls" id={advancedId}>
          <strong className="effect-advanced-heading">Parameters</strong>
          {definition.params.map((param) => (
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
        </div>
      )}
    </article>
  );
}

function EffectStyleSelect({
  definition,
  effect,
  onApplyPreset,
}: {
  definition: EffectDefinition;
  effect: EffectInstance;
  onApplyPreset: (instanceId: string, values: Record<string, number>) => void;
}) {
  const selectedPreset = matchingPresetName(definition, effect);
  return (
    <div className="effect-style-select">
      <label className="field-label" htmlFor={`voice-style-${effect.instance_id}`}>
        Style
      </label>
      <AppSelect
        ariaLabel="Voice style"
        id={`voice-style-${effect.instance_id}`}
        onChange={(value) => {
          const preset = definition.presets.find((item) => item.name === value);
          if (preset) onApplyPreset(effect.instance_id, preset.values);
        }}
        options={[
          { value: "", label: "Custom" },
          ...definition.presets.map((preset) => ({
            value: preset.name,
            label: preset.name,
          })),
        ]}
        value={selectedPreset ?? ""}
      />
    </div>
  );
}

function matchingPresetName(
  definition: EffectDefinition,
  effect: EffectInstance,
): string | null {
  for (const preset of definition.presets) {
    const matches = Object.entries(preset.values).every(([paramId, expected]) => {
      const param = definition.params.find((item) => item.id === paramId);
      const actual = effect.params[paramId] ?? param?.default;
      return typeof actual === "number" && Math.abs(actual - expected) <= 0.001;
    });
    if (matches) return preset.name;
  }
  return null;
}
