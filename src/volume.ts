export function sliderPercent(value: number): number {
  return Math.max(0, Math.min(100, Math.round(value)));
}

export function volumeToPercent(volume: number): number {
  return sliderPercent(volume * 100);
}

export function appVolumePercent(value: number): number {
  return Math.max(1, sliderPercent(value));
}

export function appVolumeToPercent(volume: number): number {
  return appVolumePercent(volume * 100);
}

export function thumbPosition(percent: number): string {
  const clamped = Math.max(0, Math.min(100, percent));
  return `calc(13px + (100% - 26px) * ${clamped / 100})`;
}
