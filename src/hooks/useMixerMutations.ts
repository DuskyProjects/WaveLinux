import { useCallback, useRef } from "react";

import { defaultMixBus, mixOutputDevices } from "../mixer-ui";
import { updateWaveLinuxState } from "../state";
import { invoke } from "../tauri";
import type {
  AppStream,
  Channel,
  EffectInstance,
  Mix,
  MixBus,
  MixerSettings,
} from "../types";
import type { SetEffectChain } from "../views/EffectsView";

type LatestNumberQueue = {
  inFlight: boolean;
  latest: number | null;
};

type ChannelVolumeQueue = {
  inFlight: boolean;
  latest: Map<string, number>;
  needsRefresh: boolean;
};

type EffectWrite = {
  effects: EffectInstance[];
  waiters: {
    resolve: (channel: Channel) => void;
    reject: (error: unknown) => void;
  }[];
};

type UseMixerMutationsOptions = {
  refresh: () => Promise<void>;
  reportError: (message: string) => void;
};

export function useMixerMutations({
  refresh,
  reportError,
}: UseMixerMutationsOptions) {
  const setState = updateWaveLinuxState;
  const mixVolumeQueues = useRef<Record<string, LatestNumberQueue>>({});
  const channelVolumeQueues = useRef(new Map<string, ChannelVolumeQueue>());
  const effectQueues = useRef(new Map<string, { inFlight: boolean; latest: EffectWrite | null }>());
  const settingsQueue = useRef<{ inFlight: boolean; latest: MixerSettings | null }>({
    inFlight: false,
    latest: null,
  });

  // Acknowledged mutations are followed by monotonic state deltas. The Tauri
  // bridge performs a bounded compatibility refresh only if a delta is lost.
  const run = useCallback(
    async <T,>(command: string, args?: Record<string, unknown>, message?: string): Promise<T> => {
      try {
        const result = await invoke<T>(command, args);
        if (message) {
          reportError(message);
        }
        return result;
      } catch (error) {
        reportError(String(error));
        throw error;
      }
    },
    [],
  );

  const patchMixVolume = useCallback((mixId: string, volume: number) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          mixes: current.config.mixes.map((mix) =>
            mix.id === mixId ? { ...mix, volume } : mix,
          ),
        },
      };
    });
  }, []);

  const patchMix = useCallback((mixId: string, patch: Partial<Mix>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          mixes: current.config.mixes.map((mix) =>
            mix.id === mixId ? { ...mix, ...patch } : mix,
          ),
        },
      };
    });
  }, []);

  const patchChannelBusVolume = useCallback((
    channelId: string,
    mixId: string,
    volume: number,
    newerWrites?: ReadonlyMap<string, number>,
  ) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          channels: current.config.channels.map((channel) => {
            if (channel.id !== channelId) return channel;
            if (newerWrites && (channel.linked ? newerWrites.size > 0 : newerWrites.has(mixId))) {
              return channel;
            }
            const mixBuses = Object.fromEntries(
              Object.entries(channel.mix_buses).map(([busMixId, bus]) => [
                busMixId,
                channel.linked || busMixId === mixId ? { ...bus, volume } : bus,
              ]),
            );
            return { ...channel, mix_buses: mixBuses };
          }),
        },
      };
    });
  }, []);

  const patchChannelBus = useCallback((channelId: string, mixId: string, patch: Partial<MixBus>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          channels: current.config.channels.map((channel) => {
            if (channel.id !== channelId) return channel;
            const bus = channel.mix_buses[mixId] ?? defaultMixBus(false);
            return {
              ...channel,
              mix_buses: {
                ...channel.mix_buses,
                [mixId]: { ...bus, ...patch },
              },
            };
          }),
        },
      };
    });
  }, []);

  const patchChannel = useCallback((channelId: string, patch: Partial<Channel>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          channels: current.config.channels.map((channel) =>
            channel.id === channelId ? { ...channel, ...patch } : channel,
          ),
        },
      };
    });
  }, []);

  const patchAppStream = useCallback((streamId: string, patch: Partial<AppStream>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        graph: {
          ...current.graph,
          app_streams: current.graph.app_streams.map((stream) =>
            stream.id === streamId ? { ...stream, ...patch } : stream,
          ),
        },
      };
    });
  }, []);

  const flushMixVolumeQueue = useCallback(
    (mixId: string) => {
      const queue = mixVolumeQueues.current[mixId];
      if (!queue || queue.inFlight || queue.latest === null) return;
      const volume = queue.latest;
      queue.latest = null;
      queue.inFlight = true;
      void invoke<Mix>("set_mix_volume", { mixId, volume })
        .then((mix) => {
          if (queue.latest === null) {
            patchMixVolume(mix.id, mix.volume);
          }
        })
        .catch((error) => {
          reportError(String(error));
          void refresh().catch(() => undefined);
        })
        .finally(() => {
          queue.inFlight = false;
          if (queue.latest !== null) {
            flushMixVolumeQueue(mixId);
          }
        });
    },
    [patchMixVolume, refresh],
  );

  const setMixVolumeFast = useCallback(
    async (mixId: string, volume: number) => {
      patchMixVolume(mixId, volume);
      const queue = mixVolumeQueues.current[mixId] ?? { inFlight: false, latest: null };
      mixVolumeQueues.current[mixId] = queue;
      queue.latest = volume;
      flushMixVolumeQueue(mixId);
    },
    [flushMixVolumeQueue, patchMixVolume],
  );

  const flushChannelVolumeQueue = useCallback(
    (channelId: string) => {
      const queue = channelVolumeQueues.current.get(channelId);
      if (!queue || queue.inFlight) return;
      const next = queue.latest.entries().next().value;
      if (!next) return;
      const [mixId, volume] = next;
      queue.latest.delete(mixId);
      queue.inFlight = true;
      void invoke<MixBus>("set_channel_volume", { channelId, mixId, volume })
        .then((bus) => {
          patchChannelBusVolume(channelId, mixId, bus.volume, queue.latest);
        })
        .catch((error) => {
          reportError(String(error));
          queue.needsRefresh = true;
        })
        .finally(() => {
          queue.inFlight = false;
          if (queue.latest.size > 0) {
            flushChannelVolumeQueue(channelId);
          } else {
            channelVolumeQueues.current.delete(channelId);
            if (queue.needsRefresh) void refresh().catch(() => undefined);
          }
        });
    },
    [patchChannelBusVolume, refresh, reportError],
  );

  const setChannelBusVolumeFast = useCallback(
    async (channelId: string, mixId: string, volume: number) => {
      patchChannelBusVolume(channelId, mixId, volume);
      const queue = channelVolumeQueues.current.get(channelId) ?? {
        inFlight: false, latest: new Map<string, number>(), needsRefresh: false,
      };
      channelVolumeQueues.current.set(channelId, queue);
      // Keep the latest edit for each bus in submission order, including when
      // a linked channel is edited through more than one mix's slider.
      queue.latest.delete(mixId);
      queue.latest.set(mixId, volume);
      flushChannelVolumeQueue(channelId);
    },
    [flushChannelVolumeQueue, patchChannelBusVolume],
  );

  const setMixMuteFast = useCallback(
    async (mixId: string, muted: boolean) => {
      patchMix(mixId, { muted });
      try {
        const mix = await invoke<Mix>("set_mix_mute", { mixId, muted });
        patchMix(mix.id, { muted: mix.muted });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, refresh],
  );

  const setMixIconFast = useCallback(
    async (mixId: string, icon: string | null) => {
      patchMix(mixId, { icon });
      try {
        const mix = await invoke<Mix>("set_mix_icon", {
          mixId,
          mix_id: mixId,
          icon,
        });
        patchMix(mix.id, { icon: mix.icon ?? null });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, refresh],
  );

  const setChannelIconFast = useCallback(
    async (channelId: string, icon: string | null) => {
      patchChannel(channelId, { icon });
      try {
        const channel = await invoke<Channel>("set_channel_icon", {
          channelId,
          channel_id: channelId,
          icon,
        });
        patchChannel(channel.id, { icon: channel.icon ?? null });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannel, refresh],
  );

  const setChannelBusMuteFast = useCallback(
    async (channelId: string, mixId: string, muted: boolean) => {
      patchChannelBus(channelId, mixId, { muted });
      try {
        const bus = await invoke<MixBus>("set_channel_mute", { channelId, mixId, muted });
        patchChannelBus(channelId, mixId, { muted: bus.muted });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannelBus, refresh],
  );

  const setChannelBusEnabledFast = useCallback(
    async (channelId: string, mixId: string, enabled: boolean) => {
      patchChannelBus(channelId, mixId, { enabled });
      try {
        const bus = await invoke<MixBus>("set_channel_bus_enabled", {
          channelId,
          channel_id: channelId,
          mixId,
          mix_id: mixId,
          enabled,
        });
        patchChannelBus(channelId, mixId, { enabled: bus.enabled, muted: bus.muted, volume: bus.volume });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannelBus, refresh],
  );

  const setChannelInputFast = useCallback(
    async (channelId: string, sourceDevice: string | null) => {
      patchChannel(channelId, { source_device: sourceDevice });
      try {
        const channel = await invoke<Channel>("set_channel_input", {
          channelId,
          channel_id: channelId,
          sourceDevice,
          source_device: sourceDevice,
        });
        patchChannel(channel.id, { source_device: channel.source_device ?? null });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannel, refresh],
  );

  const flushEffectQueue = useCallback(
    (channelId: string) => {
      const queue = effectQueues.current.get(channelId);
      if (!queue || queue.inFlight || queue.latest === null) return;
      const write = queue.latest;
      queue.latest = null;
      queue.inFlight = true;
      void invoke<Channel>("set_effect_chain", { channelId, effects: write.effects })
        .then((channel) => {
          if (queue.latest === null) patchChannel(channel.id, { effects: channel.effects });
          for (const waiter of write.waiters) waiter.resolve(channel);
        })
        .catch((error: unknown) => {
          reportError(String(error));
          if (queue.latest === null) void refresh().catch(() => undefined);
          for (const waiter of write.waiters) waiter.reject(error);
        })
        .finally(() => {
          queue.inFlight = false;
          if (queue.latest !== null) flushEffectQueue(channelId);
          else effectQueues.current.delete(channelId);
        });
    },
    [patchChannel, refresh, reportError],
  );

  const setEffectChainFast = useCallback<SetEffectChain>(
    (channelId, effects) => {
      patchChannel(channelId, { effects });
      return new Promise<Channel>((resolve, reject) => {
        const queue = effectQueues.current.get(channelId) ?? { inFlight: false, latest: null };
        effectQueues.current.set(channelId, queue);
        const waiters = queue.latest?.waiters ?? [];
        waiters.push({ resolve, reject });
        queue.latest = { effects, waiters };
        flushEffectQueue(channelId);
      });
    },
    [flushEffectQueue, patchChannel],
  );

  const setChannelEffectsEnabledFast = useCallback(
    async (channelId: string, enabled: boolean) => {
      patchChannel(channelId, { effects_enabled: enabled });
      try {
        const channel = await invoke<Channel>("set_channel_effects_enabled", {
          channelId,
          channel_id: channelId,
          enabled,
        });
        patchChannel(channel.id, {
          effects: channel.effects,
          effects_enabled: channel.effects_enabled,
        });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchChannel, refresh],
  );

  const patchSettings = useCallback((settings: MixerSettings) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          settings,
        },
      };
    });
  }, []);

  const patchSettingsFromPartial = useCallback((patch: Partial<MixerSettings>) => {
    setState((current) => {
      if (!current) return current;
      return {
        ...current,
        config: {
          ...current.config,
          settings: {
            ...current.config.settings,
            ...patch,
          },
        },
      };
    });
  }, []);

  const setMixMonitorOutputFast = useCallback(
    async (mixId: string, output: string | null) => {
      patchMix(mixId, { monitor_output: output, output_devices: output ? [output] : [] });
      if (mixId === "monitor") {
        patchSettingsFromPartial({ monitor_follows_default_output: false });
      }
      try {
        const mix = await invoke<Mix>("set_mix_monitor_output", { mixId, output });
        patchMix(mix.id, {
          monitor_output: mix.monitor_output ?? null,
          output_devices: mixOutputDevices(mix),
        });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, patchSettingsFromPartial, refresh],
  );

  const setMixOutputsFast = useCallback(
    async (mixId: string, outputs: string[]) => {
      const cleanOutputs = Array.from(new Set(outputs.map((output) => output.trim()).filter(Boolean)));
      patchMix(mixId, {
        monitor_output: cleanOutputs[0] ?? null,
        output_devices: cleanOutputs,
      });
      if (mixId === "monitor") {
        patchSettingsFromPartial({ monitor_follows_default_output: false });
      }
      try {
        const mix = await invoke<Mix>("set_mix_outputs", {
          mixId,
          mix_id: mixId,
          outputs: cleanOutputs,
        });
        patchMix(mix.id, {
          monitor_output: mix.monitor_output ?? null,
          output_devices: mixOutputDevices(mix),
        });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchMix, patchSettingsFromPartial, refresh],
  );

  const flushSettingsQueue = useCallback(() => {
    const queue = settingsQueue.current;
    if (queue.inFlight || queue.latest === null) return;
    const settings = queue.latest;
    queue.latest = null;
    queue.inFlight = true;
    void invoke<MixerSettings>("set_settings", { settings })
      .then((next) => {
        if (queue.latest === null) {
          patchSettings(next);
          reportError("Settings updated");
        }
      })
      .catch((error) => {
        reportError(String(error));
        void refresh().catch(() => undefined);
      })
      .finally(() => {
        queue.inFlight = false;
        if (queue.latest !== null) {
          flushSettingsQueue();
        }
      });
  }, [patchSettings, refresh]);

  const setSettingsFast = useCallback(
    async (settings: MixerSettings) => {
      patchSettings(settings);
      settingsQueue.current.latest = settings;
      flushSettingsQueue();
    },
    [flushSettingsQueue, patchSettings],
  );

  const setAppStreamMuteFast = useCallback(
    async (streamId: string, muted: boolean) => {
      patchAppStream(streamId, { muted });
      try {
        await invoke("set_app_stream_mute", { streamId, muted });
      } catch (error) {
        reportError(String(error));
        await refresh().catch(() => undefined);
      }
    },
    [patchAppStream, refresh],
  );


  return {
    run,
    setAppStreamMuteFast,
    setChannelBusEnabledFast,
    setChannelBusMuteFast,
    setChannelBusVolumeFast,
    setChannelEffectsEnabledFast,
    setChannelIconFast,
    setChannelInputFast,
    setEffectChainFast,
    setMixIconFast,
    setMixMonitorOutputFast,
    setMixMuteFast,
    setMixOutputsFast,
    setMixVolumeFast,
    setSettingsFast,
  };
}
