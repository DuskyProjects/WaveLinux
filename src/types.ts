export type ThemeMode = "system" | "dark" | "light";
export type ChannelKind =
  | "microphone"
  | "application"
  | "soundboard"
  | "system"
  | "generic";

export type ChannelInputMode =
  | "stereo"
  | "mono_left"
  | "mono_right"
  | "sum_mono"
  | "swap_lr";

export interface MixerSettings {
  theme: ThemeMode;
  start_at_login: boolean;
  keep_running_in_tray: boolean;
  restore_audio_graph_on_launch: boolean;
  monitor_follows_default_output: boolean;
  lock_default_input: boolean;
  lock_default_output: boolean;
  low_latency_mic_monitoring: boolean;
  hardware_direct_mic_monitoring: boolean;
  stream_sync_delay_msec: number;
  monitor_sync_delay_msec: number;
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
  output_devices?: string[];
  icon?: string | null;
  volume: number;
  muted: boolean;
}

export interface MixBus {
  volume: number;
  muted: boolean;
  enabled: boolean;
}

export interface AppMatcher {
  app_id?: string | null;
  binary?: string | null;
  process_name?: string | null;
  window_class?: string | null;
  media_name?: string | null;
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
  source_device?: string | null;
  icon?: string | null;
  input_mode: ChannelInputMode;
  linked: boolean;
  mix_buses: Record<string, MixBus>;
  app_matchers: AppMatcher[];
  effects: EffectInstance[];
}

export interface AppRoute {
  matcher: AppMatcher;
  channel_id: string;
}

export interface AppVolumePreset {
  matcher: AppMatcher;
  volume: number;
}

export interface KnownApp {
  matcher: AppMatcher;
  display_name: string;
  media_name?: string | null;
  last_seen_unix: number;
  forgotten: boolean;
}

export interface AppIdentityOverride {
  source: AppMatcher;
  target: AppMatcher;
}

export interface AppLabelOverride {
  matcher: AppMatcher;
  label: string;
}

export interface DevicePolicy {
  preferred_input?: string | null;
  preferred_output?: string | null;
  restorable_input?: string | null;
  restorable_output?: string | null;
  active_input_fallback: boolean;
  active_output_fallback: boolean;
  hardware_profile_assignments: Record<string, string>;
  fallback_hardware_profile: FallbackHardwareProfile;
}

export type DeviceBus = "usb" | "bluetooth" | "pci" | "platform" | "virtual" | "unknown";
export type ProfileConfidence = "low" | "medium" | "high";
export type BluetoothMicPolicy =
  | "never_if_hfp"
  | "allow_explicit_call_mode"
  | "allow_duplex_a2dp_if_supported"
  | "advisory_only";

export interface LatencyPolicy {
  stable_msec?: number | null;
  low_latency_msec?: number | null;
  bluetooth_floor_msec?: number | null;
}

export interface RoutingPolicy {
  input_priority?: number | null;
  output_priority?: number | null;
  allow_auto_select_input: boolean;
  allow_auto_select_output: boolean;
  prefer_non_bluetooth_input: boolean;
}

export interface FallbackHardwareProfile {
  id: string;
  name: string;
  latency_policy: LatencyPolicy;
  routing_policy: RoutingPolicy;
  bluetooth_mic_policy: BluetoothMicPolicy;
  confidence: ProfileConfidence;
}

export interface HardwareProfileSummary {
  id: string;
  name: string;
  source: string;
  confidence: ProfileConfidence;
  latency_policy: LatencyPolicy;
  routing_policy: RoutingPolicy;
  bluetooth_mic_policy: BluetoothMicPolicy;
}

export interface HardwareProfileUiState {
  profiles: HardwareProfileSummary[];
  assignments: Record<string, string>;
  fallback_profile: FallbackHardwareProfile;
}

export type StreamerDeviceFamily =
  | "stream_deck"
  | "rode"
  | "go_xlr"
  | "midi_surface"
  | "loupedeck"
  | "x_keys"
  | "unknown_supported";

export type StreamerTransport = "hid" | "midi" | "audio_profile" | "bridge";
export type StreamerPermissionStatus =
  | "ready"
  | "permission_denied"
  | "busy"
  | "missing_runtime"
  | "unsupported_protocol";
export type StreamerControlKind = "button" | "dial" | "fader" | "pad" | "unknown";

export interface StreamerDeviceCapabilities {
  buttons: boolean;
  dials: boolean;
  faders: boolean;
  pads: boolean;
  display_feedback: boolean;
  midi_feedback: boolean;
  audio_endpoint: boolean;
}

export interface StreamerDeviceSummary {
  id: string;
  name: string;
  description: string;
  family: StreamerDeviceFamily;
  transport: StreamerTransport;
  vendor_id?: string | null;
  product_id?: string | null;
  capabilities: StreamerDeviceCapabilities;
  connected: boolean;
  enabled: boolean;
  permission_status: StreamerPermissionStatus;
  matched_profile_id?: string | null;
  source: string;
  message: string;
}

export interface StreamerDevicesConfig {
  version: number;
  profiles: Record<string, StreamerBindingProfile>;
}

export interface StreamerBindingProfile {
  device_id: string;
  family?: StreamerDeviceFamily | null;
  name: string;
  enabled: boolean;
  safe_preset: boolean;
  bindings: StreamerBinding[];
}

export interface StreamerBinding {
  control_id: string;
  label: string;
  control_kind: StreamerControlKind;
  action: StreamerAction;
}

export type StreamerAction =
  | { kind: "noop" }
  | { kind: "mix_mute_toggle"; mix_id: string }
  | { kind: "mix_volume_set"; mix_id: string; volume: number }
  | { kind: "mix_volume_set_from_control"; mix_id: string }
  | { kind: "mix_volume_adjust"; mix_id: string; delta: number }
  | { kind: "channel_mute_toggle"; channel_id: string; mix_id: string }
  | { kind: "channel_bus_enabled_toggle"; channel_id: string; mix_id: string }
  | { kind: "channel_volume_set"; channel_id: string; mix_id: string; volume: number }
  | { kind: "channel_volume_set_from_control"; channel_id: string; mix_id: string }
  | { kind: "channel_volume_adjust"; channel_id: string; mix_id: string; delta: number }
  | { kind: "effect_bypass_toggle"; channel_id: string; instance_id: string }
  | { kind: "start_or_repair_audio" }
  | { kind: "cleanup_audio_graph" }
  | { kind: "cleanup_stale_audio_graph" };

export interface StreamerLearnResult {
  device_id: string;
  control_id?: string | null;
  control_kind: StreamerControlKind;
  message: string;
}

export interface StreamerActionResult {
  performed: boolean;
  message: string;
}

export type ElgatoDeviceKind =
  | "wave_xlr"
  | "wave_microphone"
  | "capture_audio"
  | "audio_endpoint";

export interface ElgatoDeviceSummary {
  id: string;
  name: string;
  description: string;
  kind: ElgatoDeviceKind;
  controls_supported: boolean;
  bus?: string | null;
  vendor_id?: string | null;
  product_id?: string | null;
  alsa_card?: string | null;
  matched_profile_id?: string | null;
  message: string;
}

export type ElgatoWaveXlrKnobTarget = "gain" | "headphones";

export interface ElgatoWaveXlrState {
  connected: boolean;
  gain_raw: number;
  gain_max_raw: number;
  gain_percent: number;
  muted: boolean;
  hp_volume_db: number;
  hp_min_db: number;
  hp_max_db: number;
  low_impedance: boolean;
  volume_select: ElgatoWaveXlrKnobTarget;
  api_version?: string | null;
  firmware_version?: string | null;
  serial?: string | null;
}

export interface MixerConfig {
  version: number;
  mixes: Mix[];
  channels: Channel[];
  app_routes: AppRoute[];
  app_volume_presets: AppVolumePreset[];
  app_history: KnownApp[];
  app_identity_overrides: AppIdentityOverride[];
  app_label_overrides: AppLabelOverride[];
  device_policy: DevicePolicy;
  streamer_devices: StreamerDevicesConfig;
  settings: MixerSettings;
  audio: AudioSpec;
}

export interface DeviceInfo {
  id: string;
  index?: string | null;
  name: string;
  description: string;
  is_available: boolean;
  is_default: boolean;
  is_virtual: boolean;
  bus?: DeviceBus | null;
  vendor_id?: string | null;
  product_id?: string | null;
  alsa_card?: string | null;
  alsa_device?: string | null;
  driver?: string | null;
  bluetooth_modalias?: string | null;
  active_profile?: string | null;
  active_codec?: string | null;
  pipewire_properties?: Record<string, string>;
  matched_profile_id?: string | null;
  matched_profile_source?: string | null;
  profile_confidence?: ProfileConfidence | null;
  active_latency_policy?: LatencyPolicy | null;
  active_routing_policy?: RoutingPolicy | null;
  active_bluetooth_mic_policy?: BluetoothMicPolicy | null;
}

export interface AppStream {
  id: string;
  app_id?: string | null;
  binary?: string | null;
  process_name?: string | null;
  window_class?: string | null;
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

export type AutoDeviceKind = "input" | "output";

export type AutoDeviceReason = "priority" | "system_default" | "active_output" | "unavailable";

export interface ResolvedAutoDevice {
  kind: AutoDeviceKind;
  channel_id?: string | null;
  mix_id?: string | null;
  device_id?: string | null;
  device_name?: string | null;
  device_description?: string | null;
  priority?: number | null;
  reason: AutoDeviceReason;
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
  auto_devices: ResolvedAutoDevice[];
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
  audio_graph_running: boolean;
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

export interface SoundCheckReport {
  diagnostics: Diagnostic[];
  active_stream_count: number;
  virtual_mix_count: number;
  missing_effects: string[];
  debug_log_path: string;
  recent_log_lines: string[];
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

export interface ManagedModule {
  module_id: string;
  role?: string | null;
  channel_id?: string | null;
  mix_id?: string | null;
  node_name?: string | null;
  source_name?: string | null;
  sink_name?: string | null;
}

export interface SinkInputRoute {
  id: string;
  module_id?: string | null;
  role?: string | null;
  channel_id?: string | null;
  mix_id?: string | null;
  sink?: string | null;
  sink_name?: string | null;
  target_object?: string | null;
}

export interface SourceOutputRoute {
  id: string;
  module_id?: string | null;
  role?: string | null;
  channel_id?: string | null;
  mix_id?: string | null;
  source_id?: string | null;
  source_name?: string | null;
  target_object?: string | null;
  application_name?: string | null;
  node_name?: string | null;
  media_name?: string | null;
}

export interface StaleProcess {
  pid: string;
  command: string;
}

export type RouteHealthReason =
  | "missing_source"
  | "missing_sink"
  | "missing_source_output"
  | "missing_sink_input"
  | "stale_config"
  | "duplicate";

export interface RouteHealthIssue {
  module_id?: string | null;
  role: string;
  channel_id?: string | null;
  mix_id?: string | null;
  source_name?: string | null;
  sink_name?: string | null;
  reason: RouteHealthReason;
}

export interface RepairReport {
  dry_run: boolean;
  planned: {
    commands: Array<CommandExecution["command"]>;
    managed_nodes: string[];
  };
  outputs: CommandExecution[];
}

export interface GraphDebugReport {
  dry_run: boolean;
  audio_graph_running: boolean;
  planned: RepairReport["planned"];
  managed_modules: ManagedModule[];
  sink_input_routes: SinkInputRoute[];
  source_output_routes: SourceOutputRoute[];
  route_health: RouteHealthIssue[];
  stale_processes: StaleProcess[];
  graph: RuntimeGraph;
  diagnostics: Diagnostic[];
  debug_log_path: string;
  recent_log_lines: string[];
}

export interface UpdateInfo {
  available: boolean;
  install_supported: boolean;
  current_version: string;
  version?: string | null;
  date?: string | null;
  body?: string | null;
  url?: string | null;
  release_url: string;
  channel: "stable" | "beta" | string;
  endpoint: string;
  message: string;
}

export interface UpdateInstallResult {
  installed: boolean;
  version?: string | null;
  message: string;
}

export interface EffectPluginInstallResult {
  attempted: boolean;
  success: boolean;
  manager: string;
  packages: string[];
  aur_packages: string[];
  missing_before: string[];
  missing_after: string[];
  stdout: string;
  stderr: string;
  message: string;
}
