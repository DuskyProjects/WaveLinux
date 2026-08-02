import { describe, expect, it } from "vitest";
import { demoState } from "./demo";
import { effectRuntimeTitle, resolveEffectRuntimeState } from "./effect-runtime";
import type { Channel, EffectRuntimeStatus } from "./types";

function selectedChannel(): Channel {
  const channel = structuredClone(demoState.config.channels[0]);
  channel.id = "hardware_in";
  channel.effects = [
    {
      instance_id: "rnnoise-1",
      effect_id: "rnnoise",
      name: null,
      bypassed: false,
      params: { vad_threshold: 72 },
    },
  ];
  channel.effects_enabled = true;
  return channel;
}

function runtime(overrides: Partial<EffectRuntimeStatus> = {}): EffectRuntimeStatus {
  return {
    channel_id: "hardware_in",
    state: "green",
    selected_effect_count: 1,
    desired_enabled: true,
    desired_generation: 9,
    applied_generation: 9,
    in_flight_generation: null,
    coalesced_requests: 0,
    pending: false,
    core_healthy: true,
    control_socket: "/run/user/1000/wavelinux6/control/wavelinux6-chain-hardware_in.sock",
    last_error: null,
    ...overrides,
  };
}

describe("effect runtime state", () => {
  it("is grey only when no effects are selected", () => {
    const channel = selectedChannel();
    channel.effects = [];
    expect(resolveEffectRuntimeState(channel, runtime())).toBe("grey");
  });

  it.each([
    ["globally bypassed", { desired_enabled: false }],
    ["pending", { pending: true }],
    ["core unavailable", { core_healthy: false }],
    ["application failed", { state: "red" as const, last_error: "connection refused" }],
    ["stale acknowledgement", { applied_generation: 8 }],
  ])("is red when selected effects are %s", (_label, overrides) => {
    expect(resolveEffectRuntimeState(selectedChannel(), runtime(overrides))).toBe("red");
  });

  it("is green only for the current healthy acknowledgement", () => {
    expect(resolveEffectRuntimeState(selectedChannel(), runtime())).toBe("green");
  });

  it("exposes the backend failure in the status title", () => {
    expect(effectRuntimeTitle(selectedChannel(), runtime({ state: "red", last_error: "socket unavailable" })))
      .toContain("socket unavailable");
  });
});
