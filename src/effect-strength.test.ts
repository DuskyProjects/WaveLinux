import { describe, expect, it } from "vitest";
import {
  simpleEffectParams,
  simpleEffectStrength,
  simpleEffectStrengthLabel,
} from "./effect-strength";
import type { EffectDefinition, EffectInstance } from "./types";

function definition(id: string, defaults: Record<string, number>): EffectDefinition {
  return {
    id,
    name: id,
    description: id,
    plugin_hint: null,
    presets: [],
    params: Object.entries(defaults).map(([paramId, value]) => ({
      id: paramId,
      label: paramId,
      min: -100,
      max: 500,
      default: value,
      unit: "",
    })),
  };
}

function effect(effectId: string, params: Record<string, number>): EffectInstance {
  return {
    instance_id: `${effectId}-1`,
    effect_id: effectId,
    bypassed: false,
    params,
  };
}

describe("simple effect strength", () => {
  it("maps RNNoise strength to the complete tuned near-field control set", () => {
    expect(simpleEffectParams("rnnoise", 0)).toEqual({
      vad_threshold: 25,
      hold_ms: 250,
      minimum_voice_level_db: -65,
      dry_mix: 0,
    });
    expect(simpleEffectParams("rnnoise", 100)).toEqual({
      vad_threshold: 95,
      hold_ms: 75,
      minimum_voice_level_db: -28,
      dry_mix: 0,
    });
  });

  it("round-trips RNNoise aggressiveness without exposing advanced controls", () => {
    const params = simpleEffectParams("rnnoise", 72);
    const strength = simpleEffectStrength(
      effect("rnnoise", params),
      definition("rnnoise", params),
    );

    expect(strength).toBeGreaterThanOrEqual(70);
    expect(strength).toBeLessThanOrEqual(73);
  });

  it("clamps invalid and out-of-range strength values", () => {
    expect(simpleEffectParams("highpass", Number.NaN)).toEqual({ frequency_hz: 20 });
    expect(simpleEffectParams("highpass", 250)).toEqual({ frequency_hz: 200 });
  });

  it("maps every gate percentage to a direct effective threshold in decibels", () => {
    expect(simpleEffectParams("gate", 0)).toEqual({
      threshold_db: -70,
      attack_ms: 8,
      hold_ms: 180,
      release_ms: 320,
      range_db: 0,
    });
    expect(simpleEffectParams("gate", 20)).toEqual({
      threshold_db: -60,
      attack_ms: 4.9,
      hold_ms: 156,
      release_ms: 276,
      range_db: -18,
    });
    expect(simpleEffectParams("gate", 50)).toEqual({
      threshold_db: -45,
      attack_ms: 3.1,
      hold_ms: 120,
      release_ms: 210,
      range_db: -45,
    });
    expect(simpleEffectParams("gate", 80)).toEqual({
      threshold_db: -30,
      attack_ms: 1.7,
      hold_ms: 84,
      release_ms: 144,
      range_db: -72,
    });
    expect(simpleEffectParams("gate", 100)).toEqual({
      threshold_db: -20,
      attack_ms: 1,
      hold_ms: 60,
      release_ms: 100,
      range_db: -90,
    });
  });

  it("round-trips gate percentages and places named gate presets in order", () => {
    const defaults = simpleEffectParams("gate", 50);
    const gateDefinition = definition("gate", defaults);
    for (const expected of [0, 25, 50, 75, 100]) {
      const actual = simpleEffectStrength(
        effect("gate", simpleEffectParams("gate", expected)),
        gateDefinition,
      );
      expect(Math.abs(actual - expected)).toBeLessThanOrEqual(1);
    }

    const strengthFor = (threshold_db: number, range_db: number) =>
      simpleEffectStrength(
        effect("gate", { threshold_db, range_db }),
        gateDefinition,
      );
    expect(strengthFor(-60, -30)).toBe(20);
    expect(strengthFor(-35, -60)).toBe(70);
    expect(strengthFor(-30, -70)).toBe(80);
    expect(strengthFor(-20, -90)).toBe(100);
  });

  it("shows meaningful primary units alongside simplified percentages", () => {
    expect(simpleEffectStrengthLabel("gate", 0)).toBe("0% (-70 dB)");
    expect(simpleEffectStrengthLabel("gate", 25)).toBe("25% (-57.5 dB)");
    expect(simpleEffectStrengthLabel("gate", 100)).toBe("100% (-20 dB)");
    expect(simpleEffectStrengthLabel("highpass", 0)).toBe("0% (20 Hz)");
    expect(simpleEffectStrengthLabel("highpass", 100)).toBe("100% (200 Hz)");
    expect(simpleEffectStrengthLabel("limiter", 0)).toBe("0% (-3 dB ceiling)");
    expect(simpleEffectStrengthLabel("limiter", 50)).toBe("50% (-1 dB ceiling)");
    expect(simpleEffectStrengthLabel("compressor", 64)).toBe("64%");
  });

  it("keeps every gate percentage monotonic without slider dead zones", () => {
    const gateDefinition = definition("gate", simpleEffectParams("gate", 50));
    let previous = simpleEffectParams("gate", 0);
    for (let expected = 1; expected <= 100; expected += 1) {
      const params = simpleEffectParams("gate", expected);
      expect(params.threshold_db).toBeGreaterThanOrEqual(previous.threshold_db);
      expect(params.range_db).toBeLessThanOrEqual(previous.range_db);
      expect(params.attack_ms).toBeLessThanOrEqual(previous.attack_ms);
      expect(params.hold_ms).toBeLessThanOrEqual(previous.hold_ms);
      expect(params.release_ms).toBeLessThanOrEqual(previous.release_ms);
      expect(
        Math.abs(simpleEffectStrength(effect("gate", params), gateDefinition) - expected),
      ).toBeLessThanOrEqual(1);
      previous = params;
    }
  });

  it("orders compressor presets by their combined dynamics strength", () => {
    const compressorDefinition = definition("compressor", {
      threshold_db: -20,
      ratio: 4,
      attack_ms: 5,
      release_ms: 100,
      makeup_gain_db: 0,
    });
    const strengthFor = (params: Record<string, number>) =>
      simpleEffectStrength(effect("compressor", params), compressorDefinition);
    const gentle = strengthFor({
      threshold_db: -20,
      ratio: 2,
      attack_ms: 10,
      release_ms: 120,
      makeup_gain_db: 2,
    });
    const broadcast = strengthFor({
      threshold_db: -18,
      ratio: 4,
      attack_ms: 5,
      release_ms: 100,
      makeup_gain_db: 3,
    });
    const streaming = strengthFor({
      threshold_db: -16,
      ratio: 6,
      attack_ms: 3,
      release_ms: 80,
      makeup_gain_db: 4,
    });

    expect(gentle).toBeLessThan(broadcast);
    expect(broadcast).toBeLessThan(streaming);
  });

  it("places high-pass presets on a useful logarithmic 20-200 Hz scale", () => {
    const highpassDefinition = definition("highpass", { frequency_hz: 20 });
    const strengthFor = (frequency_hz: number) =>
      simpleEffectStrength(
        effect("highpass", { frequency_hz }),
        highpassDefinition,
      );

    expect(strengthFor(20)).toBe(0);
    expect(strengthFor(40)).toBeCloseTo(30.1, 1);
    expect(strengthFor(80)).toBeCloseTo(60.2, 1);
    expect(strengthFor(120)).toBeCloseTo(77.8, 1);
    expect(strengthFor(200)).toBe(100);
  });

  it("gives every limiter preset a distinct position and matching ceiling", () => {
    const limiterDefinition = definition("limiter", {
      input_gain_db: 0,
      ceiling_db: -3,
    });
    const strengthFor = (input_gain_db: number, ceiling_db: number) =>
      simpleEffectStrength(
        effect("limiter", { input_gain_db, ceiling_db }),
        limiterDefinition,
      );

    expect(strengthFor(0, -3)).toBe(0);
    expect(strengthFor(0, -1)).toBe(50);
    expect(strengthFor(3, -0.5)).toBe(75);
    expect(simpleEffectParams("limiter", 100)).toEqual({
      input_gain_db: 6,
      ceiling_db: -0.5,
    });
  });
});
