#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use time::format_description::well_known::Rfc3339;
use wavelinux_engine::{
    prewarm_hardware_profiles_from_xdg, EngineError, GraphDebugReport,
    HardwareProfilePrewarmReport, SoundCheckReport, WaveLinuxEngine,
};
use wavelinux_model::{
    AppMatcher, AppRoute, AppStateSnapshot, AppVolumePreset, Channel, ChannelInputMode,
    ChannelKind, EffectAvailability, EffectCatalog, EffectInstance, FallbackHardwareProfile,
    HardwareProfileUiState, KnownApp, LatencyPolicy, LevelMeter, Mix, MixBus, MixerConfig,
    MixerSettings, ReleaseChannel, RoutingPolicy, StreamerAction, StreamerActionResult,
    StreamerBindingProfile, StreamerDeviceSummary, StreamerDevicesConfig, StreamerLearnResult,
};

mod elgato;
mod streamer_devices;

struct EngineState {
    engine: Arc<WaveLinuxEngine>,
}

struct ProcessLock {
    _file: File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UiThemePreference {
    theme_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UiThemeDefinition {
    id: String,
    name: String,
    surface: String,
    #[serde(default = "default_theme_variant")]
    variant: String,
    #[serde(default)]
    tokens: BTreeMap<String, String>,
}

const RELEASES_URL: &str = "https://github.com/DuskyProjects/WaveLinux/releases";
const STABLE_UPDATE_ENDPOINT: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/latest/download/latest.json";
const BETA_UPDATE_ENDPOINT: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/download/prerelease/latest.json";
const UI_THEME_PREFERENCE_FILE: &str = "ui-theme.json";
const UI_THEMES_DIR: &str = "themes";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
struct UpdateInfo {
    available: bool,
    install_supported: bool,
    current_version: String,
    version: Option<String>,
    date: Option<String>,
    body: Option<String>,
    url: Option<String>,
    release_url: String,
    channel: String,
    endpoint: String,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
struct UpdateInstallResult {
    installed: bool,
    version: Option<String>,
    message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
struct EffectPluginInstallResult {
    attempted: bool,
    success: bool,
    manager: String,
    packages: Vec<String>,
    aur_packages: Vec<String>,
    missing_before: Vec<String>,
    missing_after: Vec<String>,
    stdout: String,
    stderr: String,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Unknown,
}

impl PackageManager {
    fn id(self) -> &'static str {
        match self {
            Self::Apt => "apt",
            Self::Dnf => "dnf",
            Self::Pacman => "pacman",
            Self::Zypper => "zypper",
            Self::Unknown => "unknown",
        }
    }
}

#[tauri::command]
fn get_state(engine: State<'_, EngineState>) -> Result<AppStateSnapshot, String> {
    tauri_result(engine.engine.get_state())
}

#[tauri::command]
fn observe_state(engine: State<'_, EngineState>) -> Result<AppStateSnapshot, String> {
    tauri_result(engine.engine.observe_state())
}

#[tauri::command]
fn observe_meters(engine: State<'_, EngineState>) -> Result<Vec<LevelMeter>, String> {
    tauri_result(engine.engine.observe_meters())
}

#[tauri::command]
fn create_mix(engine: State<'_, EngineState>, name: String) -> Result<Mix, String> {
    tauri_result(engine.engine.create_mix(name))
}

#[tauri::command]
fn rename_mix(engine: State<'_, EngineState>, mix_id: String, name: String) -> Result<Mix, String> {
    tauri_result(engine.engine.rename_mix(mix_id, name))
}

#[tauri::command]
fn move_mix(engine: State<'_, EngineState>, mix_id: String, direction: i32) -> Result<Mix, String> {
    tauri_result(engine.engine.move_mix(mix_id, direction))
}

#[tauri::command]
fn delete_mix(engine: State<'_, EngineState>, mix_id: String) -> Result<Mix, String> {
    tauri_result(engine.engine.delete_mix(mix_id))
}

#[tauri::command]
fn set_mix_volume(
    engine: State<'_, EngineState>,
    mix_id: String,
    volume: f32,
) -> Result<Mix, String> {
    tauri_result(engine.engine.set_mix_volume(mix_id, volume))
}

#[tauri::command]
fn set_mix_mute(
    engine: State<'_, EngineState>,
    mix_id: String,
    muted: bool,
) -> Result<Mix, String> {
    tauri_result(engine.engine.set_mix_mute(mix_id, muted))
}

#[tauri::command]
fn set_mix_icon(
    engine: State<'_, EngineState>,
    mix_id: String,
    icon: Option<String>,
) -> Result<Mix, String> {
    tauri_result(engine.engine.set_mix_icon(mix_id, icon))
}

#[tauri::command]
fn set_channel_icon(
    engine: State<'_, EngineState>,
    channel_id: String,
    icon: Option<String>,
) -> Result<Channel, String> {
    tauri_result(engine.engine.set_channel_icon(channel_id, icon))
}

#[tauri::command]
fn set_mix_monitor_output(
    engine: State<'_, EngineState>,
    mix_id: String,
    output: Option<String>,
) -> Result<Mix, String> {
    tauri_result(engine.engine.set_mix_monitor_output(mix_id, output))
}

#[tauri::command]
fn set_mix_outputs(
    engine: State<'_, EngineState>,
    mix_id: String,
    outputs: Vec<String>,
) -> Result<Mix, String> {
    tauri_result(engine.engine.set_mix_outputs(mix_id, outputs))
}

#[tauri::command]
fn create_channel(
    engine: State<'_, EngineState>,
    name: String,
    kind: ChannelKind,
) -> Result<Channel, String> {
    tauri_result(engine.engine.create_channel(name, kind))
}

#[tauri::command]
fn rename_channel(
    engine: State<'_, EngineState>,
    channel_id: String,
    name: String,
) -> Result<Channel, String> {
    tauri_result(engine.engine.rename_channel(channel_id, name))
}

#[tauri::command]
fn move_channel(
    engine: State<'_, EngineState>,
    channel_id: String,
    direction: i32,
) -> Result<Channel, String> {
    tauri_result(engine.engine.move_channel(channel_id, direction))
}

#[tauri::command]
fn delete_channel(engine: State<'_, EngineState>, channel_id: String) -> Result<Channel, String> {
    tauri_result(engine.engine.delete_channel(channel_id))
}

#[tauri::command]
fn set_channel_linked(
    engine: State<'_, EngineState>,
    channel_id: String,
    linked: bool,
) -> Result<Channel, String> {
    tauri_result(engine.engine.set_channel_linked(channel_id, linked))
}

#[tauri::command]
fn set_channel_input(
    engine: State<'_, EngineState>,
    channel_id: String,
    source_device: Option<String>,
) -> Result<Channel, String> {
    tauri_result(engine.engine.set_channel_input(channel_id, source_device))
}

#[tauri::command]
fn set_hardware_input_device(
    engine: State<'_, EngineState>,
    channel_id: String,
    source_device: Option<String>,
) -> Result<Channel, String> {
    tauri_result(
        engine
            .engine
            .set_hardware_input_device(channel_id, source_device),
    )
}

#[tauri::command]
fn set_channel_input_mode(
    engine: State<'_, EngineState>,
    channel_id: String,
    input_mode: ChannelInputMode,
) -> Result<Channel, String> {
    tauri_result(engine.engine.set_channel_input_mode(channel_id, input_mode))
}

#[tauri::command]
fn set_channel_bus_enabled(
    engine: State<'_, EngineState>,
    channel_id: String,
    mix_id: String,
    enabled: bool,
) -> Result<MixBus, String> {
    tauri_result(
        engine
            .engine
            .set_channel_bus_enabled(channel_id, mix_id, enabled),
    )
}

#[tauri::command]
fn set_settings(
    engine: State<'_, EngineState>,
    settings: MixerSettings,
) -> Result<MixerSettings, String> {
    tauri_result(engine.engine.set_settings(settings))
}

#[tauri::command]
fn list_hardware_profiles(
    engine: State<'_, EngineState>,
) -> Result<HardwareProfileUiState, String> {
    tauri_result(engine.engine.list_hardware_profiles())
}

#[tauri::command]
fn set_device_hardware_profile(
    engine: State<'_, EngineState>,
    device_id: String,
    profile_id: Option<String>,
) -> Result<HardwareProfileUiState, String> {
    tauri_result(
        engine
            .engine
            .set_device_hardware_profile(device_id, profile_id),
    )
}

#[tauri::command]
fn set_fallback_hardware_profile(
    engine: State<'_, EngineState>,
    fallback_profile: FallbackHardwareProfile,
) -> Result<HardwareProfileUiState, String> {
    tauri_result(
        engine
            .engine
            .set_fallback_hardware_profile(fallback_profile),
    )
}

#[tauri::command]
fn set_hardware_profile_policy(
    engine: State<'_, EngineState>,
    profile_id: String,
    name: Option<String>,
    latency_policy: LatencyPolicy,
    routing_policy: RoutingPolicy,
) -> Result<HardwareProfileUiState, String> {
    tauri_result(engine.engine.set_hardware_profile_policy(
        profile_id,
        name,
        latency_policy,
        routing_policy,
    ))
}

#[tauri::command]
fn list_streamer_devices(
    engine: State<'_, EngineState>,
) -> Result<Vec<StreamerDeviceSummary>, String> {
    let state = engine.engine.get_state().map_err(|err| err.to_string())?;
    let mut devices = streamer_devices::discover_devices(&state);
    let missing_profiles = devices.iter().any(|device| {
        !state
            .config
            .streamer_devices
            .profiles
            .contains_key(&device.id)
    });
    let bindings = if missing_profiles {
        let defaults = streamer_devices::default_profiles_for_devices(&devices, &state.config);
        engine
            .engine
            .ensure_streamer_binding_profiles(defaults)
            .map_err(|err| err.to_string())?
    } else {
        state.config.streamer_devices
    };
    for device in &mut devices {
        if let Some(profile) = bindings.profiles.get(&device.id) {
            device.enabled = profile.enabled;
        }
    }
    Ok(devices)
}

#[tauri::command]
fn get_streamer_bindings(engine: State<'_, EngineState>) -> Result<StreamerDevicesConfig, String> {
    engine
        .engine
        .streamer_devices_config()
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn set_streamer_device_enabled(
    engine: State<'_, EngineState>,
    device_id: String,
    enabled: bool,
) -> Result<StreamerDevicesConfig, String> {
    engine
        .engine
        .set_streamer_device_enabled(device_id, enabled)
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn set_streamer_binding_profile(
    engine: State<'_, EngineState>,
    profile: StreamerBindingProfile,
) -> Result<StreamerBindingProfile, String> {
    engine
        .engine
        .set_streamer_binding_profile(profile)
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn learn_streamer_control(
    engine: State<'_, EngineState>,
    device_id: String,
) -> Result<StreamerLearnResult, String> {
    let state = engine.engine.get_state().map_err(|err| err.to_string())?;
    let devices = streamer_devices::discover_devices(&state);
    streamer_devices::learn_control(&devices, &device_id)
}

#[tauri::command]
fn run_streamer_action_test(
    engine: State<'_, EngineState>,
    action: StreamerAction,
) -> Result<StreamerActionResult, String> {
    streamer_devices::run_action(&engine.engine, action)
}

#[tauri::command]
fn list_elgato_devices(
    engine: State<'_, EngineState>,
) -> Result<Vec<elgato::ElgatoDeviceSummary>, String> {
    let state = engine.engine.get_state().map_err(|err| err.to_string())?;
    Ok(elgato::summarize_devices(
        state.graph.inputs.iter(),
        state.graph.outputs.iter(),
    ))
}

#[tauri::command]
fn read_elgato_wave_xlr(
    engine: State<'_, EngineState>,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    elgato::read_wave_xlr_state().map_err(|err| err.to_string())
}

#[tauri::command]
fn set_elgato_wave_xlr_gain(
    engine: State<'_, EngineState>,
    gain_raw: u16,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    elgato::set_wave_xlr_gain(gain_raw).map_err(|err| err.to_string())
}

#[tauri::command]
fn set_elgato_wave_xlr_mute(
    engine: State<'_, EngineState>,
    muted: bool,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    elgato::set_wave_xlr_mute(muted).map_err(|err| err.to_string())
}

#[tauri::command]
fn set_elgato_wave_xlr_hp_volume_db(
    engine: State<'_, EngineState>,
    db: f32,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    elgato::set_wave_xlr_hp_volume_db(db).map_err(|err| err.to_string())
}

#[tauri::command]
fn set_elgato_wave_xlr_low_impedance(
    engine: State<'_, EngineState>,
    enabled: bool,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    elgato::set_wave_xlr_low_impedance(enabled).map_err(|err| err.to_string())
}

#[tauri::command]
fn set_channel_volume(
    engine: State<'_, EngineState>,
    channel_id: String,
    mix_id: String,
    volume: f32,
) -> Result<MixBus, String> {
    tauri_result(engine.engine.set_channel_volume(channel_id, mix_id, volume))
}

#[tauri::command]
fn set_channel_mute(
    engine: State<'_, EngineState>,
    channel_id: String,
    mix_id: String,
    muted: bool,
) -> Result<MixBus, String> {
    tauri_result(engine.engine.set_channel_mute(channel_id, mix_id, muted))
}

#[tauri::command]
fn assign_app_to_channel(
    engine: State<'_, EngineState>,
    channel_id: String,
    matcher: AppMatcher,
) -> Result<AppRoute, String> {
    tauri_result(engine.engine.assign_app_to_channel(channel_id, matcher))
}

#[tauri::command]
fn remove_app_route(
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
) -> Result<Option<AppRoute>, String> {
    tauri_result(engine.engine.remove_app_route(matcher))
}

#[tauri::command]
fn set_app_volume_preset(
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    volume: f32,
) -> Result<AppVolumePreset, String> {
    tauri_result(engine.engine.set_app_volume_preset(matcher, volume))
}

#[tauri::command]
fn remove_app_volume_preset(
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
) -> Result<Option<AppVolumePreset>, String> {
    tauri_result(engine.engine.remove_app_volume_preset(matcher))
}

#[tauri::command]
fn forget_app(
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
) -> Result<Option<KnownApp>, String> {
    tauri_result(engine.engine.forget_app(matcher))
}

#[tauri::command]
fn restore_app(
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
) -> Result<Option<KnownApp>, String> {
    tauri_result(engine.engine.restore_app(matcher))
}

#[tauri::command]
fn pin_app_identity(
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    label: String,
) -> Result<KnownApp, String> {
    tauri_result(engine.engine.pin_app_identity(matcher, label))
}

#[tauri::command]
fn merge_app_identity(
    engine: State<'_, EngineState>,
    source: AppMatcher,
    target: AppMatcher,
) -> Result<KnownApp, String> {
    tauri_result(engine.engine.merge_app_identity(source, target))
}

#[tauri::command]
fn reset_app_identity(
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
) -> Result<Option<KnownApp>, String> {
    tauri_result(engine.engine.reset_app_identity(matcher))
}

#[tauri::command]
fn move_app_stream(
    engine: State<'_, EngineState>,
    stream_id: String,
    channel_id: String,
) -> Result<wavelinux_engine::CommandExecution, String> {
    tauri_result(engine.engine.move_app_stream(stream_id, channel_id))
}

#[tauri::command]
fn move_app_stream_to_default(
    engine: State<'_, EngineState>,
    stream_id: String,
) -> Result<wavelinux_engine::CommandExecution, String> {
    tauri_result(engine.engine.move_app_stream_to_default(stream_id))
}

#[tauri::command]
fn set_app_stream_volume(
    engine: State<'_, EngineState>,
    stream_id: String,
    volume: f32,
) -> Result<wavelinux_engine::CommandExecution, String> {
    tauri_result(engine.engine.set_app_stream_volume(stream_id, volume))
}

#[tauri::command]
fn set_app_stream_mute(
    engine: State<'_, EngineState>,
    stream_id: String,
    muted: bool,
) -> Result<wavelinux_engine::CommandExecution, String> {
    tauri_result(engine.engine.set_app_stream_mute(stream_id, muted))
}

#[tauri::command]
fn set_effect_chain(
    engine: State<'_, EngineState>,
    channel_id: String,
    effects: Vec<EffectInstance>,
) -> Result<Channel, String> {
    tauri_result(engine.engine.set_effect_chain(channel_id, effects))
}

#[tauri::command]
fn set_effect_param(
    engine: State<'_, EngineState>,
    channel_id: String,
    instance_id: String,
    param_id: String,
    value: f32,
) -> Result<Channel, String> {
    tauri_result(
        engine
            .engine
            .set_effect_param(channel_id, instance_id, param_id, value),
    )
}

#[tauri::command]
fn bypass_effect(
    engine: State<'_, EngineState>,
    channel_id: String,
    instance_id: String,
    bypassed: bool,
) -> Result<Channel, String> {
    tauri_result(
        engine
            .engine
            .bypass_effect(channel_id, instance_id, bypassed),
    )
}

#[tauri::command]
fn run_sound_check(engine: State<'_, EngineState>) -> Result<SoundCheckReport, String> {
    tauri_result(engine.engine.run_diagnostics())
}

#[tauri::command]
fn run_diagnostics(engine: State<'_, EngineState>) -> Result<SoundCheckReport, String> {
    tauri_result(engine.engine.run_diagnostics())
}

#[tauri::command]
fn get_graph_debug_report(engine: State<'_, EngineState>) -> Result<GraphDebugReport, String> {
    tauri_result(engine.engine.get_graph_debug_report())
}

#[tauri::command]
fn cleanup_stale_audio_graph(
    engine: State<'_, EngineState>,
) -> Result<Vec<wavelinux_engine::CommandExecution>, String> {
    tauri_result(engine.engine.cleanup_stale_audio_graph())
}

#[tauri::command]
fn restore_device(engine: State<'_, EngineState>, kind: String) -> Result<MixerConfig, String> {
    tauri_result(engine.engine.restore_device(kind))
}

#[tauri::command]
fn get_ui_theme_preference(app: AppHandle) -> Result<Option<UiThemePreference>, String> {
    let path = ui_theme_preference_path(&app)?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let Ok(preference) = serde_json::from_str::<UiThemePreference>(&raw) else {
        return Ok(None);
    };
    let Ok(theme_id) = clean_ui_theme_id(&normalize_ui_theme_id(&preference.theme_id)) else {
        return Ok(None);
    };
    Ok(Some(UiThemePreference { theme_id }))
}

#[tauri::command]
fn set_ui_theme_preference(app: AppHandle, theme_id: String) -> Result<UiThemePreference, String> {
    let preference = UiThemePreference {
        theme_id: clean_ui_theme_id(&normalize_ui_theme_id(&theme_id))?,
    };
    let path = ui_theme_preference_path(&app)?;
    let data = serde_json::to_string_pretty(&preference).map_err(|err| err.to_string())?;
    fs::write(path, data).map_err(|err| err.to_string())?;
    Ok(preference)
}

#[tauri::command]
fn list_ui_themes(app: AppHandle) -> Result<Vec<UiThemeDefinition>, String> {
    let dir = ui_themes_dir(&app)?;
    let mut seen = built_in_theme_ids();
    let mut themes = Vec::new();
    for entry in fs::read_dir(dir).map_err(|err| err.to_string())? {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(theme) = serde_json::from_str::<UiThemeDefinition>(&raw) else {
            continue;
        };
        let Ok(theme) = normalize_ui_theme(theme) else {
            continue;
        };
        if seen.insert(theme.id.clone()) {
            themes.push(theme);
        }
    }
    themes.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(themes)
}

#[tauri::command]
fn open_ui_theme_folder(app: AppHandle) -> Result<(), String> {
    let dir = ui_themes_dir(&app)?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|err| err.to_string())
}

#[tauri::command]
async fn check_for_updates(
    app: AppHandle,
    engine: State<'_, EngineState>,
    release_channel: Option<ReleaseChannel>,
) -> Result<UpdateInfo, String> {
    let mut settings = engine
        .engine
        .get_state()
        .map_err(|err| err.to_string())?
        .config
        .settings;
    if let Some(release_channel) = release_channel {
        settings.release_channel = release_channel;
    }
    let endpoint = update_endpoint(&settings);
    let endpoint_url = endpoint
        .parse::<url::Url>()
        .map_err(|err| err.to_string())?;
    let current_update_version = current_update_version();
    let current_version = current_update_version.to_string();
    let updater = app
        .updater_builder()
        .version_comparator({
            let current_update_version = current_update_version.clone();
            move |_current_version, remote_release| {
                remote_release.version > current_update_version.clone()
            }
        })
        .endpoints(vec![endpoint_url])
        .map_err(|err| err.to_string())?
        .build()
        .map_err(|err| err.to_string())?;
    let channel = release_channel_name(&settings).to_string();
    let install_supported = is_appimage_install();
    let update = match updater.check().await {
        Ok(update) => update,
        Err(err) if is_missing_update_metadata_error(&err.to_string()) => {
            return Ok(UpdateInfo {
                available: false,
                install_supported,
                current_version,
                version: None,
                date: None,
                body: None,
                url: None,
                release_url: RELEASES_URL.to_string(),
                channel,
                endpoint,
                message: "No signed update metadata has been published for this channel yet".into(),
            });
        }
        Err(err) => return Err(err.to_string()),
    };
    Ok(match update {
        Some(update) => {
            let date = update.date.and_then(|date| date.format(&Rfc3339).ok());
            let version = update.version.clone();
            UpdateInfo {
                available: true,
                install_supported,
                current_version,
                version: Some(version.clone()),
                date,
                body: update.body,
                url: Some(update.download_url.to_string()),
                release_url: RELEASES_URL.to_string(),
                channel,
                endpoint,
                message: if install_supported {
                    format!("WaveLinux {version} is available")
                } else {
                    format!(
                        "WaveLinux {version} is available; update through your package manager or install the AppImage"
                    )
                },
            }
        }
        None => UpdateInfo {
            available: false,
            install_supported,
            current_version,
            version: None,
            date: None,
            body: None,
            url: None,
            release_url: RELEASES_URL.to_string(),
            channel,
            endpoint,
            message: "WaveLinux is up to date".into(),
        },
    })
}

#[tauri::command]
async fn install_update(
    app: AppHandle,
    engine: State<'_, EngineState>,
    release_channel: Option<ReleaseChannel>,
) -> Result<UpdateInstallResult, String> {
    if !is_appimage_install() {
        return Err(
            "Self-update is available for AppImage installs. Use deb, rpm, or AUR updates through your package manager."
                .into(),
        );
    }

    let mut settings = engine
        .engine
        .get_state()
        .map_err(|err| err.to_string())?
        .config
        .settings;
    if let Some(release_channel) = release_channel {
        settings.release_channel = release_channel;
    }
    let endpoint = update_endpoint(&settings);
    let endpoint_url = endpoint
        .parse::<url::Url>()
        .map_err(|err| err.to_string())?;
    let current_update_version = current_update_version();
    let updater = app
        .updater_builder()
        .version_comparator(move |_current_version, remote_release| {
            remote_release.version > current_update_version.clone()
        })
        .endpoints(vec![endpoint_url])
        .map_err(|err| err.to_string())?
        .build()
        .map_err(|err| err.to_string())?;

    let update = match updater.check().await {
        Ok(update) => update,
        Err(err) if is_missing_update_metadata_error(&err.to_string()) => None,
        Err(err) => return Err(err.to_string()),
    };

    let Some(update) = update else {
        return Ok(UpdateInstallResult {
            installed: false,
            version: None,
            message: "No signed update metadata has been published for this channel yet".into(),
        });
    };

    let _ = engine.engine.cleanup_audio_graph();
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|err| err.to_string())?;
    app.restart()
}

#[tauri::command]
fn open_release_page(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(RELEASES_URL, None::<String>)
        .map_err(|err| err.to_string())
}

#[tauri::command]
fn install_effect_plugins(
    engine: State<'_, EngineState>,
) -> Result<EffectPluginInstallResult, String> {
    let before = engine
        .engine
        .refresh_effect_availability()
        .map_err(|err| err.to_string())?;
    let missing_before = missing_effect_names(&before);
    if missing_before.is_empty() {
        return Ok(EffectPluginInstallResult {
            attempted: false,
            success: true,
            manager: detect_package_manager().id().into(),
            packages: Vec::new(),
            aur_packages: Vec::new(),
            missing_before,
            missing_after: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
            message: "All optional effect plugins are already installed and detected".into(),
        });
    }

    let missing_ids = missing_effect_ids(&before);
    let manager = detect_package_manager();
    if manager == PackageManager::Unknown {
        return Ok(EffectPluginInstallResult {
            attempted: false,
            success: false,
            manager: manager.id().into(),
            packages: Vec::new(),
            aur_packages: Vec::new(),
            missing_before,
            missing_after: missing_effect_names(&before),
            stdout: String::new(),
            stderr: String::new(),
            message: "No supported package manager was found. Install DeepFilterNet3, RNNoise, and SWH LADSPA packages manually.".into(),
        });
    }

    let (packages, aur_packages) = resolve_effect_plugin_packages(manager, &missing_ids);
    if packages.is_empty() && aur_packages.is_empty() {
        return Ok(EffectPluginInstallResult {
            attempted: false,
            success: false,
            manager: manager.id().into(),
            packages,
            aur_packages,
            missing_before,
            missing_after: missing_effect_names(&before),
            stdout: String::new(),
            stderr: String::new(),
            message: format!(
                "No known installable packages were found for {}. Install the missing effect plugins manually.",
                manager.id()
            ),
        });
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut command_failed = None;

    if !packages.is_empty() {
        match install_system_packages(manager, &packages) {
            Ok(output) => {
                append_output(&mut stdout, &mut stderr, output);
            }
            Err(err) => {
                command_failed = Some(err);
            }
        }
    }

    if command_failed.is_none() && !aur_packages.is_empty() {
        match install_aur_packages(&aur_packages) {
            Ok(output) => {
                append_output(&mut stdout, &mut stderr, output);
            }
            Err(err) => {
                command_failed = Some(err);
            }
        }
    }

    let after = engine
        .engine
        .refresh_effect_availability()
        .map_err(|err| err.to_string())?;
    let missing_after = missing_effect_names(&after);
    let success = command_failed.is_none() && missing_after.is_empty();
    let message = if let Some(err) = command_failed {
        format!("Effect plugin install did not complete: {err}")
    } else if missing_after.is_empty() {
        "Effect plugins installed and detected. Repair audio if a running FX chain needs the new plugins.".into()
    } else {
        format!(
            "Install finished, but WaveLinux still cannot verify: {}",
            missing_after.join(", ")
        )
    };

    Ok(EffectPluginInstallResult {
        attempted: true,
        success,
        manager: manager.id().into(),
        packages,
        aur_packages,
        missing_before,
        missing_after,
        stdout,
        stderr,
        message,
    })
}

fn tauri_result<T>(result: Result<T, EngineError>) -> Result<T, String> {
    result.map_err(|err| err.to_string())
}

fn ensure_elgato_wave_xlr_detected(engine: &WaveLinuxEngine) -> Result<(), String> {
    let state = engine.get_state().map_err(|err| err.to_string())?;
    let detected = elgato::summarize_devices(state.graph.inputs.iter(), state.graph.outputs.iter())
        .into_iter()
        .any(|device| device.controls_supported);
    if detected {
        Ok(())
    } else {
        Err("Elgato Wave XLR controls are unavailable because no supported Elgato device is detected".into())
    }
}

fn missing_effect_ids(availability: &[EffectAvailability]) -> Vec<String> {
    availability
        .iter()
        .filter(|effect| !effect.available)
        .map(|effect| effect.effect_id.clone())
        .collect()
}

fn missing_effect_names(availability: &[EffectAvailability]) -> Vec<String> {
    let catalog = EffectCatalog::default();
    availability
        .iter()
        .filter(|effect| !effect.available)
        .map(|effect| {
            catalog
                .effects
                .iter()
                .find(|definition| definition.id == effect.effect_id)
                .map(|definition| definition.name.clone())
                .unwrap_or_else(|| effect.effect_id.clone())
        })
        .collect()
}

fn resolve_effect_plugin_packages(
    manager: PackageManager,
    missing_ids: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut packages = Vec::new();
    let mut aur_packages = Vec::new();

    if missing_ids.iter().any(|id| id == "deepfilternet") {
        match manager {
            PackageManager::Apt => {
                push_first_available_package(
                    manager,
                    &mut packages,
                    &["deepfilternet-ladspa", "deepfilternet"],
                );
            }
            PackageManager::Dnf | PackageManager::Zypper => {
                push_first_available_package(manager, &mut packages, &["deepfilternet"]);
            }
            PackageManager::Pacman => {
                push_first_available_package(
                    manager,
                    &mut packages,
                    &["deepfilternet-plugin-pipewire-bin"],
                );
                if packages
                    .iter()
                    .all(|package| package != "deepfilternet-plugin-pipewire-bin")
                {
                    push_first_available_aur_package(
                        &mut aur_packages,
                        &[
                            "deepfilternet-plugin-pipewire-bin",
                            "deepfilternet-ladspa",
                            "deepfilternet",
                        ],
                    );
                }
            }
            PackageManager::Unknown => {}
        }
    }

    if missing_ids.iter().any(|id| id == "rnnoise") {
        match manager {
            PackageManager::Apt => {
                push_first_available_package(
                    manager,
                    &mut packages,
                    &["librnnoise-ladspa", "noise-suppression-for-voice"],
                );
            }
            PackageManager::Dnf => {
                push_first_available_package(
                    manager,
                    &mut packages,
                    &["noise-suppression-for-voice", "rnnoise"],
                );
            }
            PackageManager::Pacman => {
                push_first_available_package(
                    manager,
                    &mut packages,
                    &["noise-suppression-for-voice"],
                );
                if packages
                    .iter()
                    .all(|package| package != "noise-suppression-for-voice")
                {
                    push_first_available_aur_package(
                        &mut aur_packages,
                        &["noise-suppression-for-voice"],
                    );
                }
            }
            PackageManager::Zypper => {
                push_first_available_package(manager, &mut packages, &["rnnoise"]);
            }
            PackageManager::Unknown => {}
        }
    }

    if missing_ids
        .iter()
        .any(|id| matches!(id.as_str(), "compressor" | "gate" | "limiter"))
    {
        match manager {
            PackageManager::Apt => push_first_available_package(
                manager,
                &mut packages,
                &["swh-plugins", "lsp-plugins-ladspa"],
            ),
            PackageManager::Dnf | PackageManager::Zypper => push_first_available_package(
                manager,
                &mut packages,
                &["ladspa-swh-plugins", "lsp-plugins-ladspa"],
            ),
            PackageManager::Pacman => {
                push_first_available_package(manager, &mut packages, &["swh-plugins"]);
            }
            PackageManager::Unknown => {}
        }
    }

    (packages, aur_packages)
}

fn push_first_available_package(
    manager: PackageManager,
    packages: &mut Vec<String>,
    candidates: &[&str],
) {
    if let Some(package) = candidates
        .iter()
        .find(|package| package_available(manager, package))
    {
        push_unique(packages, package);
    }
}

fn push_first_available_aur_package(packages: &mut Vec<String>, candidates: &[&str]) {
    if let Some(package) = candidates
        .iter()
        .find(|package| aur_package_available(package))
    {
        push_unique(packages, package);
    }
}

fn push_unique(packages: &mut Vec<String>, package: &str) {
    if packages.iter().all(|item| item != package) {
        packages.push(package.into());
    }
}

fn detect_package_manager() -> PackageManager {
    if command_exists("apt-get") {
        PackageManager::Apt
    } else if command_exists("dnf") {
        PackageManager::Dnf
    } else if command_exists("pacman") {
        PackageManager::Pacman
    } else if command_exists("zypper") {
        PackageManager::Zypper
    } else {
        PackageManager::Unknown
    }
}

fn package_available(manager: PackageManager, package: &str) -> bool {
    let (program, args): (&str, Vec<&str>) = match manager {
        PackageManager::Apt => ("apt-cache", vec!["show", package]),
        PackageManager::Dnf => ("dnf", vec!["-q", "info", package]),
        PackageManager::Pacman => ("pacman", vec!["-Si", package]),
        PackageManager::Zypper => (
            "zypper",
            vec!["--non-interactive", "search", "--exact-match", package],
        ),
        PackageManager::Unknown => return false,
    };
    command_status_success(program, &args)
}

fn aur_package_available(package: &str) -> bool {
    if command_exists("paru") {
        command_status_success("paru", &["-Si", package])
    } else if command_exists("yay") {
        command_status_success("yay", &["-Si", package])
    } else {
        false
    }
}

fn install_system_packages(
    manager: PackageManager,
    packages: &[String],
) -> Result<Vec<Output>, String> {
    let mut outputs = Vec::new();
    match manager {
        PackageManager::Apt => {
            outputs.push(run_privileged_command("apt-get", &["update".into()])?);
            let mut args = vec!["install".into(), "-y".into()];
            args.extend(packages.iter().cloned());
            outputs.push(run_privileged_command("apt-get", &args)?);
        }
        PackageManager::Dnf => {
            let mut args = vec!["install".into(), "-y".into()];
            args.extend(packages.iter().cloned());
            outputs.push(run_privileged_command("dnf", &args)?);
        }
        PackageManager::Pacman => {
            let mut args = vec!["-S".into(), "--needed".into(), "--noconfirm".into()];
            args.extend(packages.iter().cloned());
            outputs.push(run_privileged_command("pacman", &args)?);
        }
        PackageManager::Zypper => {
            let mut args = vec![
                "--non-interactive".into(),
                "install".into(),
                "--no-recommends".into(),
            ];
            args.extend(packages.iter().cloned());
            outputs.push(run_privileged_command("zypper", &args)?);
        }
        PackageManager::Unknown => {}
    }
    Ok(outputs)
}

fn install_aur_packages(packages: &[String]) -> Result<Vec<Output>, String> {
    let helper = if command_exists("paru") {
        "paru"
    } else if command_exists("yay") {
        "yay"
    } else {
        return Err("No AUR helper found for DeepFilterNet3. Install paru or yay, or install deepfilternet-plugin-pipewire-bin manually.".into());
    };

    let mut args = vec!["-S".into(), "--needed".into(), "--noconfirm".into()];
    args.extend(packages.iter().cloned());
    Ok(vec![run_command_capture(helper, &args)?])
}

fn run_privileged_command(program: &str, args: &[String]) -> Result<Output, String> {
    if running_as_root() {
        return run_command_capture(program, args);
    }
    if command_exists("pkexec") {
        let mut pkexec_args = vec![program.to_string()];
        pkexec_args.extend(args.iter().cloned());
        return run_command_capture("pkexec", &pkexec_args);
    }
    if command_exists("sudo") {
        let mut sudo_args = vec!["-n".to_string(), program.to_string()];
        sudo_args.extend(args.iter().cloned());
        return run_command_capture("sudo", &sudo_args);
    }
    Err("No pkexec or sudo command is available for privileged package installation".into())
}

fn run_command_capture(program: &str, args: &[String]) -> Result<Output, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("{program} failed to start: {err}"))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{} {} exited with status {}: {}",
            program,
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn command_status_success(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

fn running_as_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn append_output(stdout: &mut String, stderr: &mut String, outputs: Vec<Output>) {
    for output in outputs {
        stdout.push_str(&String::from_utf8_lossy(&output.stdout));
        stderr.push_str(&String::from_utf8_lossy(&output.stderr));
    }
}

fn ui_theme_preference_path(app: &AppHandle) -> Result<PathBuf, String> {
    let config_dir = app.path().app_config_dir().map_err(|err| err.to_string())?;
    fs::create_dir_all(&config_dir).map_err(|err| err.to_string())?;
    Ok(config_dir.join(UI_THEME_PREFERENCE_FILE))
}

fn ui_themes_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let config_dir = app.path().app_config_dir().map_err(|err| err.to_string())?;
    let theme_dir = config_dir.join(UI_THEMES_DIR);
    fs::create_dir_all(&theme_dir).map_err(|err| err.to_string())?;
    Ok(theme_dir)
}

fn built_in_theme_ids() -> BTreeSet<String> {
    [
        "wavelink2",
        "wavelink3",
        "wavelink3_dark",
        "classic",
        "wavelink",
        "wavelink_dark",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn default_theme_variant() -> String {
    "custom".into()
}

fn normalize_ui_theme(theme: UiThemeDefinition) -> Result<UiThemeDefinition, String> {
    let id = clean_ui_theme_id(&theme.id)?;
    if built_in_theme_ids().contains(&id) {
        return Err("custom UI theme cannot replace a built-in theme".into());
    }
    let name = clean_ui_theme_name(&theme.name)?;
    let surface = match theme.surface.as_str() {
        "wavelink2" | "classic" => "wavelink2".into(),
        "wavelink3" | "wavelink" => "wavelink3".into(),
        _ => return Err("theme surface must be wavelink2 or wavelink3".into()),
    };
    let variant = match theme.variant.as_str() {
        "light" | "dark" | "custom" => theme.variant,
        _ => "custom".into(),
    };
    let mut tokens = BTreeMap::new();
    for (key, value) in theme.tokens {
        if !valid_theme_token_key(&key) {
            return Err(format!("unsupported theme token: {key}"));
        }
        if value.len() > 120 {
            return Err(format!("theme token {key} is too long"));
        }
        tokens.insert(key, value);
    }
    Ok(UiThemeDefinition {
        id,
        name,
        surface,
        variant,
        tokens,
    })
}

fn normalize_ui_theme_id(value: &str) -> String {
    match value.trim() {
        "classic" => "wavelink2".into(),
        "wavelink" => "wavelink3".into(),
        "wavelink_dark" => "wavelink3_dark".into(),
        value => value.to_string(),
    }
}

fn clean_ui_theme_id(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    let valid_length = (2..=41).contains(&trimmed.len());
    let valid_first = trimmed
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
    let valid_chars = trimmed
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_');
    if valid_length && valid_first && valid_chars {
        Ok(trimmed.to_string())
    } else {
        Err("invalid UI theme id".into())
    }
}

fn clean_ui_theme_name(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("theme name is required".into());
    }
    Ok(trimmed.chars().take(80).collect())
}

fn valid_theme_token_key(value: &str) -> bool {
    value.strip_prefix("--wl-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    })
}

fn update_endpoint(settings: &MixerSettings) -> String {
    match release_channel_name(settings) {
        "beta" => std::env::var("WAVELINUX_BETA_UPDATE_ENDPOINT")
            .or_else(|_| std::env::var("WAVELINUX_UPDATE_ENDPOINT"))
            .unwrap_or_else(|_| BETA_UPDATE_ENDPOINT.into()),
        _ => std::env::var("WAVELINUX_STABLE_UPDATE_ENDPOINT")
            .or_else(|_| std::env::var("WAVELINUX_UPDATE_ENDPOINT"))
            .unwrap_or_else(|_| STABLE_UPDATE_ENDPOINT.into()),
    }
}

fn current_update_version() -> semver::Version {
    let version = build_release_tag()
        .map(|tag| tag.trim_start_matches('v'))
        .unwrap_or(env!("CARGO_PKG_VERSION"));

    semver::Version::parse(version)
        .or_else(|_| semver::Version::parse(env!("CARGO_PKG_VERSION")))
        .expect("package version is valid semver")
}

fn build_release_tag() -> Option<&'static str> {
    option_env!("WAVELINUX_RELEASE_TAG")
        .or(option_env!("GITHUB_REF_NAME"))
        .filter(|tag| !tag.trim().is_empty())
}

fn release_channel_name(settings: &MixerSettings) -> &'static str {
    match &settings.release_channel {
        ReleaseChannel::Beta => "beta",
        ReleaseChannel::Stable => "stable",
    }
}

fn is_appimage_install() -> bool {
    std::env::var_os("APPIMAGE").is_some()
}

fn is_missing_update_metadata_error(message: &str) -> bool {
    message.contains("Could not fetch a valid release JSON")
        || message.contains("ReleaseNotFound")
        || message.contains("status code 404")
}

fn shutdown_audio_graph(engine: &WaveLinuxEngine, shutdown_started: &AtomicBool) {
    if shutdown_started.swap(true, Ordering::SeqCst) {
        return;
    }
    engine.stop_background();
    let _ = engine.cleanup_audio_graph();
}

fn show_main_window(app: &AppHandle) {
    let window = app.get_webview_window("main").or_else(|| {
        WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title(format!("WaveLinux {}", env!("CARGO_PKG_VERSION")))
            .inner_size(1280.0, 820.0)
            .min_inner_size(960.0, 640.0)
            .resizable(true)
            .build()
            .ok()
    });
    if let Some(window) = window {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn acquire_process_lock() -> std::io::Result<Option<ProcessLock>> {
    let lock_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let lock_path = lock_dir.join("wavelinux-4.lock");
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(error);
    }

    file.set_len(0)?;
    writeln!(file, "{}", std::process::id())?;
    Ok(Some(ProcessLock { _file: file }))
}

fn build_tray(
    app: &AppHandle,
    engine: Arc<WaveLinuxEngine>,
    shutdown_started: Arc<AtomicBool>,
    allow_exit: Arc<AtomicBool>,
) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show WaveLinux", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let icon = Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;
    let tooltip = format!("WaveLinux {}", env!("CARGO_PKG_VERSION"));

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip(&tooltip)
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "show" => {
                show_main_window(app);
            }
            "quit" => {
                allow_exit.store(true, Ordering::SeqCst);
                shutdown_audio_graph(&engine, &shutdown_started);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn run_hardware_profile_prewarm() -> i32 {
    match prewarm_hardware_profiles_from_xdg() {
        Ok(report) => {
            print_hardware_profile_prewarm_report(&report);
            0
        }
        Err(err) => {
            eprintln!("WaveLinux hardware profile prewarm failed: {err}");
            1
        }
    }
}

fn print_hardware_profile_prewarm_report(report: &HardwareProfilePrewarmReport) {
    println!(
        "WaveLinux hardware profile prewarm: devices={} matched={} fetched={} diagnostics={}",
        report.devices,
        report.matched,
        report.fetched,
        report.diagnostics.len()
    );
    for diagnostic in &report.diagnostics {
        eprintln!(
            "[{:?}] {}: {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
    }
}

fn main() {
    if std::env::args().any(|arg| {
        matches!(
            arg.as_str(),
            "--prewarm-hardware-profiles" | "--check-hardware-profiles"
        )
    }) {
        std::process::exit(run_hardware_profile_prewarm());
    }

    let shutdown_started = Arc::new(AtomicBool::new(false));
    let allow_exit = Arc::new(AtomicBool::new(false));
    let run_allow_exit = Arc::clone(&allow_exit);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_state,
            observe_state,
            observe_meters,
            create_mix,
            rename_mix,
            move_mix,
            delete_mix,
            set_mix_volume,
            set_mix_mute,
            set_mix_icon,
            set_channel_icon,
            set_mix_monitor_output,
            set_mix_outputs,
            create_channel,
            rename_channel,
            move_channel,
            delete_channel,
            set_channel_linked,
            set_channel_input,
            set_hardware_input_device,
            set_channel_input_mode,
            set_channel_bus_enabled,
            set_settings,
            list_hardware_profiles,
            set_device_hardware_profile,
            set_fallback_hardware_profile,
            set_hardware_profile_policy,
            list_streamer_devices,
            get_streamer_bindings,
            set_streamer_device_enabled,
            set_streamer_binding_profile,
            learn_streamer_control,
            run_streamer_action_test,
            list_elgato_devices,
            read_elgato_wave_xlr,
            set_elgato_wave_xlr_gain,
            set_elgato_wave_xlr_mute,
            set_elgato_wave_xlr_hp_volume_db,
            set_elgato_wave_xlr_low_impedance,
            set_channel_volume,
            set_channel_mute,
            assign_app_to_channel,
            remove_app_route,
            set_app_volume_preset,
            remove_app_volume_preset,
            forget_app,
            restore_app,
            pin_app_identity,
            merge_app_identity,
            reset_app_identity,
            move_app_stream,
            move_app_stream_to_default,
            set_app_stream_volume,
            set_app_stream_mute,
            set_effect_chain,
            set_effect_param,
            bypass_effect,
            run_sound_check,
            run_diagnostics,
            get_graph_debug_report,
            cleanup_stale_audio_graph,
            restore_device,
            get_ui_theme_preference,
            set_ui_theme_preference,
            list_ui_themes,
            open_ui_theme_folder,
            check_for_updates,
            install_update,
            open_release_page,
            install_effect_plugins,
        ])
        .on_window_event(move |window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building WaveLinux");

    let Some(_process_lock) =
        acquire_process_lock().expect("failed to acquire WaveLinux process lock")
    else {
        eprintln!("WaveLinux is already running; refusing to start a duplicate audio engine");
        return;
    };

    let engine = WaveLinuxEngine::from_xdg().expect("failed to start WaveLinux engine");
    let background = engine.spawn_background();
    let streamer_runtime = streamer_devices::StreamerDeviceRuntime::start(Arc::clone(&engine));
    let run_engine = Arc::clone(&engine);
    let run_shutdown = Arc::clone(&shutdown_started);
    app.manage(EngineState {
        engine: Arc::clone(&engine),
    });
    build_tray(
        app.handle(),
        Arc::clone(&engine),
        Arc::clone(&shutdown_started),
        Arc::clone(&allow_exit),
    )
    .expect("failed to build WaveLinux tray");

    app.run(move |_app, event| match event {
        tauri::RunEvent::ExitRequested { api, .. } if !run_allow_exit.load(Ordering::SeqCst) => {
            api.prevent_exit();
        }
        tauri::RunEvent::Exit => {
            shutdown_audio_graph(&run_engine, &run_shutdown);
        }
        _ => {}
    });

    drop(streamer_runtime);
    engine.stop_background();
    let _ = background.join();
    let _ = engine.cleanup_audio_graph();
}
