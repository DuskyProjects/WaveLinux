import { describe, expect, it } from "vitest";

import { defaultMixBus, mixOutputDevices } from "./mixer-ui";
import type { Mix } from "./types";

function mix(overrides: Partial<Mix> = {}): Mix {
  return {
    id: "monitor",
    name: "Monitor",
    virtual_sink_name: "wavelinux6_mix_monitor",
    virtual_source_name: "wavelinux6_mix_monitor_source",
    volume: 1,
    muted: false,
    ...overrides,
  };
}

describe("defaultMixBus", () => {
  it("creates a unity, unmuted, enabled bus by default", () => {
    expect(defaultMixBus()).toEqual({ volume: 1, muted: false, enabled: true });
  });

  it("can create a disabled bus without changing its audio defaults", () => {
    expect(defaultMixBus(false)).toEqual({ volume: 1, muted: false, enabled: false });
  });
});

describe("mixOutputDevices", () => {
  it("prefers the explicit multi-output route", () => {
    expect(
      mixOutputDevices(
        mix({
          monitor_output: "fallback-output",
          output_devices: ["usb-output", "bluetooth-output"],
        }),
      ),
    ).toEqual(["usb-output", "bluetooth-output"]);
  });

  it("migrates a legacy monitor output into the output list", () => {
    expect(mixOutputDevices(mix({ monitor_output: "legacy-output" }))).toEqual([
      "legacy-output",
    ]);
  });

  it("returns no outputs when neither representation is configured", () => {
    expect(mixOutputDevices(mix())).toEqual([]);
  });
});
