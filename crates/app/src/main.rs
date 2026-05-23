#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use time::format_description::well_known::Rfc3339;
use wavelinux_engine::{EngineError, GraphDebugReport, SoundCheckReport, WaveLinuxEngine};
use wavelinux_model::{
    AppMatcher, AppRoute, AppStateSnapshot, AppVolumePreset, Channel, ChannelInputMode,
    ChannelKind, ConfigBackup, EffectInstance, KnownApp, LevelMeter, Mix, MixBus, MixerConfig,
    MixerSettings, Scene, SetupTemplate,
};

struct EngineState {
    engine: Arc<WaveLinuxEngine>,
}

struct ProcessLock {
    _file: File,
}

const RELEASES_URL: &str = "https://github.com/DuskyProjects/WaveLinux/releases";
const STABLE_UPDATE_ENDPOINT: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/latest/download/latest.json";
const BETA_UPDATE_ENDPOINT: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/download/prerelease/latest.json";

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
fn set_mix_monitor_output(
    engine: State<'_, EngineState>,
    mix_id: String,
    output: Option<String>,
) -> Result<Mix, String> {
    tauri_result(engine.engine.set_mix_monitor_output(mix_id, output))
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
fn set_settings(
    engine: State<'_, EngineState>,
    settings: MixerSettings,
) -> Result<MixerSettings, String> {
    tauri_result(engine.engine.set_settings(settings))
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
fn repair_audio_graph(
    engine: State<'_, EngineState>,
) -> Result<wavelinux_engine::RepairReport, String> {
    tauri_result(engine.engine.repair_audio_graph())
}

#[tauri::command]
fn cleanup_audio_graph(
    engine: State<'_, EngineState>,
) -> Result<Vec<wavelinux_engine::CommandExecution>, String> {
    tauri_result(engine.engine.cleanup_audio_graph())
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
fn save_scene(engine: State<'_, EngineState>, name: String) -> Result<Scene, String> {
    tauri_result(engine.engine.save_scene(name))
}

#[tauri::command]
fn import_scene(engine: State<'_, EngineState>, scene: Scene) -> Result<Scene, String> {
    tauri_result(engine.engine.import_scene(scene))
}

#[tauri::command]
fn export_backup(engine: State<'_, EngineState>) -> Result<ConfigBackup, String> {
    tauri_result(engine.engine.export_backup())
}

#[tauri::command]
fn import_backup(
    engine: State<'_, EngineState>,
    backup: ConfigBackup,
) -> Result<ConfigBackup, String> {
    tauri_result(engine.engine.import_backup(backup))
}

#[tauri::command]
fn load_scene(engine: State<'_, EngineState>, scene_id: String) -> Result<Scene, String> {
    tauri_result(engine.engine.load_scene(scene_id))
}

#[tauri::command]
fn delete_scene(engine: State<'_, EngineState>, scene_id: String) -> Result<Scene, String> {
    tauri_result(engine.engine.delete_scene(scene_id))
}

#[tauri::command]
fn list_scenes(engine: State<'_, EngineState>) -> Result<Vec<Scene>, String> {
    tauri_result(engine.engine.list_scenes())
}

#[tauri::command]
fn list_setup_templates(engine: State<'_, EngineState>) -> Vec<SetupTemplate> {
    engine.engine.list_setup_templates()
}

#[tauri::command]
fn apply_setup_template(
    engine: State<'_, EngineState>,
    template_id: String,
) -> Result<SetupTemplate, String> {
    tauri_result(engine.engine.apply_setup_template(template_id))
}

#[tauri::command]
async fn check_for_updates(
    app: AppHandle,
    engine: State<'_, EngineState>,
) -> Result<UpdateInfo, String> {
    let settings = engine
        .engine
        .get_state()
        .map_err(|err| err.to_string())?
        .config
        .settings;
    let endpoint = update_endpoint(&settings);
    let endpoint_url = endpoint
        .parse::<url::Url>()
        .map_err(|err| err.to_string())?;
    let updater = app
        .updater_builder()
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
                current_version: env!("CARGO_PKG_VERSION").to_string(),
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
                current_version: update.current_version,
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
            current_version: env!("CARGO_PKG_VERSION").to_string(),
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
) -> Result<UpdateInstallResult, String> {
    if !is_appimage_install() {
        return Err(
            "Self-update is available for AppImage installs. Use deb, rpm, or AUR updates through your package manager."
                .into(),
        );
    }

    let settings = engine
        .engine
        .get_state()
        .map_err(|err| err.to_string())?
        .config
        .settings;
    let endpoint = update_endpoint(&settings);
    let endpoint_url = endpoint
        .parse::<url::Url>()
        .map_err(|err| err.to_string())?;
    let updater = app
        .updater_builder()
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

fn tauri_result<T>(result: Result<T, EngineError>) -> Result<T, String> {
    result.map_err(|err| err.to_string())
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

fn release_channel_name(settings: &MixerSettings) -> &'static str {
    match settings.release_channel {
        wavelinux_model::ReleaseChannel::Beta => "beta",
        wavelinux_model::ReleaseChannel::Stable => "stable",
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
            .title("WaveLinux 4.0")
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

    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("WaveLinux 4.0")
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

fn main() {
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
            set_mix_monitor_output,
            create_channel,
            rename_channel,
            move_channel,
            delete_channel,
            set_channel_linked,
            set_channel_input,
            set_hardware_input_device,
            set_channel_input_mode,
            set_settings,
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
            repair_audio_graph,
            cleanup_audio_graph,
            cleanup_stale_audio_graph,
            restore_device,
            save_scene,
            import_scene,
            export_backup,
            import_backup,
            load_scene,
            delete_scene,
            list_scenes,
            list_setup_templates,
            apply_setup_template,
            check_for_updates,
            install_update,
            open_release_page,
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

    engine.stop_background();
    let _ = background.join();
    let _ = engine.cleanup_audio_graph();
}
