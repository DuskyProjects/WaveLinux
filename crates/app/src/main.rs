#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::backtrace::Backtrace;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;
use time::format_description::well_known::Rfc3339;
use wavelinux_app::{elgato, peripheral_protocol::ElgatoCommand, streamer_devices};
use wavelinux_engine::{
    prewarm_hardware_profiles_from_xdg, EngineError, GraphDebugReport,
    HardwareProfilePrewarmReport, SoundCheckReport, WaveLinuxEngine,
};
use wavelinux_model::{
    app_display_name, graph_prefix, AppMatcher, AppRoute, AppStateSnapshot, AppVolumePreset,
    Channel, ChannelInputMode, ChannelKind, EffectInstance, FallbackHardwareProfile,
    HardwareProfileUiState, KnownApp, LatencyPolicy, LevelMeter, Mix, MixBus, MixerConfig,
    MixerSettings, ReleaseChannel, RoutingPolicy, StreamerAction, StreamerActionResult,
    StreamerBindingProfile, StreamerDeviceSummary, StreamerDevicesConfig, StreamerLearnResult,
    StreamerPermissionStatus,
};

struct EngineState {
    engine: Arc<WaveLinuxEngine>,
    meter_streaming_requested: Arc<AtomicBool>,
    meter_streaming: Arc<AtomicBool>,
    operation_revision: AtomicU64,
    streamer_runtime: Arc<streamer_devices::StreamerRuntimeController>,
}

const STATE_DELTA_EVENT: &str = "wavelinux://state-delta";
const METERS_EVENT: &str = "wavelinux://meters";
const OPERATION_EVENT: &str = "wavelinux://operation";
const OPERATION_PROTOCOL_VERSION: u16 = 1;
const UI_EVENT_WAIT: Duration = Duration::from_secs(1);
const IDLE_METER_INTERVAL: Duration = Duration::from_millis(250);
const METER_RECONNECT_INTERVAL: Duration = Duration::from_millis(500);
const METER_FALLBACK_INTERVAL: Duration = Duration::from_millis(500);
const METER_EVENT_MIN_DELTA: f32 = 0.004;

#[derive(Debug, Clone, Serialize)]
struct StateDeltaEvent {
    revision: u64,
    config_revision: u64,
    graph_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<wavelinux_model::MixerConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<wavelinux_model::RuntimeGraph>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<Vec<wavelinux_model::Diagnostic>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<wavelinux_model::EngineStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    catalog: Option<wavelinux_model::EffectCatalog>,
}

#[derive(Debug, Clone, Serialize)]
struct MetersEvent {
    revision: u64,
    meters: Vec<LevelMeter>,
}

#[derive(Debug, Clone, Serialize)]
struct OperationEvent {
    protocol_version: u16,
    revision: u64,
    request_id: String,
    command: &'static str,
    status: &'static str,
    state_revision: u64,
    config_revision: u64,
    graph_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct OperationResponse<T> {
    protocol_version: u16,
    revision: u64,
    request_id: String,
    command: &'static str,
    status: &'static str,
    state_revision: u64,
    config_revision: u64,
    graph_revision: u64,
    value: T,
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
const STABLE_RELEASE_URL: &str = "https://github.com/DuskyProjects/WaveLinux/releases/latest";
const BETA_RELEASE_URL: &str = "https://github.com/DuskyProjects/WaveLinux/releases/tag/prerelease";
const STABLE_UPDATE_ENDPOINT: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/latest/download/latest.json";
const BETA_UPDATE_ENDPOINT: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/download/prerelease/latest.json";
const UI_THEME_PREFERENCE_FILE: &str = "ui-theme.json";
const UI_THEMES_DIR: &str = "themes";
const WEBKIT_DMABUF_DISABLE_ENV: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
const WEBKIT_COMPOSITING_DISABLE_ENV: &str = "WEBKIT_DISABLE_COMPOSITING_MODE";
const WEBKIT_SANDBOX_DISABLE_ENV: &str = "WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS";
const WEBKIT_WORKAROUNDS_DISABLE_ENV: &str = "WAVELINUX_DISABLE_WEBKIT_WORKAROUNDS";
const WEBKIT_SANDBOX_KEEP_ENV: &str = "WAVELINUX_KEEP_WEBKIT_SANDBOX";
const TOKIO_WORKER_THREADS_ENV: &str = "TOKIO_WORKER_THREADS";
const DEFAULT_TOKIO_WORKER_THREADS: &str = "4";
const RUNTIME_INSTALL_SKIP_ENV: &str = "WAVELINUX_SKIP_RUNTIME_INSTALL";
const RUNTIME_INSTALL_FORCE_ENV: &str = "WAVELINUX_INSTALL_RUNTIME_ON_START";
const RUNTIME_DEPS_ASSUME_ENV: &str = "WAVELINUX_ASSUME_RUNTIME_DEPS";
const RUNTIME_DEPENDENCY_HELPER_ENV: &str = "WAVELINUX_RUNTIME_DEPENDENCY_HELPER";
const AUDIO_SERVICE_START_SKIP_ENV: &str = "WAVELINUX_SKIP_AUDIO_SERVICE_START";
const AUDIO_DAEMON_FALLBACK_DISABLE_ENV: &str = "WAVELINUX_DISABLE_AUDIO_DAEMON_FALLBACK";
const HOST_COMMAND_ENV_REMOVE: &[&str] = &[
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "CEF_PATH",
    "CEF_ROOT",
    "GDK_BACKEND",
    "GDK_PIXBUF_MODULE_FILE",
    "GIO_EXTRA_MODULES",
    "GIO_MODULE_DIR",
    "GI_TYPELIB_PATH",
    "GSETTINGS_SCHEMA_DIR",
    "GST_PLUGIN_PATH",
    "GST_PLUGIN_PATH_1_0",
    "GST_PLUGIN_SCANNER",
    "GST_PLUGIN_SCANNER_1_0",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "GST_PTP_HELPER_1_0",
    "GST_REGISTRY_REUSE_PLUGIN_SCANNER",
    "GTK_DATA_PREFIX",
    "GTK_EXE_PREFIX",
    "GTK_IM_MODULE_FILE",
    "GTK_PATH",
    "GTK_THEME",
    "LD_AUDIT",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "LIBRARY_PATH",
    "PERLLIB",
    "PYTHONHOME",
    "PYTHONPATH",
    "QT_PLUGIN_PATH",
    "WEBKIT_EXEC_PATH",
    "XDG_DATA_DIRS",
];
const PIPEWIRE_CLIENT_STACK_PROBES: &[(&str, &[&str])] = &[
    (
        "libpipewire-0.3.so.0",
        &[
            "/usr/lib/libpipewire-0.3.so.0",
            "/usr/lib64/libpipewire-0.3.so.0",
            "/usr/lib/x86_64-linux-gnu/libpipewire-0.3.so.0",
            "/usr/lib/aarch64-linux-gnu/libpipewire-0.3.so.0",
            "/usr/lib/arm-linux-gnueabihf/libpipewire-0.3.so.0",
        ],
    ),
    (
        "libpipewire-module-client-node.so",
        &[
            "/usr/lib/pipewire-0.3/libpipewire-module-client-node.so",
            "/usr/lib64/pipewire-0.3/libpipewire-module-client-node.so",
            "/usr/lib/x86_64-linux-gnu/pipewire-0.3/libpipewire-module-client-node.so",
            "/usr/lib/aarch64-linux-gnu/pipewire-0.3/libpipewire-module-client-node.so",
            "/usr/lib/arm-linux-gnueabihf/pipewire-0.3/libpipewire-module-client-node.so",
        ],
    ),
    (
        "libpipewire-module-protocol-native.so",
        &[
            "/usr/lib/pipewire-0.3/libpipewire-module-protocol-native.so",
            "/usr/lib64/pipewire-0.3/libpipewire-module-protocol-native.so",
            "/usr/lib/x86_64-linux-gnu/pipewire-0.3/libpipewire-module-protocol-native.so",
            "/usr/lib/aarch64-linux-gnu/pipewire-0.3/libpipewire-module-protocol-native.so",
            "/usr/lib/arm-linux-gnueabihf/pipewire-0.3/libpipewire-module-protocol-native.so",
        ],
    ),
    (
        "libspa-support.so",
        &[
            "/usr/lib/spa-0.2/support/libspa-support.so",
            "/usr/lib64/spa-0.2/support/libspa-support.so",
            "/usr/lib/x86_64-linux-gnu/spa-0.2/support/libspa-support.so",
            "/usr/lib/aarch64-linux-gnu/spa-0.2/support/libspa-support.so",
            "/usr/lib/arm-linux-gnueabihf/spa-0.2/support/libspa-support.so",
        ],
    ),
    (
        "libspa-audioconvert.so",
        &[
            "/usr/lib/spa-0.2/audioconvert/libspa-audioconvert.so",
            "/usr/lib64/spa-0.2/audioconvert/libspa-audioconvert.so",
            "/usr/lib/x86_64-linux-gnu/spa-0.2/audioconvert/libspa-audioconvert.so",
            "/usr/lib/aarch64-linux-gnu/spa-0.2/audioconvert/libspa-audioconvert.so",
            "/usr/lib/arm-linux-gnueabihf/spa-0.2/audioconvert/libspa-audioconvert.so",
        ],
    ),
];
const WAYLAND_HOST_STACK_PROBES: &[(&str, &[&str])] = &[
    (
        "libwayland-client.so.0",
        &[
            "/usr/lib/libwayland-client.so.0",
            "/usr/lib64/libwayland-client.so.0",
            "/usr/lib/x86_64-linux-gnu/libwayland-client.so.0",
        ],
    ),
    (
        "libwayland-cursor.so.0",
        &[
            "/usr/lib/libwayland-cursor.so.0",
            "/usr/lib64/libwayland-cursor.so.0",
            "/usr/lib/x86_64-linux-gnu/libwayland-cursor.so.0",
        ],
    ),
    (
        "libwayland-egl.so.1",
        &[
            "/usr/lib/libwayland-egl.so.1",
            "/usr/lib64/libwayland-egl.so.1",
            "/usr/lib/x86_64-linux-gnu/libwayland-egl.so.1",
        ],
    ),
    (
        "libwayland-server.so.0",
        &[
            "/usr/lib/libwayland-server.so.0",
            "/usr/lib64/libwayland-server.so.0",
            "/usr/lib/x86_64-linux-gnu/libwayland-server.so.0",
        ],
    ),
];
fn prepare_appimage_bundled_runtime() {
    let Some(runtime_dir) = appimage_bundled_runtime_dir() else {
        return;
    };

    prepend_env_path("PATH", runtime_dir.join("bin"));
    prepend_env_path("LD_LIBRARY_PATH", runtime_dir.join("lib"));
    prepend_env_path("LADSPA_PATH", runtime_dir.join("lib/ladspa"));
}

fn appimage_bundled_runtime_dir() -> Option<PathBuf> {
    let appdir = std::env::var_os("APPDIR")
        .map(PathBuf::from)
        .or_else(appdir_from_current_exe);
    appdir
        .map(|path| path.join("usr/wavelinux-runtime"))
        .filter(|path| path.is_dir())
}

fn appdir_from_current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut current = exe.parent();
    while let Some(path) = current {
        if path.join("AppRun").is_file() && path.join("usr").is_dir() {
            return Some(path.to_path_buf());
        }
        current = path.parent();
    }
    None
}

fn prepend_env_path(key: &str, path: PathBuf) {
    if !path.is_dir() {
        return;
    }

    let current = std::env::var_os(key).unwrap_or_default();
    let mut paths = Vec::new();
    paths.push(path);
    if !current.is_empty() {
        paths.extend(std::env::split_paths(&current));
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var(key, joined);
    }
}

fn apply_webkit_runtime_defaults() {
    if std::env::var_os(WEBKIT_WORKAROUNDS_DISABLE_ENV).is_some() {
        return;
    }

    // WebKitGTK's DMA-BUF renderer can abort the WebProcess on some compositor/GPU stacks.
    // Set this before Tauri initializes WebKit so child processes inherit it.
    set_env_default(WEBKIT_DMABUF_DISABLE_ENV, "1");
    set_env_default(WEBKIT_COMPOSITING_DISABLE_ENV, "1");

    let missing_helpers = missing_webkit_sandbox_helpers();
    let session_bus = session_bus_path_status();
    if std::env::var_os(WEBKIT_SANDBOX_KEEP_ENV).is_none()
        && std::env::var_os(WEBKIT_SANDBOX_DISABLE_ENV).is_none()
        && (!missing_helpers.is_empty() || matches!(session_bus, Some((_, false))))
    {
        set_env_default(WEBKIT_SANDBOX_DISABLE_ENV, "1");
        eprintln!(
            "WaveLinux WebKit compatibility: disabled WebKit sandbox because required runtime pieces are missing or inaccessible."
        );
        if !missing_helpers.is_empty() {
            eprintln!(
                "WaveLinux WebKit compatibility: missing helpers: {}. On Arch/CachyOS install: sudo pacman -S --needed bubblewrap xdg-dbus-proxy",
                missing_helpers.join(", ")
            );
        }
        if let Some((path, false)) = session_bus {
            eprintln!(
                "WaveLinux WebKit compatibility: DBus session bus socket is not accessible: {path}"
            );
        }
    }
}

fn set_env_default(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        std::env::set_var(key, value);
    }
}

fn command_exists(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }

    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|directory| directory.join(program).is_file())
        })
        .unwrap_or(false)
}

fn path_exists_or_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn missing_pipewire_client_stack() -> Vec<&'static str> {
    missing_library_stack(PIPEWIRE_CLIENT_STACK_PROBES)
}

fn missing_wayland_host_stack() -> Vec<&'static str> {
    missing_library_stack(WAYLAND_HOST_STACK_PROBES)
}

fn missing_library_stack<'a>(probes: &'a [(&'a str, &'a [&'a str])]) -> Vec<&'a str> {
    probes
        .iter()
        .filter_map(|(name, candidates)| {
            candidates
                .iter()
                .all(|path| !path_exists_or_symlink(Path::new(path)))
                .then_some(*name)
        })
        .collect()
}

fn appimage_library_roots(appdir: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for path in [
        appdir.join("usr/lib"),
        appdir.join("usr/lib64"),
        appdir.join("usr/lib32"),
        appdir.join("lib"),
        appdir.join("lib64"),
        appdir.join("lib32"),
        appdir.join("usr/lib/x86_64-linux-gnu"),
        appdir.join("usr/lib/aarch64-linux-gnu"),
        appdir.join("usr/lib/arm-linux-gnueabihf"),
    ] {
        if path.is_dir() {
            roots.push(path);
        }
    }

    let usr_lib = appdir.join("usr/lib");
    if let Ok(entries) = fs::read_dir(usr_lib) {
        for path in entries.flatten().map(|entry| entry.path()) {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if path.is_dir() && name.contains("linux-gnu") && roots.iter().all(|root| root != &path)
            {
                roots.push(path);
            }
        }
    }

    roots
}

fn collect_entries_with_prefix(root: &Path, prefix: &str, output: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with(prefix) && path_exists_or_symlink(&path) {
            output.push(path.display().to_string());
        }
    }
}

fn push_existing_path(path: PathBuf, output: &mut Vec<String>) {
    if path_exists_or_symlink(&path) {
        output.push(path.display().to_string());
    }
}

fn appimage_bundled_pipewire_conflicts() -> Vec<String> {
    let Some(appdir) = std::env::var_os("APPDIR")
        .map(PathBuf::from)
        .or_else(appdir_from_current_exe)
    else {
        return Vec::new();
    };

    let mut conflicts = Vec::new();
    for root in appimage_library_roots(&appdir) {
        collect_entries_with_prefix(&root, "libpipewire-0.3.so", &mut conflicts);

        let gstreamer = root.join("gstreamer-1.0");
        collect_entries_with_prefix(&gstreamer, "libgstpipewire.so", &mut conflicts);

        push_existing_path(root.join("pipewire-0.3"), &mut conflicts);
        push_existing_path(root.join("spa-0.2"), &mut conflicts);
    }

    conflicts.sort();
    conflicts.dedup();
    conflicts
}

fn appimage_bundled_wayland_conflicts() -> Vec<String> {
    let Some(appdir) = std::env::var_os("APPDIR")
        .map(PathBuf::from)
        .or_else(appdir_from_current_exe)
    else {
        return Vec::new();
    };

    let mut conflicts = Vec::new();
    for root in appimage_library_roots(&appdir) {
        for prefix in [
            "libwayland-client.so",
            "libwayland-cursor.so",
            "libwayland-egl.so",
            "libwayland-server.so",
        ] {
            collect_entries_with_prefix(&root, prefix, &mut conflicts);
        }
    }

    conflicts.sort();
    conflicts.dedup();
    conflicts
}

fn missing_webkit_sandbox_helpers() -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !command_exists("bwrap") {
        missing.push("bwrap");
    } else if !bwrap_can_create_minimal_sandbox() {
        missing.push("bwrap usable sandbox");
    }
    if !command_exists("xdg-dbus-proxy") {
        missing.push("xdg-dbus-proxy");
    }
    missing
}

fn bwrap_can_create_minimal_sandbox() -> bool {
    host_command("bwrap")
        .args(["--ro-bind", "/", "/", "/usr/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn session_bus_path_status() -> Option<(String, bool)> {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok()?;
    let path = address
        .strip_prefix("unix:path=")
        .and_then(|value| value.split(',').next())
        .filter(|value| !value.is_empty())?;
    Some((path.to_string(), Path::new(path).exists()))
}

fn runtime_dependency_helper_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os(RUNTIME_DEPENDENCY_HELPER_ENV) {
        candidates.push(PathBuf::from(path));
        return candidates;
    }

    if let Some(runtime_dir) = appimage_bundled_runtime_dir() {
        candidates.push(runtime_dir.join("bin/check-dependencies.sh"));
    }
    candidates.push(PathBuf::from("/usr/lib/wavelinux6/check-dependencies.sh"));

    for root in std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .into_iter()
        .chain(std::env::current_dir().ok())
        .flat_map(|path| path.ancestors().map(Path::to_path_buf).collect::<Vec<_>>())
    {
        candidates.push(root.join("scripts/check-dependencies.sh"));
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

fn runtime_dependency_helper() -> Result<PathBuf, String> {
    let candidates = runtime_dependency_helper_candidates();
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .ok_or_else(|| {
            format!(
                "the authoritative dependency helper was not found; checked: {}",
                candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn run_runtime_dependency_helper(args: &[&str]) -> Result<Output, String> {
    let helper = runtime_dependency_helper()?;
    host_command("bash")
        .arg(&helper)
        .args(args)
        .output()
        .map_err(|err| format!("could not run {}: {err}", helper.display()))
}

fn run_runtime_dependency_helper_interactive(args: &[&str]) -> Result<(), String> {
    let helper = runtime_dependency_helper()?;
    let status = host_command("bash")
        .arg(&helper)
        .args(args)
        .status()
        .map_err(|err| format!("could not run {}: {err}", helper.display()))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("{} exited with status {status}", helper.display()))
}

fn dependency_output_text(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(stderr);
    }
    text
}

fn print_runtime_dependency_report() -> i32 {
    let helper_result = run_runtime_dependency_helper(&["--strict-runtime"]);
    let helper_ok = helper_result
        .as_ref()
        .is_ok_and(|output| output.status.success());
    match &helper_result {
        Ok(output) => {
            let text = dependency_output_text(output);
            if !text.is_empty() {
                println!("{text}");
            }
        }
        Err(err) => eprintln!("WaveLinux dependency check failed: {err}"),
    }

    let missing_helpers = missing_webkit_sandbox_helpers();
    let session_bus = session_bus_path_status();
    let missing_pipewire_stack = missing_pipewire_client_stack();
    let missing_wayland_stack = missing_wayland_host_stack();
    let appimage_pipewire_conflicts = appimage_bundled_pipewire_conflicts();
    let appimage_wayland_conflicts = appimage_bundled_wayland_conflicts();

    println!("WaveLinux application runtime diagnostics");
    println!(
        "Dependency helper: {}",
        runtime_dependency_helper()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|err| format!("unavailable ({err})"))
    );
    println!("AppImage runtime: {}", is_appimage_install());
    println!(
        "AppImage bundled runtime: {}",
        appimage_bundled_runtime_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "unavailable".into())
    );
    println!("bwrap available: {}", command_exists("bwrap"));
    println!(
        "xdg-dbus-proxy available: {}",
        command_exists("xdg-dbus-proxy")
    );
    match &session_bus {
        Some((path, exists)) => println!("DBus session bus: {path} exists={exists}"),
        None => println!("DBus session bus: unavailable or not a unix:path address"),
    }
    println!(
        "session: XDG_SESSION_TYPE={} DISPLAY={} WAYLAND_DISPLAY={}",
        std::env::var("XDG_SESSION_TYPE").unwrap_or_default(),
        std::env::var("DISPLAY").unwrap_or_default(),
        std::env::var("WAYLAND_DISPLAY").unwrap_or_default()
    );
    println!(
        "webkit env: {}={} {}={} {}={}",
        WEBKIT_DMABUF_DISABLE_ENV,
        std::env::var(WEBKIT_DMABUF_DISABLE_ENV).unwrap_or_default(),
        WEBKIT_COMPOSITING_DISABLE_ENV,
        std::env::var(WEBKIT_COMPOSITING_DISABLE_ENV).unwrap_or_default(),
        WEBKIT_SANDBOX_DISABLE_ENV,
        std::env::var(WEBKIT_SANDBOX_DISABLE_ENV).unwrap_or_default()
    );
    println!(
        "LADSPA_PATH: {}",
        std::env::var("LADSPA_PATH").unwrap_or_default()
    );

    if helper_ok {
        println!("Runtime packages: ok");
    } else {
        println!("Runtime packages: missing or dependency helper unavailable");
    }
    println!(
        "Arch runtime packages: {}",
        if helper_ok { "ok" } else { "check failed" }
    );

    if missing_pipewire_stack.is_empty() {
        println!("PipeWire client stack: ok");
    } else {
        println!(
            "PipeWire client stack missing: {}",
            missing_pipewire_stack.join(" ")
        );
    }

    if missing_wayland_stack.is_empty() {
        println!("Host Wayland stack: ok");
    } else {
        println!(
            "Host Wayland stack missing: {}",
            missing_wayland_stack.join(" ")
        );
    }

    if appimage_pipewire_conflicts.is_empty() {
        println!("AppImage PipeWire bundle: ok");
    } else {
        println!(
            "AppImage PipeWire bundle conflicts: {}",
            appimage_pipewire_conflicts.join(" ")
        );
    }

    if appimage_wayland_conflicts.is_empty() {
        println!("AppImage Wayland bundle: ok");
    } else {
        println!(
            "AppImage Wayland bundle conflicts: {}",
            appimage_wayland_conflicts.join(" ")
        );
    }

    println!("Standard effects: bundled in wavelinux6-audio-core");

    if !missing_helpers.is_empty() {
        println!(
            "WebKit sandbox helpers missing: {}",
            missing_helpers.join(" ")
        );
    }

    if helper_ok
        && missing_helpers.is_empty()
        && missing_pipewire_stack.is_empty()
        && missing_wayland_stack.is_empty()
        && appimage_pipewire_conflicts.is_empty()
        && appimage_wayland_conflicts.is_empty()
        && !matches!(session_bus, Some((_, false)))
    {
        0
    } else {
        1
    }
}

fn install_runtime_dependencies_from_cli() -> i32 {
    match run_runtime_dependency_helper_interactive(&["--install", "--strict-runtime"]) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("WaveLinux setup: dependency install failed: {err}");
            1
        }
    }
}

fn ensure_runtime_dependencies_before_ui() {
    if std::env::var_os(RUNTIME_DEPS_ASSUME_ENV).is_some()
        || std::env::var_os(RUNTIME_INSTALL_SKIP_ENV).is_some()
        || (!is_appimage_install() && std::env::var_os(RUNTIME_INSTALL_FORCE_ENV).is_none())
    {
        return;
    }

    let check = match run_runtime_dependency_helper(&["--strict-runtime"]) {
        Ok(output) if output.status.success() => return,
        Ok(output) => dependency_output_text(&output),
        Err(err) => {
            show_runtime_setup_message(
                "WaveLinux setup failed",
                &format!("WaveLinux cannot check its host dependencies: {err}"),
                RuntimeSetupMessageKind::Error,
            );
            std::process::exit(1);
        }
    };

    let prompt = format!(
        "WaveLinux needs host audio and desktop packages for this Linux install.\n\n{check}\n\nWaveLinux will request administrator permission only for those system packages."
    );

    if !confirm_runtime_dependency_install(&prompt) {
        show_runtime_setup_message(
            "WaveLinux setup cancelled",
            "Dependency installation was cancelled. Run the WaveLinux installer again from a terminal for package-manager details.",
            RuntimeSetupMessageKind::Error,
        );
        std::process::exit(1);
    }

    match run_runtime_dependency_helper_interactive(&["--install", "--strict-runtime"]) {
        Ok(()) => match run_runtime_dependency_helper(&["--strict-runtime"]) {
            Ok(output) if output.status.success() => {
                show_runtime_setup_message(
                    "WaveLinux setup complete",
                    "Runtime packages were installed. WaveLinux will continue launching now.",
                    RuntimeSetupMessageKind::Info,
                );
            }
            Ok(output) => {
                show_runtime_setup_message(
                    "WaveLinux setup incomplete",
                    &dependency_output_text(&output),
                    RuntimeSetupMessageKind::Error,
                );
                std::process::exit(1);
            }
            Err(err) => {
                show_runtime_setup_message(
                    "WaveLinux setup incomplete",
                    &err,
                    RuntimeSetupMessageKind::Error,
                );
                std::process::exit(1);
            }
        },
        Err(err) => {
            show_runtime_setup_message(
                "WaveLinux setup failed",
                &format!("WaveLinux could not install required runtime packages.\n\n{err}"),
                RuntimeSetupMessageKind::Error,
            );
            std::process::exit(1);
        }
    }
}

fn ensure_audio_services_before_ui() {
    if std::env::var_os(AUDIO_SERVICE_START_SKIP_ENV).is_some() || !command_exists("pactl") {
        return;
    }

    let initial_error = match pactl_info_status() {
        Ok(()) => return,
        Err(err) => err,
    };

    eprintln!(
        "WaveLinux audio setup: pactl cannot connect; attempting to start user PipeWire services."
    );
    let attempts = start_user_audio_services();
    for _ in 0..12 {
        if pactl_info_status().is_ok() {
            eprintln!("WaveLinux audio setup: pactl connection is ready after service start.");
            return;
        }
        thread::sleep(Duration::from_millis(150));
    }

    let final_error = pactl_info_status().err().unwrap_or(initial_error.clone());
    let attempted = if attempts.is_empty() {
        "No service start method was available.".into()
    } else {
        attempts.join("\n")
    };
    let message = format!(
        "WaveLinux cannot connect to PipeWire/PulseAudio through pactl, so virtual sinks cannot be created.\n\nInitial error:\n{initial_error}\n\nAfter service start attempts:\n{final_error}\n\nTried:\n{attempted}"
    );
    show_runtime_setup_message(
        "WaveLinux audio service unavailable",
        &message,
        RuntimeSetupMessageKind::Error,
    );
    std::process::exit(1);
}

fn pactl_info_status() -> Result<(), String> {
    let output = host_command("pactl")
        .arg("info")
        .output()
        .map_err(|err| format!("pactl info failed to start: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "pactl info exited with status {}: {}{}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            if output.stdout.is_empty() {
                String::new()
            } else {
                format!("\n{}", String::from_utf8_lossy(&output.stdout).trim())
            }
        ))
    }
}

fn start_user_audio_services() -> Vec<String> {
    let mut attempts = Vec::new();
    if command_exists("systemctl") {
        for unit in user_audio_service_units() {
            let output = host_command("systemctl")
                .args(["--user", "start", unit])
                .output()
                .map_err(|err| format!("systemctl failed to start: {err}"));
            attempts.push(command_attempt_summary(
                "systemctl",
                &["--user", "start", unit],
                output,
            ));
            if pactl_info_status().is_ok() {
                return attempts;
            }
        }
    }

    if std::env::var_os(AUDIO_DAEMON_FALLBACK_DISABLE_ENV).is_some() {
        return attempts;
    }

    for program in ["pipewire", "pipewire-pulse", "wireplumber"] {
        if !command_exists(program) {
            continue;
        }
        let output = spawn_detached_audio_daemon(program);
        attempts.push(command_attempt_summary(program, &[], output));
        thread::sleep(Duration::from_millis(150));
        if pactl_info_status().is_ok() {
            break;
        }
    }

    attempts
}

fn user_audio_service_units() -> &'static [&'static str] {
    &[
        "pipewire.socket",
        "pipewire-pulse.socket",
        "pipewire.service",
        "pipewire-pulse.service",
        "wireplumber.service",
    ]
}

fn spawn_detached_audio_daemon(program: &str) -> Result<Output, String> {
    host_command(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| Output {
            status: successful_exit_status(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
        .map_err(|err| format!("{program} failed to start: {err}"))
}

#[cfg(unix)]
fn successful_exit_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

#[cfg(not(unix))]
fn successful_exit_status() -> std::process::ExitStatus {
    Command::new("cmd")
        .args(["/C", "exit", "0"])
        .status()
        .expect("failed to synthesize successful exit status")
}

fn command_attempt_summary(program: &str, args: &[&str], output: Result<Output, String>) -> String {
    let command = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    match output {
        Ok(output) if output.status.success() => format!("{command}: ok"),
        Ok(output) => format!(
            "{command}: status {} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(err) => format!("{command}: {err}"),
    }
}

#[derive(Debug, Clone, Copy)]
enum RuntimeSetupMessageKind {
    Info,
    Error,
}

fn confirm_runtime_dependency_install(message: &str) -> bool {
    if std::env::var_os("WAVELINUX_ASSUME_YES").is_some() {
        return true;
    }

    if command_exists("zenity") {
        return host_command("zenity")
            .args([
                "--question",
                "--title",
                "WaveLinux setup",
                "--width",
                "620",
                "--ok-label",
                "Install",
                "--cancel-label",
                "Cancel",
                "--text",
                message,
            ])
            .status()
            .is_ok_and(|status| status.success());
    }

    if command_exists("kdialog") {
        return host_command("kdialog")
            .args(["--title", "WaveLinux setup", "--yesno", message])
            .status()
            .is_ok_and(|status| status.success());
    }

    if command_exists("xmessage") {
        return host_command("xmessage")
            .args([
                "-center",
                "-buttons",
                "Install:0,Cancel:1",
                "-title",
                "WaveLinux setup",
                message,
            ])
            .status()
            .is_ok_and(|status| status.success());
    }

    eprintln!("WaveLinux setup: {message}");
    true
}

fn show_runtime_setup_message(title: &str, message: &str, kind: RuntimeSetupMessageKind) {
    eprintln!("{title}: {message}");

    if command_exists("zenity") {
        let dialog_kind = match kind {
            RuntimeSetupMessageKind::Info => "--info",
            RuntimeSetupMessageKind::Error => "--error",
        };
        let _ = host_command("zenity")
            .args([
                dialog_kind,
                "--title",
                title,
                "--width",
                "620",
                "--text",
                message,
            ])
            .status();
        return;
    }

    if command_exists("kdialog") {
        let dialog_kind = match kind {
            RuntimeSetupMessageKind::Info => "--msgbox",
            RuntimeSetupMessageKind::Error => "--error",
        };
        let _ = host_command("kdialog")
            .args(["--title", title, dialog_kind, message])
            .status();
        return;
    }

    if command_exists("xmessage") {
        let _ = host_command("xmessage")
            .args(["-center", "-title", title, message])
            .status();
        return;
    }

    if command_exists("notify-send") {
        let _ = host_command("notify-send").args([title, message]).status();
    }
}

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
fn set_meter_streaming(
    window: tauri::Window,
    engine: State<'_, EngineState>,
    enabled: bool,
) -> bool {
    engine
        .meter_streaming_requested
        .store(enabled, Ordering::Release);
    let visible = window.is_visible().unwrap_or(true);
    let minimized = window.is_minimized().unwrap_or(false);
    let applied = enabled && visible && !minimized;
    engine.meter_streaming.store(applied, Ordering::Release);
    applied
}

#[tauri::command]
fn create_mix(
    app: AppHandle,
    engine: State<'_, EngineState>,
    name: String,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "create_mix",
        engine.engine.create_mix(name),
    )
}

#[tauri::command]
fn rename_mix(
    app: AppHandle,
    engine: State<'_, EngineState>,
    mix_id: String,
    name: String,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "rename_mix",
        engine.engine.rename_mix(mix_id, name),
    )
}

#[tauri::command]
fn move_mix(
    app: AppHandle,
    engine: State<'_, EngineState>,
    mix_id: String,
    direction: i32,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "move_mix",
        engine.engine.move_mix(mix_id, direction),
    )
}

#[tauri::command]
fn delete_mix(
    app: AppHandle,
    engine: State<'_, EngineState>,
    mix_id: String,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "delete_mix",
        engine.engine.delete_mix(mix_id),
    )
}

#[tauri::command]
fn set_mix_volume(
    app: AppHandle,
    engine: State<'_, EngineState>,
    mix_id: String,
    volume: f32,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_mix_volume",
        engine.engine.set_mix_volume(mix_id, volume),
    )
}

#[tauri::command]
fn set_mix_mute(
    app: AppHandle,
    engine: State<'_, EngineState>,
    mix_id: String,
    muted: bool,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_mix_mute",
        engine.engine.set_mix_mute(mix_id, muted),
    )
}

#[tauri::command]
fn set_mix_icon(
    app: AppHandle,
    engine: State<'_, EngineState>,
    mix_id: String,
    icon: Option<String>,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_mix_icon",
        engine.engine.set_mix_icon(mix_id, icon),
    )
}

#[tauri::command]
fn set_channel_icon(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    icon: Option<String>,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_channel_icon",
        engine.engine.set_channel_icon(channel_id, icon),
    )
}

#[tauri::command]
fn set_mix_monitor_output(
    app: AppHandle,
    engine: State<'_, EngineState>,
    mix_id: String,
    output: Option<String>,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_mix_monitor_output",
        engine.engine.set_mix_monitor_output(mix_id, output),
    )
}

#[tauri::command]
fn set_mix_outputs(
    app: AppHandle,
    engine: State<'_, EngineState>,
    mix_id: String,
    outputs: Vec<String>,
    request_id: Option<String>,
) -> Result<OperationResponse<Mix>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_mix_outputs",
        engine.engine.set_mix_outputs(mix_id, outputs),
    )
}

#[tauri::command]
fn create_channel(
    app: AppHandle,
    engine: State<'_, EngineState>,
    name: String,
    kind: ChannelKind,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "create_channel",
        engine.engine.create_channel(name, kind),
    )
}

#[tauri::command]
fn rename_channel(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    name: String,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "rename_channel",
        engine.engine.rename_channel(channel_id, name),
    )
}

#[tauri::command]
fn move_channel(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    direction: i32,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "move_channel",
        engine.engine.move_channel(channel_id, direction),
    )
}

#[tauri::command]
fn delete_channel(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "delete_channel",
        engine.engine.delete_channel(channel_id),
    )
}

#[tauri::command]
fn set_channel_linked(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    linked: bool,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_channel_linked",
        engine.engine.set_channel_linked(channel_id, linked),
    )
}

#[tauri::command]
fn set_channel_input(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    source_device: Option<String>,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_channel_input",
        engine.engine.set_channel_input(channel_id, source_device),
    )
}

#[tauri::command]
fn set_hardware_input_device(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    source_device: Option<String>,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_hardware_input_device",
        engine
            .engine
            .set_hardware_input_device(channel_id, source_device),
    )
}

#[tauri::command]
fn set_channel_input_mode(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    input_mode: ChannelInputMode,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_channel_input_mode",
        engine.engine.set_channel_input_mode(channel_id, input_mode),
    )
}

#[tauri::command]
fn set_channel_bus_enabled(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    mix_id: String,
    enabled: bool,
    request_id: Option<String>,
) -> Result<OperationResponse<MixBus>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_channel_bus_enabled",
        engine
            .engine
            .set_channel_bus_enabled(channel_id, mix_id, enabled),
    )
}

#[tauri::command]
fn set_settings(
    app: AppHandle,
    engine: State<'_, EngineState>,
    settings: MixerSettings,
    request_id: Option<String>,
) -> Result<OperationResponse<MixerSettings>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_settings",
        engine.engine.set_settings(settings),
    )
}

#[tauri::command]
fn list_hardware_profiles(
    engine: State<'_, EngineState>,
) -> Result<HardwareProfileUiState, String> {
    tauri_result(engine.engine.list_hardware_profiles())
}

#[tauri::command]
fn set_device_hardware_profile(
    app: AppHandle,
    engine: State<'_, EngineState>,
    device_id: String,
    profile_id: Option<String>,
    request_id: Option<String>,
) -> Result<OperationResponse<HardwareProfileUiState>, String> {
    let result = engine
        .engine
        .set_device_hardware_profile(device_id, profile_id);
    operation_result(
        &app,
        &engine,
        request_id,
        "set_device_hardware_profile",
        result,
    )
}

#[tauri::command]
fn set_fallback_hardware_profile(
    app: AppHandle,
    engine: State<'_, EngineState>,
    fallback_profile: FallbackHardwareProfile,
    request_id: Option<String>,
) -> Result<OperationResponse<HardwareProfileUiState>, String> {
    let result = engine
        .engine
        .set_fallback_hardware_profile(fallback_profile);
    operation_result(
        &app,
        &engine,
        request_id,
        "set_fallback_hardware_profile",
        result,
    )
}

#[tauri::command]
fn set_hardware_profile_policy(
    app: AppHandle,
    engine: State<'_, EngineState>,
    profile_id: String,
    name: Option<String>,
    latency_policy: LatencyPolicy,
    routing_policy: RoutingPolicy,
    request_id: Option<String>,
) -> Result<OperationResponse<HardwareProfileUiState>, String> {
    let result =
        engine
            .engine
            .set_hardware_profile_policy(profile_id, name, latency_policy, routing_policy);
    operation_result(
        &app,
        &engine,
        request_id,
        "set_hardware_profile_policy",
        result,
    )
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
        let bindings = engine
            .engine
            .ensure_streamer_binding_profiles(defaults)
            .map_err(|err| err.to_string())?;
        engine.streamer_runtime.sync(Arc::clone(&engine.engine))?;
        bindings
    } else {
        state.config.streamer_devices
    };
    for device in &mut devices {
        if let Some(profile) = bindings.profiles.get(&device.id) {
            device.enabled = streamer_devices::native_bindings_available(device) && profile.enabled;
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
    if enabled {
        let state = engine.engine.get_state().map_err(|err| err.to_string())?;
        let devices = streamer_devices::discover_devices(&state);
        if let Some(device) = devices.iter().find(|device| device.id == device_id) {
            if !streamer_devices::native_bindings_available(device) {
                return Err(format!(
                    "{} is detected, but bindings are unavailable while status is {}",
                    device.name,
                    streamer_permission_status_label(&device.permission_status)
                ));
            }
        }
    }
    let config = engine
        .engine
        .set_streamer_device_enabled(device_id, enabled)
        .map_err(|err| err.to_string())?;
    engine.streamer_runtime.sync(Arc::clone(&engine.engine))?;
    Ok(config)
}

#[tauri::command]
fn set_streamer_binding_profile(
    engine: State<'_, EngineState>,
    profile: StreamerBindingProfile,
) -> Result<StreamerBindingProfile, String> {
    let profile = engine
        .engine
        .set_streamer_binding_profile(profile)
        .map_err(|err| err.to_string())?;
    engine.streamer_runtime.sync(Arc::clone(&engine.engine))?;
    Ok(profile)
}

#[tauri::command]
fn learn_streamer_control(
    engine: State<'_, EngineState>,
    device_id: String,
) -> Result<StreamerLearnResult, String> {
    let state = engine.engine.get_state().map_err(|err| err.to_string())?;
    let devices = streamer_devices::discover_devices(&state);
    let device = devices
        .into_iter()
        .find(|device| device.id == device_id)
        .ok_or_else(|| "Streamer device is no longer detected".to_string())?;
    engine.streamer_runtime.learn_control(device)
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
    engine
        .streamer_runtime
        .run_elgato_command(&engine.engine, ElgatoCommand::ReadWaveXlr)
}

#[tauri::command]
fn set_elgato_wave_xlr_gain(
    engine: State<'_, EngineState>,
    gain_raw: u16,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    engine
        .streamer_runtime
        .run_elgato_command(&engine.engine, ElgatoCommand::SetWaveXlrGain { gain_raw })
}

#[tauri::command]
fn set_elgato_wave_xlr_mute(
    engine: State<'_, EngineState>,
    muted: bool,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    engine
        .streamer_runtime
        .run_elgato_command(&engine.engine, ElgatoCommand::SetWaveXlrMute { muted })
}

#[tauri::command]
fn set_elgato_wave_xlr_hp_volume_db(
    engine: State<'_, EngineState>,
    db: f32,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    engine.streamer_runtime.run_elgato_command(
        &engine.engine,
        ElgatoCommand::SetWaveXlrHeadphoneVolume { db },
    )
}

#[tauri::command]
fn set_elgato_wave_xlr_low_impedance(
    engine: State<'_, EngineState>,
    enabled: bool,
) -> Result<elgato::ElgatoWaveXlrState, String> {
    ensure_elgato_wave_xlr_detected(&engine.engine)?;
    engine.streamer_runtime.run_elgato_command(
        &engine.engine,
        ElgatoCommand::SetWaveXlrLowImpedance { enabled },
    )
}

#[tauri::command]
fn set_channel_volume(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    mix_id: String,
    volume: f32,
    request_id: Option<String>,
) -> Result<OperationResponse<MixBus>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_channel_volume",
        engine.engine.set_channel_volume(channel_id, mix_id, volume),
    )
}

#[tauri::command]
fn set_channel_mute(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    mix_id: String,
    muted: bool,
    request_id: Option<String>,
) -> Result<OperationResponse<MixBus>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_channel_mute",
        engine.engine.set_channel_mute(channel_id, mix_id, muted),
    )
}

#[tauri::command]
fn assign_app_to_channel(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    matcher: AppMatcher,
    request_id: Option<String>,
) -> Result<OperationResponse<AppRoute>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "assign_app_to_channel",
        engine.engine.assign_app_to_channel(channel_id, matcher),
    )
}

#[tauri::command]
fn remove_app_route(
    app: AppHandle,
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    request_id: Option<String>,
) -> Result<OperationResponse<Option<AppRoute>>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "remove_app_route",
        engine.engine.remove_app_route(matcher),
    )
}

#[tauri::command]
fn set_app_volume_preset(
    app: AppHandle,
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    volume: f32,
    request_id: Option<String>,
) -> Result<OperationResponse<AppVolumePreset>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_app_volume_preset",
        engine.engine.set_app_volume_preset(matcher, volume),
    )
}

#[tauri::command]
fn remove_app_volume_preset(
    app: AppHandle,
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    request_id: Option<String>,
) -> Result<OperationResponse<Option<AppVolumePreset>>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "remove_app_volume_preset",
        engine.engine.remove_app_volume_preset(matcher),
    )
}

#[tauri::command]
fn forget_app(
    app: AppHandle,
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    request_id: Option<String>,
) -> Result<OperationResponse<Option<KnownApp>>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "forget_app",
        engine.engine.forget_app(matcher),
    )
}

#[tauri::command]
fn restore_app(
    app: AppHandle,
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    request_id: Option<String>,
) -> Result<OperationResponse<Option<KnownApp>>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "restore_app",
        engine.engine.restore_app(matcher),
    )
}

#[tauri::command]
fn pin_app_identity(
    app: AppHandle,
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    label: String,
    request_id: Option<String>,
) -> Result<OperationResponse<KnownApp>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "pin_app_identity",
        engine.engine.pin_app_identity(matcher, label),
    )
}

#[tauri::command]
fn merge_app_identity(
    app: AppHandle,
    engine: State<'_, EngineState>,
    source: AppMatcher,
    target: AppMatcher,
    request_id: Option<String>,
) -> Result<OperationResponse<KnownApp>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "merge_app_identity",
        engine.engine.merge_app_identity(source, target),
    )
}

#[tauri::command]
fn reset_app_identity(
    app: AppHandle,
    engine: State<'_, EngineState>,
    matcher: AppMatcher,
    request_id: Option<String>,
) -> Result<OperationResponse<Option<KnownApp>>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "reset_app_identity",
        engine.engine.reset_app_identity(matcher),
    )
}

#[tauri::command]
fn move_app_stream(
    app: AppHandle,
    engine: State<'_, EngineState>,
    stream_id: String,
    channel_id: String,
    request_id: Option<String>,
) -> Result<OperationResponse<wavelinux_engine::CommandExecution>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "move_app_stream",
        engine.engine.move_app_stream(stream_id, channel_id),
    )
}

#[tauri::command]
fn move_app_stream_to_default(
    app: AppHandle,
    engine: State<'_, EngineState>,
    stream_id: String,
    request_id: Option<String>,
) -> Result<OperationResponse<wavelinux_engine::CommandExecution>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "move_app_stream_to_default",
        engine.engine.move_app_stream_to_default(stream_id),
    )
}

#[tauri::command]
fn set_app_stream_volume(
    app: AppHandle,
    engine: State<'_, EngineState>,
    stream_id: String,
    volume: f32,
    request_id: Option<String>,
) -> Result<OperationResponse<wavelinux_engine::CommandExecution>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_app_stream_volume",
        engine.engine.set_app_stream_volume(stream_id, volume),
    )
}

#[tauri::command]
fn set_app_stream_mute(
    app: AppHandle,
    engine: State<'_, EngineState>,
    stream_id: String,
    muted: bool,
    request_id: Option<String>,
) -> Result<OperationResponse<wavelinux_engine::CommandExecution>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_app_stream_mute",
        engine.engine.set_app_stream_mute(stream_id, muted),
    )
}

#[tauri::command]
fn set_effect_chain(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    effects: Vec<EffectInstance>,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_effect_chain",
        engine.engine.set_effect_chain(channel_id, effects),
    )
}

#[tauri::command]
fn set_effect_param(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    instance_id: String,
    param_id: String,
    value: f32,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_effect_param",
        engine
            .engine
            .set_effect_param(channel_id, instance_id, param_id, value),
    )
}

#[tauri::command]
fn bypass_effect(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    instance_id: String,
    bypassed: bool,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "bypass_effect",
        engine
            .engine
            .bypass_effect(channel_id, instance_id, bypassed),
    )
}

#[tauri::command]
fn set_channel_effects_enabled(
    app: AppHandle,
    engine: State<'_, EngineState>,
    channel_id: String,
    enabled: bool,
    request_id: Option<String>,
) -> Result<OperationResponse<Channel>, String> {
    operation_result(
        &app,
        &engine,
        request_id,
        "set_channel_effects_enabled",
        engine
            .engine
            .set_channel_effects_enabled(channel_id, enabled),
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
    let release_url = release_url_for_settings(&settings).to_string();
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
                release_url,
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
                release_url,
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
            release_url,
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

    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|err| err.to_string())?;
    app.restart()
}

#[tauri::command]
fn open_release_page(
    app: AppHandle,
    release_channel: Option<ReleaseChannel>,
) -> Result<(), String> {
    let url = release_channel
        .as_ref()
        .map(release_url_for_channel)
        .unwrap_or(RELEASES_URL);
    app.opener()
        .open_url(url, None::<String>)
        .map_err(|err| err.to_string())
}

fn tauri_result<T>(result: Result<T, EngineError>) -> Result<T, String> {
    result.map_err(|err| err.to_string())
}

fn operation_result<T>(
    app: &AppHandle,
    engine: &EngineState,
    request_id: Option<String>,
    command: &'static str,
    result: Result<T, EngineError>,
) -> Result<OperationResponse<T>, String> {
    let revision = engine.operation_revision.fetch_add(1, Ordering::AcqRel) + 1;
    let request_id = request_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("backend-{revision}"));
    let revisions = engine.engine.revisions();
    match result {
        Ok(value) => {
            let event = OperationEvent {
                protocol_version: OPERATION_PROTOCOL_VERSION,
                revision,
                request_id: request_id.clone(),
                command,
                status: "succeeded",
                state_revision: revisions.state,
                config_revision: revisions.config,
                graph_revision: revisions.graph,
                error: None,
            };
            let _ = app.emit(OPERATION_EVENT, event);
            Ok(OperationResponse {
                protocol_version: OPERATION_PROTOCOL_VERSION,
                revision,
                request_id,
                command,
                status: "succeeded",
                state_revision: revisions.state,
                config_revision: revisions.config,
                graph_revision: revisions.graph,
                value,
            })
        }
        Err(err) => {
            let error = err.to_string();
            let event = OperationEvent {
                protocol_version: OPERATION_PROTOCOL_VERSION,
                revision,
                request_id,
                command,
                status: "failed",
                state_revision: revisions.state,
                config_revision: revisions.config,
                graph_revision: revisions.graph,
                error: Some(error.clone()),
            };
            let _ = app.emit(OPERATION_EVENT, event);
            Err(error)
        }
    }
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

fn host_command(program: &str) -> Command {
    let mut command = Command::new(program);
    sanitize_host_command_env(&mut command);
    command
}

fn sanitize_host_command_env(command: &mut Command) {
    for key in HOST_COMMAND_ENV_REMOVE {
        command.env_remove(key);
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
    update_endpoint_for_channel(&settings.release_channel)
}

fn update_endpoint_for_channel(release_channel: &ReleaseChannel) -> String {
    match release_channel_name_value(release_channel) {
        "beta" => std::env::var("WAVELINUX_BETA_UPDATE_ENDPOINT")
            .or_else(|_| std::env::var("WAVELINUX_UPDATE_ENDPOINT"))
            .unwrap_or_else(|_| BETA_UPDATE_ENDPOINT.into()),
        _ => std::env::var("WAVELINUX_STABLE_UPDATE_ENDPOINT")
            .or_else(|_| std::env::var("WAVELINUX_UPDATE_ENDPOINT"))
            .unwrap_or_else(|_| STABLE_UPDATE_ENDPOINT.into()),
    }
}

fn release_url_for_settings(settings: &MixerSettings) -> &'static str {
    release_url_for_channel(&settings.release_channel)
}

fn release_url_for_channel(release_channel: &ReleaseChannel) -> &'static str {
    match release_channel {
        ReleaseChannel::Beta => BETA_RELEASE_URL,
        ReleaseChannel::Stable => STABLE_RELEASE_URL,
    }
}

fn current_update_version() -> semver::Version {
    option_env!("WAVELINUX_UPDATE_VERSION")
        .and_then(release_tag_update_version)
        .or_else(|| build_release_tag().and_then(release_tag_update_version))
        .or_else(|| semver::Version::parse(env!("CARGO_PKG_VERSION")).ok())
        .expect("package version is valid semver")
}

fn is_wavelinux6_build() -> bool {
    env!("CARGO_PKG_VERSION").starts_with("6.")
}

fn apply_wavelinux6_env() {
    if !is_wavelinux6_build() {
        return;
    }
    set_env_default(TOKIO_WORKER_THREADS_ENV, DEFAULT_TOKIO_WORKER_THREADS);
    set_env_default("WAVELINUX_XDG_APP_NAME", "WaveLinux6");
    set_env_default("WAVELINUX_GRAPH_PREFIX", "wavelinux6");
    set_env_default("WAVELINUX_GRAPH_PROPERTY_PREFIX", "wavelinux6");
    set_env_default("WAVELINUX_APP_DISPLAY_NAME", "WaveLinux 6");
}

fn release_tag_update_version(tag: &str) -> Option<semver::Version> {
    let version = tag.trim().trim_start_matches('v');
    if version.is_empty() || version.eq_ignore_ascii_case("prerelease") {
        return None;
    }
    semver::Version::parse(version).ok()
}

fn build_release_tag() -> Option<&'static str> {
    option_env!("WAVELINUX_RELEASE_TAG")
        .or(option_env!("GITHUB_REF_NAME"))
        .filter(|tag| !tag.trim().is_empty())
}

fn release_channel_name(settings: &MixerSettings) -> &'static str {
    release_channel_name_value(&settings.release_channel)
}

fn streamer_permission_status_label(status: &StreamerPermissionStatus) -> &'static str {
    match status {
        StreamerPermissionStatus::Ready => "ready",
        StreamerPermissionStatus::PermissionDenied => "permission denied",
        StreamerPermissionStatus::Busy => "busy",
        StreamerPermissionStatus::MissingRuntime => "missing runtime",
        StreamerPermissionStatus::UnsupportedProtocol => "unsupported protocol",
    }
}

fn release_channel_name_value(release_channel: &ReleaseChannel) -> &'static str {
    match release_channel {
        ReleaseChannel::Beta => "beta",
        ReleaseChannel::Stable => "stable",
    }
}

fn is_appimage_install() -> bool {
    std::env::var_os("APPIMAGE").is_some()
        || std::env::var_os("APPDIR").is_some()
        || std::env::current_exe().is_ok_and(|path| {
            path.components().any(|component| {
                component
                    .as_os_str()
                    .to_string_lossy()
                    .starts_with(".mount_Wave")
            })
        })
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
            .title(app_display_name())
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

fn process_runtime_base_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("wavelinux-{}", unsafe { libc::geteuid() }))
        })
}

fn process_lock_path_for(runtime_base: &Path, prefix: &str) -> PathBuf {
    runtime_base.join(prefix).join("app.lock")
}

fn ensure_private_runtime_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn acquire_process_lock() -> std::io::Result<Option<ProcessLock>> {
    let lock_path = process_lock_path_for(&process_runtime_base_dir(), &graph_prefix());
    if let Some(lock_dir) = lock_path.parent() {
        ensure_private_runtime_dir(lock_dir)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
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
    let show = MenuItem::with_id(
        app,
        "show",
        format!("Show {}", app_display_name()),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let icon = Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;
    let tooltip = app_display_name().to_string();

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

fn spawn_ui_event_bridge(
    app: AppHandle,
    engine: Arc<WaveLinuxEngine>,
    meter_streaming: Arc<AtomicBool>,
) -> Vec<thread::JoinHandle<()>> {
    let state_app = app.clone();
    let state_engine = Arc::clone(&engine);
    let state_worker = thread::Builder::new()
        .name("wavelinux6-state-events".into())
        .spawn(move || {
            let Ok(mut previous) = state_engine.cached_state_snapshot() else {
                return;
            };
            let mut revisions = state_engine.revisions();
            while !state_engine.is_stopping() {
                let next_revisions = state_engine.wait_for_change(revisions.state, UI_EVENT_WAIT);
                if next_revisions.state == revisions.state {
                    continue;
                }
                let Ok(next) = state_engine.cached_state_snapshot() else {
                    revisions = next_revisions;
                    continue;
                };
                let mut previous_graph = previous.graph.clone();
                let mut next_graph = next.graph.clone();
                previous_graph.meters.clear();
                next_graph.meters.clear();
                let event = StateDeltaEvent {
                    revision: next_revisions.state,
                    config_revision: next_revisions.config,
                    graph_revision: next_revisions.graph,
                    config: (next.config != previous.config).then(|| next.config.clone()),
                    graph: (next_graph != previous_graph).then_some(next_graph),
                    diagnostics: (next.diagnostics != previous.diagnostics)
                        .then(|| next.diagnostics.clone()),
                    engine: (next.engine != previous.engine).then(|| next.engine.clone()),
                    catalog: (next.catalog != previous.catalog).then(|| next.catalog.clone()),
                };
                // Emit revision-only events as well. They let optimistic UI
                // mutations prove that the backend revision was observed
                // without paying for a redundant full-state snapshot.
                let _ = state_app.emit(STATE_DELTA_EVENT, &event);
                previous = next;
                revisions = next_revisions;
            }
        })
        .expect("failed to start WaveLinux state event bridge");

    let meter_worker = thread::Builder::new()
        .name("wavelinux6-meter-events".into())
        .spawn(move || {
            let mut revision = 0_u64;
            let mut previous = Vec::new();
            let mut stream = None;
            let mut next_connect_at = Instant::now();
            let mut next_fallback_at = Instant::now();
            while !engine.is_stopping() {
                let enabled = meter_streaming.load(Ordering::Acquire);
                if !enabled {
                    if stream.take().is_some() {
                        engine.close_meter_stream();
                    }
                    if meter_event_changed(&previous, &[]) {
                        revision = revision.saturating_add(1);
                        let event = MetersEvent {
                            revision,
                            meters: Vec::new(),
                        };
                        let _ = app.emit(METERS_EVENT, &event);
                        previous.clear();
                    }
                    thread::sleep(IDLE_METER_INTERVAL);
                    continue;
                }

                let now = Instant::now();
                if stream.is_none() && now >= next_connect_at {
                    match engine.open_meter_stream() {
                        Ok(client) => stream = Some(client),
                        Err(_) => next_connect_at = now + METER_RECONNECT_INTERVAL,
                    }
                }

                let meters = if let Some(client) = stream.as_mut() {
                    match engine.read_meter_stream(client) {
                        Ok(meters) => Some(meters),
                        Err(_) => {
                            stream = None;
                            next_connect_at = Instant::now() + METER_RECONNECT_INTERVAL;
                            None
                        }
                    }
                } else if now >= next_fallback_at {
                    engine.record_meter_fallback_poll();
                    next_fallback_at = now + METER_FALLBACK_INTERVAL;
                    Some(engine.observe_meters().unwrap_or_default())
                } else {
                    None
                };
                let Some(meters) = meters else {
                    thread::sleep(Duration::from_millis(25));
                    continue;
                };
                if meter_event_changed(&previous, &meters) {
                    revision = revision.saturating_add(1);
                    let event = MetersEvent {
                        revision,
                        meters: meters.clone(),
                    };
                    let _ = app.emit(METERS_EVENT, &event);
                    previous = meters;
                }
            }
            if stream.is_some() {
                engine.close_meter_stream();
            }
        })
        .expect("failed to start WaveLinux meter event bridge");

    vec![state_worker, meter_worker]
}

fn meter_event_changed(previous: &[LevelMeter], next: &[LevelMeter]) -> bool {
    previous.len() != next.len()
        || previous.iter().zip(next).any(|(left, right)| {
            left.node_id != right.node_id
                || meter_value_changed(left.peak_left, right.peak_left)
                || meter_value_changed(left.peak_right, right.peak_right)
        })
}

fn meter_value_changed(previous: f32, next: f32) -> bool {
    !previous.is_finite()
        || !next.is_finite()
        || (previous == 0.0) != (next == 0.0)
        || (previous - next).abs() >= METER_EVENT_MIN_DELTA
}

fn main() {
    apply_wavelinux6_env();
    install_process_panic_hook();
    prepare_appimage_bundled_runtime();

    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|arg| arg == "--probe-binary") {
        println!("WaveLinux 6 {} binary probe: ok", env!("CARGO_PKG_VERSION"));
        return;
    }

    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--install-runtime-dependencies" | "--install-runtime"
        )
    }) {
        std::process::exit(install_runtime_dependencies_from_cli());
    }

    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--check-runtime-dependencies" | "--check-runtime"
        )
    }) {
        apply_webkit_runtime_defaults();
        std::process::exit(print_runtime_dependency_report());
    }

    if args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "--prewarm-hardware-profiles" | "--check-hardware-profiles"
        )
    }) {
        std::process::exit(run_hardware_profile_prewarm());
    }

    ensure_runtime_dependencies_before_ui();
    ensure_audio_services_before_ui();
    apply_webkit_runtime_defaults();

    let shutdown_started = Arc::new(AtomicBool::new(false));
    let allow_exit = Arc::new(AtomicBool::new(false));
    let run_allow_exit = Arc::clone(&allow_exit);
    let meter_streaming_requested = Arc::new(AtomicBool::new(false));
    let meter_streaming = Arc::new(AtomicBool::new(false));
    let meter_streaming_requested_for_window_events = Arc::clone(&meter_streaming_requested);
    let meter_streaming_for_window_events = Arc::clone(&meter_streaming);

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
            set_meter_streaming,
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
            set_channel_effects_enabled,
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
        ])
        .on_window_event(move |window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                meter_streaming_for_window_events.store(false, Ordering::Release);
                api.prevent_close();
                let _ = window.hide();
            }
            tauri::WindowEvent::Focused(true) | tauri::WindowEvent::Resized(_) => {
                let requested = meter_streaming_requested_for_window_events.load(Ordering::Acquire);
                let visible = window.is_visible().unwrap_or(true);
                let minimized = window.is_minimized().unwrap_or(false);
                meter_streaming_for_window_events
                    .store(requested && visible && !minimized, Ordering::Release);
            }
            tauri::WindowEvent::Destroyed => {
                meter_streaming_for_window_events.store(false, Ordering::Release);
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building WaveLinux");

    let Some(_process_lock) =
        acquire_process_lock().expect("failed to acquire WaveLinux process lock")
    else {
        eprintln!(
            "{} is already running; refusing to start a duplicate audio engine",
            app_display_name()
        );
        return;
    };

    let app_log_version = current_update_version().to_string();
    let engine = WaveLinuxEngine::from_xdg_for_app_version(&app_log_version)
        .expect("failed to start WaveLinux engine");
    let background = engine.spawn_background();
    let streamer_runtime = Arc::new(streamer_devices::StreamerRuntimeController::default());
    streamer_runtime
        .sync(Arc::clone(&engine))
        .expect("failed to initialize streamer device runtime");
    let run_engine = Arc::clone(&engine);
    let run_shutdown = Arc::clone(&shutdown_started);
    app.manage(EngineState {
        engine: Arc::clone(&engine),
        meter_streaming_requested,
        meter_streaming: Arc::clone(&meter_streaming),
        operation_revision: AtomicU64::new(0),
        streamer_runtime: Arc::clone(&streamer_runtime),
    });
    build_tray(
        app.handle(),
        Arc::clone(&engine),
        Arc::clone(&shutdown_started),
        Arc::clone(&allow_exit),
    )
    .expect("failed to build WaveLinux tray");
    let ui_event_workers =
        spawn_ui_event_bridge(app.handle().clone(), Arc::clone(&engine), meter_streaming);

    app.run(move |_app, event| match event {
        tauri::RunEvent::ExitRequested { api, .. } if !run_allow_exit.load(Ordering::SeqCst) => {
            api.prevent_exit();
        }
        tauri::RunEvent::Exit => {
            shutdown_audio_graph(&run_engine, &run_shutdown);
        }
        _ => {}
    });

    streamer_runtime.stop();
    engine.stop_background();
    let _ = background.join();
    for worker in ui_event_workers {
        let _ = worker.join();
    }
    let _ = engine.cleanup_audio_graph();
}

fn install_process_panic_hook() {
    std::panic::set_hook(Box::new(|panic| {
        let thread = thread::current();
        let thread_name = thread.name().unwrap_or("unnamed");
        let payload = panic
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        let location = panic
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown".into());
        let record = format!(
            "process=wavelinux6 pid={} thread={} location={} payload={} backtrace=\n{}\n",
            std::process::id(),
            thread_name,
            location,
            payload,
            Backtrace::force_capture(),
        );
        eprintln!("wavelinux6-crash {record}");
        let config_root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        if let Some(config_root) = config_root {
            let directory = config_root.join(graph_prefix());
            if fs::create_dir_all(&directory).is_ok() {
                if let Ok(mut file) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(directory.join("wavelinux-crash.log"))
                {
                    let _ = file.write_all(record.as_bytes());
                    let _ = file.sync_data();
                }
            }
        }
    }));
}

#[cfg(test)]
mod updater_tests {
    use super::*;

    #[test]
    fn process_lock_uses_versioned_runtime_namespace() {
        let lock_path = process_lock_path_for(Path::new("/run/user/1000"), "wavelinux6");
        assert_eq!(
            lock_path,
            PathBuf::from("/run/user/1000/wavelinux6/app.lock")
        );
        assert!(!lock_path.to_string_lossy().contains("-5.lock"));
    }

    #[test]
    fn meter_events_coalesce_sub_visual_changes() {
        let previous = vec![LevelMeter {
            node_id: "hardware_in".into(),
            peak_left: 0.2,
            peak_right: 0.2,
        }];
        let mut next = previous.clone();
        next[0].peak_left += METER_EVENT_MIN_DELTA * 0.5;
        assert!(!meter_event_changed(&previous, &next));

        next[0].peak_left += METER_EVENT_MIN_DELTA;
        assert!(meter_event_changed(&previous, &next));
    }

    #[test]
    fn meter_events_always_publish_zero_transitions() {
        let previous = vec![LevelMeter {
            node_id: "hardware_in".into(),
            peak_left: METER_EVENT_MIN_DELTA * 0.5,
            peak_right: 0.0,
        }];
        let next = vec![LevelMeter {
            node_id: "hardware_in".into(),
            peak_left: 0.0,
            peak_right: 0.0,
        }];
        assert!(meter_event_changed(&previous, &next));
    }

    #[test]
    fn main_window_can_subscribe_to_backend_events() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main.json")).unwrap();
        let permissions = capability["permissions"].as_array().unwrap();
        for required in ["core:event:allow-listen", "core:event:allow-unlisten"] {
            assert!(
                permissions.iter().any(|permission| permission == required),
                "missing {required} from the main-window capability"
            );
        }
    }

    #[test]
    fn release_urls_follow_selected_channel() {
        assert_eq!(
            release_url_for_channel(&ReleaseChannel::Stable),
            STABLE_RELEASE_URL
        );
        assert_eq!(
            release_url_for_channel(&ReleaseChannel::Beta),
            BETA_RELEASE_URL
        );
    }

    #[test]
    fn update_endpoints_follow_selected_channel() {
        assert_eq!(
            update_endpoint_for_channel(&ReleaseChannel::Stable),
            STABLE_UPDATE_ENDPOINT
        );
        assert_eq!(
            update_endpoint_for_channel(&ReleaseChannel::Beta),
            BETA_UPDATE_ENDPOINT
        );
    }

    #[test]
    fn moving_prerelease_tag_is_not_treated_as_a_version() {
        assert_eq!(release_tag_update_version("prerelease"), None);
        assert_eq!(
            release_tag_update_version(" v4.3.0-testing.7 ")
                .unwrap()
                .to_string(),
            "4.3.0-testing.7"
        );
    }

    #[test]
    fn audio_service_start_covers_pipewire_pulse_and_session_manager() {
        let units = user_audio_service_units();
        assert!(units.contains(&"pipewire.service"));
        assert!(units.contains(&"pipewire-pulse.service"));
        assert!(units.contains(&"wireplumber.service"));
    }
}
