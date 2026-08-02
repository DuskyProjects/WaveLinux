import type { MouseEvent } from "react";
import { effectRuntimeTitle, resolveEffectRuntimeState } from "../effect-runtime";
import type { Channel, EffectRuntimeStatus } from "../types";

export function EffectStateButton({
  channel,
  onToggle,
  runtime,
  stopPropagation = false,
}: {
  channel: Channel;
  onToggle: (enabled: boolean) => void | Promise<void>;
  runtime?: EffectRuntimeStatus;
  stopPropagation?: boolean;
}) {
  const state = resolveEffectRuntimeState(channel, runtime);
  const title = effectRuntimeTitle(channel, runtime);
  const handleClick = (event: MouseEvent<HTMLButtonElement>) => {
    if (stopPropagation) event.stopPropagation();
    void onToggle(state !== "green");
  };

  return (
    <button
      aria-label={title}
      className="fx-state-button"
      data-state={state}
      disabled={state === "grey"}
      onClick={handleClick}
      title={title}
      type="button"
    >
      <span aria-hidden="true" className={`fx-led ${state}`} />
    </button>
  );
}
