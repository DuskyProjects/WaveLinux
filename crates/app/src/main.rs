#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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
    app_display_name, graph_prefix, AppMatcher, AppRoute, AppStateSnapshot, AppVolumePreset,
    Channel, ChannelInputMode, ChannelKind, EffectAvailability, EffectCatalog, EffectInstance,
    FallbackHardwareProfile, HardwareProfileUiState, KnownApp, LatencyPolicy, LevelMeter, Mix,
    MixBus, MixerConfig, MixerSettings, ReleaseChannel, RoutingPolicy, StreamerAction,
    StreamerActionResult, StreamerBindingProfile, StreamerDeviceSummary, StreamerDevicesConfig,
    StreamerLearnResult, StreamerPermissionStatus,
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
const STABLE_RELEASE_URL: &str = "https://github.com/DuskyProjects/WaveLinux/releases/latest";
const BETA_RELEASE_URL: &str = "https://github.com/DuskyProjects/WaveLinux/releases/tag/prerelease";
const STABLE_UPDATE_ENDPOINT: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/latest/download/latest.json";
const BETA_UPDATE_ENDPOINT: &str =
    "https://github.com/DuskyProjects/WaveLinux/releases/download/prerelease/latest.json";
const WAVELINUX5_UPDATES_DISABLED_MESSAGE: &str =
    "WaveLinux5 is a local test build; stable self-updates are disabled.";
const UI_THEME_PREFERENCE_FILE: &str = "ui-theme.json";
const UI_THEMES_DIR: &str = "themes";
const WEBKIT_DMABUF_DISABLE_ENV: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
const WEBKIT_COMPOSITING_DISABLE_ENV: &str = "WEBKIT_DISABLE_COMPOSITING_MODE";
const WEBKIT_SANDBOX_DISABLE_ENV: &str = "WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS";
const WEBKIT_WORKAROUNDS_DISABLE_ENV: &str = "WAVELINUX_DISABLE_WEBKIT_WORKAROUNDS";
const WEBKIT_SANDBOX_KEEP_ENV: &str = "WAVELINUX_KEEP_WEBKIT_SANDBOX";
const RUNTIME_INSTALL_SKIP_ENV: &str = "WAVELINUX_SKIP_RUNTIME_INSTALL";
const RUNTIME_INSTALL_FORCE_ENV: &str = "WAVELINUX_INSTALL_RUNTIME_ON_START";
const RUNTIME_DEPS_ASSUME_ENV: &str = "WAVELINUX_ASSUME_RUNTIME_DEPS";
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
const APT_RUNTIME_PACKAGES: &[&str] = &[
    "pipewire",
    "wireplumber",
    "pipewire-pulse",
    "pipewire-bin",
    "pulseaudio-utils",
    "alsa-utils",
    "libwebkit2gtk-4.1-0",
    "libayatana-appindicator3-1",
    "libusb-1.0-0",
    "bubblewrap",
    "xdg-dbus-proxy",
    "xwayland",
    "libegl1",
    "libgl1",
    "libgbm1",
    "libdrm2",
    "gstreamer1.0-plugins-base",
    "gstreamer1.0-plugins-good",
    "fonts-dejavu-core",
    "xdg-desktop-portal",
];
const APT_APPIMAGE_HOST_PACKAGES: &[&str] = &[
    "pipewire",
    "wireplumber",
    "pipewire-pulse",
    "pipewire-bin",
    "pulseaudio-utils",
    "alsa-utils",
    "xwayland",
    "libegl1",
    "libgl1",
    "libgbm1",
    "libdrm2",
    "fonts-dejavu-core",
    "xdg-desktop-portal",
];
const APT_PORTAL_BACKENDS: &[&str] = &[
    "xdg-desktop-portal-gtk",
    "xdg-desktop-portal-kde",
    "xdg-desktop-portal-gnome",
    "xdg-desktop-portal-wlr",
];
const DNF_RUNTIME_PACKAGES: &[&str] = &[
    "pipewire",
    "wireplumber",
    "pipewire-pulseaudio",
    "pulseaudio-utils",
    "alsa-utils",
    "webkit2gtk4.1",
    "libappindicator-gtk3",
    "libusb1",
    "bubblewrap",
    "xdg-dbus-proxy",
    "xorg-x11-server-Xwayland",
    "mesa-libEGL",
    "mesa-libGL",
    "mesa-libgbm",
    "libdrm",
    "gstreamer1-plugins-base",
    "gstreamer1-plugins-good",
    "google-noto-sans-fonts",
    "xdg-desktop-portal",
];
const DNF_APPIMAGE_HOST_PACKAGES: &[&str] = &[
    "pipewire",
    "wireplumber",
    "pipewire-pulseaudio",
    "pulseaudio-utils",
    "alsa-utils",
    "xorg-x11-server-Xwayland",
    "mesa-libEGL",
    "mesa-libGL",
    "mesa-libgbm",
    "libdrm",
    "google-noto-sans-fonts",
    "xdg-desktop-portal",
];
const DNF_PORTAL_BACKENDS: &[&str] = &[
    "xdg-desktop-portal-gtk",
    "xdg-desktop-portal-kde",
    "xdg-desktop-portal-gnome",
    "xdg-desktop-portal-wlr",
    "xdg-desktop-portal-hyprland",
];
const ARCH_RUNTIME_PACKAGES: &[&str] = &[
    "pipewire",
    "wireplumber",
    "pipewire-pulse",
    "libpulse",
    "alsa-utils",
    "webkit2gtk-4.1",
    "bubblewrap",
    "xdg-dbus-proxy",
    "xorg-xwayland",
    "mesa",
    "libglvnd",
    "gtk3",
    "gstreamer",
    "gst-plugins-base-libs",
    "gst-plugins-good",
    "noto-fonts",
    "libayatana-appindicator",
    "libusb",
    "xdg-desktop-portal",
];
const ARCH_APPIMAGE_HOST_PACKAGES: &[&str] = &[
    "pipewire",
    "wireplumber",
    "pipewire-pulse",
    "libpulse",
    "alsa-utils",
    "xorg-xwayland",
    "mesa",
    "libglvnd",
    "noto-fonts",
    "xdg-desktop-portal",
];
const ARCH_PORTAL_BACKENDS: &[&str] = &[
    "xdg-desktop-portal-gtk",
    "xdg-desktop-portal-kde",
    "xdg-desktop-portal-hyprland",
    "xdg-desktop-portal-wlr",
    "xdg-desktop-portal-gnome",
];
const ZYPPER_RUNTIME_PACKAGES: &[&str] = &[
    "pipewire",
    "wireplumber",
    "pipewire-pulseaudio",
    "pulseaudio-utils",
    "alsa",
    "libwebkit2gtk-4_1-0",
    "typelib-1_0-AyatanaAppIndicator3-0_1",
    "libusb-1_0-0",
    "bubblewrap",
    "xdg-dbus-proxy",
    "xwayland",
    "Mesa-libEGL1",
    "Mesa-libGL1",
    "libgbm1",
    "libdrm2",
    "gstreamer-plugins-base",
    "gstreamer-plugins-good",
    "google-noto-sans-fonts",
    "xdg-desktop-portal",
];
const ZYPPER_APPIMAGE_HOST_PACKAGES: &[&str] = &[
    "pipewire",
    "wireplumber",
    "pipewire-pulseaudio",
    "pulseaudio-utils",
    "alsa",
    "xwayland",
    "Mesa-libEGL1",
    "Mesa-libGL1",
    "libgbm1",
    "libdrm2",
    "google-noto-sans-fonts",
    "xdg-desktop-portal",
];
const ZYPPER_PORTAL_BACKENDS: &[&str] = &[
    "xdg-desktop-portal-gtk",
    "xdg-desktop-portal-kde",
    "xdg-desktop-portal-gnome",
    "xdg-desktop-portal-wlr",
];
const DEEPFILTERNET_LADSPA_NAMES: &[&str] = &[
    "libdeep_filter_ladspa.so",
    "deep_filter_ladspa.so",
    "libdeepfilternet_ladspa.so",
    "deepfilternet_ladspa.so",
    "libdeep_filter_net_ladspa.so",
    "deep_filter_net_ladspa.so",
];
const RNNOISE_LADSPA_NAMES: &[&str] = &["librnnoise_ladspa.so", "rnnoise_ladspa.so"];
const COMPRESSOR_LADSPA_NAMES: &[&str] = &["sc4_1882.so", "compressor.so"];
const GATE_LADSPA_NAMES: &[&str] = &["gate_1410.so"];
const LIMITER_LADSPA_NAMES: &[&str] = &["fast_lookahead_limiter_1913.so", "hard_limiter_1413.so"];

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

fn existing_ladspa_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(value) = std::env::var_os("LADSPA_PATH") {
        paths.extend(std::env::split_paths(&value));
    }
    paths.extend([
        PathBuf::from("/usr/lib/ladspa"),
        PathBuf::from("/usr/lib64/ladspa"),
        PathBuf::from("/usr/local/lib/ladspa"),
        PathBuf::from("/usr/local/lib64/ladspa"),
        PathBuf::from("/usr/lib/x86_64-linux-gnu/ladspa"),
        PathBuf::from("/usr/lib/aarch64-linux-gnu/ladspa"),
        PathBuf::from("/usr/lib/arm-linux-gnueabihf/ladspa"),
    ]);

    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn ladspa_has_any(names: &[&str]) -> bool {
    existing_ladspa_paths()
        .into_iter()
        .any(|root| root.is_dir() && names.iter().any(|name| root.join(name).is_file()))
}

fn ladspa_file_has_marker(marker: &[u8], names: &[&str]) -> bool {
    existing_ladspa_paths().into_iter().any(|root| {
        if !root.is_dir() {
            return false;
        }
        names.iter().any(|name| {
            fs::read(root.join(name))
                .ok()
                .is_some_and(|bytes| bytes.windows(marker.len()).any(|window| window == marker))
        })
    })
}

fn missing_ladspa_effect_ids() -> Vec<String> {
    let mut missing = Vec::new();
    if !ladspa_file_has_marker(b"DeepFilterNet3", DEEPFILTERNET_LADSPA_NAMES) {
        push_unique(&mut missing, "deepfilternet");
    }
    if !ladspa_has_any(RNNOISE_LADSPA_NAMES) {
        push_unique(&mut missing, "rnnoise");
    }
    if !ladspa_has_any(COMPRESSOR_LADSPA_NAMES) {
        push_unique(&mut missing, "compressor");
    }
    if !ladspa_has_any(GATE_LADSPA_NAMES) {
        push_unique(&mut missing, "gate");
    }
    if !ladspa_has_any(LIMITER_LADSPA_NAMES) {
        push_unique(&mut missing, "limiter");
    }
    missing
}

fn effect_names_from_ids(ids: &[String]) -> Vec<String> {
    let catalog = EffectCatalog::default();
    ids.iter()
        .map(|id| {
            catalog
                .effects
                .iter()
                .find(|definition| &definition.id == id)
                .map(|definition| definition.name.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect()
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
    PIPEWIRE_CLIENT_STACK_PROBES
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

fn is_arch_like_system() -> bool {
    [
        "/etc/arch-release",
        "/etc/cachyos-release",
        "/etc/manjaro-release",
    ]
    .into_iter()
    .any(|path| Path::new(path).exists())
}

fn pacman_package_installed(package: &str) -> bool {
    host_command("pacman")
        .args(["-Qq", package])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn missing_arch_runtime_packages() -> Vec<&'static str> {
    if !is_arch_like_system() || !command_exists("pacman") {
        return Vec::new();
    }

    let packages = if is_appimage_install() {
        ARCH_APPIMAGE_HOST_PACKAGES
    } else {
        ARCH_RUNTIME_PACKAGES
    };

    packages
        .iter()
        .copied()
        .filter(|package| !pacman_package_installed(package))
        .chain(
            (!ARCH_PORTAL_BACKENDS
                .iter()
                .any(|package| pacman_package_installed(package)))
            .then_some("xdg-desktop-portal-gtk"),
        )
        .collect()
}

fn print_runtime_dependency_report() -> i32 {
    let manager = detect_package_manager();
    let missing_runtime = if manager == PackageManager::Unknown {
        Vec::new()
    } else {
        missing_runtime_packages_for_manager(manager)
    };
    let missing_arch = missing_arch_runtime_packages();
    let missing_helpers = missing_webkit_sandbox_helpers();
    let session_bus = session_bus_path_status();
    let missing_pipewire_stack = missing_pipewire_client_stack();
    let appimage_pipewire_conflicts = appimage_bundled_pipewire_conflicts();
    let missing_effect_ids = missing_ladspa_effect_ids();
    let missing_effect_names = effect_names_from_ids(&missing_effect_ids);
    let (effect_packages, aur_effect_packages) =
        resolve_effect_plugin_packages(manager, &missing_effect_ids);

    println!("WaveLinux runtime dependency check");
    println!("Package manager: {}", manager.id());
    println!("AppImage runtime: {}", is_appimage_install());
    println!(
        "AppImage bundled runtime: {}",
        appimage_bundled_runtime_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "unavailable".into())
    );
    println!("Arch-like system: {}", is_arch_like_system());
    println!("pacman available: {}", command_exists("pacman"));
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

    if manager == PackageManager::Unknown {
        println!("Runtime packages: package manager unsupported");
    } else if missing_runtime.is_empty() {
        println!("Runtime packages: ok");
    } else {
        println!("Runtime packages missing: {}", missing_runtime.join(" "));
        println!(
            "Install command: {}",
            install_command_for_user(manager, &missing_runtime)
        );
    }

    if missing_arch.is_empty() {
        println!("Arch runtime packages: ok");
    } else {
        println!("Arch runtime packages missing: {}", missing_arch.join(" "));
        println!(
            "Install on Arch/CachyOS: sudo pacman -Syu --needed {}",
            missing_arch.join(" ")
        );
    }

    if missing_pipewire_stack.is_empty() {
        println!("PipeWire client stack: ok");
    } else {
        println!(
            "PipeWire client stack missing: {}",
            missing_pipewire_stack.join(" ")
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

    if missing_effect_names.is_empty() {
        println!("Effect plugins: ok");
    } else {
        println!(
            "Effect plugins missing: {}",
            missing_effect_names.join(", ")
        );
        if !effect_packages.is_empty() {
            println!(
                "Effect install command: {}",
                install_command_for_user(manager, &effect_packages)
            );
        }
        if !aur_effect_packages.is_empty() {
            println!(
                "AUR effect install command: {}",
                install_aur_command_for_user(&aur_effect_packages)
            );
        }
        if effect_packages.is_empty() && aur_effect_packages.is_empty() {
            println!("Effect install command: no known package candidates were available");
        }
    }

    if !missing_helpers.is_empty() {
        println!(
            "WebKit sandbox helpers missing: {}",
            missing_helpers.join(" ")
        );
    }

    if missing_runtime.is_empty()
        && missing_arch.is_empty()
        && missing_helpers.is_empty()
        && missing_pipewire_stack.is_empty()
        && appimage_pipewire_conflicts.is_empty()
        && !matches!(session_bus, Some((_, false)))
    {
        0
    } else {
        1
    }
}

fn install_runtime_dependencies_from_cli() -> i32 {
    let manager = detect_package_manager();
    if manager == PackageManager::Unknown {
        eprintln!("WaveLinux setup: no supported package manager was found.");
        return 1;
    }

    let missing_runtime = missing_runtime_packages_for_manager(manager);
    let missing_effect_ids = missing_ladspa_effect_ids();
    let missing_effect_names = effect_names_from_ids(&missing_effect_ids);
    let (effect_packages, aur_effect_packages) =
        resolve_effect_plugin_packages(manager, &missing_effect_ids);
    let mut packages = missing_runtime.clone();
    for package in &effect_packages {
        push_unique(&mut packages, package);
    }

    if packages.is_empty() && aur_effect_packages.is_empty() {
        if missing_effect_names.is_empty() {
            println!("WaveLinux runtime packages and effect plugins are already installed.");
        } else {
            println!(
                "WaveLinux runtime packages are installed, but these effect plugins are still missing: {}",
                missing_effect_names.join(", ")
            );
            println!("No known package candidates were available for automatic install.");
        }
        return 0;
    }

    println!(
        "Installing WaveLinux setup packages with {}: {}",
        manager.id(),
        packages.join(" ")
    );
    if !packages.is_empty() {
        println!("Command: {}", install_command_for_user(manager, &packages));
    }
    if !aur_effect_packages.is_empty() {
        println!(
            "AUR command: {}",
            install_aur_command_for_user(&aur_effect_packages)
        );
    }

    let system_install = if packages.is_empty() {
        Ok(Vec::new())
    } else {
        install_system_packages(manager, &packages)
    };
    match system_install {
        Ok(_) => {
            if !aur_effect_packages.is_empty() {
                if let Err(err) = install_aur_packages(&aur_effect_packages) {
                    eprintln!("WaveLinux setup: effect plugin AUR install failed: {err}");
                }
            }
            let missing_after = missing_runtime_packages_for_manager(manager);
            let missing_effects_after = effect_names_from_ids(&missing_ladspa_effect_ids());
            if missing_after.is_empty() {
                if missing_effects_after.is_empty() {
                    println!("WaveLinux runtime dependency and effect plugin install completed.");
                } else {
                    println!(
                        "WaveLinux runtime dependency install completed. Missing effect plugins after install: {}",
                        missing_effects_after.join(", ")
                    );
                }
                0
            } else {
                eprintln!(
                    "WaveLinux setup: install completed, but these packages still look missing: {}",
                    missing_after.join(" ")
                );
                1
            }
        }
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

    let manager = detect_package_manager();
    if manager == PackageManager::Unknown {
        eprintln!(
            "WaveLinux setup: no supported package manager was found for AppImage runtime preflight."
        );
        return;
    }

    let missing_runtime = missing_runtime_packages_for_manager(manager);
    let missing_effect_ids = missing_ladspa_effect_ids();
    let missing_effect_names = effect_names_from_ids(&missing_effect_ids);
    let (effect_packages, aur_effect_packages) =
        resolve_effect_plugin_packages(manager, &missing_effect_ids);
    let mut packages = missing_runtime.clone();
    for package in &effect_packages {
        push_unique(&mut packages, package);
    }

    if packages.is_empty() && aur_effect_packages.is_empty() {
        return;
    }

    let mut package_lines = Vec::new();
    if !missing_runtime.is_empty() {
        package_lines.push(format!("Runtime: {}", missing_runtime.join(" ")));
    }
    if !missing_effect_names.is_empty() {
        package_lines.push(format!(
            "Effect plugins: {}",
            missing_effect_names.join(", ")
        ));
    }
    let mut command_lines = Vec::new();
    if !packages.is_empty() {
        command_lines.push(install_command_for_user(manager, &packages));
    }
    if !aur_effect_packages.is_empty() {
        command_lines.push(install_aur_command_for_user(&aur_effect_packages));
    }
    let commands = command_lines.join("\n");
    let prompt = format!(
        "WaveLinux needs setup packages for this Linux install.\n\nPackages:\n{}\n\nWaveLinux will ask for administrator permission and run:\n\n{}",
        package_lines.join("\n"),
        commands
    );

    if !confirm_runtime_dependency_install(&prompt) {
        if missing_runtime.is_empty() {
            show_runtime_setup_message(
                "WaveLinux effect setup skipped",
                "WaveLinux will continue launching, but missing LADSPA effect plugins will stay unavailable until installed.",
                RuntimeSetupMessageKind::Info,
            );
            return;
        } else {
            let message = format!(
                "WaveLinux setup was cancelled. Install these packages, then open WaveLinux again:\n\n{commands}"
            );
            show_runtime_setup_message(
                "WaveLinux setup cancelled",
                &message,
                RuntimeSetupMessageKind::Error,
            );
            std::process::exit(1);
        }
    }

    let system_install = if packages.is_empty() {
        Ok(Vec::new())
    } else {
        install_system_packages(manager, &packages)
    };
    match system_install {
        Ok(_) => {
            if !aur_effect_packages.is_empty() {
                if let Err(err) = install_aur_packages(&aur_effect_packages) {
                    eprintln!("WaveLinux setup: effect plugin AUR install failed: {err}");
                }
            }
            let missing_after = missing_runtime_packages_for_manager(manager);
            let missing_effects_after = effect_names_from_ids(&missing_ladspa_effect_ids());
            if missing_after.is_empty() {
                if missing_effects_after.is_empty() {
                    show_runtime_setup_message(
                        "WaveLinux setup complete",
                        "Runtime packages and LADSPA effect plugins were installed. WaveLinux will continue launching now.",
                        RuntimeSetupMessageKind::Info,
                    );
                } else if !missing_effects_after.is_empty() && !missing_effect_ids.is_empty() {
                    let message = format!(
                        "WaveLinux will continue launching, but these effect plugins are still missing:\n\n{}",
                        missing_effects_after.join(", ")
                    );
                    show_runtime_setup_message(
                        "WaveLinux effect setup incomplete",
                        &message,
                        RuntimeSetupMessageKind::Info,
                    );
                }
            } else {
                let message = format!(
                    "WaveLinux tried to install required packages, but these still look missing:\n\n{}\n\nManual command:\n{}",
                    missing_after.join(" "),
                    install_command_for_user(manager, &missing_after)
                );
                show_runtime_setup_message(
                    "WaveLinux setup incomplete",
                    &message,
                    RuntimeSetupMessageKind::Error,
                );
                std::process::exit(1);
            }
        }
        Err(err) => {
            if missing_runtime.is_empty() {
                let message = format!(
                    "WaveLinux could not install missing effect plugins.\n\n{err}\n\nWaveLinux will continue launching, but those effects will stay unavailable."
                );
                show_runtime_setup_message(
                    "WaveLinux effect setup failed",
                    &message,
                    RuntimeSetupMessageKind::Info,
                );
                return;
            }
            let message = format!(
                "WaveLinux could not install required runtime packages.\n\n{err}\n\nManual command:\n{commands}"
            );
            show_runtime_setup_message(
                "WaveLinux setup failed",
                &message,
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
    let release_url = release_url_for_settings(&settings).to_string();
    if wavelinux5_test_line() {
        let current_version = current_update_version().to_string();
        return Ok(UpdateInfo {
            available: false,
            install_supported: false,
            current_version,
            version: None,
            date: None,
            body: None,
            url: None,
            release_url,
            channel: release_channel_name(&settings).to_string(),
            endpoint,
            message: WAVELINUX5_UPDATES_DISABLED_MESSAGE.into(),
        });
    }
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
    if wavelinux5_test_line() {
        return Err(WAVELINUX5_UPDATES_DISABLED_MESSAGE.into());
    }
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
            message: "All effect plugins are already installed and detected".into(),
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

fn runtime_packages_for_manager(manager: PackageManager) -> &'static [&'static str] {
    if is_appimage_install() {
        return match manager {
            PackageManager::Apt => APT_APPIMAGE_HOST_PACKAGES,
            PackageManager::Dnf => DNF_APPIMAGE_HOST_PACKAGES,
            PackageManager::Pacman => ARCH_APPIMAGE_HOST_PACKAGES,
            PackageManager::Zypper => ZYPPER_APPIMAGE_HOST_PACKAGES,
            PackageManager::Unknown => &[],
        };
    }

    match manager {
        PackageManager::Apt => APT_RUNTIME_PACKAGES,
        PackageManager::Dnf => DNF_RUNTIME_PACKAGES,
        PackageManager::Pacman => ARCH_RUNTIME_PACKAGES,
        PackageManager::Zypper => ZYPPER_RUNTIME_PACKAGES,
        PackageManager::Unknown => &[],
    }
}

fn portal_backend_packages_for_manager(manager: PackageManager) -> &'static [&'static str] {
    match manager {
        PackageManager::Apt => APT_PORTAL_BACKENDS,
        PackageManager::Dnf => DNF_PORTAL_BACKENDS,
        PackageManager::Pacman => ARCH_PORTAL_BACKENDS,
        PackageManager::Zypper => ZYPPER_PORTAL_BACKENDS,
        PackageManager::Unknown => &[],
    }
}

fn runtime_package_available(manager: PackageManager, package: &str) -> bool {
    if manager == PackageManager::Pacman {
        return true;
    }
    package_available(manager, package)
}

fn package_installed(manager: PackageManager, package: &str) -> bool {
    match manager {
        PackageManager::Apt => host_command("dpkg-query")
            .args(["-W", "-f=${Status}", package])
            .output()
            .is_ok_and(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("install ok installed")
            }),
        PackageManager::Dnf | PackageManager::Zypper => {
            command_status_success("rpm", &["-q", package])
        }
        PackageManager::Pacman => pacman_package_installed(package),
        PackageManager::Unknown => false,
    }
}

fn missing_runtime_packages_for_manager(manager: PackageManager) -> Vec<String> {
    let mut packages = Vec::new();
    for package in runtime_packages_for_manager(manager) {
        if runtime_package_available(manager, package) && !package_installed(manager, package) {
            push_unique(&mut packages, package);
        }
    }

    let portal_backends = portal_backend_packages_for_manager(manager);
    if !portal_backends.is_empty()
        && !portal_backends
            .iter()
            .any(|package| package_installed(manager, package))
    {
        if let Some(package) = portal_backends
            .iter()
            .find(|package| runtime_package_available(manager, package))
        {
            if !package_installed(manager, package) {
                push_unique(&mut packages, package);
            }
        }
    }

    packages
}

fn install_command_for_user(manager: PackageManager, packages: &[String]) -> String {
    let package_list = packages.join(" ");
    match manager {
        PackageManager::Apt => {
            format!("sudo apt-get update && sudo apt-get install -y {package_list}")
        }
        PackageManager::Dnf => format!("sudo dnf install -y {package_list}"),
        PackageManager::Pacman => format!("sudo pacman -Syu --needed {package_list}"),
        PackageManager::Zypper => {
            format!("sudo zypper --non-interactive install --no-recommends {package_list}")
        }
        PackageManager::Unknown => format!("install manually: {package_list}"),
    }
}

fn install_aur_command_for_user(packages: &[String]) -> String {
    let package_list = packages.join(" ");
    if command_exists("paru") {
        format!("paru -S --needed {package_list}")
    } else if command_exists("yay") {
        format!("yay -S --needed {package_list}")
    } else {
        format!("install manually from AUR: {package_list}")
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
            let mut args = vec!["-Syu".into(), "--needed".into(), "--noconfirm".into()];
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

    let helpers = privilege_helper_order(
        command_exists("sudo"),
        command_exists("pkexec"),
        stdin_is_terminal(),
    );
    let mut failures = Vec::new();
    for helper in helpers {
        let mut helper_args = vec![program.to_string()];
        helper_args.extend(args.iter().cloned());
        match run_command_capture(helper, &helper_args) {
            Ok(output) => return Ok(output),
            Err(err) => failures.push(err),
        }
    }

    if failures.is_empty() {
        Err("No sudo or pkexec command is available for privileged package installation".into())
    } else {
        Err(format!(
            "privileged package installation failed after trying {}: {}",
            privilege_failure_subject(&failures),
            failures.join("; ")
        ))
    }
}

fn privilege_helper_order(
    sudo_available: bool,
    pkexec_available: bool,
    stdin_is_terminal: bool,
) -> Vec<&'static str> {
    let preferred = if stdin_is_terminal {
        ["sudo", "pkexec"]
    } else {
        ["pkexec", "sudo"]
    };
    preferred
        .into_iter()
        .filter(|helper| match *helper {
            "sudo" => sudo_available,
            "pkexec" => pkexec_available,
            _ => false,
        })
        .collect()
}

fn privilege_failure_subject(failures: &[String]) -> &'static str {
    match failures.len() {
        1 => "one helper",
        _ => "multiple helpers",
    }
}

fn run_command_capture(program: &str, args: &[String]) -> Result<Output, String> {
    let output = host_command(program)
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
    host_command(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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

fn running_as_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn stdin_is_terminal() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
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

fn wavelinux5_test_line() -> bool {
    env!("CARGO_PKG_VERSION").starts_with("5.")
}

fn apply_wavelinux5_test_line_env() {
    if !wavelinux5_test_line() {
        return;
    }
    set_env_default("WAVELINUX_XDG_APP_NAME", "WaveLinux5");
    set_env_default("WAVELINUX_GRAPH_PREFIX", "wavelinux5");
    set_env_default("WAVELINUX_GRAPH_PROPERTY_PREFIX", "wavelinux5");
    set_env_default("WAVELINUX_APP_DISPLAY_NAME", "WaveLinux5");
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

fn acquire_process_lock() -> std::io::Result<Option<ProcessLock>> {
    let lock_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let lock_path = lock_dir.join(format!("{}-5.lock", graph_prefix()));
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

fn main() {
    apply_wavelinux5_test_line_env();
    prepare_appimage_bundled_runtime();

    let args: Vec<String> = std::env::args().collect();

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

#[cfg(test)]
mod updater_tests {
    use super::*;

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
    fn privileged_install_prefers_sudo_in_terminal() {
        assert_eq!(
            privilege_helper_order(true, true, true),
            vec!["sudo", "pkexec"]
        );
    }

    #[test]
    fn privileged_install_prefers_pkexec_without_terminal() {
        assert_eq!(
            privilege_helper_order(true, true, false),
            vec!["pkexec", "sudo"]
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
