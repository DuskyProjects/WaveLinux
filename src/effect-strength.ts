import type { EffectDefinition, EffectInstance } from "./types";

export const simpleStrengthEffectIds = new Set([
  "rnnoise",
  "highpass",
  "compressor",
  "gate",
  "limiter",
]);

export const simplePresetEffectIds = new Set([
  "highpass",
  "compressor",
  "gate",
  "limiter",
]);

function clampStrength(value: number): number {
  return Math.max(0, Math.min(100, Number.isFinite(value) ? value : 0));
}

function clampUnit(value: number): number {
  return Math.max(0, Math.min(1, Number.isFinite(value) ? value : 0));
}

function roundTo(value: number, step: number): number {
  const decimals = Math.max(0, Math.ceil(-Math.log10(step)));
  return Number((Math.round(value / step) * step).toFixed(decimals));
}

function effectParamValue(
  effect: EffectInstance,
  definition: EffectDefinition,
  paramId: string,
): number {
  return (
    effect.params[paramId] ??
    definition.params.find((param) => param.id === paramId)?.default ??
    0
  );
}

export function simpleEffectStrength(
  effect: EffectInstance,
  definition: EffectDefinition,
): number {
  let normalized = 0;
  switch (effect.effect_id) {
    case "rnnoise": {
      const vad = (effectParamValue(effect, definition, "vad_threshold") - 25) / 70;
      const nearLevel =
        (effectParamValue(effect, definition, "minimum_voice_level_db") + 65) / 37;
      // VAD is the best compatibility signal for older configs. The level gate
      // still contributes so Advanced edits are reflected without making an
      // existing aggressive setup appear unexpectedly weak.
      normalized = vad * 0.8 + nearLevel * 0.2;
      break;
    }
    case "highpass": {
      const frequency = Math.max(20, effectParamValue(effect, definition, "frequency_hz"));
      normalized = Math.log10(frequency / 20);
      break;
    }
    case "compressor": {
      const threshold = clampUnit(
        (-effectParamValue(effect, definition, "threshold_db") - 10) / 20,
      );
      const ratio = clampUnit(
        (effectParamValue(effect, definition, "ratio") - 1.5) / 5,
      );
      const attack = clampUnit(
        (8 - effectParamValue(effect, definition, "attack_ms")) / 6,
      );
      const release = clampUnit(
        (140 - effectParamValue(effect, definition, "release_ms")) / 80,
      );
      const makeup = clampUnit(
        effectParamValue(effect, definition, "makeup_gain_db") / 4,
      );
      normalized =
        threshold * 0.35
        + ratio * 0.25
        + attack * 0.15
        + release * 0.15
        + makeup * 0.1;
      break;
    }
    case "gate": {
      normalized = clampUnit(
        (effectParamValue(effect, definition, "threshold_db") + 70) / 50,
      );
      break;
    }
    case "limiter": {
      const inputGain = effectParamValue(effect, definition, "input_gain_db");
      const ceiling = effectParamValue(effect, definition, "ceiling_db");
      // The simplified scale deliberately passes through all shipped presets:
      // Gentle=0%, Broadcast=50%, Loud=75%, and +6 dB drive=100%.
      normalized = inputGain > 0
        ? 0.5 + inputGain / 12
        : (ceiling + 3) / 4;
      break;
    }
  }
  return roundTo(clampStrength(normalized * 100), 0.1);
}

export function simpleEffectParams(
  effectId: string,
  strength: number,
): Record<string, number> {
  const amount = clampStrength(strength) / 100;
  switch (effectId) {
    case "rnnoise":
      return {
        vad_threshold: roundTo(25 + 70 * amount, 0.1),
        hold_ms: roundTo(250 - 175 * amount, 1),
        // At the upper end, require near-field voice energy. This rejects speech
        // from televisions and people across a room while keeping lower settings
        // suitable for quieter or more distant microphones.
        minimum_voice_level_db: roundTo(-65 + 37 * amount, 0.1),
        // Aggressiveness changes detection, not wet/dry balance. Mixing the
        // untreated mic back in defeats suppression and raises room noise.
        dry_mix: 0,
      };
    case "highpass":
      return { frequency_hz: roundTo(20 * Math.pow(10, amount), 0.5) };
    case "compressor":
      return {
        threshold_db: roundTo(-10 - 20 * amount, 0.1),
        ratio: roundTo(1.5 + 5 * amount, 0.1),
        attack_ms: roundTo(8 - 6 * amount, 0.1),
        release_ms: roundTo(140 - 80 * amount, 1),
        makeup_gain_db: roundTo(4 * amount, 0.1),
      };
    case "gate":
      // Make every percentage a direct 0.5 dB threshold step. The upper end is
      // deliberately strong enough to reject distant speech below -20 dB,
      // while 0% remains fully transparent because its range is 0 dB.
      return {
        threshold_db: roundTo(-70 + 50 * amount, 0.1),
        attack_ms: roundTo(8 - 7 * Math.sqrt(amount), 0.1),
        hold_ms: roundTo(180 - 120 * amount, 1),
        release_ms: roundTo(320 - 220 * amount, 1),
        range_db: amount === 0 ? 0 : roundTo(-90 * amount, 0.1),
      };
    case "limiter": {
      const ceiling = amount <= 0.5
        ? -3 + 4 * amount
        : amount <= 0.75
          ? -1 + 2 * (amount - 0.5)
          : -0.5;
      return {
        input_gain_db: amount <= 0.5 ? 0 : roundTo(12 * (amount - 0.5), 0.1),
        ceiling_db: roundTo(ceiling, 0.1),
      };
    }
    default:
      return {};
  }
}

export function simpleEffectStrengthLabel(effectId: string, strength: number): string {
  const amount = roundTo(clampStrength(strength), 0.1);
  const percentage = formatControlNumber(amount);
  const params = simpleEffectParams(effectId, amount);
  switch (effectId) {
    case "gate":
      return `${percentage}% (${formatControlNumber(params.threshold_db)} dB)`;
    case "highpass":
      return `${percentage}% (${formatControlNumber(params.frequency_hz)} Hz)`;
    case "limiter":
      return `${percentage}% (${formatControlNumber(params.ceiling_db)} dB ceiling)`;
    default:
      return `${percentage}%`;
  }
}

function formatControlNumber(value: number): string {
  return Number.isInteger(value) ? value.toFixed(0) : value.toFixed(1);
}
