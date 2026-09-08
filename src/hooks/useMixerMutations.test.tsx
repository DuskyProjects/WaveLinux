import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { demoState } from "../demo";
import { replaceWaveLinuxState, useWaveLinuxSelector } from "../state";
import type { Channel, Mix, MixBus } from "../types";
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

  it("serializes linked volume edits across different mixes", async () => {
    const initial = structuredClone(demoState);
    const channel = initial.config.channels[0];
    channel.linked = true;
    replaceWaveLinuxState(initial);
    const first = deferred<MixBus>();
    const last = deferred<MixBus>();
    invokeMock.mockReturnValueOnce(first.promise).mockReturnValueOnce(last.promise);
    const { result } = renderHook(() => ({
      mutations: useMixerMutations({ refresh: vi.fn(), reportError: vi.fn() }),
      channel: useWaveLinuxSelector((state) => state?.config.channels[0]),
    }));
    await act(async () => {
      await result.current.mutations.setChannelBusVolumeFast(channel.id, "monitor", 0.4);
      await result.current.mutations.setChannelBusVolumeFast(channel.id, "stream", 0.8);
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);
    await act(async () => {
      first.resolve({ ...channel.mix_buses.monitor, volume: 0.4 });
      await first.promise;
    });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(2));
    expect(result.current.channel?.mix_buses.monitor.volume).toBe(0.8);
    expect(result.current.channel?.mix_buses.stream.volume).toBe(0.8);
    await act(async () => {
      last.resolve({ ...channel.mix_buses.stream, volume: 0.8 });
      await last.promise;
    });
    expect(result.current.channel?.mix_buses.monitor.volume).toBe(0.8);
  });

  it("keeps separate unlinked mix values when coalescing pending channel edits", async () => {
    const initial = structuredClone(demoState);
    const channel = initial.config.channels[0];
    channel.linked = false;
    replaceWaveLinuxState(initial);
    const first = deferred<MixBus>();
    invokeMock.mockReturnValueOnce(first.promise).mockImplementation((_command, args) =>
      Promise.resolve({ ...channel.mix_buses[args.mixId], volume: args.volume }),
    );
    const { result } = renderHook(() => ({
      mutations: useMixerMutations({ refresh: vi.fn(), reportError: vi.fn() }),
      channel: useWaveLinuxSelector((state) => state?.config.channels[0]),
    }));
    await act(async () => {
      await result.current.mutations.setChannelBusVolumeFast(channel.id, "monitor", 0.2);
      await result.current.mutations.setChannelBusVolumeFast(channel.id, "stream", 0.3);
      await result.current.mutations.setChannelBusVolumeFast(channel.id, "monitor", 0.6);
      await result.current.mutations.setChannelBusVolumeFast(channel.id, "stream", 0.5);
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);
    await act(async () => {
      first.resolve({ ...channel.mix_buses.monitor, volume: 0.2 });
      await first.promise;
    });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(3));
    expect(invokeMock.mock.calls.slice(1).map(([, args]) => [args.mixId, args.volume]))
      .toEqual([["monitor", 0.6], ["stream", 0.5]]);
    expect(result.current.channel?.mix_buses.monitor.volume).toBe(0.6);
    expect(result.current.channel?.mix_buses.stream.volume).toBe(0.5);
  });

  it("recovers failed channel volume writes after newer edits have finished", async () => {
    const channel = demoState.config.channels[0];
    const first = deferred<MixBus>();
    const last = deferred<MixBus>();
    invokeMock.mockReturnValueOnce(first.promise).mockReturnValueOnce(last.promise);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const reportError = vi.fn();
    const { result } = renderHook(() => useMixerMutations({ refresh, reportError }));
    await act(async () => {
      await result.current.setChannelBusVolumeFast(channel.id, "monitor", 0.4);
      await result.current.setChannelBusVolumeFast(channel.id, "stream", 0.8);
      first.reject(new Error("volume write failed"));
    });
    await waitFor(() => expect(invokeMock).toHaveBeenCalledTimes(2));
    expect(refresh).not.toHaveBeenCalled();
    await act(async () => {
      last.resolve({ ...channel.mix_buses.stream, volume: 0.8 });
      await last.promise;
    });
    await waitFor(() => expect(refresh).toHaveBeenCalledTimes(1));
    expect(reportError).toHaveBeenCalledWith("Error: volume write failed");
  });

  it("serializes effect writes and coalesces intermediate slider positions", async () => {
    const channel = structuredClone(demoState.config.channels[0]);
    const first = deferred<Channel>();
    const last = deferred<Channel>();
    invokeMock.mockReturnValueOnce(first.promise).mockReturnValueOnce(last.promise);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() => ({
      mutations: useMixerMutations({ refresh, reportError: vi.fn() }),
      effects: useWaveLinuxSelector((state) => state?.config.channels[0].effects),
    }));
    const edits = [40, 60, 80].map((value) => channel.effects.map((effect) => ({
      ...effect,
      params: { ...effect.params, vad_threshold: value },
    })));
    let writes!: Promise<Channel>[];
    act(() => {
      writes = edits.map((effects) => result.current.mutations.setEffectChainFast(channel.id, effects));
    });
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(result.current.effects).toEqual(edits[2]);

    await act(async () => {
      first.resolve({ ...channel, effects: edits[0] });
      await writes[0];
    });
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock).toHaveBeenLastCalledWith("set_effect_chain", { channelId: channel.id, effects: edits[2] });
    expect(result.current.effects).toEqual(edits[2]);

    await act(async () => {
      last.resolve({ ...channel, effects: edits[2] });
      await Promise.all(writes);
    });
    await expect(writes[1]).resolves.toMatchObject({ effects: edits[2] });
    await expect(writes[2]).resolves.toMatchObject({ effects: edits[2] });
    expect(refresh).not.toHaveBeenCalled();
  });

  it("continues with the newest effect edit after an earlier write fails", async () => {
    const channel = structuredClone(demoState.config.channels[0]);
    const first = deferred<Channel>();
    invokeMock.mockReturnValueOnce(first.promise).mockResolvedValueOnce(channel);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const reportError = vi.fn();
    const { result } = renderHook(() => useMixerMutations({ refresh, reportError }));
    let failed!: Promise<unknown>;
    let latest!: Promise<Channel>;
    act(() => {
      failed = result.current.setEffectChainFast(channel.id, []).catch((error: unknown) => error);
      latest = result.current.setEffectChainFast(channel.id, channel.effects);
    });
    await act(async () => {
      first.reject(new Error("temporary failure"));
      await Promise.all([failed, latest]);
    });
    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(reportError).toHaveBeenCalledWith("Error: temporary failure");
    expect(refresh).not.toHaveBeenCalled();
    await expect(latest).resolves.toEqual(channel);
  });

  it("rejects every coalesced effect edit and refreshes when the final write fails", async () => {
    const channel = structuredClone(demoState.config.channels[0]);
    const first = deferred<Channel>();
    const failure = new Error("core unavailable");
    invokeMock.mockReturnValueOnce(first.promise).mockRejectedValueOnce(failure);
    const refresh = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() => useMixerMutations({ refresh, reportError: vi.fn() }));
    let results!: Promise<PromiseSettledResult<Channel>[]>;
    act(() => {
      results = Promise.allSettled([0, 1, 2].map(() =>
        result.current.setEffectChainFast(channel.id, channel.effects),
      ));
    });
    await act(async () => {
      first.resolve(channel);
      await results;
    });
    expect((await results).map((result) => result.status)).toEqual(["fulfilled", "rejected", "rejected"]);
    expect(refresh).toHaveBeenCalledTimes(1);
    // A failed queue must also accept a later retry.
    invokeMock.mockResolvedValueOnce(channel);
    await act(async () => {
      await result.current.setEffectChainFast(channel.id, channel.effects);
    });
    expect(invokeMock).toHaveBeenCalledTimes(3);
  });

  it("selecting a stream output preserves monitor default following", async () => {
    const initial = structuredClone(demoState);
    initial.config.settings.monitor_follows_default_output = true;
    replaceWaveLinuxState(initial);
    const stream = initial.config.mixes.find((mix) => mix.id === "stream")!;
    invokeMock.mockResolvedValueOnce({ ...stream, monitor_output: "speaker" });
    const { result } = renderHook(() => ({
      mutations: useMixerMutations({ refresh: vi.fn(), reportError: vi.fn() }),
      followsDefault: useWaveLinuxSelector((state) => state?.config.settings.monitor_follows_default_output),
    }));
    await act(async () => {
      await result.current.mutations.setMixMonitorOutputFast("stream", "speaker");
    });
    expect(result.current.followsDefault).toBe(true);
  });
});
