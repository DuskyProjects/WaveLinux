import { describe, expect, it } from "vitest";
import {
  appVolumePercent,
  appVolumeToPercent,
  sliderPercent,
  thumbPosition,
  volumeToPercent,
} from "./volume";

describe("mixer value helpers", () => {
  it("clamps and rounds normal mixer values", () => {
    expect(sliderPercent(-1)).toBe(0);
    expect(sliderPercent(52.6)).toBe(53);
    expect(sliderPercent(150)).toBe(100);
    expect(volumeToPercent(0.755)).toBe(76);
  });

  it("keeps active application streams above zero", () => {
    expect(appVolumePercent(0)).toBe(1);
    expect(appVolumeToPercent(0)).toBe(1);
    expect(appVolumePercent(45.6)).toBe(46);
  });

  it("keeps vertical fader thumbs inside their fixed endpoint padding", () => {
    expect(thumbPosition(-20)).toBe("calc(13px + (100% - 26px) * 0)");
    expect(thumbPosition(50)).toBe("calc(13px + (100% - 26px) * 0.5)");
    expect(thumbPosition(120)).toBe("calc(13px + (100% - 26px) * 1)");
  });
});
