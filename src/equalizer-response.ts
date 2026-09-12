import type { EffectDefinition, EffectInstance } from "./types";

export const EQ_MIN_FREQUENCY = 20;
export const EQ_MAX_FREQUENCY = 20_000;
export const EQ_SAMPLE_RATE = 48_000;
export const FREQUENCIES = [63, 125, 250, 500, 1000, 2000, 4000, 8000];
const NAMES = [
  "Bass",
  "Warmth",
  "Body",
  "Boxiness",
  "Midrange",
  "Clarity",
  "Presence",
  "Air",
];
const COLORS = [
  "#d34cba",
  "#c68a16",
  "#d99836",
  "#e68246",
  "#bb7bd1",
  "#648ddd",
  "#48a891",
  "#86ae3b",
];

export function equalizerBands(
  definition: EffectDefinition,
  effect: EffectInstance,
) {
  return definition.params
    .filter(
      (param) => param.id.startsWith("band_") && param.id.endsWith("_gain_db"),
    )
    .map((param, index) => {
      const prefix = param.id.replace(/gain_db$/, "");
      const frequency = clampGain(
        effect.params[`${prefix}frequency_hz`] ?? FREQUENCIES[index],
        20,
        20000,
      );
      return {
        ...param,
        frequency,
        frequencyId: `${prefix}frequency_hz`,
        qId: `${prefix}q`,
        typeId: `${prefix}type`,
        shape: Math.round(clampGain(effect.params[`${prefix}type`] ?? 0, 0, 4)),
        frequencyLabel: frequencyLabel(frequency),
        name: NAMES[index] ?? param.label,
        color: COLORS[index] ?? COLORS[0],
        gain: clampGain(
          effect.params[param.id] ?? param.default,
          param.min,
          param.max,
        ),
        q: clampGain(
          effect.params[`${prefix}q`] ?? (index === 0 || index === 7 ? 0.9 : 1),
          0.2,
          10,
        ),
      };
    });
}

export function clampGain(value: number, min = -12, max = 12) {
  return Math.max(min, Math.min(max, Number.isFinite(value) ? value : 0));
}

export function frequencyPosition(frequency: number) {
  return (
    Math.log(frequency / EQ_MIN_FREQUENCY) /
    Math.log(EQ_MAX_FREQUENCY / EQ_MIN_FREQUENCY)
  );
}

// The same RBJ peaking filters, frequencies and Q values as the native EQ.
// Plot the actual combined filter response, rather than interpolating the dots.
export function peakingResponseDb(
  frequency: number,
  center: number,
  gain: number,
  q: number,
  sampleRate = EQ_SAMPLE_RATE,
  shape = 0,
) {
  if (shape < 3 && Math.abs(gain) < 0.01) return 0;
  const a = 10 ** (gain / 40);
  const omega =
    (2 * Math.PI * Math.min(center, sampleRate * 0.45)) / sampleRate;
  const alpha = Math.sin(omega) / (2 * q);
  let b = [1 + alpha * a, -2 * Math.cos(omega), 1 - alpha * a];
  let d = [1 + alpha / a, -2 * Math.cos(omega), 1 - alpha / a];
  const c = Math.cos(omega),
    t = 2 * Math.sqrt(a) * alpha,
    p = a + 1,
    m = a - 1;
  if (shape === 1) {
    b = [a * (p - m * c + t), 2 * a * (m - p * c), a * (p - m * c - t)];
    d = [p + m * c + t, -2 * (m + p * c), p + m * c - t];
  } else if (shape === 2) {
    b = [a * (p + m * c + t), -2 * a * (m + p * c), a * (p + m * c - t)];
    d = [p - m * c + t, 2 * (m - p * c), p - m * c - t];
  } else if (shape === 3 || shape === 4) {
    b =
      shape === 3
        ? [(1 + c) / 2, -(1 + c), (1 + c) / 2]
        : [(1 - c) / 2, 1 - c, (1 - c) / 2];
    d = [1 + alpha, -2 * c, 1 - alpha];
  }
  const w = (2 * Math.PI * frequency) / sampleRate;
  const power = (c: number[]) => {
    const re = c[0] + c[1] * Math.cos(w) + c[2] * Math.cos(2 * w);
    const im = -c[1] * Math.sin(w) - c[2] * Math.sin(2 * w);
    return re * re + im * im;
  };
  return 10 * Math.log10(Math.max(1e-20, power(b)) / Math.max(1e-20, power(d)));
}

export function responsePoints(
  bands: { frequency: number; gain: number; q: number; shape?: number }[],
) {
  return Array.from({ length: 241 }, (_, index) => {
    const position = index / 240;
    const frequency =
      EQ_MIN_FREQUENCY * (EQ_MAX_FREQUENCY / EQ_MIN_FREQUENCY) ** position;
    return {
      x: position * 1000,
      y:
        ((12 -
          clampGain(
            bands.reduce(
              (sum, band) =>
                sum +
                peakingResponseDb(
                  frequency,
                  band.frequency,
                  band.gain,
                  band.q,
                  EQ_SAMPLE_RATE,
                  band.shape,
                ),
              0,
            ),
          )) /
          24) *
        300,
    };
  });
}

export function responsePath(points: { x: number; y: number }[]) {
  return points
    .map(
      (point, index) =>
        `${index === 0 ? "M" : "L"}${point.x.toFixed(2)},${point.y.toFixed(2)}`,
    )
    .join(" ");
}

export function gainLabel(value: number) {
  return `${value > 0 ? "+" : ""}${value.toFixed(1)} dB`;
}

export function frequencyLabel(value: number) {
  return value >= 1000
    ? `${Number((value / 1000).toFixed(2))} kHz`
    : `${Math.round(value)} Hz`;
}
export function positionFrequency(position: number) {
  return Math.round(20 * 1000 ** clampGain(position, 0, 1));
}
export const EQ_SHAPES = [
  "Bell",
  "Bass shelf",
  "Treble shelf",
  "Low cut",
  "High cut",
];
export function eqDefaults(definition: EffectDefinition) {
  return Object.fromEntries(
    definition.params.map((param) => [param.id, param.default]),
  );
}
