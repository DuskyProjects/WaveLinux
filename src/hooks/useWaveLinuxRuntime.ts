import { useEffect, useRef } from "react";
import { connectWaveLinuxEvents } from "../state";
import { invoke } from "../tauri";

type WaveLinuxRuntimeOptions = {
  meterActive: boolean;
  refresh: () => Promise<void>;
  reportError: (message: string) => void;
};

function startupRetryDelay(attempt: number): number {
  return Math.min(1_000, 100 * 2 ** Math.min(attempt, 4));
}

export function useWaveLinuxRuntime({
  meterActive,
  refresh,
  reportError,
}: WaveLinuxRuntimeOptions): void {
  const meterContextActiveRef = useRef(meterActive);
  const syncMeterStreamingRef = useRef<() => void>(() => undefined);

  useEffect(() => {
    meterContextActiveRef.current = meterActive;
    syncMeterStreamingRef.current();
  }, [meterActive]);

  useEffect(() => {
    let stopped = false;
    let disconnect: () => void = () => undefined;
    let stateRetryTimer: number | null = null;
    let eventRetryTimer: number | null = null;
    let meterRetryTimer: number | null = null;
    let stateErrorShown = false;
    let eventErrorShown = false;
    let meterDesired = false;
    let meterApplied: boolean | null = null;
    let meterInFlight = false;
    let meterForcePending = false;
    let meterRetryAttempt = 0;

    const bootstrapState = (attempt = 0) => {
      if (stopped) return;
      void refresh().catch((error) => {
        if (stopped) return;
        if (!stateErrorShown && attempt >= 7) {
          stateErrorShown = true;
          reportError(`Waiting for audio engine: ${String(error)}`);
        }
        stateRetryTimer = window.setTimeout(
          () => bootstrapState(attempt + 1),
          startupRetryDelay(attempt),
        );
      });
    };

    const connectEvents = (attempt = 0) => {
      if (stopped) return;
      void connectWaveLinuxEvents()
        .then((unlisten) => {
          if (stopped) {
            unlisten();
            return;
          }
          disconnect();
          disconnect = unlisten;
          // Close the gap between the initial snapshot and listener registration.
          void refresh().catch(() => undefined);
        })
        .catch((error) => {
          if (stopped) return;
          if (!eventErrorShown && attempt >= 7) {
            eventErrorShown = true;
            reportError(`Waiting for app events: ${String(error)}`);
          }
          eventRetryTimer = window.setTimeout(
            () => connectEvents(attempt + 1),
            startupRetryDelay(attempt),
          );
        });
    };

    const flushMeterStreaming = (force = false) => {
      if (stopped) return;
      meterForcePending ||= force;
      if (meterInFlight) return;
      const enabled = meterDesired;
      const forced = meterForcePending;
      meterForcePending = false;
      if (!forced && meterApplied === enabled) return;

      meterInFlight = true;
      let failed = false;
      void invoke<boolean>("set_meter_streaming", { enabled })
        .then(() => {
          if (stopped) return;
          meterApplied = enabled;
          meterRetryAttempt = 0;
        })
        .catch(() => {
          if (stopped) return;
          failed = true;
          meterApplied = null;
          if (meterRetryTimer !== null) window.clearTimeout(meterRetryTimer);
          const retryDelay = startupRetryDelay(meterRetryAttempt);
          meterRetryAttempt += 1;
          meterRetryTimer = window.setTimeout(() => {
            meterRetryTimer = null;
            flushMeterStreaming(true);
          }, retryDelay);
        })
        .finally(() => {
          meterInFlight = false;
          if (!stopped && !failed && (meterDesired !== enabled || meterForcePending)) {
            flushMeterStreaming();
          }
        });
    };

    const requestMeterStreaming = (force = false) => {
      const enabled =
        meterContextActiveRef.current && document.visibilityState !== "hidden";
      if (meterDesired !== enabled) {
        meterRetryAttempt = 0;
        if (meterRetryTimer !== null) {
          window.clearTimeout(meterRetryTimer);
          meterRetryTimer = null;
        }
      }
      meterDesired = enabled;
      flushMeterStreaming(force);
    };

    syncMeterStreamingRef.current = () => requestMeterStreaming();
    bootstrapState();
    connectEvents();
    requestMeterStreaming();

    const handlePageShow = () => requestMeterStreaming(true);
    const handleVisibilityChange = () => requestMeterStreaming(true);
    window.addEventListener("pageshow", handlePageShow);
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      stopped = true;
      syncMeterStreamingRef.current = () => undefined;
      disconnect();
      if (stateRetryTimer !== null) window.clearTimeout(stateRetryTimer);
      if (eventRetryTimer !== null) window.clearTimeout(eventRetryTimer);
      if (meterRetryTimer !== null) window.clearTimeout(meterRetryTimer);
      window.removeEventListener("pageshow", handlePageShow);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      // React StrictMode may mount this hook twice, so a stale cleanup must not
      // send an asynchronous disable after the replacement mount enables it.
    };
  }, [refresh, reportError]);
}
