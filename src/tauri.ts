import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { DEFAULT_UI_THEME_ID, loadStoredThemeId, saveStoredThemeId } from "./themes";
import type {
  AppMatcher,
  AppStateSnapshot,
  Channel,
  EffectCatalog,
  EffectInstance,
  FallbackHardwareProfile,
  GraphDebugReport,
  HardwareProfileSummary,
  HardwareProfileUiState,
  Mix,
  MixBus,
} from "./types";

const isTauri =
  typeof window !== "undefined" &&
  ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);

const singleInstanceEffectIds = new Set([
  "deepfilternet",
  "rnnoise",
  "highpass",
  "eq",
  "compressor",
  "gate",
  "limiter",
]);

export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (isTauri) {
    return tauriInvoke<T>(command, args);
  }

  if (command === "get_state" || command === "observe_state") {
    return cloneDemoState() as T;
  }
  if (command === "observe_meters") {
    return structuredClone(demoState.graph.meters) as T;
  }

  console.info(`[demo] ${command}`, args ?? {});
  return demoMutation(command, args) as T;
}

export function initialSnapshot(): AppStateSnapshot | null {
  return isTauri ? null : cloneDemoState();
}

function demoMutation(command: string, args?: Record<string, unknown>): unknown {
  if (command === "get_ui_theme_preference") {
    return { theme_id: loadStoredThemeId() };
  }

  if (command === "set_ui_theme_preference") {
    const themeId = stringArg(args, "themeId") || stringArg(args, "theme_id") || DEFAULT_UI_THEME_ID;
    saveStoredThemeId(themeId);
    return { theme_id: themeId };
  }

  if (command === "list_ui_themes") {
    return [];
  }

  if (command === "open_ui_theme_folder") {
    return {};
  }

  if (command === "list_hardware_profiles") {
    return demoHardwareProfileState();
  }

  if (command === "set_device_hardware_profile") {
    const deviceId = stringArg(args, "deviceId") || stringArg(args, "device_id");
    const profileId = stringArg(args, "profileId") || stringArg(args, "profile_id");
    if (deviceId) {
      if (profileId) {
        demoState.config.device_policy.hardware_profile_assignments[deviceId] = profileId;
      } else {
        delete demoState.config.device_policy.hardware_profile_assignments[deviceId];
      }
      applyDemoHardwareProfiles();
    }
    return demoHardwareProfileState();
  }

  if (command === "set_fallback_hardware_profile") {
    const fallbackProfile =
      (args?.fallbackProfile as FallbackHardwareProfile | undefined) ??
      (args?.fallback_profile as FallbackHardwareProfile | undefined);
    if (fallbackProfile) {
      demoState.config.device_policy.fallback_hardware_profile = structuredClone(fallbackProfile);
      applyDemoHardwareProfiles();
    }
    return demoHardwareProfileState();
  }

  if (command === "set_hardware_profile_policy") {
    const profileId = stringArg(args, "profileId") || stringArg(args, "profile_id");
    const name = stringArg(args, "name");
    const latencyPolicy = (args?.latencyPolicy ?? args?.latency_policy) as HardwareProfileSummary["latency_policy"] | undefined;
    const routingPolicy = (args?.routingPolicy ?? args?.routing_policy) as HardwareProfileSummary["routing_policy"] | undefined;
    if (profileId === demoState.config.device_policy.fallback_hardware_profile.id) {
      demoState.config.device_policy.fallback_hardware_profile = {
        ...demoState.config.device_policy.fallback_hardware_profile,
        name: name || demoState.config.device_policy.fallback_hardware_profile.name,
        latency_policy: latencyPolicy ?? demoState.config.device_policy.fallback_hardware_profile.latency_policy,
        routing_policy: routingPolicy ?? demoState.config.device_policy.fallback_hardware_profile.routing_policy,
      };
    } else {
      demoHardwareProfiles = demoHardwareProfiles.map((profile) =>
        profile.id === profileId
          ? {
              ...profile,
              name: name || profile.name,
              source: "local",
              latency_policy: latencyPolicy ?? profile.latency_policy,
              routing_policy: routingPolicy ?? profile.routing_policy,
            }
          : profile,
      );
    }
    applyDemoHardwareProfiles();
    return demoHardwareProfileState();
  }

  if (command === "list_elgato_devices") {
    return [];
  }

  if (command === "list_streamer_devices") {
    return [];
  }

  if (command === "get_streamer_bindings") {
    return structuredClone(demoState.config.streamer_devices);
  }

  if (command === "set_streamer_device_enabled") {
    const deviceId = stringArg(args, "deviceId") || stringArg(args, "device_id");
    const enabled = boolArg(args, "enabled", true);
    if (deviceId) {
      demoState.config.streamer_devices.profiles[deviceId] = {
        device_id: deviceId,
        family: null,
        name: "Streamer Device",
        enabled,
        safe_preset: false,
        bindings: demoState.config.streamer_devices.profiles[deviceId]?.bindings ?? [],
      };
    }
    return structuredClone(demoState.config.streamer_devices);
  }

  if (command === "set_streamer_binding_profile") {
    const profile = args?.profile;
    if (profile && typeof profile === "object" && "device_id" in profile) {
      const next = structuredClone(profile) as typeof demoState.config.streamer_devices.profiles[string];
      demoState.config.streamer_devices.profiles[next.device_id] = next;
      return next;
    }
    return {};
  }

  if (command === "learn_streamer_control") {
    return {
      device_id: stringArg(args, "deviceId") || stringArg(args, "device_id"),
      control_id: null,
      control_kind: "unknown",
      message: "No streamer device is connected in demo mode",
    };
  }

  if (command === "run_streamer_action_test") {
    return { performed: false, message: "No streamer device is connected in demo mode" };
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
      output_devices: [],
      icon: null,
      volume: 1,
      muted: false,
    };
    demoState.config.mixes.push(mix);
    for (const channel of demoState.config.channels) {
      channel.mix_buses[id] = { volume: 1, muted: false, enabled: false };
    }
    return mix;
  }

  if (command === "rename_mix") {
    const mix = findMix(stringArg(args, "mixId"));
    const name = stringArg(args, "name");
    if (mix && name) mix.name = name;
    return mix ?? {};
  }

  if (command === "move_mix") {
    const mixId = stringArg(args, "mixId");
    const direction = rawNumberArg(args, "direction", 0);
    const index = demoState.config.mixes.findIndex((mix) => mix.id === mixId);
    const target = offsetIndex(index, direction, demoState.config.mixes.length);
    if (index >= 0 && target !== index) {
      const [mix] = demoState.config.mixes.splice(index, 1);
      demoState.config.mixes.splice(target, 0, mix);
      return mix;
    }
    return findMix(mixId) ?? {};
  }

  if (command === "delete_mix") {
    const mixId = stringArg(args, "mixId");
    const index = demoState.config.mixes.findIndex((mix) => mix.id === mixId);
    if (index >= 0 && demoState.config.mixes.length > 1) {
      const [removed] = demoState.config.mixes.splice(index, 1);
      for (const channel of demoState.config.channels) {
        delete channel.mix_buses[mixId];
      }
      return removed;
    }
    return {};
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

  if (command === "set_mix_icon") {
    const mix = findMix(stringArg(args, "mixId"));
    if (mix) {
      const icon = stringArg(args, "icon");
      mix.icon = icon || null;
    }
    return mix ?? {};
  }

  if (command === "set_mix_monitor_output") {
    const mix = findMix(stringArg(args, "mixId"));
    if (mix) {
      const output = args?.output;
      mix.monitor_output = typeof output === "string" && output.length > 0 ? output : null;
      mix.output_devices = mix.monitor_output ? [mix.monitor_output] : [];
    }
    return mix ?? {};
  }

  if (command === "set_mix_outputs") {
    const mix = findMix(stringArg(args, "mixId"));
    if (mix) {
      const outputs = Array.isArray(args?.outputs)
        ? args.outputs.filter((output): output is string => typeof output === "string" && output.trim().length > 0)
        : [];
      mix.output_devices = Array.from(new Set(outputs.map((output) => output.trim())));
      mix.monitor_output = mix.output_devices[0] ?? null;
    }
    return mix ?? {};
  }

  if (command === "create_channel") {
    const name = String(args?.name ?? "New Channel");
    const id = slug(name);
    const kind =
      args?.kind === "microphone" ||
      args?.kind === "soundboard" ||
      args?.kind === "system" ||
      args?.kind === "generic"
        ? args.kind
        : "application";
    const channel: Channel = {
      id,
      name,
      kind,
      virtual_sink_name: `wavelinux_channel_${id}`,
      source_device: null,
      icon: null,
      input_mode: kind === "generic" || kind === "microphone" ? "sum_mono" : "stereo",
      linked: false,
      mix_buses: Object.fromEntries(
        demoState.config.mixes.map((mix) => [
          mix.id,
          {
            volume: 1,
            muted:
              mix.id === "monitor" &&
              (args?.kind === "generic" || args?.kind === "microphone"),
            enabled: mix.id !== "discord_mix",
          },
        ]),
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

  if (command === "set_channel_icon") {
    const channel = findChannel(stringArg(args, "channelId"));
    if (channel) {
      const icon = stringArg(args, "icon");
      channel.icon = icon || null;
    }
    return channel ?? {};
  }

  if (command === "move_channel") {
    const channelId = stringArg(args, "channelId");
    const direction = rawNumberArg(args, "direction", 0);
    const index = demoState.config.channels.findIndex((channel) => channel.id === channelId);
    const target = offsetIndex(index, direction, demoState.config.channels.length);
    if (index >= 0 && target !== index) {
      const [channel] = demoState.config.channels.splice(index, 1);
      demoState.config.channels.splice(target, 0, channel);
      return channel;
    }
    return findChannel(channelId) ?? {};
  }

  if (command === "set_channel_linked") {
    const channel = findChannel(stringArg(args, "channelId"));
    if (channel) channel.linked = boolArg(args, "linked", channel.linked);
    return channel ?? {};
  }

  if (command === "set_channel_input" || command === "set_hardware_input_device") {
    const channel = findChannel(stringArg(args, "channelId"));
    if (channel) {
      const sourceDevice = stringArg(args, "sourceDevice");
      channel.source_device = sourceDevice || null;
      if (channel.id === "hardware_in") {
        demoState.config.device_policy.preferred_input = channel.source_device;
      }
    }
    return channel ?? {};
  }

  if (command === "set_channel_input_mode") {
    const channel = findChannel(stringArg(args, "channelId"));
    if (channel && (channel.kind === "generic" || channel.kind === "microphone")) {
      channel.input_mode = "sum_mono";
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
      demoState.config.settings.keep_running_in_tray = true;
    }
    return demoState.config.settings;
  }

  if (command === "check_for_updates") {
    const releaseChannel =
      stringArg(args, "releaseChannel") ||
      stringArg(args, "release_channel") ||
      demoState.config.settings.release_channel;
    return {
      available: false,
      install_supported: false,
      current_version: "4.2.1",
      version: null,
      date: null,
      body: null,
      url: null,
      release_url: "https://github.com/DuskyProjects/WaveLinux/releases",
      channel: releaseChannel,
      endpoint:
        releaseChannel === "beta"
          ? "https://github.com/DuskyProjects/WaveLinux/releases/download/prerelease/latest.json"
          : "https://github.com/DuskyProjects/WaveLinux/releases/latest/download/latest.json",
      message: "WaveLinux is up to date",
    };
  }

  if (command === "install_update") {
    return {
      installed: false,
      version: null,
      message: "Self-update is available for AppImage installs",
    };
  }

  if (command === "install_effect_plugins") {
    for (const effect of demoState.graph.effect_availability) {
      effect.available = true;
      if (effect.effect_id === "deepfilternet") {
        effect.detail = "/usr/lib/ladspa/libdeep_filter_ladspa.so (DeepFilterNet3 model)";
      }
    }
    return {
      attempted: true,
      success: true,
      manager: "demo",
      packages: [],
      aur_packages: [],
      missing_before: [],
      missing_after: [],
      stdout: "",
      stderr: "",
      message: "Effect plugins installed and detected",
    };
  }

  if (command === "open_release_page") {
    return null;
  }

  if (command === "repair_audio_graph") {
    demoState.engine.audio_graph_running = true;
    demoState.engine.message = "Audio graph running";
    return { dry_run: true, planned: { commands: [], managed_nodes: [] }, outputs: [] };
  }

  if (command === "cleanup_audio_graph") {
    demoState.engine.audio_graph_running = false;
    demoState.engine.message = "Audio graph stopped";
    return [];
  }

  if (command === "cleanup_stale_audio_graph") {
    return [];
  }

  if (command === "restore_device") {
    const kind = stringArg(args, "kind");
    if (kind === "input") {
      demoState.config.channels
        .filter((channel) => channel.kind === "generic")
        .forEach((channel) => {
          channel.source_device = demoState.config.device_policy.restorable_input ?? "alsa_input.usb_interface";
        });
      demoState.config.device_policy.active_input_fallback = false;
    }
    if (kind === "output") {
      const monitor = findMix("monitor");
      if (monitor) {
        monitor.monitor_output = demoState.config.device_policy.restorable_output ?? "alsa_output.usb";
        monitor.output_devices = monitor.monitor_output ? [monitor.monitor_output] : [];
      }
      demoState.config.device_policy.active_output_fallback = false;
    }
    return demoState.config;
  }

  if (command === "run_sound_check" || command === "run_diagnostics") {
    return {
      diagnostics: structuredClone(demoState.diagnostics),
      active_stream_count: demoState.graph.app_streams.length,
      virtual_mix_count: demoState.config.mixes.length,
      missing_effects: demoState.graph.effect_availability
        .filter((effect) => !effect.available)
        .map((effect) => effect.effect_id),
      debug_log_path: "/home/dusky/.config/wavelinux/wavelinux-engine.log",
      recent_log_lines: ["demo diagnostics run", "demo mode has no live engine log"],
    };
  }

  if (command === "get_graph_debug_report") {
    return demoGraphDebugReport();
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

  if (command === "set_channel_bus_enabled") {
    const channel = findChannel(stringArg(args, "channelId"));
    const mixId = stringArg(args, "mixId");
    const bus = channel?.mix_buses[mixId];
    if (bus) bus.enabled = boolArg(args, "enabled", bus.enabled);
    return bus ?? ({ volume: 1, muted: false, enabled: false } satisfies MixBus);
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
    const matcher = args?.matcher as AppMatcher | undefined;
    if (channelId && matcher && typeof matcher === "object") {
      const resolved = resolveDemoMatcher(matcher);
      demoState.config.app_routes = demoState.config.app_routes.filter(
        (route) => matcherKey(route.matcher) !== matcherKey(resolved),
      );
      demoState.config.app_routes.push({
        channel_id: channelId,
        matcher: resolved,
      });
    }
    return demoState.config.app_routes.at(-1) ?? {};
  }

  if (command === "remove_app_route") {
    const matcher = args?.matcher as AppMatcher | undefined;
    if (matcher && typeof matcher === "object") {
      const key = matcherKey(resolveDemoMatcher(matcher));
      const before = demoState.config.app_routes.length;
      demoState.config.app_routes = demoState.config.app_routes.filter(
        (route) => matcherKey(route.matcher) !== key && matcherKey(route.matcher) !== matcherKey(matcher),
      );
      if (before !== demoState.config.app_routes.length) {
        return { matcher, channel_id: "" };
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

  if (command === "set_app_volume_preset") {
    const matcher = args?.matcher as AppMatcher | undefined;
    if (matcher && typeof matcher === "object") {
      const resolved = resolveDemoMatcher(matcher);
      const key = matcherKey(resolved);
      demoState.config.app_volume_presets = demoState.config.app_volume_presets.filter(
        (preset) => matcherKey(preset.matcher) !== key,
      );
      demoState.config.app_volume_presets.push({
        matcher: resolved,
        volume: numberArg(args, "volume", 1),
      });
      return demoState.config.app_volume_presets.at(-1) ?? {};
    }
    return {};
  }

  if (command === "remove_app_volume_preset") {
    const matcher = args?.matcher as AppMatcher | undefined;
    if (matcher && typeof matcher === "object") {
      const key = matcherKey(resolveDemoMatcher(matcher));
      const before = demoState.config.app_volume_presets.length;
      demoState.config.app_volume_presets = demoState.config.app_volume_presets.filter(
        (preset) => matcherKey(preset.matcher) !== key && matcherKey(preset.matcher) !== matcherKey(matcher),
      );
      if (before !== demoState.config.app_volume_presets.length) {
        return { matcher, volume: 1 };
      }
    }
    return null;
  }

  if (command === "forget_app") {
    const matcher = args?.matcher as AppMatcher | undefined;
    if (matcher && typeof matcher === "object") {
      const key = matcherKey(resolveDemoMatcher(matcher));
      const rawKey = matcherKey(matcher);
      demoState.config.app_routes = demoState.config.app_routes.filter(
        (route) => matcherKey(route.matcher) !== key && matcherKey(route.matcher) !== rawKey,
      );
      demoState.config.app_volume_presets = demoState.config.app_volume_presets.filter(
        (preset) => matcherKey(preset.matcher) !== key && matcherKey(preset.matcher) !== rawKey,
      );
      const app = demoState.config.app_history.find((item) => matcherKey(item.matcher) === key || matcherKey(item.matcher) === rawKey);
      if (app) {
        app.forgotten = true;
        return app;
      }
    }
    return null;
  }

  if (command === "restore_app") {
    const matcher = args?.matcher as AppMatcher | undefined;
    if (matcher && typeof matcher === "object") {
      const key = matcherKey(resolveDemoMatcher(matcher));
      const rawKey = matcherKey(matcher);
      const app = demoState.config.app_history.find((item) => matcherKey(item.matcher) === key || matcherKey(item.matcher) === rawKey);
      if (app) {
        app.forgotten = false;
        return app;
      }
    }
    return null;
  }

  if (command === "pin_app_identity") {
    const matcher = args?.matcher as AppMatcher | undefined;
    const label = stringArg(args, "label").trim();
    if (matcher && typeof matcher === "object" && label) {
      const resolved = resolveDemoMatcher(matcher);
      demoState.config.app_label_overrides = demoState.config.app_label_overrides.filter(
        (item) => matcherKey(item.matcher) !== matcherKey(resolved),
      );
      demoState.config.app_label_overrides.push({ matcher: resolved, label });
      const app = upsertDemoKnownApp(resolved, label);
      return structuredClone(app);
    }
    return {};
  }

  if (command === "merge_app_identity") {
    const source = args?.source as AppMatcher | undefined;
    const target = args?.target as AppMatcher | undefined;
    if (source && target && typeof source === "object" && typeof target === "object") {
      const resolvedTarget = resolveDemoMatcher(target);
      demoState.config.app_identity_overrides = demoState.config.app_identity_overrides.filter(
        (item) => matcherKey(item.source) !== matcherKey(source),
      );
      demoState.config.app_identity_overrides.push({ source, target: resolvedTarget });
      for (const route of demoState.config.app_routes) {
        if (matcherKey(route.matcher) === matcherKey(source)) route.matcher = resolvedTarget;
      }
      for (const preset of demoState.config.app_volume_presets) {
        if (matcherKey(preset.matcher) === matcherKey(source)) preset.matcher = resolvedTarget;
      }
      const sourceApp = demoState.config.app_history.find((app) => matcherKey(app.matcher) === matcherKey(source));
      if (sourceApp) sourceApp.forgotten = true;
      const targetApp = upsertDemoKnownApp(
        resolvedTarget,
        findDemoKnownApp(resolvedTarget)?.display_name ?? matcherDisplayName(resolvedTarget),
      );
      return structuredClone(targetApp);
    }
    return {};
  }

  if (command === "reset_app_identity") {
    const matcher = args?.matcher as AppMatcher | undefined;
    if (matcher && typeof matcher === "object") {
      const key = matcherKey(matcher);
      demoState.config.app_identity_overrides = demoState.config.app_identity_overrides.filter(
        (item) => matcherKey(item.source) !== key && matcherKey(item.target) !== key,
      );
      demoState.config.app_label_overrides = demoState.config.app_label_overrides.filter(
        (item) => matcherKey(item.matcher) !== key,
      );
      const app = findDemoKnownApp(matcher);
      if (app) {
        app.display_name = matcherDisplayName(app.matcher);
        app.forgotten = false;
        return structuredClone(app);
      }
    }
    return null;
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
      channel.effects = normalizeDemoEffectChain(
        (args.effects as EffectInstance[])
          .map(normalizeDemoEffect)
          .filter((effect): effect is EffectInstance => Boolean(effect)),
      );
    }
    return channel ?? {};
  }

  if (command === "set_effect_param") {
    const channel = findChannel(stringArg(args, "channelId"));
    const effect = channel?.effects.find((item) => item.instance_id === stringArg(args, "instanceId"));
    const paramId = stringArg(args, "paramId");
    const definition = catalog.effects.find((item) => item.id === effect?.effect_id);
    const param = definition?.params.find((item) => item.id === paramId);
    if (effect && param) {
      effect.params[paramId] = clampNumber(rawNumberArg(args, "value", param.default), param.min, param.max, param.default);
    }
    return channel ?? {};
  }

  if (command === "bypass_effect") {
    const channel = findChannel(stringArg(args, "channelId"));
    const effect = channel?.effects.find((item) => item.instance_id === stringArg(args, "instanceId"));
    if (effect) effect.bypassed = boolArg(args, "bypassed", effect.bypassed);
    if (channel && effect && !effect.bypassed) {
      channel.effects = normalizeDemoEffectChain(channel.effects, effect.instance_id);
    }
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

function matcherKey(matcher: AppMatcher): string {
  return [
    matcher.app_id ? `app:${matcher.app_id.toLowerCase()}` : "",
    matcher.process_name ? `process:${matcher.process_name.toLowerCase()}` : "",
    matcher.binary ? `binary:${matcher.binary.toLowerCase()}` : "",
    matcher.window_class ? `class:${matcher.window_class.toLowerCase()}` : "",
    matcher.media_name ? `media:${matcher.media_name.toLowerCase()}` : "",
  ]
    .filter(Boolean)
    .join("|");
}

function matcherDisplayName(matcher: AppMatcher): string {
  return matcher.app_id ?? matcher.process_name ?? matcher.binary ?? matcher.window_class ?? matcher.media_name ?? "Unknown app";
}

function findDemoKnownApp(matcher: AppMatcher) {
  const key = matcherKey(matcher);
  return demoState.config.app_history.find((app) => matcherKey(app.matcher) === key);
}

function resolveDemoMatcher(matcher: AppMatcher): AppMatcher {
  return demoState.config.app_identity_overrides.find((item) => matcherKey(item.source) === matcherKey(matcher))?.target ?? matcher;
}

function upsertDemoKnownApp(matcher: AppMatcher, displayName: string) {
  const existing = findDemoKnownApp(matcher);
  if (existing) {
    existing.display_name = displayName;
    existing.forgotten = false;
    return existing;
  }
  const app = {
    matcher,
    display_name: displayName,
    media_name: null,
    last_seen_unix: Math.floor(Date.now() / 1000),
    forgotten: false,
  };
  demoState.config.app_history.push(app);
  return app;
}

function demoGraphDebugReport(): GraphDebugReport {
  return {
    dry_run: true,
    audio_graph_running: demoState.engine.audio_graph_running,
    planned: {
      commands: demoState.engine.audio_graph_running
        ? []
        : [
            {
              domain: "device",
              program: "pactl",
              args: ["load-module", "module-null-sink", "sink_name=wavelinux_mix_stream"],
              description: "create stream mix sink",
            },
          ],
      managed_nodes: demoState.config.mixes.map((mix) => mix.virtual_sink_name),
    },
    managed_modules: demoState.engine.audio_graph_running
      ? demoState.config.mixes.map((mix, index) => ({
          module_id: String(100 + index),
          role: "mix",
          channel_id: null,
          mix_id: mix.id,
          node_name: mix.virtual_sink_name,
          source_name: mix.virtual_source_name,
          sink_name: mix.virtual_sink_name,
        }))
      : [],
    sink_input_routes: [],
    source_output_routes: [],
    stale_processes: [],
    graph: structuredClone(demoState.graph),
    diagnostics: structuredClone(demoState.diagnostics),
    debug_log_path: "/home/dusky/.config/wavelinux/wavelinux-engine.log",
    recent_log_lines: ["demo graph report", "no live PipeWire graph in browser mode"],
  };
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

function offsetIndex(index: number, direction: number, length: number): number {
  if (index < 0 || length <= 0) return index;
  return Math.max(0, Math.min(length - 1, index + Math.trunc(direction)));
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

function defaultDemoFallbackProfile(): FallbackHardwareProfile {
  return {
    id: "default.generic-audio",
    name: "Default Generic Audio",
    latency_policy: {
      stable_msec: 80,
      low_latency_msec: 60,
      bluetooth_floor_msec: 240,
    },
    routing_policy: {
      input_priority: 35,
      output_priority: 30,
      allow_auto_select_input: true,
      allow_auto_select_output: true,
      prefer_non_bluetooth_input: true,
    },
    bluetooth_mic_policy: "never_if_hfp",
    confidence: "low",
  };
}

const safeLatencyPolicy = {
  stable_msec: 80,
  low_latency_msec: 60,
  bluetooth_floor_msec: 240,
};

let demoHardwareProfiles = [
  demoHardwareProfile("jds-labs.element-ii", "JDS Labs Element II", "local", "high", {
    input_priority: 0,
    output_priority: 78,
    allow_auto_select_input: false,
    allow_auto_select_output: true,
    prefer_non_bluetooth_input: true,
  }),
  demoHardwareProfile("sony.wh-1000xm4", "Sony WH-1000XM4", "local", "high", {
    input_priority: 10,
    output_priority: 70,
    allow_auto_select_input: false,
    allow_auto_select_output: true,
    prefer_non_bluetooth_input: true,
  }),
  demoHardwareProfile("dji.wireless-mic-rx", "DJI Wireless Mic Receiver", "local", "high", {
    input_priority: 88,
    output_priority: 0,
    allow_auto_select_input: true,
    allow_auto_select_output: false,
    prefer_non_bluetooth_input: true,
  }),
] satisfies HardwareProfileSummary[];

function demoHardwareProfileState(): HardwareProfileUiState {
  return {
    profiles: [demoHardwareProfileFromFallback(), ...structuredClone(demoHardwareProfiles)],
    assignments: structuredClone(demoState.config.device_policy.hardware_profile_assignments),
    fallback_profile: structuredClone(demoState.config.device_policy.fallback_hardware_profile),
  };
}

function demoHardwareProfile(
  id: string,
  name: string,
  source: string,
  confidence: HardwareProfileSummary["confidence"],
  routing_policy: HardwareProfileSummary["routing_policy"],
): HardwareProfileSummary {
  return {
    id,
    name,
    source,
    confidence,
    latency_policy: structuredClone(safeLatencyPolicy),
    routing_policy,
    bluetooth_mic_policy: "never_if_hfp",
  };
}

function demoHardwareProfileFromFallback(): HardwareProfileSummary {
  const fallback = demoState.config.device_policy.fallback_hardware_profile;
  return {
    id: fallback.id,
    name: fallback.name,
    source: "default",
    confidence: fallback.confidence,
    latency_policy: structuredClone(fallback.latency_policy),
    routing_policy: structuredClone(fallback.routing_policy),
    bluetooth_mic_policy: fallback.bluetooth_mic_policy,
  };
}

function applyDemoHardwareProfiles() {
  for (const device of [...demoState.graph.inputs, ...demoState.graph.outputs]) {
    const assignedProfileId = demoState.config.device_policy.hardware_profile_assignments[device.id];
    const autoProfileId = device.id.includes("bluez")
      ? "sony.wh-1000xm4"
      : device.id.includes("usb_interface") || device.id.includes("usb")
        ? "jds-labs.element-ii"
        : "";
    const profileId = assignedProfileId || autoProfileId || demoState.config.device_policy.fallback_hardware_profile.id;
    const profile =
      profileId === demoState.config.device_policy.fallback_hardware_profile.id
        ? demoHardwareProfileFromFallback()
        : demoHardwareProfiles.find((item) => item.id === profileId) ?? demoHardwareProfileFromFallback();
    device.matched_profile_id = profile.id;
    device.matched_profile_source = assignedProfileId ? `assigned:${profile.source}` : profile.source;
    device.profile_confidence = profile.confidence;
    device.active_latency_policy = structuredClone(profile.latency_policy);
    device.active_routing_policy = structuredClone(profile.routing_policy);
    device.active_bluetooth_mic_policy = profile.bluetooth_mic_policy;
  }
}

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
      name: "DeepFilterNet3",
      description: "DeepFilterNet3 neural noise suppression",
      plugin_hint: {},
      params: [
        {
          id: "input_trim_db",
          label: "Input Trim",
          min: -24,
          max: 0,
          default: -6,
          unit: " dB",
        },
        {
          id: "output_makeup_db",
          label: "Output Makeup",
          min: 0,
          max: 18,
          default: 6,
          unit: " dB",
        },
        {
          id: "attenuation_limit_db",
          label: "Reduction Limit",
          min: 0,
          max: 100,
          default: 18,
          unit: " dB",
        },
        {
          id: "min_processing_threshold_db",
          label: "Min Threshold",
          min: -15,
          max: 35,
          default: -15,
          unit: " dB",
        },
        {
          id: "max_erb_processing_threshold_db",
          label: "Max ERB Threshold",
          min: -15,
          max: 35,
          default: 30,
          unit: " dB",
        },
        {
          id: "max_df_processing_threshold_db",
          label: "Max DF Threshold",
          min: -15,
          max: 35,
          default: 20,
          unit: " dB",
        },
        {
          id: "min_processing_buffer_frames",
          label: "Min Buffer",
          min: 0,
          max: 10,
          default: 8,
          unit: " frames",
        },
        {
          id: "post_filter_beta",
          label: "Post Filter Beta",
          min: 0,
          max: 0.05,
          default: 0,
          unit: "",
        },
      ],
      presets: [
        {
          name: "Balanced Voice",
          values: {
            input_trim_db: -6,
            output_makeup_db: 6,
            attenuation_limit_db: 18,
            min_processing_threshold_db: -15,
            max_erb_processing_threshold_db: 30,
            max_df_processing_threshold_db: 20,
            min_processing_buffer_frames: 8,
            post_filter_beta: 0,
          },
        },
        {
          name: "Natural Voice",
          values: {
            input_trim_db: -3,
            output_makeup_db: 3,
            attenuation_limit_db: 12,
            min_processing_threshold_db: -15,
            max_erb_processing_threshold_db: 30,
            max_df_processing_threshold_db: 10,
            min_processing_buffer_frames: 6,
            post_filter_beta: 0,
          },
        },
        {
          name: "Noisy Room",
          values: {
            input_trim_db: -6,
            output_makeup_db: 6,
            attenuation_limit_db: 70,
            min_processing_threshold_db: -15,
            max_erb_processing_threshold_db: 30,
            max_df_processing_threshold_db: 20,
            min_processing_buffer_frames: 8,
            post_filter_beta: 0,
          },
        },
      ],
    },
    {
      id: "rnnoise",
      name: "Noise Suppression",
      description: "RNNoise speech cleanup",
      plugin_hint: {},
      params: [
        { id: "vad_threshold", label: "VAD Threshold", min: 0, max: 99, default: 50, unit: "%" },
        { id: "hold_ms", label: "Hold Open", min: 0, max: 1000, default: 200, unit: " ms" },
        { id: "lead_in_ms", label: "Lead-In", min: 0, max: 200, default: 0, unit: " ms" },
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
        { id: "threshold_db", label: "Threshold", min: -30, max: 0, default: -20, unit: " dB" },
        { id: "ratio", label: "Ratio", min: 1, max: 20, default: 4, unit: ":1" },
        { id: "attack_ms", label: "Attack", min: 1.5, max: 200, default: 5, unit: " ms" },
        { id: "release_ms", label: "Release", min: 5, max: 800, default: 100, unit: " ms" },
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

function normalizeDemoEffect(effect: EffectInstance): EffectInstance | null {
  const effectId = String(effect.effect_id ?? "").trim();
  const definition = catalog.effects.find((item) => item.id === effectId);
  if (!definition) return null;
  const params = Object.fromEntries(
    definition.params.map((param) => [
      param.id,
      clampNumber(effect.params?.[param.id], param.min, param.max, param.default),
    ]),
  );
  const name = typeof effect.name === "string" ? effect.name.trim() : "";
  const instanceId =
    typeof effect.instance_id === "string" && effect.instance_id.trim()
      ? effect.instance_id.trim()
      : crypto.randomUUID();
  return {
    ...effect,
    instance_id: instanceId,
    effect_id: definition.id,
    name: name || null,
    bypassed: Boolean(effect.bypassed),
    params,
  };
}

function normalizeDemoEffectChain(effects: EffectInstance[], preferredInstanceId?: string): EffectInstance[] {
  const singleInstanceIndexes = new Map<string, number[]>();
  for (const [index, effect] of effects.entries()) {
    if (!isSingleInstanceDemoEffect(effect.effect_id)) continue;
    const indexes = singleInstanceIndexes.get(effect.effect_id) ?? [];
    indexes.push(index);
    singleInstanceIndexes.set(effect.effect_id, indexes);
  }
  if (singleInstanceIndexes.size === 0) return effects;

  const keepIndexes = new Set<number>();
  for (const indexes of singleInstanceIndexes.values()) {
    const preferred = indexes.find((index) => effects[index]?.instance_id === preferredInstanceId);
    const active = [...indexes].reverse().find((index) => effects[index] && !effects[index].bypassed);
    const keepIndex = preferred ?? active ?? indexes.at(-1);
    if (keepIndex !== undefined) keepIndexes.add(keepIndex);
  }

  return effects.filter((effect, index) => !isSingleInstanceDemoEffect(effect.effect_id) || keepIndexes.has(index));
}

function isSingleInstanceDemoEffect(effectId: string): boolean {
  return singleInstanceEffectIds.has(effectId);
}

function clampNumber(value: unknown, min: number, max: number, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.max(min, Math.min(max, value))
    : fallback;
}

export const demoState: AppStateSnapshot = {
  config: {
    version: 8,
    audio: {
      sample_rate_hz: 48000,
      bit_depth: 24,
      channel_layout: "stereo",
      mono_inputs_to_stereo: true,
    },
    settings: {
      theme: "system",
      start_at_login: false,
      keep_running_in_tray: true,
      restore_audio_graph_on_launch: false,
      monitor_follows_default_output: true,
      lock_default_input: false,
      lock_default_output: false,
      low_latency_mic_monitoring: false,
      stream_sync_delay_msec: 0,
      monitor_sync_delay_msec: 0,
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
        output_devices: ["alsa_output.usb"],
        icon: "headphones",
        volume: 0.86,
        muted: false,
      },
      {
        id: "stream",
        name: "Stream",
        virtual_sink_name: "wavelinux_mix_stream",
        virtual_source_name: "wavelinux_mix_stream_source",
        monitor_output: null,
        output_devices: [],
        icon: "radio",
        volume: 1,
        muted: false,
      },
      {
        id: "discord_mix",
        name: "Discord Mix",
        virtual_sink_name: "wavelinux_mix_discord_mix",
        virtual_source_name: "wavelinux_mix_discord_mix_source",
        monitor_output: null,
        output_devices: [],
        icon: "chat",
        volume: 1,
        muted: false,
      },
    ],
    channels: ["Input", "System", "Game", "Chat", "Music", "Browser", "SFX"].map((name, index) => {
      const id = slug(name);
      const kind = index === 0 ? "generic" : id === "system" ? "system" : id === "sfx" ? "soundboard" : "application";
      return {
        id,
        name,
        kind,
        virtual_sink_name: `wavelinux_channel_${id}`,
        source_device: index === 0 ? "alsa_input.usb_interface" : null,
        icon: null,
        input_mode: kind === "generic" ? "sum_mono" : "stereo",
        linked: false,
        mix_buses: {
          monitor: { volume: index === 0 ? 1 : 0.76, muted: false, enabled: true },
          stream: { volume: index === 2 ? 0.52 : 0.84, muted: false, enabled: true },
          discord_mix: { volume: index === 1 ? 0.2 : 0.75, muted: index === 3, enabled: index < 4 },
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
    app_volume_presets: [
      { matcher: { app_id: "discord", binary: null, process_name: null, window_class: null }, volume: 0.82 },
      { matcher: { app_id: "spotify", binary: null, process_name: null, window_class: null }, volume: 0.66 },
      { matcher: { app_id: "firefox", binary: null, process_name: null, window_class: null }, volume: 0.76 },
    ],
    app_history: [
      { matcher: { app_id: "discord", binary: null, process_name: null, window_class: null }, display_name: "Discord", media_name: "Voice", last_seen_unix: Math.floor(Date.now() / 1000), forgotten: false },
      { matcher: { app_id: "spotify", binary: null, process_name: null, window_class: null }, display_name: "Spotify", media_name: "Playback", last_seen_unix: Math.floor(Date.now() / 1000) - 1800, forgotten: false },
      { matcher: { app_id: "firefox", binary: null, process_name: null, window_class: null }, display_name: "Firefox", media_name: "YouTube", last_seen_unix: Math.floor(Date.now() / 1000) - 360, forgotten: false },
    ],
    app_identity_overrides: [],
    app_label_overrides: [],
    device_policy: {
      preferred_input: "alsa_input.usb_interface",
      preferred_output: "alsa_output.usb",
      restorable_input: null,
      restorable_output: null,
      active_input_fallback: false,
      active_output_fallback: false,
      hardware_profile_assignments: {},
      fallback_hardware_profile: defaultDemoFallbackProfile(),
    },
    streamer_devices: {
      version: 1,
      profiles: {},
    },
  },
  graph: {
    inputs: [
      { id: "alsa_input.usb_interface", name: "alsa_input.usb_interface", description: "USB Interface Line In", is_available: true, is_default: true, is_virtual: false, bus: "usb" },
      { id: "alsa_input.capture_card", name: "alsa_input.capture_card", description: "HDMI Capture Card Audio", is_available: true, is_default: false, is_virtual: false, bus: "usb" },
      { id: "bluez_input.headset", name: "bluez_input.headset", description: "Bluetooth Headset Mic", is_available: true, is_default: false, is_virtual: false, bus: "bluetooth" },
      { id: "alsa_output.usb.monitor", name: "alsa_output.usb.monitor", description: "USB Headphones Monitor", is_available: true, is_default: false, is_virtual: false, bus: "usb" },
    ],
    outputs: [
      { id: "alsa_output.usb", name: "alsa_output.usb", description: "USB Headphones", is_available: true, is_default: true, is_virtual: false, bus: "usb" },
      { id: "bluez_output.headset", name: "bluez_output.headset", description: "Bluetooth Headset", is_available: true, is_default: false, is_virtual: false, bus: "bluetooth" },
    ],
    app_streams: [
      { id: "42", app_id: "firefox", binary: "firefox", process_name: "firefox", window_class: "firefox", display_name: "Firefox", media_name: "YouTube", routed_channel_id: "browser", volume: 0.76, muted: false },
      { id: "77", app_id: "discord", binary: "Discord", process_name: "Discord", window_class: "discord", display_name: "Discord", media_name: "Voice", routed_channel_id: "chat", volume: 0.86, muted: false },
      { id: "81", app_id: "spotify", binary: "spotify", process_name: "spotify", window_class: "spotify", display_name: "Spotify", media_name: "Playback", routed_channel_id: "music", volume: 0.66, muted: false },
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
    { code: "plugin.deepfilternet", severity: "info", message: "DeepFilterNet3 LADSPA detected", action: null },
  ],
  engine: {
    dry_run: true,
    healthy: true,
    audio_graph_running: false,
    message: "Demo mode",
    last_refresh_unix: 0,
  },
  catalog,
};

applyDemoHardwareProfiles();
