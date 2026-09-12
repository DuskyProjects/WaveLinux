import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Compressor } from "./Compressor";
import { demoState } from "../demo";
import type { EffectInstance, LevelMeter } from "../types";

const meterState = vi.hoisted(() => ({ meters: [] as LevelMeter[] }));
vi.mock("../state", () => ({ useWaveLinuxMeters: () => meterState.meters }));
const definition = demoState.catalog.effects.find(
  (item) => item.id === "compressor",
)!;
const effect: EffectInstance = {
  instance_id: "comp-1",
  effect_id: "compressor",
  bypassed: false,
  params: {},
};
afterEach(() => {
  meterState.meters = [];
  vi.useRealTimers();
});

describe("Compressor", () => {
  it("does not invent a waveform when native telemetry is unavailable", () => {
    const { container } = render(
      <Compressor
        definition={definition}
        effect={effect}
        onUpdateParam={vi.fn()}
        demo={false}
      />,
    );
    expect(screen.getByText("Waiting for audio")).toBeVisible();
    expect(screen.getByText("Play audio to see it here")).toBeVisible();
    expect(container.querySelector(".compressor-wave-before")).toHaveAttribute(
      "d",
      "",
    );
  });
  it("commits once per threshold gesture, preserves active edits and cancels interrupted drags", () => {
    const update = vi.fn();
    const { rerender } = render(
      <Compressor
        definition={definition}
        effect={effect}
        onUpdateParam={update}
        demo={false}
      />,
    );
    const threshold = screen.getByRole("slider", { name: "Threshold" });
    fireEvent.change(threshold, { target: { value: "-32" } });
    expect(update).not.toHaveBeenCalled();
    rerender(
      <Compressor
        definition={definition}
        effect={{ ...effect, params: { threshold_db: -18 } }}
        onUpdateParam={update}
        demo={false}
      />,
    );
    expect(threshold).toHaveValue("-32");
    fireEvent.pointerUp(threshold);
    fireEvent.blur(threshold);
    expect(update).toHaveBeenCalledExactlyOnceWith(
      "comp-1",
      "threshold_db",
      -32,
    );
    fireEvent.change(threshold, { target: { value: "-42" } });
    fireEvent.pointerCancel(threshold);
    expect(threshold).toHaveValue("-18");
    fireEvent.blur(threshold);
    expect(update).toHaveBeenCalledTimes(1);
  });
  it("uses processor measurements, freezes history on pause and expires stale readings", () => {
    vi.useFakeTimers();
    meterState.meters = [
      {
        node_id: "mic",
        peak_left: 0.9,
        peak_right: 0.9,
        compressor: {
          input_peak: 0.1,
          output_peak: 0.01,
          gain_reduction_db: 20,
        },
      },
    ];
    const props = {
      channelId: "mic",
      definition,
      effect,
      onUpdateParam: vi.fn(),
      demo: false,
    };
    const { container, rerender } = render(<Compressor {...props} />);
    expect(screen.getByRole("meter", { name: "Input" })).toHaveAttribute(
      "aria-valuetext",
      "-20 dB",
    );
    expect(screen.getByRole("meter", { name: "Output" })).toHaveAttribute(
      "aria-valuetext",
      "-40 dB",
    );
    expect(screen.getByRole("meter", { name: "Reduction" })).toHaveAttribute(
      "aria-valuetext",
      "−20.0 dB",
    );
    const path = container.querySelector(".compressor-wave-before")!;
    const before = path.getAttribute("d");
    fireEvent.click(screen.getByRole("button", { name: "Pause display" }));
    meterState.meters = [
      {
        ...meterState.meters[0],
        compressor: { input_peak: 1, output_peak: 0.1, gain_reduction_db: 20 },
      },
    ];
    rerender(<Compressor {...props} />);
    expect(path.getAttribute("d")).toBe(before);
    expect(screen.getByRole("meter", { name: "Input" })).toHaveAttribute(
      "aria-valuetext",
      "0 dB",
    );
    fireEvent.click(screen.getByRole("button", { name: "Resume display" }));
    expect(path.getAttribute("d")).not.toBe(before);
    act(() => vi.advanceTimersByTime(1001));
    expect(screen.getByText("Waiting for audio")).toBeVisible();
    expect(path).toHaveAttribute("d", "");
    rerender(<Compressor {...props} enabled={false} />);
    expect(screen.getByText("Compressor is off")).toBeVisible();
  });
});

it("does not light reduction segments when the displayed reduction is zero", () => {
  meterState.meters = [
    {
      node_id: "mic",
      peak_left: 0,
      peak_right: 0,
      compressor: { input_peak: 0, output_peak: 0, gain_reduction_db: 0.0001 },
    },
  ];
  const { container } = render(
    <Compressor
      channelId="mic"
      definition={definition}
      effect={effect}
      onUpdateParam={vi.fn()}
      demo={false}
    />,
  );
  expect(screen.getByRole("meter", { name: "Reduction" })).toHaveAttribute(
    "aria-valuetext",
    "0.0 dB",
  );
  expect(container.querySelectorAll(".reduction .lit")).toHaveLength(0);
});

it("keeps expired readings expired when pausing and resuming the display", () => {
  vi.useFakeTimers();
  meterState.meters = [
    {
      node_id: "mic",
      peak_left: 0.1,
      peak_right: 0.1,
      compressor: { input_peak: 0.1, output_peak: 0.01, gain_reduction_db: 20 },
    },
  ];
  const { container } = render(
    <Compressor
      channelId="mic"
      definition={definition}
      effect={effect}
      onUpdateParam={vi.fn()}
      demo={false}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Pause display" }));
  act(() => vi.advanceTimersByTime(1001));
  expect(screen.getByText("Waiting for audio")).toBeVisible();
  fireEvent.click(screen.getByRole("button", { name: "Resume display" }));
  expect(screen.getByText("Waiting for audio")).toBeVisible();
  expect(container.querySelector(".compressor-wave-before")).toHaveAttribute(
    "d",
    "",
  );
});
