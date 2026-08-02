import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { GraphicEqualizer } from "./GraphicEqualizer";
import type { EffectDefinition, EffectInstance } from "../types";

const frequencies = ["63", "125", "250", "500", "1k", "2k", "4k", "8k"];
const definition: EffectDefinition = {
  id: "eq",
  name: "8-Band EQ",
  description: "Graphic equalizer",
  plugin_hint: null,
  presets: [],
  params: frequencies.map((frequency, index) => ({
    id: `band_${index + 1}_gain_db`,
    label: frequency,
    min: -12,
    max: 12,
    default: 0,
    unit: "dB",
  })),
};
const effect: EffectInstance = {
  instance_id: "eq-1",
  effect_id: "eq",
  bypassed: false,
  params: {},
};

describe("GraphicEqualizer", () => {
  it("renders one vertical fader control for every EQ band", () => {
    render(
      <GraphicEqualizer definition={definition} effect={effect} onUpdateParam={vi.fn()} />,
    );

    expect(screen.getByRole("group", { name: "8-band equalizer" })).toBeInTheDocument();
    expect(screen.getAllByRole("slider")).toHaveLength(8);
    for (const frequency of frequencies) {
      expect(screen.getByRole("slider", { name: `${frequency} Hz gain` })).toBeInTheDocument();
    }
  });

  it("commits the latest value only when the user completes an edit", () => {
    const onUpdateParam = vi.fn();
    render(
      <GraphicEqualizer
        definition={definition}
        effect={effect}
        onUpdateParam={onUpdateParam}
      />,
    );
    const slider = screen.getByRole("slider", { name: "63 Hz gain" });

    fireEvent.change(slider, { target: { value: "5" } });
    expect(onUpdateParam).not.toHaveBeenCalled();
    fireEvent.keyUp(slider, { key: "Enter" });

    expect(onUpdateParam).toHaveBeenCalledOnce();
    expect(onUpdateParam).toHaveBeenCalledWith("eq-1", "band_1_gain_db", 5);
  });
});
