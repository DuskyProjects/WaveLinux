import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useRef, useSyncExternalStore } from "react";
import type {
  AppStateSnapshot,
  Diagnostic,
  EffectCatalog,
  EngineStatus,
  LevelMeter,
  MixerConfig,
  RuntimeGraph,
} from "./types";

export type StateDeltaEvent = {
  revision: number;
  config_revision: number;
  graph_revision: number;
  config?: MixerConfig;
  graph?: RuntimeGraph;
  diagnostics?: Diagnostic[];
  engine?: EngineStatus;
  catalog?: EffectCatalog;
};

export type MetersEvent = {
  revision: number;
  meters: LevelMeter[];
};

export type OperationAcknowledgement = {
  protocol_version: number;
  revision: number;
  request_id: string;
  command: string;
  status: "succeeded" | "failed";
  state_revision: number;
  config_revision: number;
  graph_revision: number;
  error?: string;
};

type StateUpdater =
  | AppStateSnapshot
  | null
  | ((current: AppStateSnapshot | null) => AppStateSnapshot | null);

export type WaveLinuxRevisionTarget = {
  state_revision: number;
  config_revision: number;
  graph_revision: number;
};

type RevisionWaiter = {
  target: number;
  timer: ReturnType<typeof window.setTimeout>;
  resolve: (delivered: boolean) => void;
};

let snapshot: AppStateSnapshot | null = null;
let stateRevision = 0;
let configRevision = 0;
let graphRevision = 0;
let meterRevision = 0;
let operationRevision = 0;
let meters: LevelMeter[] = [];
let meterLevels = new Map<string, number>();
let meterPaintFrame: number | null = null;
const paintedMeterLevels = new WeakMap<HTMLElement, number>();
const stateListeners = new Set<() => void>();
const meterListeners = new Set<() => void>();
const pendingDeltas: StateDeltaEvent[] = [];
const revisionWaiters = new Set<RevisionWaiter>();

function emitState(): void {
  for (const listener of stateListeners) listener();
}

function emitMeters(): void {
  for (const listener of meterListeners) listener();
}

export function initializeWaveLinuxState(initial: AppStateSnapshot | null): void {
  if (snapshot !== null || initial === null) return;
  snapshot = initial;
  replaceMeters(initial.graph.meters);
}

export function replaceWaveLinuxState(next: AppStateSnapshot): void {
  snapshot = next;
  if (meterRevision === 0) replaceMeters(next.graph.meters);
  emitState();
  if (pendingDeltas.length > 0) {
    const queued = pendingDeltas.splice(0).sort((left, right) => left.revision - right.revision);
    for (const delta of queued) applyStateDelta(delta);
  }
}

export function reconcileWaveLinuxState(
  next: AppStateSnapshot,
  target: WaveLinuxRevisionTarget,
): void {
  snapshot = next;
  stateRevision = Math.max(stateRevision, target.state_revision);
  configRevision = Math.max(configRevision, target.config_revision);
  graphRevision = Math.max(graphRevision, target.graph_revision);
  if (meterRevision === 0) replaceMeters(next.graph.meters);
  emitState();
  flushRevisionWaiters();
  if (pendingDeltas.length > 0) {
    const queued = pendingDeltas.splice(0).sort((left, right) => left.revision - right.revision);
    for (const delta of queued) applyStateDelta(delta);
  }
}

export function updateWaveLinuxState(update: StateUpdater): void {
  const next = typeof update === "function" ? update(snapshot) : update;
  if (Object.is(next, snapshot)) return;
  snapshot = next;
  emitState();
}

export function applyStateDelta(delta: StateDeltaEvent): void {
  if (snapshot === null) {
    pendingDeltas.push(delta);
    return;
  }
  if (delta.revision <= stateRevision) return;
  const hasStateChange = Boolean(
    delta.config || delta.graph || delta.diagnostics || delta.engine || delta.catalog,
  );
  if (hasStateChange) {
    const previousMeters = snapshot.graph.meters;
    snapshot = {
      config: delta.config ?? snapshot.config,
      graph: delta.graph ? { ...delta.graph, meters: previousMeters } : snapshot.graph,
      diagnostics: delta.diagnostics ?? snapshot.diagnostics,
      engine: delta.engine ?? snapshot.engine,
      catalog: delta.catalog ?? snapshot.catalog,
    };
  }
  stateRevision = delta.revision;
  configRevision = Math.max(configRevision, delta.config_revision);
  graphRevision = Math.max(graphRevision, delta.graph_revision);
  if (hasStateChange) emitState();
  flushRevisionWaiters();
}

export function applyMeters(event: MetersEvent): void {
  if (event.revision <= meterRevision) return;
  meterRevision = event.revision;
  replaceMeters(event.meters);
  emitMeters();
}

export function applyOperationAcknowledgement(event: OperationAcknowledgement): void {
  if (event.protocol_version !== 1 || event.revision <= operationRevision) return;
  operationRevision = event.revision;
}

function replaceMeters(next: LevelMeter[]): void {
  meters = next;
  meterLevels = new Map(
    next.map((meter) => {
      const peak = Math.max(meter.peak_left, meter.peak_right);
      const level = Number.isFinite(peak) ? Math.max(0, Math.min(1, peak)) : 0;
      return [meter.node_id, level] as const;
    }),
  );
  scheduleMeterPaint();
}

function scheduleMeterPaint(): void {
  if (typeof window === "undefined" || typeof document === "undefined") return;
  if (meterPaintFrame !== null) return;
  meterPaintFrame = window.requestAnimationFrame(() => {
    meterPaintFrame = null;
    for (const element of document.querySelectorAll<HTMLElement>("[data-meter-id]")) {
      const meterId = element.dataset.meterId;
      if (!meterId) continue;
      const target = Math.max(0, Math.min(1, meterLevels.get(meterId) ?? 0));
      const previous = paintedMeterLevels.get(element);
      if (previous !== undefined && Math.abs(previous - target) < 0.0005) continue;
      paintedMeterLevels.set(element, target);
      if (element.classList.contains("vu-fill")) {
        element.style.transform = `scaleY(${target})`;
      } else if (element.classList.contains("vu-cap")) {
        element.style.transform = `translateY(${(1 - target) * 100}%)`;
      } else {
        element.style.transform = `scaleX(${target})`;
      }
    }
  });
}

function subscribeState(listener: () => void): () => void {
  stateListeners.add(listener);
  return () => stateListeners.delete(listener);
}

function subscribeMeters(listener: () => void): () => void {
  meterListeners.add(listener);
  return () => meterListeners.delete(listener);
}

export function useWaveLinuxSelector<T>(selector: (state: AppStateSnapshot | null) => T): T {
  const cache = useRef<{
    source: AppStateSnapshot | null;
    selector: (state: AppStateSnapshot | null) => T;
    value: T;
  } | null>(null);
  const getSelection = () => {
    if (cache.current?.source === snapshot && cache.current.selector === selector) {
      return cache.current.value;
    }
    const value = selector(snapshot);
    cache.current = { source: snapshot, selector, value };
    return value;
  };
  return useSyncExternalStore(subscribeState, getSelection, getSelection);
}

export function useWaveLinuxMeters(): LevelMeter[] {
  return useSyncExternalStore(subscribeMeters, () => meters, () => meters);
}

function meterLevel(nodeId: string): number {
  return meterLevels.get(nodeId) ?? 0;
}

export function useWaveLinuxMeterLevel(nodeId: string): number {
  return useSyncExternalStore(
    subscribeMeters,
    () => meterLevel(nodeId),
    () => meterLevel(nodeId),
  );
}

export function useWaveLinuxMetersAvailable(): boolean {
  return useSyncExternalStore(
    subscribeMeters,
    () => meters.length > 0,
    () => meters.length > 0,
  );
}

export function waveLinuxRevisions(): {
  state: number;
  config: number;
  graph: number;
  meters: number;
  operations: number;
} {
  return {
    state: stateRevision,
    config: configRevision,
    graph: graphRevision,
    meters: meterRevision,
    operations: operationRevision,
  };
}

export function waitForWaveLinuxStateRevision(
  target: number,
  timeoutMs = 750,
): Promise<boolean> {
  if (stateRevision >= target) return Promise.resolve(true);
  return new Promise((resolve) => {
    const waiter: RevisionWaiter = {
      target,
      timer: window.setTimeout(() => {
        revisionWaiters.delete(waiter);
        resolve(false);
      }, timeoutMs),
      resolve,
    };
    revisionWaiters.add(waiter);
  });
}

function flushRevisionWaiters(): void {
  for (const waiter of revisionWaiters) {
    if (stateRevision < waiter.target) continue;
    revisionWaiters.delete(waiter);
    window.clearTimeout(waiter.timer);
    waiter.resolve(true);
  }
}

export async function connectWaveLinuxEvents(): Promise<UnlistenFn> {
  if (
    typeof window === "undefined" ||
    !("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  ) {
    return () => undefined;
  }
  const unlisteners: UnlistenFn[] = [];
  try {
    unlisteners.push(
      await listen<StateDeltaEvent>("wavelinux://state-delta", (event) => {
        applyStateDelta(event.payload);
      }),
    );
    unlisteners.push(
      await listen<MetersEvent>("wavelinux://meters", (event) => {
        applyMeters(event.payload);
      }),
    );
    unlisteners.push(
      await listen<OperationAcknowledgement>("wavelinux://operation", (event) => {
        applyOperationAcknowledgement(event.payload);
      }),
    );
  } catch (error) {
    unlisteners.forEach((unlisten) => unlisten());
    throw error;
  }
  return () => unlisteners.forEach((unlisten) => unlisten());
}
