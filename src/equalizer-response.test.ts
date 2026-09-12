import { describe, expect, it } from "vitest";
import {
  frequencyPosition,
  peakingResponseDb,
  responsePoints,
} from "./equalizer-response";

describe("native EQ response", () => {
  it("places octaves on a logarithmic frequency axis", () => {
    expect(frequencyPosition(20)).toBe(0);
    expect(frequencyPosition(20000)).toBe(1);
    expect(frequencyPosition(2000) - frequencyPosition(1000)).toBeCloseTo(
      frequencyPosition(1000) - frequencyPosition(500),
    );
  });
  it.each([63, 125, 250, 500, 1000, 2000, 4000, 8000])(
    "matches the requested gain at the %i Hz filter centre",
    (frequency) => {
      for (const gain of [-12, -6, 0, 6, 12])
        expect(peakingResponseDb(frequency, frequency, gain, 1)).toBeCloseTo(
          gain,
          6,
        );
      expect(peakingResponseDb(0, frequency, 6, 1)).toBeCloseTo(0, 6);
    },
  );
  it("shows a flat response for flat or cancelling bands and keeps large boosts in the chart", () => {
    const band = { frequency: 1000, gain: 6, q: 1 };
    for (const point of responsePoints([band, { ...band, gain: -6 }]))
      expect(point.y).toBeCloseTo(150, 6);
    for (const point of responsePoints(Array(8).fill({ ...band, gain: 12 }))) {
      expect(point.y).toBeGreaterThanOrEqual(0);
      expect(point.y).toBeLessThanOrEqual(300);
    }
  });
});

it("plots shelves and cuts with the same direction and centre as the native filters", () => {
  expect(peakingResponseDb(100, 1000, 6, 0.707, 48000, 1)).toBeCloseTo(6, 1);
  expect(peakingResponseDb(10000, 1000, 6, 0.707, 48000, 2)).toBeCloseTo(6, 1);
  expect(peakingResponseDb(100, 1000, 0, 0.707, 48000, 3)).toBeLessThan(-35);
  expect(peakingResponseDb(10000, 1000, 0, 0.707, 48000, 4)).toBeLessThan(-35);
  expect(peakingResponseDb(1000, 1000, 0, Math.SQRT1_2, 48000, 3)).toBeCloseTo(
    -3.0103,
    3,
  );
  expect(peakingResponseDb(500, 1000, 6, 0.5)).toBeGreaterThan(
    peakingResponseDb(500, 1000, 6, 3) + 2,
  );
});
