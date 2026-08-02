import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { demoState } from "../demo";
import type { Channel, EffectInstance } from "../types";
import { useEffectChainEditor } from "./useEffectChainEditor";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

function channelWithEffects(channel: Channel, effects: EffectInstance[]): Channel {
  return { ...channel, effects };
}

describe("useEffectChainEditor", () => {
  it("keeps a newer acknowledged edit when an older request finishes later", async () => {
    const state = structuredClone(demoState);
    const channel = state.config.channels[0];
    const first = deferred<Channel>();
    const second = deferred<Channel>();
    const setEffectChain = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    const { result } = renderHook(() =>
      useEffectChainEditor(state, channel, setEffectChain),
    );

    act(() => {
      result.current.updateEffectParam("demo-rnnoise", "vad_threshold", 50);
    });
    await waitFor(() => expect(setEffectChain).toHaveBeenCalledTimes(1));
    act(() => {
      result.current.updateEffectParam("demo-rnnoise", "vad_threshold", 70);
    });
    await waitFor(() => expect(setEffectChain).toHaveBeenCalledTimes(2));

    const newestEffects = setEffectChain.mock.calls[1][1] as EffectInstance[];
    await act(async () => {
      second.resolve(channelWithEffects(channel, newestEffects));
      await second.promise;
    });
    expect(
      result.current.selectedEffects.find(
        (effect) => effect.instance_id === "demo-rnnoise",
      )?.params.vad_threshold,
    ).toBe(70);

    const staleEffects = setEffectChain.mock.calls[0][1] as EffectInstance[];
    await act(async () => {
      first.resolve(channelWithEffects(channel, staleEffects));
      await first.promise;
    });
    expect(
      result.current.selectedEffects.find(
        (effect) => effect.instance_id === "demo-rnnoise",
      )?.params.vad_threshold,
    ).toBe(70);
    await waitFor(() => expect(result.current.pending).toBe(false));
  });

  it("reverts the draft and reports only the latest request error", async () => {
    const state = structuredClone(demoState);
    const channel = state.config.channels[0];
    const setEffectChain = vi.fn().mockRejectedValue(new Error("core unavailable"));
    const { result } = renderHook(() =>
      useEffectChainEditor(state, channel, setEffectChain),
    );

    act(() => {
      result.current.updateEffectParam("demo-rnnoise", "vad_threshold", 80);
    });

    await waitFor(() => expect(result.current.effectError).toBe("Error: core unavailable"));
    expect(result.current.selectedEffects).toEqual(channel.effects);
    expect(result.current.pending).toBe(false);
  });
});
