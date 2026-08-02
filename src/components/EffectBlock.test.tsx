import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { demoState } from "../demo";
import type { EffectInstance } from "../types";
import { EffectBlock } from "./EffectBlock";

function renderEffect(effect: EffectInstance) {
  const definition = demoState.catalog.effects.find((item) => item.id === effect.effect_id);
  const onApplyPreset = vi.fn();
  const onUpdateParam = vi.fn();
  render(
    <EffectBlock
      definition={definition}
      effect={effect}
      index={0}
      onApplyPreset={onApplyPreset}
      onDelete={vi.fn()}
      onMove={vi.fn()}
      onRename={vi.fn()}
      onUpdateParam={onUpdateParam}
      total={1}
    />,
  );
  return { onApplyPreset, onUpdateParam };
}

describe("EffectBlock", () => {
  it("shows exactly one Strength slider for a simple effect", () => {
    renderEffect({
      instance_id: "rnnoise-test",
      effect_id: "rnnoise",
      bypassed: false,
      params: {},
    });

    expect(screen.getAllByRole("slider")).toHaveLength(1);
    expect(screen.getByRole("slider", { name: "Strength" })).toHaveAttribute("step", "0.1");
    expect(screen.getByRole("button", { name: "Advanced" })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(screen.queryByRole("button", { name: "Broadcast" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Copy effect" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Bypass effect" })).not.toBeInTheDocument();
  });

  it("reveals every native parameter through the Advanced control", () => {
    const { onUpdateParam } = renderEffect({
      instance_id: "rnnoise-advanced",
      effect_id: "rnnoise",
      bypassed: false,
      params: {},
    });

    fireEvent.click(screen.getByRole("button", { name: "Advanced" }));

    expect(screen.getByRole("button", { name: "Advanced" })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    expect(screen.getAllByRole("slider")).toHaveLength(5);
    const minimumVoiceLevel = screen.getByRole("slider", { name: "Minimum Voice Level" });
    fireEvent.change(minimumVoiceLevel, { target: { value: "-38" } });
    fireEvent.pointerUp(minimumVoiceLevel, { target: { value: "-38" } });
    expect(onUpdateParam).toHaveBeenCalledWith(
      "rnnoise-advanced",
      "minimum_voice_level_db",
      -38,
    );
  });

  it.each([
    ["highpass", "Voice 80 Hz", { frequency_hz: 80 }],
    [
      "compressor",
      "Broadcast 4:1",
      {
        threshold_db: -18,
        ratio: 4,
        attack_ms: 5,
        release_ms: 100,
        makeup_gain_db: 3,
      },
    ],
    [
      "gate",
      "Nearby voices -20 dB",
      {
        threshold_db: -20,
        range_db: -90,
        attack_ms: 1,
        hold_ms: 60,
        release_ms: 100,
      },
    ],
    ["limiter", "Broadcast -1 dB", { input_gain_db: 0, ceiling_db: -1 }],
  ] as const)("keeps %s presets alongside its single Strength slider", (effectId, preset, values) => {
    const { onApplyPreset } = renderEffect({
      instance_id: `${effectId}-test`,
      effect_id: effectId,
      bypassed: false,
      params: {},
    });

    expect(screen.getAllByRole("slider")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: preset }));
    expect(onApplyPreset).toHaveBeenCalledWith(`${effectId}-test`, values);
  });

  it("keeps the Karaoke style selector instead of reducing it to Strength", () => {
    renderEffect({
      instance_id: "karaoke-test",
      effect_id: "karaoke_stage",
      bypassed: false,
      params: {},
    });

    expect(screen.getByRole("button", { name: "Voice style" })).toBeInTheDocument();
    expect(screen.queryByRole("slider", { name: "Strength" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Advanced" })).not.toBeInTheDocument();
  });

  it.each([
    ["highpass", { frequency_hz: 20 }, "0% (20 Hz)"],
    ["limiter", { input_gain_db: 0, ceiling_db: -3 }, "0% (-3 dB ceiling)"],
    ["limiter", { input_gain_db: 0, ceiling_db: -1 }, "50% (-1 dB ceiling)"],
    ["limiter", { input_gain_db: 3, ceiling_db: -0.5 }, "75% (-0.5 dB ceiling)"],
  ] as const)("shows the effective %s value for %j", (effectId, params, label) => {
    renderEffect({
      instance_id: `${effectId}-${label}`,
      effect_id: effectId,
      bypassed: false,
      params,
    });

    expect(screen.getByText(label)).toBeInTheDocument();
  });
});
