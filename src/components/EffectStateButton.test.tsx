import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { demoState } from "../demo";
import type { Channel, EffectRuntimeStatus } from "../types";
import { EffectStateButton } from "./EffectStateButton";

function channelWithEffect(): Channel {
  const channel = structuredClone(demoState.config.channels[0]);
  channel.id = "hardware_in";
  channel.effects_enabled = true;
  channel.effects = [
    {
      instance_id: "limiter-1",
      effect_id: "limiter",
      name: null,
      bypassed: false,
      params: {},
    },
  ];
  return channel;
}

function runtime(overrides: Partial<EffectRuntimeStatus> = {}): EffectRuntimeStatus {
  return {
    channel_id: "hardware_in",
    state: "green",
    selected_effect_count: 1,
    desired_enabled: true,
    desired_generation: 12,
    applied_generation: 12,
    in_flight_generation: null,
    coalesced_requests: 0,
    pending: false,
    core_healthy: true,
    control_socket: "/run/user/1000/wavelinux6/control/wavelinux6-chain-hardware_in.sock",
    last_error: null,
    ...overrides,
  };
}

describe("EffectStateButton", () => {
  it("renders grey and cannot toggle an empty chain", () => {
    const channel = channelWithEffect();
    channel.effects = [];
    const onToggle = vi.fn();
    render(<EffectStateButton channel={channel} onToggle={onToggle} runtime={runtime()} />);

    const button = screen.getByRole("button", { name: "No effects selected" });
    expect(button).toBeDisabled();
    expect(button).toHaveAttribute("data-state", "grey");
    fireEvent.click(button);
    expect(onToggle).not.toHaveBeenCalled();
  });

  it("requests bypass when the latest generation is green", () => {
    const onToggle = vi.fn();
    render(
      <EffectStateButton channel={channelWithEffect()} onToggle={onToggle} runtime={runtime()} />,
    );

    const button = screen.getByRole("button", { name: /active and verified/ });
    expect(button).toHaveAttribute("data-state", "green");
    fireEvent.click(button);
    expect(onToggle).toHaveBeenCalledWith(false);
  });

  it("requests reapply while red and exposes the backend error", () => {
    const onToggle = vi.fn();
    render(
      <EffectStateButton
        channel={channelWithEffect()}
        onToggle={onToggle}
        runtime={runtime({ state: "red", core_healthy: false, last_error: "socket unavailable" })}
      />,
    );

    const button = screen.getByRole("button", { name: /socket unavailable/ });
    expect(button).toHaveAttribute("data-state", "red");
    fireEvent.click(button);
    expect(onToggle).toHaveBeenCalledWith(true);
  });
});
