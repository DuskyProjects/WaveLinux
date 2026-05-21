export type ThemeMode = "system" | "dark" | "light";
export type ChannelKind =
  | "microphone"
  | "application"
  | "soundboard"
  | "system"
  | "generic";

export interface MixerSettings {
  theme: ThemeMode;
  start_at_login: boolean;
  lock_default_input: boolean;
  lock_default_output: boolean;
  auto_check_updates: boolean;
  auto_install_updates: boolean;
  release_channel: "stable" | "beta";
}

export interface AudioSpec {
  sample_rate_hz: number;
  bit_depth: number;
  channel_layout: string;
  mono_inputs_to_stereo: boolean;
}

export interface Mix {
  id: string;
  name: string;
  virtual_sink_name: string;
  virtual_source_name: string;
  monitor_output?: string | null;
  volume: number;
  muted: boolean;
}

export interface MixBus {
  volume: number;
  muted: boolean;
}

export interface AppMatcher {
  app_id?: string | null;
  binary?: string | null;
  process_name?: string | null;
  window_class?: string | null;
}

export interface EffectInstance {
  instance_id: string;
  effect_id: string;
  name?: string | null;
  bypassed: boolean;
  params: Record<string, number>;
}

export interface Channel {
  id: string;
  name: string;
  kind: ChannelKind;
  virtual_sink_name: string;
  linked: boolean;
  mix_buses: Record<string, MixBus>;
  app_matchers: AppMatcher[];
  effects: EffectInstance[];
}

export interface AppRoute {
  matcher: AppMatcher;
  channel_id: string;
}

export interface MixerConfig {
  version: number;
  mixes: Mix[];
  channels: Channel[];
  app_routes: AppRoute[];
  settings: MixerSettings;
  audio: AudioSpec;
}

export interface DeviceInfo {
  id: string;
  index?: string | null;
  name: string;
  description: string;
  is_default: boolean;
  is_virtual: boolean;
}

export interface AppStream {
  id: string;
  app_id?: string | null;
  process_name?: string | null;
  display_name: string;
  media_name?: string | null;
  routed_channel_id?: string | null;
  volume: number;
  muted: boolean;
}

export interface LevelMeter {
  node_id: string;
  peak_left: number;
  peak_right: number;
}

export interface EffectAvailability {
  effect_id: string;
  available: boolean;
  detail: string;
}

export interface RuntimeGraph {
  inputs: DeviceInfo[];
  outputs: DeviceInfo[];
  app_streams: AppStream[];
  meters: LevelMeter[];
  effect_availability: EffectAvailability[];
}

export interface EffectParamDefinition {
  id: string;
  label: string;
  min: number;
  max: number;
  default: number;
  unit: string;
}

export interface EffectPreset {
  name: string;
  values: Record<string, number>;
}

export interface EffectDefinition {
  id: string;
  name: string;
  description: string;
  plugin_hint: unknown;
  params: EffectParamDefinition[];
  presets: EffectPreset[];
}

export interface EffectCatalog {
  effects: EffectDefinition[];
  preferred_order: string[];
}

export type DiagnosticSeverity = "info" | "warning" | "error";

export interface Diagnostic {
  code: string;
  severity: DiagnosticSeverity;
  message: string;
  action?: string | null;
}

export interface EngineStatus {
  dry_run: boolean;
  healthy: boolean;
  message: string;
  last_refresh_unix: number;
}

export interface AppStateSnapshot {
  config: MixerConfig;
  graph: RuntimeGraph;
  diagnostics: Diagnostic[];
  engine: EngineStatus;
  catalog: EffectCatalog;
}

export interface Scene {
  id: string;
  name: string;
  created_unix: number;
  config: MixerConfig;
}

export interface SoundCheckReport {
  diagnostics: Diagnostic[];
  active_stream_count: number;
  virtual_mix_count: number;
  missing_effects: string[];
}

export interface CommandExecution {
  command: {
    domain: string;
    program: string;
    args: string[];
    description: string;
  };
  stdout: string;
  stderr: string;
  skipped: boolean;
  error?: string | null;
}
