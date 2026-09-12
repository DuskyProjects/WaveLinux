import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { AppStateSnapshot } from "./types";
import { demoState } from "./demo";
import { replaceWaveLinuxState, waveLinuxRevisions } from "./state";
import App from "./App";

const backend = vi.hoisted(() => ({
  snapshot: null as AppStateSnapshot | null,
  streaming: false,
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    async (event: string, callback: (event: { payload: unknown }) => void) => {
      backend.listeners.set(event, callback);
      return () => backend.listeners.delete(event);
    },
  ),
}));
vi.mock("./tauri", async () => {
  const { invokeDemo } = await import("./demo");
  return {
    initialSnapshot: () => backend.snapshot,
    invoke: vi.fn(async (command: string, args?: Record<string, unknown>) => {
      if (command === "set_meter_streaming") {
        backend.streaming = args?.enabled === true;
        return backend.streaming;
      }
      if (command === "get_state" || command === "observe_state")
        return structuredClone(backend.snapshot);
      return invokeDemo(command, args);
    }),
  };
});

beforeEach(() => {
  backend.snapshot = structuredClone(demoState);
  backend.snapshot.config.settings.auto_check_updates = false;
  backend.snapshot.config.channels[0].id = "hardware_in";
  backend.snapshot.config.channels[0].effects.push({
    instance_id: "test-compressor",
    effect_id: "compressor",
    bypassed: false,
    params: {},
  });
  backend.streaming = false;
  backend.listeners.clear();
  replaceWaveLinuxState(backend.snapshot);
  Reflect.set(window, "__TAURI__", {});
});
afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI__");
  window.history.replaceState({}, "", "/");
});

it.each(["mixer", "effects"])(
  "streams native microphone telemetry from %s to Effects, and suspends it when hidden or on Settings",
  async (initialView) => {
    window.history.replaceState({}, "", `/?view=${initialView}`);
    const visibility = vi
      .spyOn(document, "visibilityState", "get")
      .mockReturnValue("visible");
    render(<App />);
    await waitFor(() => expect(backend.streaming).toBe(true));
    await waitFor(() =>
      expect(backend.listeners.has("wavelinux://meters")).toBe(true),
    );
    fireEvent.click(screen.getByRole("button", { name: "Effects" }));
    await act(async () => {}); // Allow the view's streaming request to settle.
    expect(backend.streaming).toBe(true);
    const compressor = screen.getByRole("group", {
      name: "Compressor controls",
    });
    expect(
      within(compressor).queryByText("Demo signal"),
    ).not.toBeInTheDocument();
    act(() => {
      if (backend.streaming)
        backend.listeners.get("wavelinux://meters")!({
          payload: {
            revision: waveLinuxRevisions().meters + 1,
            meters: [
              {
                node_id: "hardware_in",
                spectrum: Array(64).fill(0.8),
                peak_left: 0.5,
                peak_right: 0.5,
                compressor: {
                  input_peak: 0.1,
                  output_peak: 0.01,
                  gain_reduction_db: 20,
                },
              },
            ],
          },
        });
    });
    expect(within(compressor).getByText("Live")).toBeVisible();
    expect(
      within(compressor).getByRole("meter", { name: "Input" }),
    ).toHaveAttribute("aria-valuetext", "-20 dB");
    expect(
      within(compressor).getByRole("meter", { name: "Output" }),
    ).toHaveAttribute("aria-valuetext", "-40 dB");
    expect(
      compressor.querySelector(".compressor-wave-before")!.getAttribute("d"),
    ).not.toBe("");
    visibility.mockReturnValue("hidden");
    fireEvent(document, new Event("visibilitychange"));
    await waitFor(() => expect(backend.streaming).toBe(false));
    visibility.mockReturnValue("visible");
    fireEvent(document, new Event("visibilitychange"));
    await waitFor(() => expect(backend.streaming).toBe(true));
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await waitFor(() => expect(backend.streaming).toBe(false));
  },
);

it("follows the selected channel without using the microphone as a fallback", async () => {
  for (const id of ["music", "game"]) {
    backend.snapshot!.config.channels.find(
      (channel) => channel.id === id,
    )!.effects = [
      {
        instance_id: `compressor-${id}`,
        effect_id: "compressor",
        bypassed: false,
        params: {},
      },
      { instance_id: `eq-${id}`, effect_id: "eq", bypassed: false, params: {} },
    ];
  }
  replaceWaveLinuxState(backend.snapshot!);
  window.history.replaceState({}, "", "/?view=effects");
  vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  render(<App />);
  await waitFor(() => expect(backend.streaming).toBe(true));
  await waitFor(() =>
    expect(backend.listeners.has("wavelinux://meters")).toBe(true),
  );
  const emitLevels = (includeMusic = true) =>
    act(() => {
      backend.listeners.get("wavelinux://meters")!({
        payload: {
          revision: waveLinuxRevisions().meters + 1,
          meters: [
            {
              node_id: "hardware_in",
              spectrum: Array(64).fill(0.8),
              peak_left: 0.5,
              peak_right: 0.5,
              compressor: {
                input_peak: 0.1,
                output_peak: 0.05,
                gain_reduction_db: 6,
              },
            },
            ...(includeMusic
              ? [
                  {
                    node_id: "music",
                    spectrum: Array(64).fill(0.2),
                    peak_left: 0.8,
                    peak_right: 0.8,
                    compressor: {
                      input_peak: 0.01,
                      output_peak: 0.001,
                      gain_reduction_db: 20,
                    },
                  },
                ]
              : []),
            {
              node_id: "game",
              spectrum: Array(64).fill(0),
              peak_left: 0,
              peak_right: 0,
              compressor: {
                input_peak: 0,
                output_peak: 0,
                gain_reduction_db: 0,
              },
            },
          ],
        },
      });
    });
  emitLevels();
  emitLevels();
  emitLevels();
  const controls = () =>
    within(screen.getByRole("group", { name: "Compressor controls" }));
  expect(controls().getByRole("meter", { name: "Input" })).toHaveAttribute(
    "aria-valuetext",
    "-20 dB",
  );
  const microphoneWave = screen
    .getByRole("group", { name: "Compressor controls" })
    .querySelector(".compressor-wave-before")!
    .getAttribute("d")!;
  const microphoneSpectrum = screen
    .getByTestId("eq-spectrum")
    .getAttribute("d");
  expect(microphoneSpectrum).not.toBe("");
  fireEvent.click(screen.getByRole("button", { name: "Music" }));
  expect(screen.getByTestId("eq-spectrum").getAttribute("d")).not.toBe(
    microphoneSpectrum,
  );
  expect(controls().getByRole("meter", { name: "Input" })).toHaveAttribute(
    "aria-valuetext",
    "-40 dB",
  );
  expect(controls().getByRole("meter", { name: "Output" })).toHaveAttribute(
    "aria-valuetext",
    "-60 dB",
  );
  const musicWave = screen
    .getByRole("group", { name: "Compressor controls" })
    .querySelector(".compressor-wave-before")!
    .getAttribute("d")!;
  expect(musicWave.length).toBeLessThan(microphoneWave.length);
  emitLevels(false);
  expect(screen.getByTestId("eq-spectrum")).toHaveAttribute("d", "");
  expect(controls().getByText("Waiting for audio")).toBeVisible();
  expect(
    screen
      .getByRole("group", { name: "Compressor controls" })
      .querySelector(".compressor-wave-before"),
  ).toHaveAttribute("d", "");
  fireEvent.click(screen.getByRole("button", { name: "Game" }));
  expect(screen.getByTestId("eq-spectrum").getAttribute("d")).toContain(
    "L0,300",
  );
  expect(controls().getByRole("meter", { name: "Input" })).toHaveAttribute(
    "aria-valuetext",
    "−∞ dB",
  );
  fireEvent.click(screen.getByRole("button", { name: "Input" }));
  expect(controls().getByRole("meter", { name: "Input" })).toHaveAttribute(
    "aria-valuetext",
    "-20 dB",
  );
});
