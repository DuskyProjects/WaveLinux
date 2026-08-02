import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { demoState } from "../demo";
import { replaceWaveLinuxState, useWaveLinuxSelector } from "../state";
import type { Mix } from "../types";
import { useMixerMutations } from "./useMixerMutations";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("../tauri", () => ({
  invoke: invokeMock,
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

function acknowledgedMix(volume: number): Mix {
  const current = demoState.config.mixes.find((mix) => mix.id === "monitor");
  if (!current) throw new Error("demo monitor mix is missing");
  return { ...current, volume };
}

beforeEach(() => {
  invokeMock.mockReset();
  replaceWaveLinuxState(structuredClone(demoState));
});

describe("useMixerMutations", () => {
  it("coalesces volume edits and never lets an older acknowledgement win", async () => {
    const first = deferred<Mix>();
    const second = deferred<Mix>();
    invokeMock
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const reportError = vi.fn();
    const { result } = renderHook(() => ({
      mutations: useMixerMutations({ refresh, reportError }),
      volume: useWaveLinuxSelector(
        (state) => state?.config.mixes.find((mix) => mix.id === "monitor")?.volume,
      ),
    }));

    await act(async () => {
      await result.current.mutations.setMixVolumeFast("monitor", 0.4);
      await result.current.mutations.setMixVolumeFast("monitor", 0.8);
    });

    expect(result.current.volume).toBe(0.8);
    expect(invokeMock).toHaveBeenCalledTimes(1);

    await act(async () => {
      first.resolve(acknowledgedMix(0.4));
      await first.promise;
    });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(2));
    expect(result.current.volume).toBe(0.8);

    await act(async () => {
      second.resolve(acknowledgedMix(0.8));
      await second.promise;
    });
    await waitFor(() => expect(result.current.volume).toBe(0.8));
    expect(refresh).not.toHaveBeenCalled();
    expect(reportError).not.toHaveBeenCalled();
  });

  it("reports a failed optimistic mutation and requests authoritative state", async () => {
    invokeMock.mockRejectedValueOnce(new Error("audio core unavailable"));
    const refresh = vi.fn().mockResolvedValue(undefined);
    const reportError = vi.fn();
    const { result } = renderHook(() =>
      useMixerMutations({ refresh, reportError }),
    );

    await act(async () => {
      await result.current.setMixMuteFast("monitor", true);
    });

    expect(reportError).toHaveBeenCalledWith("Error: audio core unavailable");
    expect(refresh).toHaveBeenCalledTimes(1);
  });
});
