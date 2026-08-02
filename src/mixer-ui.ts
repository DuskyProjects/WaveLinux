import type { Mix, MixBus } from "./types";

export function defaultMixBus(enabled = true): MixBus {
  return { volume: 1, muted: false, enabled };
}

export function mixOutputDevices(mix: Mix): string[] {
  const outputs = mix.output_devices ?? [];
  if (outputs.length > 0) return outputs;
  return mix.monitor_output ? [mix.monitor_output] : [];
}
