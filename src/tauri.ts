import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import type {
  AppMatcher,
  AppStateSnapshot,
  Channel,
  EffectCatalog,
  EffectInstance,
  Mix,
  Scene,
} from "./types";

const isTauri =
  typeof window !== "undefined" &&
  ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    return tauriInvoke<T>(command, args);
  }

  if (command === "get_state" || command === "observe_state") {
    return cloneDemoState() as T;
  }

  console.info(`[demo] ${command}`, args ?? {});
  return demoMutation(command, args) as T;
}

function demoMutation(command: string, args?: Record<string, unknown>): unknown {
  if (command === "list_scenes") {
    return structuredClone(demoScenes);
  }

  if (command === "save_scene") {
    const name = String(args?.name ?? "Scene").trim() || "Scene";
    const scene: Scene = {
      id: `${slug(name) || "scene"}_${crypto.randomUUID()}`,
      name,
      created_unix: Math.floor(Date.now() / 1000),
      config: structuredClone(demoState.config),
    };
    demoScenes.unshift(scene);
    return structuredClone(scene);
  }

  if (command === "load_scene") {
    const scene = demoScenes.find((scene) => scene.id === stringArg(args, "sceneId"));
    if (scene) {
      demoState.config = structuredClone(scene.config);
      return structuredClone(scene);
    }
    return {};
  }

  if (command === "create_mix") {
    const name = String(args?.name ?? "New Mix");
    const id = slug(name);
    const mix: Mix = {
      id,
      name,
      virtual_sink_name: `wavelinux_mix_${id}`,
      virtual_source_name: `wavelinux_mix_${id}_source`,
      monitor_output: null,
      volume: 1,
      muted: false,
    };
    demoState.config.mixes.push(mix);
    for (const channel of demoState.config.channels) {
      channel.mix_buses[id] = { volume: 1, muted: false };
    }
    return mix;
  }

  if (command === "set_mix_volume") {
    const mix = findMix(stringArg(args, "mixId"));
    if (mix) mix.volume = numberArg(args, "volume", mix.volume);
    return mix ?? {};
  }

  if (command === "set_mix_mute") {
    const mix = findMix(stringArg(args, "mixId"));
    if (mix) mix.muted = boolArg(args, "muted", mix.muted);
    return mix ?? {};
  }

  if (command === "set_mix_monitor_output") {
    const mix = findMix(stringArg(args, "mixId"));
    if (mix) {
      const output = args?.output;
      mix.monitor_output = typeof output === "string" && output.length > 0 ? output : null;
    }
    return mix ?? {};
  }

  if (command === "create_channel") {
    const name = String(args?.name ?? "New Channel");
    const id = slug(name);
    const channel: Channel = {
      id,
      name,
      kind:
        args?.kind === "microphone" ||
        args?.kind === "soundboard" ||
        args?.kind === "system" ||
        args?.kind === "generic"
          ? args.kind
          : "application",
      virtual_sink_name: `wavelinux_channel_${id}`,
      source_device: null,
      linked: false,
      mix_buses: Object.fromEntries(
        demoState.config.mixes.map((mix) => [mix.id, { volume: 1, muted: false }]),
      ),
      app_matchers: [],
      effects: [],
    };
    demoState.config.channels.push(channel);
    return channel;
  }

  if (command === "rename_channel") {
    const channel = findChannel(stringArg(args, "channelId"));
    const name = stringArg(args, "name");
    if (channel && name) channel.name = name;
    return channel ?? {};
  }

  if (command === "set_channel_linked") {
    const channel = findChannel(stringArg(args, "channelId"));
    if (channel) channel.linked = boolArg(args, "linked", channel.linked);
    return channel ?? {};
  }

  if (command === "set_channel_input") {
    const channel = findChannel(stringArg(args, "channelId"));
    if (channel) {
      const sourceDevice = stringArg(args, "sourceDevice");
      channel.source_device = sourceDevice || null;
    }
    return channel ?? {};
  }

  if (command === "delete_channel") {
    const channelId = stringArg(args, "channelId");
    const index = demoState.config.channels.findIndex((channel) => channel.id === channelId);
    if (index >= 0 && demoState.config.channels.length > 1) {
      const [removed] = demoState.config.channels.splice(index, 1);
      demoState.config.app_routes = demoState.config.app_routes.filter(
        (route) => route.channel_id !== channelId,
      );
      return removed;
    }
    return {};
  }

  if (command === "set_settings") {
    if (args?.settings && typeof args.settings === "object") {
      demoState.config.settings = args.settings as AppStateSnapshot["config"]["settings"];
    }
    return demoState.config.settings;
  }

  if (command === "cleanup_audio_graph") {
    return [];
  }

  if (command === "run_sound_check" || command === "run_diagnostics") {
    return {
      diagnostics: structuredClone(demoState.diagnostics),
      active_stream_count: demoState.graph.app_streams.length,
      virtual_mix_count: demoState.config.mixes.length,
      missing_effects: demoState.graph.effect_availability
        .filter((effect) => !effect.available)
        .map((effect) => effect.effect_id),
    };
  }

  if (command === "set_channel_volume") {
    const channel = findChannel(stringArg(args, "channelId"));
    const mixId = stringArg(args, "mixId");
    const bus = channel?.mix_buses[mixId];
    if (bus && channel) {
      const volume = numberArg(args, "volume", bus.volume);
      if (channel.linked) {
        for (const linkedBus of Object.values(channel.mix_buses)) linkedBus.volume = volume;
      } else {
        bus.volume = volume;
      }
    }
    return bus ?? {};
  }

  if (command === "set_channel_mute") {
    const channel = findChannel(stringArg(args, "channelId"));
    const mixId = stringArg(args, "mixId");
    const bus = channel?.mix_buses[mixId];
    if (bus) bus.muted = boolArg(args, "muted", bus.muted);
    return bus ?? {};
  }

  if (command === "move_app_stream") {
    const streamId = stringArg(args, "streamId");
    const channelId = stringArg(args, "channelId");
    const stream = demoState.graph.app_streams.find((item) => item.id === streamId);
    if (stream) stream.routed_channel_id = channelId;
    return {};
  }

  if (command === "move_app_stream_to_default") {
    const streamId = stringArg(args, "streamId");
    const stream = demoState.graph.app_streams.find((item) => item.id === streamId);
    if (stream) stream.routed_channel_id = null;
    return {};
  }

  if (command === "assign_app_to_channel") {
    const channelId = stringArg(args, "channelId");
    const matcher = args?.matcher;
    if (channelId && matcher && typeof matcher === "object") {
      demoState.config.app_routes = demoState.config.app_routes.filter(
        (route) => JSON.stringify(route.matcher) !== JSON.stringify(matcher),
      );
      demoState.config.app_routes.push({
        channel_id: channelId,
        matcher: matcher as AppStateSnapshot["config"]["app_routes"][number]["matcher"],
      });
    }
    return demoState.config.app_routes.at(-1) ?? {};
  }

  if (command === "remove_app_route") {
    const matcher = args?.matcher;
    if (matcher && typeof matcher === "object") {
      const before = demoState.config.app_routes.length;
      demoState.config.app_routes = demoState.config.app_routes.filter(
        (route) => JSON.stringify(route.matcher) !== JSON.stringify(matcher),
      );
      if (before !== demoState.config.app_routes.length) {
        return { matcher: matcher as AppMatcher, channel_id: "" };
      }
    }
    return null;
  }

  if (command === "set_app_stream_volume") {
    const streamId = stringArg(args, "streamId");
    const stream = demoState.graph.app_streams.find((item) => item.id === streamId);
    if (stream) stream.volume = numberArg(args, "volume", stream.volume);
    return {};
  }

  if (command === "set_app_stream_mute") {
    const streamId = stringArg(args, "streamId");
    const stream = demoState.graph.app_streams.find((item) => item.id === streamId);
    if (stream) stream.muted = boolArg(args, "muted", stream.muted);
    return {};
  }

  if (command === "set_effect_chain") {
    const channel = findChannel(stringArg(args, "channelId"));
    if (channel && Array.isArray(args?.effects)) {
      channel.effects = args.effects as EffectInstance[];
    }
    return channel ?? {};
  }

  if (command === "set_effect_param") {
    const channel = findChannel(stringArg(args, "channelId"));
    const effect = channel?.effects.find((item) => item.instance_id === stringArg(args, "instanceId"));
    if (effect) effect.params[stringArg(args, "paramId")] = rawNumberArg(args, "value", 0);
    return channel ?? {};
  }

  if (command === "bypass_effect") {
    const channel = findChannel(stringArg(args, "channelId"));
    const effect = channel?.effects.find((item) => item.instance_id === stringArg(args, "instanceId"));
    if (effect) effect.bypassed = boolArg(args, "bypassed", effect.bypassed);
    return channel ?? {};
  }

  return {};
}

function cloneDemoState(): AppStateSnapshot {
  return structuredClone(demoState);
}

function findMix(mixId: string): Mix | undefined {
  return demoState.config.mixes.find((mix) => mix.id === mixId);
}

function findChannel(channelId: string): Channel | undefined {
  return demoState.config.channels.find((channel) => channel.id === channelId);
}

function stringArg(args: Record<string, unknown> | undefined, key: string): string {
  const value = args?.[key];
  return typeof value === "string" ? value : "";
}

function numberArg(args: Record<string, unknown> | undefined, key: string, fallback: number): number {
  const value = args?.[key];
  return typeof value === "number" && Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : fallback;
}

function rawNumberArg(args: Record<string, unknown> | undefined, key: string, fallback: number): number {
  const value = args?.[key];
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function boolArg(args: Record<string, unknown> | undefined, key: string, fallback: boolean): boolean {
  const value = args?.[key];
  return typeof value === "boolean" ? value : fallback;
}

function slug(value: string): string {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

const demoScenes: Scene[] = [];

const catalog: EffectCatalog = {
  preferred_order: [
    "deepfilternet",
    "rnnoise",
    "highpass",
    "eq",
    "compressor",
    "gate",
    "limiter",
  ],
  effects: [
    {
      id: "deepfilternet",
      name: "DeepFilterNet",
      description: "Neural noise suppression",
      plugin_hint: {},
      params: [
        {
          id: "attenuation_limit_db",
          label: "Reduction Limit",
          min: 0,
          max: 100,
          default: 100,
          unit: " dB",
        },
      ],
      presets: [
        { name: "Natural 12 dB", values: { attenuation_limit_db: 12 } },
        { name: "Medium 24 dB", values: { attenuation_limit_db: 24 } },
        { name: "Full 100 dB", values: { attenuation_limit_db: 100 } },
      ],
    },
    {
      id: "rnnoise",
      name: "Noise Suppression",
      description: "RNNoise speech cleanup",
      plugin_hint: {},
      params: [
        { id: "vad_threshold", label: "VAD Threshold", min: 0, max: 100, default: 50, unit: "%" },
        { id: "hold_ms", label: "Hold Open", min: 0, max: 2000, default: 200, unit: " ms" },
        { id: "lead_in_ms", label: "Lead-In", min: 0, max: 500, default: 0, unit: " ms" },
      ],
      presets: [
        { name: "Gentle", values: { vad_threshold: 25, hold_ms: 250, lead_in_ms: 0 } },
        { name: "Broadcast", values: { vad_threshold: 50, hold_ms: 200, lead_in_ms: 0 } },
        { name: "Aggressive", values: { vad_threshold: 75, hold_ms: 150, lead_in_ms: 0 } },
      ],
    },
    {
      id: "highpass",
      name: "High-Pass Filter",
      description: "Rumble removal",
      plugin_hint: {},
      params: [{ id: "frequency_hz", label: "Cutoff", min: 20, max: 500, default: 80, unit: " Hz" }],
      presets: [
        { name: "Voice 80 Hz", values: { frequency_hz: 80 } },
        { name: "Rumble 120 Hz", values: { frequency_hz: 120 } },
        { name: "Music 40 Hz", values: { frequency_hz: 40 } },
      ],
    },
    {
      id: "eq",
      name: "3-Band EQ",
      description: "Tone shaping",
      plugin_hint: {},
      params: [
        { id: "low_freq_hz", label: "Low Freq", min: 40, max: 400, default: 120, unit: " Hz" },
        { id: "low_gain_db", label: "Low Gain", min: -12, max: 12, default: 0, unit: " dB" },
        { id: "mid_freq_hz", label: "Mid Freq", min: 300, max: 4000, default: 1000, unit: " Hz" },
        { id: "mid_gain_db", label: "Mid Gain", min: -12, max: 12, default: 0, unit: " dB" },
        { id: "high_freq_hz", label: "High Freq", min: 2000, max: 12000, default: 6000, unit: " Hz" },
        { id: "high_gain_db", label: "High Gain", min: -12, max: 12, default: 0, unit: " dB" },
      ],
      presets: [
        {
          name: "Flat",
          values: {
            low_freq_hz: 120,
            low_gain_db: 0,
            mid_freq_hz: 1000,
            mid_gain_db: 0,
            high_freq_hz: 6000,
            high_gain_db: 0,
          },
        },
        {
          name: "Broadcast Voice",
          values: {
            low_freq_hz: 120,
            low_gain_db: -2,
            mid_freq_hz: 2500,
            mid_gain_db: 2,
            high_freq_hz: 8000,
            high_gain_db: 1.5,
          },
        },
        {
          name: "Warm Music",
          values: {
            low_freq_hz: 100,
            low_gain_db: 2,
            mid_freq_hz: 800,
            mid_gain_db: -1,
            high_freq_hz: 10000,
            high_gain_db: 2,
          },
        },
      ],
    },
    {
      id: "compressor",
      name: "Compressor",
      description: "Dynamic range control",
      plugin_hint: {},
      params: [
        { id: "threshold_db", label: "Threshold", min: -60, max: 0, default: -20, unit: " dB" },
        { id: "ratio", label: "Ratio", min: 1, max: 20, default: 4, unit: ":1" },
        { id: "attack_ms", label: "Attack", min: 0.1, max: 200, default: 5, unit: " ms" },
        { id: "release_ms", label: "Release", min: 5, max: 1000, default: 100, unit: " ms" },
        { id: "makeup_gain_db", label: "Makeup", min: 0, max: 24, default: 0, unit: " dB" },
      ],
      presets: [
        {
          name: "Gentle 2:1",
          values: { threshold_db: -20, ratio: 2, attack_ms: 10, release_ms: 120, makeup_gain_db: 2 },
        },
        {
          name: "Broadcast 4:1",
          values: { threshold_db: -18, ratio: 4, attack_ms: 5, release_ms: 100, makeup_gain_db: 3 },
        },
        {
          name: "Streaming 6:1",
          values: { threshold_db: -16, ratio: 6, attack_ms: 3, release_ms: 80, makeup_gain_db: 4 },
        },
      ],
    },
    {
      id: "gate",
      name: "Noise Gate",
      description: "Quiet-signal attenuation",
      plugin_hint: {},
      params: [
        { id: "threshold_db", label: "Threshold", min: -80, max: 0, default: -40, unit: " dB" },
        { id: "attack_ms", label: "Attack", min: 0.1, max: 100, default: 2.5, unit: " ms" },
        { id: "hold_ms", label: "Hold", min: 0, max: 500, default: 10, unit: " ms" },
        { id: "release_ms", label: "Release", min: 10, max: 2000, default: 200, unit: " ms" },
        { id: "range_db", label: "Range", min: -80, max: 0, default: -40, unit: " dB" },
      ],
      presets: [
        {
          name: "Soft -60 dB",
          values: { threshold_db: -60, range_db: -20, attack_ms: 5, hold_ms: 20, release_ms: 200 },
        },
        {
          name: "Room mic -40 dB",
          values: { threshold_db: -40, range_db: -40, attack_ms: 2.5, hold_ms: 10, release_ms: 120 },
        },
        {
          name: "Noisy mic -30 dB",
          values: { threshold_db: -30, range_db: -50, attack_ms: 1, hold_ms: 10, release_ms: 80 },
        },
      ],
    },
    {
      id: "limiter",
      name: "Limiter",
      description: "Peak ceiling",
      plugin_hint: {},
      params: [
        { id: "input_gain_db", label: "Input Gain", min: -20, max: 20, default: 0, unit: " dB" },
        { id: "ceiling_db", label: "Ceiling", min: -20, max: 0, default: -1, unit: " dB" },
      ],
      presets: [
        { name: "Gentle -3 dB", values: { input_gain_db: 0, ceiling_db: -3 } },
        { name: "Broadcast -1 dB", values: { input_gain_db: 0, ceiling_db: -1 } },
        { name: "Loud -0.5 dB", values: { input_gain_db: 3, ceiling_db: -0.5 } },
      ],
    },
  ],
};

export const demoState: AppStateSnapshot = {
  config: {
    version: 1,
    audio: {
      sample_rate_hz: 48000,
      bit_depth: 24,
      channel_layout: "stereo",
      mono_inputs_to_stereo: true,
    },
    settings: {
      theme: "system",
      start_at_login: false,
      lock_default_input: false,
      lock_default_output: false,
      auto_check_updates: true,
      auto_install_updates: false,
      release_channel: "stable",
    },
    mixes: [
      {
        id: "monitor",
        name: "Monitor",
        virtual_sink_name: "wavelinux_mix_monitor",
        virtual_source_name: "wavelinux_mix_monitor_source",
        monitor_output: "alsa_output.usb",
        volume: 0.86,
        muted: false,
      },
      {
        id: "stream",
        name: "Stream",
        virtual_sink_name: "wavelinux_mix_stream",
        virtual_source_name: "wavelinux_mix_stream_source",
        monitor_output: null,
        volume: 1,
        muted: false,
      },
      {
        id: "discord_mix",
        name: "Discord Mix",
        virtual_sink_name: "wavelinux_mix_discord",
        virtual_source_name: "wavelinux_mix_discord_source",
        monitor_output: null,
        volume: 1,
        muted: false,
      },
    ],
    channels: ["Mic", "Game", "Chat", "Music", "Browser", "SFX"].map((name, index) => {
      const id = slug(name);
      return {
        id,
        name,
        kind: index === 0 ? "microphone" : index === 5 ? "soundboard" : "application",
        virtual_sink_name: `wavelinux_channel_${id}`,
        source_device: index === 0 ? "alsa_input.usb_mic" : null,
        linked: false,
        mix_buses: {
          monitor: { volume: index === 0 ? 0.82 : 0.76, muted: false },
          stream: { volume: index === 2 ? 0.52 : 0.84, muted: false },
          discord_mix: { volume: index === 1 ? 0.2 : 0.75, muted: index === 3 },
        },
        app_matchers: [],
        effects:
          index === 0
            ? [
                { instance_id: "demo-rnnoise", effect_id: "rnnoise", bypassed: false, params: {} },
                { instance_id: "demo-limiter", effect_id: "limiter", bypassed: false, params: {} },
              ]
            : [],
      } satisfies Channel;
    }),
    app_routes: [],
  },
  graph: {
    inputs: [
      { id: "alsa_input.usb_mic", name: "alsa_input.usb_mic", description: "USB Microphone", is_default: true, is_virtual: false },
    ],
    outputs: [
      { id: "alsa_output.usb", name: "alsa_output.usb", description: "USB Headphones", is_default: true, is_virtual: false },
      { id: "bluez_output.headset", name: "bluez_output.headset", description: "Bluetooth Headset", is_default: false, is_virtual: false },
    ],
    app_streams: [
      { id: "42", app_id: "firefox", process_name: "firefox", display_name: "Firefox", media_name: "YouTube", routed_channel_id: "browser", volume: 0.76, muted: false },
      { id: "77", app_id: "discord", process_name: "Discord", display_name: "Discord", media_name: "Voice", routed_channel_id: "chat", volume: 0.86, muted: false },
      { id: "81", app_id: "spotify", process_name: "spotify", display_name: "Spotify", media_name: "Playback", routed_channel_id: "music", volume: 0.66, muted: false },
    ],
    meters: [],
    effect_availability: catalog.effects.map((effect) => ({
      effect_id: effect.id,
      available: effect.id !== "gate",
      detail:
        effect.id === "deepfilternet"
          ? "/usr/lib/ladspa/libdeep_filter_ladspa.so"
          : "demo availability",
    })),
  },
  diagnostics: [
    { code: "host_command.pipewire", severity: "info", message: "pipewire is available", action: null },
    { code: "plugin.deepfilternet", severity: "info", message: "DeepFilterNet LADSPA detected", action: null },
  ],
  engine: { dry_run: true, healthy: true, message: "Demo mode", last_refresh_unix: 0 },
  catalog,
};
