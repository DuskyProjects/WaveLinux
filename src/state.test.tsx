import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { demoState } from "./demo";

type StateModule = typeof import("./state");

let state: StateModule;

beforeEach(async () => {
  vi.resetModules();
  state = await import("./state");
});

describe("WaveLinux state delivery", () => {
  it("applies monotonic state deltas without replacing live meter data", () => {
    const initial = structuredClone(demoState);
    initial.graph.meters = [{ node_id: "hardware_in", peak_left: 0.4, peak_right: 0.3 }];
    state.initializeWaveLinuxState(initial);

    function Probe() {
      const name = state.useWaveLinuxSelector(
        (snapshot) => snapshot?.config.channels[0]?.name ?? "missing",
      );
      const level = state.useWaveLinuxMeterLevel("hardware_in");
      return <output>{`${name}:${level}`}</output>;
    }

    render(<Probe />);
    expect(screen.getByText("Input:0.4")).toBeInTheDocument();

    const config = structuredClone(initial.config);
    config.channels[0].name = "Voice";
    const graph = structuredClone(initial.graph);
    graph.meters = [{ node_id: "hardware_in", peak_left: 0.99, peak_right: 0.99 }];
    act(() => {
      state.applyStateDelta({
        revision: 2,
        config_revision: 2,
        graph_revision: 2,
        config,
        graph,
      });
    });

    expect(screen.getByText("Voice:0.4")).toBeInTheDocument();
    expect(state.waveLinuxRevisions()).toMatchObject({ state: 2, config: 2, graph: 2 });
  });

  it("paints every matching VU element from the meter event stream", () => {
    let paint: FrameRequestCallback | undefined;
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      paint = callback;
      return 1;
    });
    document.body.innerHTML = `
      <div class="vu-fill" data-meter-id="hardware_in"></div>
      <div class="vu-cap" data-meter-id="hardware_in"></div>
      <div class="meter-horizontal" data-meter-id="hardware_in"></div>
    `;

    act(() => {
      state.applyMeters({
        revision: 1,
        meters: [{ node_id: "hardware_in", peak_left: 0.6, peak_right: 0.25 }],
      });
    });
    paint?.(0);

    expect(document.querySelector<HTMLElement>(".vu-fill")?.style.transform).toBe(
      "scaleY(0.6)",
    );
    expect(document.querySelector<HTMLElement>(".vu-cap")?.style.transform).toBe(
      "translateY(40%)",
    );
    expect(document.querySelector<HTMLElement>(".meter-horizontal")?.style.transform).toBe(
      "scaleX(0.6)",
    );
  });

  it("ignores stale meter and operation events", () => {
    state.applyMeters({
      revision: 3,
      meters: [{ node_id: "hardware_in", peak_left: 0.75, peak_right: 0.5 }],
    });
    state.applyMeters({
      revision: 2,
      meters: [{ node_id: "hardware_in", peak_left: 0.1, peak_right: 0.1 }],
    });
    state.applyOperationAcknowledgement({
      protocol_version: 1,
      revision: 4,
      request_id: "new",
      command: "set_channel_mute",
      status: "succeeded",
      state_revision: 7,
      config_revision: 6,
      graph_revision: 5,
    });
    state.applyOperationAcknowledgement({
      protocol_version: 1,
      revision: 3,
      request_id: "stale",
      command: "set_channel_mute",
      status: "succeeded",
      state_revision: 99,
      config_revision: 99,
      graph_revision: 99,
    });

    expect(state.waveLinuxRevisions()).toMatchObject({ meters: 3, operations: 4 });
  });

  it("resolves revision waiters without rerendering for revision-only deltas", async () => {
    state.initializeWaveLinuxState(structuredClone(demoState));
    let renders = 0;

    function Probe() {
      renders += 1;
      const name = state.useWaveLinuxSelector(
        (snapshot) => snapshot?.config.channels[0]?.name ?? "missing",
      );
      return <output>{name}</output>;
    }

    render(<Probe />);
    expect(renders).toBe(1);
    const delivered = state.waitForWaveLinuxStateRevision(8, 1_000);
    act(() => {
      state.applyStateDelta({
        revision: 8,
        config_revision: 5,
        graph_revision: 3,
      });
    });

    await expect(delivered).resolves.toBe(true);
    expect(renders).toBe(1);
    expect(state.waveLinuxRevisions()).toMatchObject({ state: 8, config: 5, graph: 3 });
  });

  it("does not rerender primitive selectors for unrelated state changes", () => {
    const initial = structuredClone(demoState);
    state.initializeWaveLinuxState(initial);
    let renders = 0;

    function Probe() {
      renders += 1;
      const healthy = state.useWaveLinuxSelector(
        (snapshot) => snapshot?.engine.healthy ?? false,
      );
      return <output>{String(healthy)}</output>;
    }

    render(<Probe />);
    const config = structuredClone(initial.config);
    config.channels[0].name = "Changed elsewhere";
    act(() => {
      state.applyStateDelta({
        revision: 2,
        config_revision: 2,
        graph_revision: 1,
        config,
      });
    });

    expect(screen.getByText("true")).toBeInTheDocument();
    expect(renders).toBe(1);
  });

  it("times out revision waiters when delivery is missing", async () => {
    vi.useFakeTimers();
    state.initializeWaveLinuxState(structuredClone(demoState));
    const delivered = state.waitForWaveLinuxStateRevision(4, 750);

    await vi.advanceTimersByTimeAsync(750);

    await expect(delivered).resolves.toBe(false);
    vi.useRealTimers();
  });
});
