import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Toggle, VolumeFader } from "./Controls";

describe("VolumeFader", () => {
  it("updates its draft immediately and commits once on pointer release", () => {
    const onChange = vi.fn();
    render(<VolumeFader label="Level" onChange={onChange} value={0.5} />);
    const slider = screen.getByRole("slider", { name: "Level" });

    fireEvent.change(slider, { target: { value: "75" } });

    expect(screen.getByText("75%")).toBeInTheDocument();
    expect(onChange).not.toHaveBeenCalled();

    fireEvent.pointerUp(slider, { target: { value: "75" } });

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenCalledWith(0.75);
  });

  it("commits keyboard changes on Enter", () => {
    const onChange = vi.fn();
    render(
      <VolumeFader
        label="Threshold"
        max={0}
        min={-60}
        onChange={onChange}
        unit=" dB"
        value={-30}
      />,
    );
    const slider = screen.getByRole("slider", { name: "Threshold" });

    fireEvent.change(slider, { target: { value: "-24" } });
    fireEvent.keyUp(slider, { key: "Enter" });

    expect(onChange).toHaveBeenCalledWith(-24);
  });

  it("keeps the local drag position when stale state arrives", () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <VolumeFader label="Strength" max={100} min={0} onChange={onChange} step={0.1} value={20} />,
    );
    const slider = screen.getByRole("slider", { name: "Strength" });

    fireEvent.change(slider, { target: { value: "63.4" } });
    rerender(
      <VolumeFader label="Strength" max={100} min={0} onChange={onChange} step={0.1} value={25} />,
    );

    expect(slider).toHaveValue("63.4");
    expect(slider.style.getPropertyValue("--fader-progress")).toBe("63.4%");

    fireEvent.pointerUp(slider, { target: { value: "63.4" } });
    expect(onChange).toHaveBeenCalledOnce();
    expect(onChange).toHaveBeenCalledWith(63.4);
  });
});

describe("Toggle", () => {
  it("emits the inverse value", () => {
    const onChange = vi.fn();
    render(<Toggle label="Enabled" onChange={onChange} value={false} />);

    fireEvent.click(screen.getByRole("button", { name: "Enabled" }));

    expect(onChange).toHaveBeenCalledWith(true);
  });
});
