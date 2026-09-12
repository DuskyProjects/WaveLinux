import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { GraphicEqualizer } from "./GraphicEqualizer";
import { demoState } from "../demo";
import type { EffectInstance } from "../types";

const definition = demoState.catalog.effects.find((item) => item.id === "eq")!;
const effect: EffectInstance = {
  instance_id: "eq-1",
  effect_id: "eq",
  bypassed: false,
  params: {},
};

describe("GraphicEqualizer", () => {
  it("offers named tone controls and a flat response for a reset EQ", () => {
    render(
      <GraphicEqualizer
        definition={definition}
        effect={effect}
        onUpdateParam={vi.fn()}
      />,
    );
    expect(screen.getAllByRole("slider")).toHaveLength(8);
    expect(
      screen.getByRole("slider", { name: "Bass · 63 Hz gain" }),
    ).toHaveAttribute("aria-valuenow", "0");
    expect(
      screen.getByRole("slider", { name: "Air · 8 kHz gain" }),
    ).toHaveAttribute("aria-valuetext", "0.0 dB");
    expect(
      screen.getByRole("img", { name: "Equalizer frequency response" }),
    ).toBeVisible();
  });

  it("supports keyboard fine adjustment, limits and per-band reset", () => {
    const update = vi.fn();
    render(
      <GraphicEqualizer
        definition={definition}
        effect={effect}
        onUpdateParam={update}
      />,
    );
    const bass = screen.getByRole("slider", { name: "Bass · 63 Hz gain" });
    fireEvent.keyDown(bass, { key: "ArrowUp" });
    expect(update).toHaveBeenLastCalledWith("eq-1", "band_63_gain_db", 0.5);
    fireEvent.keyDown(bass, { key: "ArrowUp", shiftKey: true });
    expect(bass).toHaveAttribute("aria-valuenow", "2.5");
    for (let i = 0; i < 6; i++) fireEvent.keyDown(bass, { key: "PageUp" });
    expect(bass).toHaveAttribute("aria-valuenow", "12");
    fireEvent.keyDown(bass, { key: "Home" });
    expect(bass).toHaveAttribute("aria-valuenow", "0");
  });

  it("resets all bands in a single operation and follows external presets", () => {
    const reset = vi.fn();
    const { rerender } = render(
      <GraphicEqualizer
        definition={definition}
        effect={effect}
        onUpdateParam={vi.fn()}
        onReset={reset}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "Reset EQ" }));
    expect(reset).toHaveBeenCalledOnce();
    rerender(
      <GraphicEqualizer
        definition={definition}
        effect={{ ...effect, params: { band_63_gain_db: -6 } }}
        onUpdateParam={vi.fn()}
        onReset={reset}
      />,
    );
    expect(
      screen.getByRole("slider", { name: "Bass · 63 Hz gain" }),
    ).toHaveAttribute("aria-valuenow", "-6");
  });
});

it("changes frequency sideways, exposes width, and keeps a cut active at zero gain", () => {
  const update = vi.fn();
  render(
    <GraphicEqualizer
      definition={definition}
      effect={effect}
      onUpdateParam={update}
    />,
  );
  const bass = screen.getByRole("slider", { name: "Bass · 63 Hz gain" });
  fireEvent.keyDown(bass, { key: "ArrowRight" });
  expect(update).toHaveBeenLastCalledWith("eq-1", "band_63_frequency_hz", 67);
  const q = screen.getByRole("spinbutton", { name: "Width (Q)" });
  fireEvent.change(q, { target: { value: "0.5" } });
  fireEvent.blur(q);
  expect(update).toHaveBeenLastCalledWith("eq-1", "band_63_q", 0.5);
  fireEvent.change(screen.getByRole("combobox", { name: "Filter shape" }), {
    target: { value: "3" },
  });
  expect(update).toHaveBeenLastCalledWith("eq-1", "band_63_type", 3);
  expect(bass).toHaveAttribute("aria-valuenow", "67");
  expect(bass).toHaveAttribute("aria-valuetext", "67 Hz");
  expect(
    screen.queryByRole("button", { name: "Boost Bass" }),
  ).not.toBeInTheDocument();
});
