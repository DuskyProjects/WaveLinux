import { describe, expect, it } from "vitest";
import { normalizeSourceEffects } from "./effect-chain";
import type { EffectInstance } from "./types";

const effect = (id: string): EffectInstance => ({ instance_id: id, effect_id: id, bypassed: false, params: {} });

describe("noise filter selection", () => {
  it.each([["rnnoise", "deepfilternet3"], ["deepfilternet3", "rnnoise"]])("replaces %s with %s while preserving other effects", (old, selected) => {
    const eq = { ...effect("eq"), params: { band_500_gain_db: -3 } };
    const result = normalizeSourceEffects([effect(old), effect(selected), eq], selected);
    expect(result.map((item) => item.effect_id)).toEqual([selected, "eq"]);
    expect(result[1].params).toEqual(eq.params);
  });
});
