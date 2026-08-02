import type {
  Channel,
  EffectRuntimeState,
  EffectRuntimeStatus,
} from "./types";

export function effectRuntimeForChannel(
  statuses: EffectRuntimeStatus[],
  channelId: string,
): EffectRuntimeStatus | undefined {
  return statuses.find((status) => status.channel_id === channelId);
}

export function resolveEffectRuntimeState(
  channel: Channel,
  runtime: EffectRuntimeStatus | undefined,
): EffectRuntimeState {
  if (channel.effects.length === 0) return "grey";
  if (
    channel.effects_enabled
    && channel.effects.some((effect) => !effect.bypassed)
    && runtime?.channel_id === channel.id
    && runtime.selected_effect_count === channel.effects.length
    && runtime.state === "green"
    && runtime.desired_enabled
    && runtime.core_healthy
    && !runtime.pending
    && runtime.applied_generation === runtime.desired_generation
  ) {
    return "green";
  }
  return "red";
}

export function effectRuntimeTitle(
  channel: Channel,
  runtime: EffectRuntimeStatus | undefined,
): string {
  const state = resolveEffectRuntimeState(channel, runtime);
  if (state === "grey") return "No effects selected";
  if (state === "green") {
    return `${channel.effects.length} effect${channel.effects.length === 1 ? "" : "s"} active and verified`;
  }
  if (runtime?.last_error) return `Effects inactive: ${runtime.last_error}`;
  if (runtime?.pending) return "Effects selected; waiting for audio-core acknowledgement";
  if (!runtime?.core_healthy) return "Effects selected; audio core is unavailable";
  if (!runtime?.desired_enabled) return "Effects selected and bypassed";
  return "Effects selected but not verified";
}
