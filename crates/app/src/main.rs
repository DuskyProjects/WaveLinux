#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State};
use wavelinux_engine::{EngineError, SoundCheckReport, WaveLinuxEngine};
use wavelinux_model::{
    AppMatcher, AppRoute, AppStateSnapshot, Channel, ChannelKind, EffectInstance, Mix, MixBus,
    MixerSettings, Scene,
};

struct EngineState {
    engine: Arc<WaveLinuxEngine>,
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
fn create_mix(engine: State<'_, EngineState>, name: String) -> Result<Mix, String> {
    tauri_result(engine.engine.create_mix(name))
}

#[tauri::command]
fn rename_mix(engine: State<'_, EngineState>, mix_id: String, name: String) -> Result<Mix, String> {
    tauri_result(engine.engine.rename_mix(mix_id, name))
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
fn move_app_stream(
    engine: State<'_, EngineState>,
    stream_id: String,
    channel_id: String,
) -> Result<wavelinux_engine::CommandExecution, String> {
    tauri_result(engine.engine.move_app_stream(stream_id, channel_id))
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
fn save_scene(engine: State<'_, EngineState>, name: String) -> Result<Scene, String> {
    tauri_result(engine.engine.save_scene(name))
}

#[tauri::command]
fn load_scene(engine: State<'_, EngineState>, scene_id: String) -> Result<Scene, String> {
    tauri_result(engine.engine.load_scene(scene_id))
}

#[tauri::command]
fn list_scenes(engine: State<'_, EngineState>) -> Result<Vec<Scene>, String> {
    tauri_result(engine.engine.list_scenes())
}

fn tauri_result<T>(result: Result<T, EngineError>) -> Result<T, String> {
    result.map_err(|err| err.to_string())
}

fn build_tray(app: &AppHandle, engine: Arc<WaveLinuxEngine>) -> tauri::Result<()> {
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
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => {
                engine.stop_background();
                let _ = engine.cleanup_audio_graph();
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
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;
    Ok(())
}

fn main() {
    let engine = WaveLinuxEngine::from_xdg().expect("failed to start WaveLinux engine");
    let background = engine.spawn_background();
    let tray_engine = Arc::clone(&engine);

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .manage(EngineState {
            engine: Arc::clone(&engine),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            observe_state,
            create_mix,
            rename_mix,
            delete_mix,
            set_mix_volume,
            set_mix_mute,
            set_mix_monitor_output,
            create_channel,
            rename_channel,
            delete_channel,
            set_channel_linked,
            set_channel_input,
            set_settings,
            set_channel_volume,
            set_channel_mute,
            assign_app_to_channel,
            move_app_stream,
            set_app_stream_volume,
            set_app_stream_mute,
            set_effect_chain,
            set_effect_param,
            bypass_effect,
            run_sound_check,
            run_diagnostics,
            repair_audio_graph,
            cleanup_audio_graph,
            save_scene,
            load_scene,
            list_scenes,
        ])
        .setup(move |app| {
            build_tray(app.handle(), Arc::clone(&tray_engine))?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running WaveLinux");

    engine.stop_background();
    let _ = background.join();
}
