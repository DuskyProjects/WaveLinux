import { beforeEach, describe, expect, it, vi } from "vitest";
import { demoState } from "./demo";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

type StateModule = typeof import("./state");
type TauriModule = typeof import("./tauri");

let state: StateModule;
let tauri: TauriModule;

function operationResponse(stateRevision: number) {
  return {
    protocol_version: 1,
    revision: stateRevision,
    request_id: `request-${stateRevision}`,
    command: "set_channel_mute",
    status: "succeeded",
    state_revision: stateRevision,
    config_revision: stateRevision,
    graph_revision: stateRevision,
    value: { muted: true },
  };
}

beforeEach(async () => {
  vi.useFakeTimers();
  vi.resetModules();
  invokeMock.mockReset();
  Object.assign(window, { __TAURI_INTERNALS__: {} });
  state = await import("./state");
  tauri = await import("./tauri");
  state.initializeWaveLinuxState(structuredClone(demoState));
});

describe("acknowledged Tauri mutations", () => {
  it("does not fetch a snapshot when the matching state delta arrives", async () => {
    invokeMock.mockResolvedValueOnce(operationResponse(3));

    await expect(
      tauri.invoke("set_channel_mute", {
        channelId: "hardware_in",
        mixId: "monitor",
        muted: true,
      }),
    ).resolves.toEqual({ muted: true });
    state.applyStateDelta({ revision: 3, config_revision: 3, graph_revision: 3 });
    await vi.advanceTimersByTimeAsync(750);

    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).not.toHaveBeenCalledWith("observe_state");
  });

  it("recovers once when a state delta is lost", async () => {
    invokeMock
      .mockResolvedValueOnce(operationResponse(5))
      .mockResolvedValueOnce(structuredClone(demoState));

    await tauri.invoke("set_channel_mute", {
      channelId: "hardware_in",
      mixId: "monitor",
      muted: true,
    });
    await vi.advanceTimersByTimeAsync(750);

    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock).toHaveBeenLastCalledWith("observe_state");
    expect(state.waveLinuxRevisions()).toMatchObject({ state: 5, config: 5, graph: 5 });
  });
});
