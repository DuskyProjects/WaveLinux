import type { LucideIcon } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent } from "react";

export function VolumeFader({
  label,
  value,
  min = 0,
  max = 1,
  unit = "%",
  compact = false,
  disabled = false,
  step,
  formatValue,
  onChange,
}: {
  label: string;
  value: number;
  min?: number;
  max?: number;
  unit?: string;
  compact?: boolean;
  disabled?: boolean;
  step?: number;
  formatValue?: (value: number) => string;
  onChange: (value: number) => void | Promise<unknown>;
}) {
  const normalizedPercent = unit === "%" && min === 0 && max === 1;
  const sliderMin = normalizedPercent ? 0 : min;
  const sliderMax = normalizedPercent ? 100 : max;
  const incomingSliderValue = normalizedPercent ? value * 100 : value;
  const [draft, setDraft] = useState(incomingSliderValue);
  const lastCommitted = useRef(incomingSliderValue);
  const hasUncommittedDraft = useRef(false);
  const display = normalizedPercent ? Math.round(draft) : Math.round(draft * 10) / 10;
  const displayText = formatValue ? formatValue(draft) : `${display}${unit}`;
  const progress = sliderMax === sliderMin
    ? 0
    : Math.max(0, Math.min(100, ((draft - sliderMin) / (sliderMax - sliderMin)) * 100));

  useEffect(() => {
    if (hasUncommittedDraft.current) return;
    const next = normalizedPercent ? value * 100 : value;
    setDraft(next);
    lastCommitted.current = next;
  }, [normalizedPercent, value]);

  const commit = useCallback(
    (raw: number) => {
      if (disabled) return;
      const next = Number.isFinite(raw)
        ? Math.max(sliderMin, Math.min(sliderMax, raw))
        : incomingSliderValue;
      hasUncommittedDraft.current = false;
      setDraft(next);
      if (lastCommitted.current === next) return;
      lastCommitted.current = next;
      void onChange(normalizedPercent ? next / 100 : next);
    },
    [disabled, incomingSliderValue, normalizedPercent, onChange, sliderMax, sliderMin],
  );

  return (
    <label
      aria-disabled={disabled}
      className={`${compact ? "fader-row compact" : "fader-row"}${disabled ? " disabled" : ""}`}
    >
      <span>{label}</span>
      <input
        aria-label={label}
        disabled={disabled}
        max={sliderMax}
        min={sliderMin}
        onBlur={(event) => commit(Number(event.currentTarget.value))}
        onChange={(event) => {
          hasUncommittedDraft.current = true;
          setDraft(Number(event.currentTarget.value));
        }}
        onKeyUp={(event) => {
          if (shouldCommitSliderKey(event)) commit(Number(event.currentTarget.value));
        }}
        onPointerUp={(event) => commit(Number(event.currentTarget.value))}
        step={step ?? (unit === "%" ? 1 : 0.1)}
        style={{ "--fader-progress": `${progress}%` } as CSSProperties}
        type="range"
        value={draft}
      />
      <strong>{displayText}</strong>
    </label>
  );
}

export function Toggle({
  label,
  value,
  disabled = false,
  onChange,
}: {
  label: string;
  value: boolean;
  disabled?: boolean;
  onChange: (value: boolean) => void | Promise<unknown>;
}) {
  return (
    <button
      className="toggle-row"
      disabled={disabled}
      onClick={() => onChange(!value)}
      type="button"
    >
      <span>{label}</span>
      <span className={value ? "toggle on" : "toggle"} />
    </button>
  );
}

export function Stat({
  icon: Icon,
  label,
  value,
}: {
  icon: LucideIcon;
  label: string;
  value: string;
}) {
  return (
    <div className="stat">
      <Icon size={17} />
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

export function EmptyState({ label }: { label: string }) {
  return <div className="empty-state">{label}</div>;
}

export function shouldCommitSliderKey(
  event: ReactKeyboardEvent<HTMLInputElement>,
): boolean {
  return event.key === "Enter";
}
