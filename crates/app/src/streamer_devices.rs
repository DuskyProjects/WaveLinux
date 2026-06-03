use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use wavelinux_engine::WaveLinuxEngine;
use wavelinux_model::{
    safe_node_id, AppStateSnapshot, DeviceInfo, MixBus, MixerConfig, StreamerAction,
    StreamerActionResult, StreamerBinding, StreamerBindingProfile, StreamerControlKind,
    StreamerDeviceCapabilities, StreamerDeviceFamily, StreamerDeviceSummary, StreamerDevicesConfig,
    StreamerLearnResult, StreamerPermissionStatus, StreamerTransport,
};

const STREAMER_POLL_MS: u64 = 800;
const HID_READ_SLEEP_MS: u64 = 35;
const HID_EVENT_DEBOUNCE_MS: u64 = 180;
const MIDI_EVENT_DEBOUNCE_MS: u64 = 80;
const LEARN_TIMEOUT: Duration = Duration::from_secs(7);
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

pub struct StreamerDeviceRuntime {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl StreamerDeviceRuntime {
    pub fn start(engine: Arc<WaveLinuxEngine>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("wavelinux-streamer-devices".into())
            .spawn(move || run_streamer_runtime(engine, thread_stop))
            .ok();
        Self { stop, handle }
    }
}

impl Drop for StreamerDeviceRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn discover_devices(state: &AppStateSnapshot) -> Vec<StreamerDeviceSummary> {
    let mut devices = BTreeMap::new();
    for device in discover_hidraw_devices() {
        insert_device(&mut devices, device);
    }
    for device in discover_midi_devices() {
        insert_device(&mut devices, device);
    }
    for device in discover_audio_profile_devices(&state.graph.inputs, &state.graph.outputs) {
        insert_device(&mut devices, device);
    }
    apply_config_to_devices(&mut devices, &state.config.streamer_devices);
    devices.into_values().collect()
}

pub fn default_profiles_for_devices(
    devices: &[StreamerDeviceSummary],
    config: &MixerConfig,
) -> Vec<StreamerBindingProfile> {
    devices
        .iter()
        .map(|device| default_profile_for_device(device, config))
        .collect()
}

pub fn learn_control(
    devices: &[StreamerDeviceSummary],
    device_id: &str,
) -> Result<StreamerLearnResult, String> {
    let Some(device) = devices.iter().find(|device| device.id == device_id) else {
        return Err("Streamer device is no longer detected".into());
    };
    match device.transport {
        StreamerTransport::Hid => learn_hid_control(device),
        StreamerTransport::Midi => learn_midi_control(device),
        StreamerTransport::AudioProfile | StreamerTransport::Bridge => Ok(StreamerLearnResult {
            device_id: device.id.clone(),
            control_id: None,
            control_kind: StreamerControlKind::Unknown,
            message: "This detected device does not expose a native button event adapter yet."
                .into(),
        }),
    }
}

pub fn run_action(
    engine: &Arc<WaveLinuxEngine>,
    action: StreamerAction,
) -> Result<StreamerActionResult, String> {
    run_action_with_value(engine, action, None)
}

fn run_action_with_value(
    engine: &Arc<WaveLinuxEngine>,
    action: StreamerAction,
    control_value: Option<f32>,
) -> Result<StreamerActionResult, String> {
    match action {
        StreamerAction::Noop => Ok(action_result(false, "No action assigned")),
        StreamerAction::MixMuteToggle { mix_id } => {
            let state = engine.get_state().map_err(|err| err.to_string())?;
            let mix = state
                .config
                .mixes
                .iter()
                .find(|mix| mix.id == mix_id)
                .ok_or_else(|| format!("Mix not found: {mix_id}"))?;
            let muted = !mix.muted;
            let mix = engine
                .set_mix_mute(mix_id, muted)
                .map_err(|err| err.to_string())?;
            Ok(action_result(
                true,
                format!(
                    "{} {}",
                    mix.name,
                    if mix.muted { "muted" } else { "unmuted" }
                ),
            ))
        }
        StreamerAction::MixVolumeSet { mix_id, volume } => {
            let mix = engine
                .set_mix_volume(mix_id, clamp_unit(volume))
                .map_err(|err| err.to_string())?;
            Ok(action_result(
                true,
                format!("{} volume {}", mix.name, percent(mix.volume)),
            ))
        }
        StreamerAction::MixVolumeSetFromControl { mix_id } => {
            let volume = control_value
                .ok_or_else(|| "This action needs a hardware control value".to_string())?;
            let mix = engine
                .set_mix_volume(mix_id, clamp_unit(volume))
                .map_err(|err| err.to_string())?;
            Ok(action_result(
                true,
                format!("{} volume {}", mix.name, percent(mix.volume)),
            ))
        }
        StreamerAction::MixVolumeAdjust { mix_id, delta } => {
            let state = engine.get_state().map_err(|err| err.to_string())?;
            let mix = state
                .config
                .mixes
                .iter()
                .find(|mix| mix.id == mix_id)
                .ok_or_else(|| format!("Mix not found: {mix_id}"))?;
            let mix = engine
                .set_mix_volume(mix.id.clone(), clamp_unit(mix.volume + delta))
                .map_err(|err| err.to_string())?;
            Ok(action_result(
                true,
                format!("{} volume {}", mix.name, percent(mix.volume)),
            ))
        }
        StreamerAction::ChannelMuteToggle { channel_id, mix_id } => {
            let state = engine.get_state().map_err(|err| err.to_string())?;
            let channel = state
                .config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .ok_or_else(|| format!("Channel not found: {channel_id}"))?;
            let bus = channel
                .mix_buses
                .get(&mix_id)
                .ok_or_else(|| format!("Mix not found on channel: {mix_id}"))?;
            let next = !bus.muted;
            let result = engine
                .set_channel_mute(channel_id, mix_id, next)
                .map_err(|err| err.to_string())?;
            Ok(action_result(
                true,
                format!(
                    "{} {}",
                    channel.name,
                    if result.muted { "muted" } else { "unmuted" }
                ),
            ))
        }
        StreamerAction::ChannelBusEnabledToggle { channel_id, mix_id } => {
            let state = engine.get_state().map_err(|err| err.to_string())?;
            let channel = state
                .config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .ok_or_else(|| format!("Channel not found: {channel_id}"))?;
            let bus = channel
                .mix_buses
                .get(&mix_id)
                .ok_or_else(|| format!("Mix not found on channel: {mix_id}"))?;
            let result = engine
                .set_channel_bus_enabled(channel_id, mix_id, !bus.enabled)
                .map_err(|err| err.to_string())?;
            Ok(action_result(
                true,
                format!(
                    "{} bus {}",
                    channel.name,
                    if result.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ),
            ))
        }
        StreamerAction::ChannelVolumeSet {
            channel_id,
            mix_id,
            volume,
        } => set_channel_volume(engine, channel_id, mix_id, clamp_unit(volume)),
        StreamerAction::ChannelVolumeSetFromControl { channel_id, mix_id } => {
            let volume = control_value
                .ok_or_else(|| "This action needs a hardware control value".to_string())?;
            set_channel_volume(engine, channel_id, mix_id, clamp_unit(volume))
        }
        StreamerAction::ChannelVolumeAdjust {
            channel_id,
            mix_id,
            delta,
        } => {
            let state = engine.get_state().map_err(|err| err.to_string())?;
            let bus = state
                .config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .and_then(|channel| channel.mix_buses.get(&mix_id))
                .ok_or_else(|| format!("Channel or mix not found: {channel_id}/{mix_id}"))?;
            set_channel_volume(engine, channel_id, mix_id, clamp_unit(bus.volume + delta))
        }
        StreamerAction::EffectBypassToggle {
            channel_id,
            instance_id,
        } => {
            let state = engine.get_state().map_err(|err| err.to_string())?;
            let channel = state
                .config
                .channels
                .iter()
                .find(|channel| channel.id == channel_id)
                .ok_or_else(|| format!("Channel not found: {channel_id}"))?;
            let effect = channel
                .effects
                .iter()
                .find(|effect| effect.instance_id == instance_id)
                .ok_or_else(|| format!("Effect not found: {instance_id}"))?;
            let next = !effect.bypassed;
            let channel = engine
                .bypass_effect(channel_id, instance_id, next)
                .map_err(|err| err.to_string())?;
            Ok(action_result(
                true,
                format!(
                    "{} effect {}",
                    channel.name,
                    if next { "bypassed" } else { "enabled" }
                ),
            ))
        }
        StreamerAction::StartOrRepairAudio => Ok(action_result(
            false,
            "Audio graph starts automatically when WaveLinux opens",
        )),
        StreamerAction::CleanupAudioGraph => Ok(action_result(
            false,
            "Audio graph cleanup runs when WaveLinux quits",
        )),
        StreamerAction::CleanupStaleAudioGraph => {
            let outputs = engine
                .cleanup_stale_audio_graph()
                .map_err(|err| err.to_string())?;
            Ok(action_result(
                true,
                format!("Stale cleanup ran {} host commands", outputs.len()),
            ))
        }
    }
}

fn set_channel_volume(
    engine: &Arc<WaveLinuxEngine>,
    channel_id: String,
    mix_id: String,
    volume: f32,
) -> Result<StreamerActionResult, String> {
    let state = engine.get_state().map_err(|err| err.to_string())?;
    let channel_name = state
        .config
        .channels
        .iter()
        .find(|channel| channel.id == channel_id)
        .map(|channel| channel.name.clone())
        .unwrap_or_else(|| channel_id.clone());
    let bus: MixBus = engine
        .set_channel_volume(channel_id, mix_id, volume)
        .map_err(|err| err.to_string())?;
    Ok(action_result(
        true,
        format!("{channel_name} volume {}", percent(bus.volume)),
    ))
}

fn run_streamer_runtime(engine: Arc<WaveLinuxEngine>, stop: Arc<AtomicBool>) {
    let mut hid_readers: BTreeMap<String, HidReader> = BTreeMap::new();
    let mut midi_readers: BTreeMap<String, MidiReader> = BTreeMap::new();
    let mut last_discovery = Instant::now() - Duration::from_millis(STREAMER_POLL_MS);
    while !stop.load(Ordering::SeqCst) {
        if last_discovery.elapsed() >= Duration::from_millis(STREAMER_POLL_MS) {
            if let Ok(state) = engine.get_state() {
                let current_devices = discover_devices(&state);
                sync_hid_readers(
                    &mut hid_readers,
                    &current_devices,
                    &state.config.streamer_devices,
                );
                sync_midi_readers(
                    &mut midi_readers,
                    &current_devices,
                    &state.config.streamer_devices,
                );
            }
            last_discovery = Instant::now();
        }

        for reader in hid_readers.values_mut() {
            reader.poll(&engine);
        }
        for reader in midi_readers.values_mut() {
            reader.poll(&engine);
        }
        thread::sleep(Duration::from_millis(HID_READ_SLEEP_MS));
    }
}

fn sync_hid_readers(
    readers: &mut BTreeMap<String, HidReader>,
    devices: &[StreamerDeviceSummary],
    config: &StreamerDevicesConfig,
) {
    let active_ids = devices
        .iter()
        .filter(|device| {
            device.transport == StreamerTransport::Hid
                && device.enabled
                && device.permission_status == StreamerPermissionStatus::Ready
        })
        .filter_map(|device| {
            let profile = config.profiles.get(&device.id)?;
            if profile.bindings.is_empty() {
                return None;
            }
            hidraw_path_from_source(&device.source)
                .map(|path| (device.id.clone(), path, profile.clone()))
        })
        .collect::<Vec<_>>();

    let keep = active_ids
        .iter()
        .map(|(device_id, _, _)| device_id.clone())
        .collect::<BTreeSet<_>>();
    readers.retain(|device_id, _| keep.contains(device_id));
    for (device_id, path, profile) in active_ids {
        if let Some(reader) = readers.get_mut(&device_id) {
            reader.profile = profile;
            continue;
        }
        if let Ok(reader) = HidReader::open(device_id.clone(), path, profile) {
            readers.insert(device_id, reader);
        }
    }
}

fn sync_midi_readers(
    readers: &mut BTreeMap<String, MidiReader>,
    devices: &[StreamerDeviceSummary],
    config: &StreamerDevicesConfig,
) {
    let active_ids = devices
        .iter()
        .filter(|device| {
            device.transport == StreamerTransport::Midi
                && device.enabled
                && device.permission_status == StreamerPermissionStatus::Ready
        })
        .filter_map(|device| {
            let profile = config.profiles.get(&device.id)?;
            if profile.bindings.is_empty() {
                return None;
            }
            midi_port_from_source(&device.source)
                .map(|port| (device.id.clone(), port, profile.clone()))
        })
        .collect::<Vec<_>>();

    let keep = active_ids
        .iter()
        .map(|(device_id, _, _)| device_id.clone())
        .collect::<BTreeSet<_>>();
    readers.retain(|device_id, _| keep.contains(device_id));
    for (device_id, port, profile) in active_ids {
        if let Some(reader) = readers.get_mut(&device_id) {
            reader.profile = profile;
            continue;
        }
        if let Ok(reader) = MidiReader::open(device_id.clone(), port, profile) {
            readers.insert(device_id, reader);
        }
    }
}

struct HidReader {
    device_id: String,
    file: File,
    previous: Vec<u8>,
    profile: StreamerBindingProfile,
    last_event: BTreeMap<String, Instant>,
}

impl HidReader {
    fn open(device_id: String, path: PathBuf, profile: StreamerBindingProfile) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).open(&path)?;
        set_nonblocking(&file)?;
        Ok(Self {
            device_id,
            file,
            previous: Vec::new(),
            profile,
            last_event: BTreeMap::new(),
        })
    }

    fn poll(&mut self, engine: &Arc<WaveLinuxEngine>) {
        let mut buffer = [0_u8; 128];
        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    let report = &buffer[..size];
                    if let Some(control_id) = control_id_from_report(&self.previous, report) {
                        self.previous = report.to_vec();
                        self.dispatch(engine, &control_id);
                    } else {
                        self.previous = report.to_vec();
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    fn dispatch(&mut self, engine: &Arc<WaveLinuxEngine>, control_id: &str) {
        dispatch_binding(
            engine,
            &self.device_id,
            &self.profile,
            &mut self.last_event,
            control_id,
            None,
            Duration::from_millis(HID_EVENT_DEBOUNCE_MS),
        );
    }
}

#[derive(Debug, Clone)]
struct StreamerControlEvent {
    control_id: String,
    value: Option<f32>,
}

struct MidiReader {
    device_id: String,
    profile: StreamerBindingProfile,
    capture: MidiCapture,
    last_event: BTreeMap<String, Instant>,
}

impl MidiReader {
    fn open(device_id: String, port: String, profile: StreamerBindingProfile) -> io::Result<Self> {
        Ok(Self {
            device_id,
            profile,
            capture: MidiCapture::spawn(&port, "wavelinux-midi-streamer")?,
            last_event: BTreeMap::new(),
        })
    }

    fn poll(&mut self, engine: &Arc<WaveLinuxEngine>) {
        loop {
            match self.capture.try_recv() {
                Ok(event) => self.dispatch(engine, event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn dispatch(&mut self, engine: &Arc<WaveLinuxEngine>, event: StreamerControlEvent) {
        dispatch_binding(
            engine,
            &self.device_id,
            &self.profile,
            &mut self.last_event,
            &event.control_id,
            event.value,
            Duration::from_millis(MIDI_EVENT_DEBOUNCE_MS),
        );
    }
}

struct MidiCapture {
    rx: Receiver<StreamerControlEvent>,
    child: Child,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MidiCapture {
    fn spawn(port: &str, thread_name: &str) -> io::Result<Self> {
        let mut child = host_command("aseqdump")
            .args(["-p", port])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "aseqdump stdout unavailable"))?;
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    if thread_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(line) = line else {
                        break;
                    };
                    if let Some(event) = control_event_from_aseqdump_line(&line) {
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                }
            })
            .ok();
        Ok(Self {
            rx,
            child,
            stop,
            handle,
        })
    }

    fn try_recv(&self) -> Result<StreamerControlEvent, TryRecvError> {
        self.rx.try_recv()
    }

    fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<StreamerControlEvent, mpsc::RecvTimeoutError> {
        self.rx.recv_timeout(timeout)
    }
}

impl Drop for MidiCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn dispatch_binding(
    engine: &Arc<WaveLinuxEngine>,
    device_id: &str,
    profile: &StreamerBindingProfile,
    last_event: &mut BTreeMap<String, Instant>,
    control_id: &str,
    value: Option<f32>,
    debounce: Duration,
) {
    let now = Instant::now();
    let Some(binding) = profile
        .bindings
        .iter()
        .find(|binding| binding.control_id == control_id)
        .cloned()
    else {
        return;
    };
    let debounce = if value.is_some()
        && matches!(
            &binding.action,
            StreamerAction::MixVolumeSetFromControl { .. }
                | StreamerAction::ChannelVolumeSetFromControl { .. }
        ) {
        Duration::ZERO
    } else {
        debounce
    };
    if last_event
        .get(control_id)
        .is_some_and(|last| now.duration_since(*last) < debounce)
    {
        return;
    }
    last_event.insert(control_id.to_string(), now);
    let _ = run_action_with_value(engine, binding.action, value).map_err(|err| {
        eprintln!("WaveLinux streamer action failed for {device_id} {control_id}: {err}");
    });
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn insert_device(
    devices: &mut BTreeMap<String, StreamerDeviceSummary>,
    device: StreamerDeviceSummary,
) {
    devices
        .entry(device.id.clone())
        .and_modify(|existing| merge_device(existing, &device))
        .or_insert(device);
}

fn merge_device(existing: &mut StreamerDeviceSummary, incoming: &StreamerDeviceSummary) {
    existing.capabilities.buttons |= incoming.capabilities.buttons;
    existing.capabilities.dials |= incoming.capabilities.dials;
    existing.capabilities.faders |= incoming.capabilities.faders;
    existing.capabilities.pads |= incoming.capabilities.pads;
    existing.capabilities.display_feedback |= incoming.capabilities.display_feedback;
    existing.capabilities.midi_feedback |= incoming.capabilities.midi_feedback;
    existing.capabilities.audio_endpoint |= incoming.capabilities.audio_endpoint;
    if existing.permission_status == StreamerPermissionStatus::UnsupportedProtocol {
        existing.permission_status = incoming.permission_status.clone();
        existing.message = incoming.message.clone();
    }
}

fn apply_config_to_devices(
    devices: &mut BTreeMap<String, StreamerDeviceSummary>,
    config: &StreamerDevicesConfig,
) {
    for device in devices.values_mut() {
        if let Some(profile) = config.profiles.get(&device.id) {
            device.enabled = profile.enabled;
        }
    }
}

fn discover_hidraw_devices() -> Vec<StreamerDeviceSummary> {
    let Ok(entries) = fs::read_dir("/sys/class/hidraw") else {
        return Vec::new();
    };
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let hidraw_name = entry.file_name().to_string_lossy().to_string();
        let dev_node = PathBuf::from("/dev").join(&hidraw_name);
        let uevent = fs::read_to_string(entry.path().join("device/uevent")).unwrap_or_default();
        let attrs = parse_key_values(&uevent);
        let name = attrs
            .get("HID_NAME")
            .cloned()
            .unwrap_or_else(|| hidraw_name.clone());
        let (vendor_id, product_id) = attrs
            .get("HID_ID")
            .and_then(|value| parse_hid_id(value))
            .unwrap_or_else(|| (None, None));
        let Some((family, capabilities, supported_message)) =
            classify_hid_device(vendor_id.as_deref(), product_id.as_deref(), &name)
        else {
            continue;
        };
        let permission_status = if family == StreamerDeviceFamily::Loupedeck {
            StreamerPermissionStatus::UnsupportedProtocol
        } else {
            hid_permission_status(&dev_node)
        };
        let message = match permission_status {
            StreamerPermissionStatus::Ready => supported_message,
            StreamerPermissionStatus::PermissionDenied => {
                "Device detected, but hidraw permissions block WaveLinux from reading controls"
                    .into()
            }
            StreamerPermissionStatus::Busy => {
                "Device detected, but another app appears to have it busy".into()
            }
            StreamerPermissionStatus::MissingRuntime => {
                "Device detected, but the HID runtime is unavailable".into()
            }
            StreamerPermissionStatus::UnsupportedProtocol => {
                "Device detected; native protocol support is not enabled for this model yet".into()
            }
        };
        let unique = attrs
            .get("HID_UNIQ")
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| hidraw_name.clone());
        let id = format!(
            "hid:{}:{}:{}",
            vendor_id.clone().unwrap_or_else(|| "unknown".into()),
            product_id.clone().unwrap_or_else(|| "unknown".into()),
            safe_node_id(&unique)
        );
        devices.push(StreamerDeviceSummary {
            id,
            name: streamer_name(&name),
            description: name,
            family,
            transport: StreamerTransport::Hid,
            vendor_id,
            product_id,
            capabilities,
            connected: true,
            enabled: true,
            permission_status,
            matched_profile_id: None,
            source: format!("hidraw:{}", dev_node.display()),
            message,
        });
    }
    devices
}

fn discover_midi_devices() -> Vec<StreamerDeviceSummary> {
    let Ok(raw) = fs::read_to_string("/proc/asound/seq/clients") else {
        return Vec::new();
    };
    let mut devices = parse_midi_clients(&raw);
    if !command_exists("aseqdump") {
        for device in &mut devices {
            device.permission_status = StreamerPermissionStatus::MissingRuntime;
            device.message =
                "MIDI control surface detected; install alsa-utils so WaveLinux can capture events"
                    .into();
        }
    }
    devices
}

fn discover_audio_profile_devices<'a>(
    inputs: impl IntoIterator<Item = &'a DeviceInfo>,
    outputs: impl IntoIterator<Item = &'a DeviceInfo>,
) -> Vec<StreamerDeviceSummary> {
    let mut devices = BTreeMap::new();
    for device in inputs.into_iter().chain(outputs) {
        if device.is_virtual {
            continue;
        }
        let Some((family, capabilities, message)) = classify_audio_device(device) else {
            continue;
        };
        let id = format!("audio:{}", safe_node_id(&device.id));
        let summary = StreamerDeviceSummary {
            id,
            name: streamer_name(&device.description),
            description: device.description.clone(),
            family,
            transport: StreamerTransport::AudioProfile,
            vendor_id: normalize_usb_id(device.vendor_id.as_deref()),
            product_id: normalize_usb_id(device.product_id.as_deref()),
            capabilities,
            connected: true,
            enabled: true,
            permission_status: StreamerPermissionStatus::UnsupportedProtocol,
            matched_profile_id: device.matched_profile_id.clone(),
            source: format!("pipewire:{}", device.id),
            message,
        };
        insert_device(&mut devices, summary);
    }
    devices.into_values().collect()
}

fn parse_midi_clients(raw: &str) -> Vec<StreamerDeviceSummary> {
    let mut devices = Vec::new();
    let mut current_client: Option<(String, String)> = None;
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Client ") {
            current_client = parse_midi_client_header(rest);
            continue;
        }
        let Some((client_id, client_name)) = current_client.as_ref() else {
            continue;
        };
        if !trimmed.starts_with("Port ") {
            continue;
        }
        let Some((port_id, port_name)) = parse_midi_port_line(trimmed) else {
            continue;
        };
        let combined_name = format!("{client_name} {port_name}");
        let Some((family, capabilities, message)) = classify_midi_device(&combined_name) else {
            continue;
        };
        let id = format!(
            "midi:{}:{}:{}",
            client_id,
            port_id,
            safe_node_id(&combined_name)
        );
        devices.push(StreamerDeviceSummary {
            id,
            name: streamer_name(&combined_name),
            description: combined_name,
            family,
            transport: StreamerTransport::Midi,
            vendor_id: None,
            product_id: None,
            capabilities,
            connected: true,
            enabled: true,
            permission_status: StreamerPermissionStatus::Ready,
            matched_profile_id: None,
            source: format!("alsa-seq:{client_id}:{port_id}"),
            message,
        });
    }
    devices
}

fn default_profile_for_device(
    device: &StreamerDeviceSummary,
    config: &MixerConfig,
) -> StreamerBindingProfile {
    let mut bindings = Vec::new();
    let monitor = config
        .mixes
        .iter()
        .find(|mix| mix.id == "monitor")
        .or_else(|| config.mixes.first());
    let stream = config
        .mixes
        .iter()
        .find(|mix| mix.id == "stream")
        .or_else(|| config.mixes.get(1))
        .or(monitor);
    if let Some(mix) = monitor {
        bindings.push(button_binding(
            default_control_id(device, 0),
            "Monitor mute",
            StreamerAction::MixMuteToggle {
                mix_id: mix.id.clone(),
            },
        ));
    }
    if let Some(mix) = stream {
        bindings.push(button_binding(
            default_control_id(device, 1),
            "Stream mute",
            StreamerAction::MixMuteToggle {
                mix_id: mix.id.clone(),
            },
        ));
    }
    bindings.push(button_binding(
        default_control_id(device, 2),
        "Prune stale audio",
        StreamerAction::CleanupStaleAudioGraph,
    ));

    let mix_id = stream
        .or(monitor)
        .map(|mix| mix.id.clone())
        .unwrap_or_else(|| "stream".into());
    for (index, channel) in config.channels.iter().take(4).enumerate() {
        bindings.push(button_binding(
            default_control_id(device, index + 3),
            format!("{} mute", channel.name),
            StreamerAction::ChannelMuteToggle {
                channel_id: channel.id.clone(),
                mix_id: mix_id.clone(),
            },
        ));
    }
    if device.transport == StreamerTransport::Midi
        && (device.capabilities.faders || device.capabilities.dials)
    {
        if let Some(mix) = monitor {
            bindings.push(fader_binding(
                "midi:cc:7",
                format!("{} volume", mix.name),
                StreamerAction::MixVolumeSetFromControl {
                    mix_id: mix.id.clone(),
                },
            ));
        }
        if let Some(mix) = stream {
            bindings.push(fader_binding(
                "midi:cc:8",
                format!("{} volume", mix.name),
                StreamerAction::MixVolumeSetFromControl {
                    mix_id: mix.id.clone(),
                },
            ));
        }
    }

    StreamerBindingProfile {
        device_id: device.id.clone(),
        family: Some(device.family.clone()),
        name: device.name.clone(),
        enabled: true,
        safe_preset: true,
        bindings,
    }
}

fn button_binding(
    control_id: String,
    label: impl Into<String>,
    action: StreamerAction,
) -> StreamerBinding {
    control_binding(control_id, label, StreamerControlKind::Button, action)
}

fn fader_binding(
    control_id: impl Into<String>,
    label: impl Into<String>,
    action: StreamerAction,
) -> StreamerBinding {
    control_binding(control_id.into(), label, StreamerControlKind::Fader, action)
}

fn control_binding(
    control_id: String,
    label: impl Into<String>,
    control_kind: StreamerControlKind,
    action: StreamerAction,
) -> StreamerBinding {
    StreamerBinding {
        control_id,
        label: label.into(),
        control_kind,
        action,
    }
}

fn default_control_id(device: &StreamerDeviceSummary, index: usize) -> String {
    match device.transport {
        StreamerTransport::Hid => {
            let byte = 1 + (index / 8);
            let bit = 1_u8 << (index % 8);
            format!("hid:byte:{byte}:{bit}")
        }
        StreamerTransport::Midi => format!("midi:note:{}", 36 + index),
        StreamerTransport::AudioProfile | StreamerTransport::Bridge => {
            format!("control:{}", index + 1)
        }
    }
}

fn learn_hid_control(device: &StreamerDeviceSummary) -> Result<StreamerLearnResult, String> {
    let Some(path) = hidraw_path_from_source(&device.source) else {
        return Err("Detected HID device does not expose a hidraw path".into());
    };
    let mut file = OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(|err| format!("Could not open {}: {err}", path.display()))?;
    set_nonblocking(&file).map_err(|err| err.to_string())?;
    let deadline = Instant::now() + LEARN_TIMEOUT;
    let mut previous = Vec::new();
    let mut buffer = [0_u8; 128];
    while Instant::now() < deadline {
        match file.read(&mut buffer) {
            Ok(0) => {}
            Ok(size) => {
                let report = &buffer[..size];
                if let Some(control_id) = control_id_from_report(&previous, report) {
                    return Ok(StreamerLearnResult {
                        device_id: device.id.clone(),
                        control_id: Some(control_id),
                        control_kind: StreamerControlKind::Button,
                        message: "Control captured from the next HID input report".into(),
                    });
                }
                previous = report.to_vec();
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(err.to_string()),
        }
        thread::sleep(Duration::from_millis(HID_READ_SLEEP_MS));
    }
    Ok(StreamerLearnResult {
        device_id: device.id.clone(),
        control_id: None,
        control_kind: StreamerControlKind::Unknown,
        message: "No button press was captured before the learn timeout".into(),
    })
}

fn learn_midi_control(device: &StreamerDeviceSummary) -> Result<StreamerLearnResult, String> {
    if !command_exists("aseqdump") {
        return Ok(StreamerLearnResult {
            device_id: device.id.clone(),
            control_id: None,
            control_kind: StreamerControlKind::Unknown,
            message: "MIDI device detected, but aseqdump is missing. Install alsa-utils to learn or run bindings.".into(),
        });
    }
    let Some(port) = midi_port_from_source(&device.source) else {
        return Err("Detected MIDI device does not expose an ALSA sequencer port".into());
    };
    let capture =
        MidiCapture::spawn(&port, "wavelinux-midi-learn").map_err(|err| err.to_string())?;
    let deadline = Instant::now() + LEARN_TIMEOUT;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = remaining.min(Duration::from_millis(100));
        match capture.recv_timeout(timeout) {
            Ok(event) => {
                return Ok(StreamerLearnResult {
                    device_id: device.id.clone(),
                    control_kind: midi_control_kind(&event.control_id),
                    control_id: Some(event.control_id),
                    message: "Control captured from the next ALSA MIDI event".into(),
                });
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(StreamerLearnResult {
        device_id: device.id.clone(),
        control_id: None,
        control_kind: StreamerControlKind::Unknown,
        message: "No MIDI event was captured before the learn timeout".into(),
    })
}

fn control_id_from_report(previous: &[u8], report: &[u8]) -> Option<String> {
    for (index, value) in report.iter().copied().enumerate() {
        let old = previous.get(index).copied().unwrap_or(0);
        if value != old && value != 0 {
            let pressed_bits = (value ^ old) & value;
            let control_value = if pressed_bits != 0 {
                pressed_bits & pressed_bits.wrapping_neg()
            } else {
                value
            };
            return Some(format!("hid:byte:{index}:{control_value}"));
        }
    }
    report
        .iter()
        .copied()
        .enumerate()
        .find(|(_, value)| *value != 0)
        .map(|(index, value)| format!("hid:byte:{index}:{value}"))
}

fn control_event_from_aseqdump_line(line: &str) -> Option<StreamerControlEvent> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("note on") {
        let note = last_number_after(&lower, "note ")?;
        let velocity = number_after(&lower, "velocity ").unwrap_or(1);
        if velocity == 0 {
            return None;
        }
        return Some(StreamerControlEvent {
            control_id: format!("midi:note:{note}"),
            value: Some(midi_unit_value(velocity)),
        });
    }
    if lower.contains("note off") {
        return None;
    }
    if lower.contains("control change") || lower.contains("controller ") {
        let controller = number_after(&lower, "controller ")?;
        return Some(StreamerControlEvent {
            control_id: format!("midi:cc:{controller}"),
            value: number_after(&lower, "value ").map(midi_unit_value),
        });
    }
    if lower.contains("program change") {
        let program = last_number_after(&lower, "program ")?;
        return Some(StreamerControlEvent {
            control_id: format!("midi:program:{program}"),
            value: None,
        });
    }
    None
}

fn classify_hid_device(
    vendor_id: Option<&str>,
    product_id: Option<&str>,
    name: &str,
) -> Option<(StreamerDeviceFamily, StreamerDeviceCapabilities, String)> {
    let text = name.to_ascii_lowercase();
    let vendor = vendor_id.unwrap_or_default();
    let product = product_id.unwrap_or_default();
    if vendor == "0fd9" && (text.contains("stream deck") || is_known_stream_deck_pid(product)) {
        return Some((
            StreamerDeviceFamily::StreamDeck,
            StreamerDeviceCapabilities {
                buttons: true,
                dials: text.contains("plus"),
                display_feedback: true,
                ..StreamerDeviceCapabilities::default()
            },
            "Stream Deck HID device detected; WaveLinux will only open hidraw while bindings are enabled".into(),
        ));
    }
    if text.contains("loupedeck") || text.contains("razer stream controller") {
        return Some((
            StreamerDeviceFamily::Loupedeck,
            StreamerDeviceCapabilities {
                buttons: true,
                dials: true,
                display_feedback: true,
                ..StreamerDeviceCapabilities::default()
            },
            "Loupedeck-style device detected; protocol support is still gated as unsupported"
                .into(),
        ));
    }
    if text.contains("x-keys") || text.contains("xkeys") || vendor == "05f3" {
        return Some((
            StreamerDeviceFamily::XKeys,
            StreamerDeviceCapabilities {
                buttons: true,
                ..StreamerDeviceCapabilities::default()
            },
            "X-keys HID device detected".into(),
        ));
    }
    None
}

fn classify_midi_device(
    name: &str,
) -> Option<(StreamerDeviceFamily, StreamerDeviceCapabilities, String)> {
    let text = name.to_ascii_lowercase();
    if text.contains("midi through") {
        return None;
    }
    if text.contains("rodecaster")
        || text.contains("rode caster")
        || text.contains("streamer x")
        || text.contains("rode")
    {
        return Some((
            StreamerDeviceFamily::Rode,
            StreamerDeviceCapabilities {
                buttons: true,
                dials: true,
                faders: true,
                pads: true,
                midi_feedback: true,
                ..StreamerDeviceCapabilities::default()
            },
            "RODE MIDI control surface detected".into(),
        ));
    }
    if text.contains("goxlr") || text.contains("tc-helicon") || text.contains("tc helicon") {
        return Some((
            StreamerDeviceFamily::GoXlr,
            StreamerDeviceCapabilities {
                buttons: true,
                dials: true,
                faders: true,
                midi_feedback: true,
                ..StreamerDeviceCapabilities::default()
            },
            "GoXLR-style MIDI control surface detected".into(),
        ));
    }
    if text.contains("x-touch")
        || text.contains("nanokontrol")
        || text.contains("nano kontrol")
        || text.contains("launch control")
        || text.contains("midi mix")
        || text.contains("midimix")
    {
        return Some((
            StreamerDeviceFamily::MidiSurface,
            StreamerDeviceCapabilities {
                buttons: true,
                dials: true,
                faders: true,
                midi_feedback: true,
                ..StreamerDeviceCapabilities::default()
            },
            "Generic MIDI control surface detected".into(),
        ));
    }
    None
}

fn classify_audio_device(
    device: &DeviceInfo,
) -> Option<(StreamerDeviceFamily, StreamerDeviceCapabilities, String)> {
    let profile = device.matched_profile_id.as_deref().unwrap_or_default();
    let text = format!("{} {} {}", device.id, device.name, device.description).to_ascii_lowercase();
    let audio_cap = StreamerDeviceCapabilities {
        audio_endpoint: true,
        ..StreamerDeviceCapabilities::default()
    };
    if profile.starts_with("rode.")
        || text.contains("rodecaster")
        || text.contains("rode caster")
        || text.contains("streamer x")
        || text.contains("nt-usb")
    {
        return Some((
            StreamerDeviceFamily::Rode,
            StreamerDeviceCapabilities {
                buttons: true,
                dials: true,
                pads: true,
                audio_endpoint: true,
                ..StreamerDeviceCapabilities::default()
            },
            "RODE audio endpoint detected; hardware control requires a visible MIDI/control port"
                .into(),
        ));
    }
    if profile.starts_with("tc-helicon.goxlr")
        || text.contains("goxlr")
        || text.contains("tc-helicon")
    {
        return Some((
            StreamerDeviceFamily::GoXlr,
            StreamerDeviceCapabilities {
                buttons: true,
                dials: true,
                faders: true,
                audio_endpoint: true,
                ..StreamerDeviceCapabilities::default()
            },
            "GoXLR audio endpoint detected; native bindings use control-surface events when exposed".into(),
        ));
    }
    if text.contains("beacn") {
        return Some((
            StreamerDeviceFamily::UnknownSupported,
            audio_cap,
            "BEACN audio endpoint detected; WaveLinux has no native control protocol for this device yet".into(),
        ));
    }
    None
}

fn is_known_stream_deck_pid(product_id: &str) -> bool {
    matches!(
        product_id,
        "0060" | "0063" | "006c" | "006d" | "0070" | "0080" | "0084" | "0086" | "0090" | "009a"
    )
}

fn hid_permission_status(path: &Path) -> StreamerPermissionStatus {
    match OpenOptions::new().read(true).open(path) {
        Ok(_) => StreamerPermissionStatus::Ready,
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            StreamerPermissionStatus::PermissionDenied
        }
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => StreamerPermissionStatus::Busy,
        Err(_) => StreamerPermissionStatus::PermissionDenied,
    }
}

fn parse_key_values(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

fn parse_hid_id(value: &str) -> Option<(Option<String>, Option<String>)> {
    let mut parts = value.split(':');
    let _bus = parts.next()?;
    let vendor_id = parts.next().and_then(|value| normalize_usb_id(Some(value)));
    let product_id = parts.next().and_then(|value| normalize_usb_id(Some(value)));
    Some((vendor_id, product_id))
}

fn parse_midi_client_header(rest: &str) -> Option<(String, String)> {
    let (id, name_part) = rest.split_once(':')?;
    let name = name_part.split('"').nth(1)?.to_string();
    Some((id.trim().to_string(), name))
}

fn parse_midi_port_line(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("Port ")?;
    let (id, name_part) = rest.split_once(':')?;
    let name = name_part.split('"').nth(1)?.to_string();
    Some((id.trim().to_string(), name))
}

fn streamer_name(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "Streamer Device".into()
    } else {
        value.chars().take(96).collect()
    }
}

fn normalize_usb_id(value: Option<&str>) -> Option<String> {
    let normalized = value?
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    if normalized.is_empty() {
        None
    } else if normalized.len() > 4 {
        Some(normalized[normalized.len() - 4..].to_string())
    } else {
        Some(format!("{normalized:0>4}"))
    }
}

fn hidraw_path_from_source(source: &str) -> Option<PathBuf> {
    source.strip_prefix("hidraw:").map(PathBuf::from)
}

fn midi_port_from_source(source: &str) -> Option<String> {
    let rest = source.strip_prefix("alsa-seq:")?;
    let mut parts = rest.split(':');
    let client = parts.next()?;
    let port = parts.next()?;
    Some(format!("{client}:{port}"))
}

fn midi_control_kind(control_id: &str) -> StreamerControlKind {
    if control_id.starts_with("midi:note:") || control_id.starts_with("midi:program:") {
        StreamerControlKind::Button
    } else if control_id.starts_with("midi:cc:") {
        StreamerControlKind::Fader
    } else {
        StreamerControlKind::Unknown
    }
}

fn number_after(text: &str, marker: &str) -> Option<u16> {
    let start = text.find(marker)? + marker.len();
    number_at(text, start)
}

fn last_number_after(text: &str, marker: &str) -> Option<u16> {
    let start = text.rfind(marker)? + marker.len();
    number_at(text, start)
}

fn number_at(text: &str, start: usize) -> Option<u16> {
    let digits = text[start..]
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    digits.parse().ok()
}

fn midi_unit_value(value: u16) -> f32 {
    (value as f32 / 127.0).clamp(0.0, 1.0)
}

fn action_result(performed: bool, message: impl Into<String>) -> StreamerActionResult {
    StreamerActionResult {
        performed,
        message: message.into(),
    }
}

fn clamp_unit(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn percent(value: f32) -> String {
    format!("{}%", (clamp_unit(value) * 100.0).round())
}

#[allow(dead_code)]
fn command_exists(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[allow(dead_code)]
fn run_command_status(program: &str, args: &[&str]) -> bool {
    host_command(program)
        .args(args)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hidraw_devices_when_sysfs_is_absent_or_empty() {
        let raw = "";
        assert!(parse_midi_clients(raw).is_empty());
    }

    #[test]
    fn parses_rode_midi_client() {
        let raw = r#"
Client 24 : "RODECaster Pro II" [Kernel]
    Port   0 : "RODECaster Pro II MIDI 1" (RWeX)
"#;
        let devices = parse_midi_clients(raw);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].family, StreamerDeviceFamily::Rode);
        assert_eq!(devices[0].transport, StreamerTransport::Midi);
    }

    #[test]
    fn parses_hid_id() {
        let (vendor, product) = parse_hid_id("0003:00000FD9:00000080").unwrap();
        assert_eq!(vendor.as_deref(), Some("0fd9"));
        assert_eq!(product.as_deref(), Some("0080"));
    }

    #[test]
    fn default_profile_skips_audio_lifecycle_actions() {
        let config = MixerConfig::default();
        let device = StreamerDeviceSummary {
            id: "hidraw-test".into(),
            name: "Stream Deck".into(),
            family: StreamerDeviceFamily::StreamDeck,
            transport: StreamerTransport::Hid,
            capabilities: StreamerDeviceCapabilities {
                buttons: true,
                ..StreamerDeviceCapabilities::default()
            },
            connected: true,
            permission_status: StreamerPermissionStatus::Ready,
            source: "hidraw:/dev/hidraw0".into(),
            ..StreamerDeviceSummary::default()
        };

        let profile = default_profile_for_device(&device, &config);

        assert!(!profile.bindings.iter().any(|binding| {
            matches!(
                binding.action,
                StreamerAction::StartOrRepairAudio | StreamerAction::CleanupAudioGraph
            )
        }));
        assert!(profile
            .bindings
            .iter()
            .any(|binding| matches!(binding.action, StreamerAction::CleanupStaleAudioGraph)));
    }

    #[test]
    fn extracts_changed_hid_control() {
        let control = control_id_from_report(&[0, 0, 0], &[0, 4, 0]);
        assert_eq!(control.as_deref(), Some("hid:byte:1:4"));
    }

    #[test]
    fn extracts_new_hid_bit_from_combined_report_byte() {
        let control = control_id_from_report(&[0, 1, 0], &[0, 3, 0]);
        assert_eq!(control.as_deref(), Some("hid:byte:1:2"));
    }

    #[test]
    fn parses_aseqdump_note_on() {
        let event = control_event_from_aseqdump_line(
            " 24:0   Note on                 0, note 36, velocity 127",
        )
        .unwrap();
        assert_eq!(event.control_id, "midi:note:36");
        assert_eq!(event.value, Some(1.0));
    }

    #[test]
    fn ignores_aseqdump_note_on_with_zero_velocity() {
        let event = control_event_from_aseqdump_line(
            " 24:0   Note on                 0, note 36, velocity 0",
        );
        assert!(event.is_none());
    }

    #[test]
    fn parses_aseqdump_control_change() {
        let event = control_event_from_aseqdump_line(
            " 24:0   Control change          0, controller 7, value 64",
        )
        .unwrap();
        assert_eq!(event.control_id, "midi:cc:7");
        assert!(event.value.unwrap() > 0.5 && event.value.unwrap() < 0.51);
    }
}
