use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::raw::{c_int, c_uint};
use std::process::Command;
use std::time::Duration;

use libloading::Library;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use wavelinux_model::DeviceInfo;

// Wave XLR control details follow the OpenWave USB backend protocol notes.
const ELGATO_VENDOR_ID: u16 = 0x0fd9;
const WAVE_XLR_PRODUCT_ID: u16 = 0x007d;
const WAVE_XLR_GAIN_MAX: u16 = 0x5000;
const WAVE_XLR_HP_MIN_DB: f32 = -60.0;
const WAVE_XLR_HP_MAX_DB: f32 = 0.0;

const BREQUEST_READ: u8 = 0x85;
const BREQUEST_WRITE: u8 = 0x05;
const RT_CLASS_IN: u8 = 0xa1;
const RT_CLASS_OUT: u8 = 0x21;
const WVALUE_CONFIG: u16 = 0x0000;
const WVALUE_DEVICE_INFO: u16 = 0x000a;
const WINDEX: u16 = 0x3303;
const CONFIG_LEN: usize = 34;
const DEVICE_INFO_LEN: usize = 51;
const USB_TIMEOUT: Duration = Duration::from_millis(1_000);

const OFF_GAIN: usize = 0;
const OFF_MUTE: usize = 4;
const OFF_HP_VOLUME: usize = 9;
const OFF_VOLUME_SELECT: usize = 14;
const OFF_LOW_IMPEDANCE: usize = 33;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ElgatoDeviceSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: ElgatoDeviceKind,
    pub controls_supported: bool,
    pub bus: Option<String>,
    pub vendor_id: Option<String>,
    pub product_id: Option<String>,
    pub alsa_card: Option<String>,
    pub matched_profile_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ElgatoDeviceKind {
    WaveXlr,
    WaveMicrophone,
    CaptureAudio,
    AudioEndpoint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ElgatoWaveXlrState {
    pub connected: bool,
    pub gain_raw: u16,
    pub gain_max_raw: u16,
    pub gain_percent: f32,
    pub muted: bool,
    pub hp_volume_db: f32,
    pub hp_min_db: f32,
    pub hp_max_db: f32,
    pub low_impedance: bool,
    pub volume_select: ElgatoWaveXlrKnobTarget,
    pub api_version: Option<String>,
    pub firmware_version: Option<String>,
    pub serial: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ElgatoWaveXlrKnobTarget {
    Gain,
    Headphones,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ElgatoWaveXlrDeviceInfo {
    api_version: String,
    firmware_version: String,
    serial: String,
}

#[derive(Debug, Error)]
pub enum ElgatoError {
    #[error("Elgato Wave XLR was not opened; the device may be disconnected or USB access to 0fd9:007d may be denied")]
    NotOpened,
    #[error("libusb is unavailable: {0}")]
    LibUsbUnavailable(String),
    #[error("libusb symbol is unavailable: {0}")]
    LibUsbSymbol(String),
    #[error("USB {operation} failed with libusb error {code}")]
    UsbCode { operation: &'static str, code: i32 },
    #[error("Wave XLR returned {actual} bytes for {name}, expected at least {expected}")]
    ShortRead {
        name: &'static str,
        expected: usize,
        actual: usize,
    },
}

pub fn summarize_devices<'a>(
    inputs: impl IntoIterator<Item = &'a DeviceInfo>,
    outputs: impl IntoIterator<Item = &'a DeviceInfo>,
) -> Vec<ElgatoDeviceSummary> {
    let mut devices = BTreeMap::new();
    for device in inputs.into_iter().chain(outputs) {
        if !is_elgato_device(device) {
            continue;
        }
        let key = elgato_device_key(device);
        devices
            .entry(key)
            .and_modify(|summary: &mut ElgatoDeviceSummary| merge_summary(summary, device))
            .or_insert_with(|| summary_from_device(device));
    }
    devices.into_values().collect()
}

pub fn read_wave_xlr_state() -> Result<ElgatoWaveXlrState, ElgatoError> {
    WaveXlrController::open()?.read_state()
}

pub fn set_wave_xlr_gain(gain_raw: u16) -> Result<ElgatoWaveXlrState, ElgatoError> {
    let mut controller = WaveXlrController::open()?;
    controller.set_gain(gain_raw)?;
    controller.read_state()
}

pub fn set_wave_xlr_mute(muted: bool) -> Result<ElgatoWaveXlrState, ElgatoError> {
    let mut controller = WaveXlrController::open()?;
    controller.set_mute(muted)?;
    sync_alsa_mute(muted);
    controller.read_state()
}

pub fn set_wave_xlr_hp_volume_db(db: f32) -> Result<ElgatoWaveXlrState, ElgatoError> {
    let mut controller = WaveXlrController::open()?;
    let db = clamp_hp_db(db);
    let raw = hp_db_to_raw(db);
    controller.set_hp_volume_raw(raw)?;
    sync_alsa_hp_volume(raw);
    controller.read_state()
}

pub fn set_wave_xlr_low_impedance(enabled: bool) -> Result<ElgatoWaveXlrState, ElgatoError> {
    let mut controller = WaveXlrController::open()?;
    controller.set_low_impedance(enabled)?;
    controller.read_state()
}

type LibUsbInit = unsafe extern "C" fn(*mut *mut c_void) -> c_int;
type LibUsbExit = unsafe extern "C" fn(*mut c_void);
type LibUsbOpenDeviceWithVidPid = unsafe extern "C" fn(*mut c_void, u16, u16) -> *mut c_void;
type LibUsbClose = unsafe extern "C" fn(*mut c_void);
type LibUsbControlTransfer =
    unsafe extern "C" fn(*mut c_void, u8, u8, u16, u16, *mut u8, u16, c_uint) -> c_int;

struct LibUsb {
    library: Library,
    context: *mut c_void,
}

impl LibUsb {
    fn load() -> Result<Self, ElgatoError> {
        let mut last_error = String::new();
        for name in ["libusb-1.0.so.0", "libusb-1.0.so"] {
            let library = match unsafe { Library::new(name) } {
                Ok(library) => library,
                Err(err) => {
                    last_error = err.to_string();
                    continue;
                }
            };
            let mut context = std::ptr::null_mut();
            let code = {
                let init = unsafe {
                    library
                        .get::<LibUsbInit>(b"libusb_init")
                        .map_err(|err| ElgatoError::LibUsbSymbol(err.to_string()))?
                };
                unsafe { init(&mut context) }
            };
            if code < 0 {
                return Err(ElgatoError::UsbCode {
                    operation: "init",
                    code,
                });
            }
            return Ok(Self { library, context });
        }
        Err(ElgatoError::LibUsbUnavailable(last_error))
    }

    fn open_wave_xlr(&self) -> Result<*mut c_void, ElgatoError> {
        let open = unsafe {
            self.library
                .get::<LibUsbOpenDeviceWithVidPid>(b"libusb_open_device_with_vid_pid")
                .map_err(|err| ElgatoError::LibUsbSymbol(err.to_string()))?
        };
        let handle = unsafe { open(self.context, ELGATO_VENDOR_ID, WAVE_XLR_PRODUCT_ID) };
        if handle.is_null() {
            Err(ElgatoError::NotOpened)
        } else {
            Ok(handle)
        }
    }

    fn close(&self, handle: *mut c_void) {
        if handle.is_null() {
            return;
        }
        if let Ok(close) = unsafe { self.library.get::<LibUsbClose>(b"libusb_close") } {
            unsafe { close(handle) };
        }
    }

    fn control_transfer(
        &self,
        handle: *mut c_void,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<usize, ElgatoError> {
        let control_transfer = unsafe {
            self.library
                .get::<LibUsbControlTransfer>(b"libusb_control_transfer")
                .map_err(|err| ElgatoError::LibUsbSymbol(err.to_string()))?
        };
        let timeout_ms = USB_TIMEOUT.as_millis().min(c_uint::MAX as u128) as c_uint;
        let code = unsafe {
            control_transfer(
                handle,
                request_type,
                request,
                value,
                index,
                data.as_mut_ptr(),
                data.len().min(u16::MAX as usize) as u16,
                timeout_ms,
            )
        };
        if code < 0 {
            Err(ElgatoError::UsbCode {
                operation: "control transfer",
                code,
            })
        } else {
            Ok(code as usize)
        }
    }
}

impl Drop for LibUsb {
    fn drop(&mut self) {
        if let Ok(exit) = unsafe { self.library.get::<LibUsbExit>(b"libusb_exit") } {
            unsafe { exit(self.context) };
        }
    }
}

struct WaveXlrController {
    handle: *mut c_void,
    libusb: LibUsb,
}

impl WaveXlrController {
    fn open() -> Result<Self, ElgatoError> {
        let libusb = LibUsb::load()?;
        let handle = libusb.open_wave_xlr()?;
        Ok(Self { handle, libusb })
    }

    fn read_state(&mut self) -> Result<ElgatoWaveXlrState, ElgatoError> {
        let config = self.read_config()?;
        let info = self.read_device_info().ok();
        let gain_raw = u16::from_le_bytes([config[OFF_GAIN], config[OFF_GAIN + 1]]);
        let hp_raw = i16::from_le_bytes([config[OFF_HP_VOLUME], config[OFF_HP_VOLUME + 1]]);
        Ok(ElgatoWaveXlrState {
            connected: true,
            gain_raw,
            gain_max_raw: WAVE_XLR_GAIN_MAX,
            gain_percent: gain_raw as f32 / WAVE_XLR_GAIN_MAX as f32,
            muted: config[OFF_MUTE] != 0,
            hp_volume_db: hp_raw_to_db(hp_raw),
            hp_min_db: WAVE_XLR_HP_MIN_DB,
            hp_max_db: WAVE_XLR_HP_MAX_DB,
            low_impedance: config[OFF_LOW_IMPEDANCE] != 0,
            volume_select: if config[OFF_VOLUME_SELECT] == 2 {
                ElgatoWaveXlrKnobTarget::Headphones
            } else {
                ElgatoWaveXlrKnobTarget::Gain
            },
            api_version: info.as_ref().map(|info| info.api_version.clone()),
            firmware_version: info.as_ref().map(|info| info.firmware_version.clone()),
            serial: info
                .map(|info| info.serial)
                .filter(|serial| !serial.is_empty()),
        })
    }

    fn set_gain(&mut self, gain_raw: u16) -> Result<(), ElgatoError> {
        let mut config = self.read_config()?;
        let gain_raw = gain_raw.min(WAVE_XLR_GAIN_MAX);
        config[OFF_GAIN..OFF_GAIN + 2].copy_from_slice(&gain_raw.to_le_bytes());
        self.write_config(&config)
    }

    fn set_mute(&mut self, muted: bool) -> Result<(), ElgatoError> {
        let mut config = self.read_config()?;
        config[OFF_MUTE] = u8::from(muted);
        self.write_config(&config)
    }

    fn set_hp_volume_raw(&mut self, hp_raw: i16) -> Result<(), ElgatoError> {
        let mut config = self.read_config()?;
        config[OFF_HP_VOLUME..OFF_HP_VOLUME + 2].copy_from_slice(&hp_raw.to_le_bytes());
        self.write_config(&config)
    }

    fn set_low_impedance(&mut self, enabled: bool) -> Result<(), ElgatoError> {
        let mut config = self.read_config()?;
        config[OFF_LOW_IMPEDANCE] = u8::from(enabled);
        self.write_config(&config)
    }

    fn read_config(&mut self) -> Result<Vec<u8>, ElgatoError> {
        self.control_read(WVALUE_CONFIG, CONFIG_LEN, "config")
    }

    fn write_config(&mut self, config: &[u8]) -> Result<(), ElgatoError> {
        self.control_write(WVALUE_CONFIG, config)
    }

    fn read_device_info(&mut self) -> Result<ElgatoWaveXlrDeviceInfo, ElgatoError> {
        let data = self.control_read(WVALUE_DEVICE_INFO, DEVICE_INFO_LEN, "device info")?;
        let serial = data[27..47]
            .iter()
            .copied()
            .take_while(|byte| *byte != 0)
            .map(|byte| byte as char)
            .collect::<String>();
        Ok(ElgatoWaveXlrDeviceInfo {
            api_version: format!("{}.{}", data[0], data[1]),
            firmware_version: format!("{}.{}.{}", data[6], data[7], data[8]),
            serial,
        })
    }

    fn control_read(
        &mut self,
        value: u16,
        expected: usize,
        name: &'static str,
    ) -> Result<Vec<u8>, ElgatoError> {
        let mut buffer = vec![0; expected];
        let actual = self.libusb.control_transfer(
            self.handle,
            RT_CLASS_IN,
            BREQUEST_READ,
            value,
            WINDEX,
            &mut buffer,
        )?;
        if actual < expected {
            return Err(ElgatoError::ShortRead {
                name,
                expected,
                actual,
            });
        }
        buffer.truncate(actual);
        Ok(buffer)
    }

    fn control_write(&mut self, value: u16, data: &[u8]) -> Result<(), ElgatoError> {
        let mut data = data.to_vec();
        let expected = data.len();
        let actual = self.libusb.control_transfer(
            self.handle,
            RT_CLASS_OUT,
            BREQUEST_WRITE,
            value,
            WINDEX,
            &mut data,
        )?;
        if actual != expected {
            return Err(ElgatoError::ShortRead {
                name: "config write",
                expected,
                actual,
            });
        }
        Ok(())
    }
}

impl Drop for WaveXlrController {
    fn drop(&mut self) {
        self.libusb.close(self.handle);
        self.handle = std::ptr::null_mut();
    }
}

fn summary_from_device(device: &DeviceInfo) -> ElgatoDeviceSummary {
    let kind = device_kind(device);
    let controls_supported = kind == ElgatoDeviceKind::WaveXlr;
    ElgatoDeviceSummary {
        id: elgato_device_key(device),
        name: summary_name(device, kind),
        description: device.description.clone(),
        kind,
        controls_supported,
        bus: device
            .bus
            .map(|bus| format!("{bus:?}").to_ascii_lowercase()),
        vendor_id: device.vendor_id.clone(),
        product_id: device.product_id.clone(),
        alsa_card: device.alsa_card.clone(),
        matched_profile_id: device.matched_profile_id.clone(),
        message: if controls_supported {
            "Wave XLR hardware controls available".into()
        } else {
            "Audio profile available; hardware controls are not supported yet".into()
        },
    }
}

fn merge_summary(summary: &mut ElgatoDeviceSummary, device: &DeviceInfo) {
    if summary.description.len() < device.description.len() {
        summary.description = device.description.clone();
    }
    summary.vendor_id = summary
        .vendor_id
        .clone()
        .or_else(|| device.vendor_id.clone());
    summary.product_id = summary
        .product_id
        .clone()
        .or_else(|| device.product_id.clone());
    summary.alsa_card = summary
        .alsa_card
        .clone()
        .or_else(|| device.alsa_card.clone());
    summary.matched_profile_id = summary
        .matched_profile_id
        .clone()
        .or_else(|| device.matched_profile_id.clone());
    if device_kind(device) == ElgatoDeviceKind::WaveXlr {
        summary.kind = ElgatoDeviceKind::WaveXlr;
        summary.controls_supported = true;
        summary.name = "Elgato Wave XLR".into();
        summary.message = "Wave XLR hardware controls available".into();
    }
}

fn summary_name(device: &DeviceInfo, kind: ElgatoDeviceKind) -> String {
    match kind {
        ElgatoDeviceKind::WaveXlr => "Elgato Wave XLR".into(),
        ElgatoDeviceKind::WaveMicrophone => {
            if normalized_text(&device.description).contains("wave:3") {
                "Elgato Wave:3".into()
            } else {
                "Elgato Wave microphone".into()
            }
        }
        ElgatoDeviceKind::CaptureAudio => {
            if normalized_text(&device.description).contains("cam link") {
                "Elgato Cam Link audio".into()
            } else if normalized_text(&device.description).contains("hd60") {
                "Elgato HD60 audio".into()
            } else {
                "Elgato capture audio".into()
            }
        }
        ElgatoDeviceKind::AudioEndpoint => {
            if device.description.trim().is_empty() {
                device.name.clone()
            } else {
                device.description.clone()
            }
        }
    }
}

fn elgato_device_key(device: &DeviceInfo) -> String {
    if let Some(card) = &device.alsa_card {
        return format!("alsa-card-{card}");
    }
    if let (Some(vendor), Some(product)) = (&device.vendor_id, &device.product_id) {
        return format!(
            "usb-{}-{}",
            normalize_hex_id(vendor),
            normalize_hex_id(product)
        );
    }
    if let Some(profile) = &device.matched_profile_id {
        return profile.clone();
    }
    normalized_text(&device.description)
        .replace(' ', "-")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .collect::<String>()
}

fn is_elgato_device(device: &DeviceInfo) -> bool {
    let text = normalized_device_text(device);
    device
        .vendor_id
        .as_deref()
        .is_some_and(|vendor| normalize_hex_id(vendor) == "0fd9")
        || device
            .matched_profile_id
            .as_deref()
            .is_some_and(|profile| profile.starts_with("elgato."))
        || text.contains("elgato")
        || text.contains("wave xlr")
        || text.contains("wave:3")
}

fn device_kind(device: &DeviceInfo) -> ElgatoDeviceKind {
    let text = normalized_device_text(device);
    if device
        .product_id
        .as_deref()
        .is_some_and(|product| normalize_hex_id(product) == "007d")
        || device.matched_profile_id.as_deref() == Some("elgato.wave-xlr")
        || text.contains("wave xlr")
    {
        return ElgatoDeviceKind::WaveXlr;
    }
    if device.matched_profile_id.as_deref() == Some("elgato.wave-3") || text.contains("wave:3") {
        return ElgatoDeviceKind::WaveMicrophone;
    }
    if device
        .matched_profile_id
        .as_deref()
        .is_some_and(|profile| profile == "elgato.hd60-x" || profile == "elgato.cam-link-4k")
        || text.contains("hd60")
        || text.contains("cam link")
    {
        return ElgatoDeviceKind::CaptureAudio;
    }
    ElgatoDeviceKind::AudioEndpoint
}

fn normalized_device_text(device: &DeviceInfo) -> String {
    normalized_text(
        &[
            device.id.as_str(),
            device.name.as_str(),
            device.description.as_str(),
            device.matched_profile_id.as_deref().unwrap_or_default(),
        ]
        .join(" "),
    )
}

fn normalized_text(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn normalize_hex_id(value: &str) -> String {
    let normalized = value
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .collect::<String>();
    let trimmed = normalized.trim_start_matches('0');
    if trimmed.is_empty() {
        "0000".into()
    } else {
        format!("{trimmed:0>4}")
    }
}

fn hp_raw_to_db(raw: i16) -> f32 {
    raw as f32 / 256.0
}

fn hp_db_to_raw(db: f32) -> i16 {
    (clamp_hp_db(db) * 256.0).round() as i16
}

fn clamp_hp_db(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(WAVE_XLR_HP_MIN_DB, WAVE_XLR_HP_MAX_DB)
    } else {
        WAVE_XLR_HP_MAX_DB
    }
}

fn fw_hp_to_alsa(raw: i16) -> i32 {
    let db = hp_raw_to_db(raw);
    ((db / 0.5) + 120.0).round().clamp(0.0, 120.0) as i32
}

fn detect_wave_xlr_alsa_card() -> Option<String> {
    let output = Command::new("aplay").arg("-l").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("wave xlr") || lower.contains("elgato")) {
            continue;
        }
        let card_text = line.strip_prefix("card ")?.split(':').next()?.trim();
        if !card_text.is_empty() && card_text.chars().all(|ch| ch.is_ascii_digit()) {
            return Some(card_text.into());
        }
    }
    None
}

fn sync_alsa_mute(muted: bool) {
    let Some(card) = detect_wave_xlr_alsa_card() else {
        return;
    };
    let _ = Command::new("amixer")
        .args([
            "-c",
            &card,
            "cset",
            "numid=5",
            if muted { "off" } else { "on" },
        ])
        .output();
}

fn sync_alsa_hp_volume(raw: i16) {
    let Some(card) = detect_wave_xlr_alsa_card() else {
        return;
    };
    let value = fw_hp_to_alsa(raw).to_string();
    let _ = Command::new("amixer")
        .args(["-c", &card, "cset", "numid=4", &value])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavelinux_model::DeviceBus;

    fn test_device(description: &str, profile: Option<&str>) -> DeviceInfo {
        DeviceInfo {
            id: description.replace(' ', "_"),
            index: None,
            name: description.into(),
            description: description.into(),
            is_available: true,
            is_default: false,
            is_virtual: false,
            bus: Some(DeviceBus::Usb),
            vendor_id: Some("0x0fd9".into()),
            product_id: None,
            alsa_card: Some("3".into()),
            alsa_device: None,
            driver: None,
            bluetooth_modalias: None,
            active_profile: None,
            active_codec: None,
            pipewire_properties: BTreeMap::new(),
            matched_profile_id: profile.map(ToOwned::to_owned),
            matched_profile_source: None,
            profile_confidence: None,
            active_latency_policy: None,
            active_routing_policy: None,
            active_bluetooth_mic_policy: None,
        }
    }

    #[test]
    fn detects_wave_xlr_control_support_from_profile() {
        let device = test_device("Elgato Wave XLR", Some("elgato.wave-xlr"));
        let summaries = summarize_devices([&device], std::iter::empty::<&DeviceInfo>());
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].kind, ElgatoDeviceKind::WaveXlr);
        assert!(summaries[0].controls_supported);
    }

    #[test]
    fn keeps_capture_devices_as_audio_profile_only() {
        let device = test_device("Elgato HD60 X Audio", Some("elgato.hd60-x"));
        let summaries = summarize_devices([&device], std::iter::empty::<&DeviceInfo>());
        assert_eq!(summaries[0].kind, ElgatoDeviceKind::CaptureAudio);
        assert!(!summaries[0].controls_supported);
    }

    #[test]
    fn ignores_generic_hd60_text_without_elgato_identity() {
        let mut device = test_device("Generic HD60 Audio", None);
        device.vendor_id = Some("1234".into());
        let summaries = summarize_devices([&device], std::iter::empty::<&DeviceInfo>());
        assert!(summaries.is_empty());
    }

    #[test]
    fn ignores_wave_xlr_product_id_without_elgato_identity() {
        let mut device = test_device("Generic USB audio", None);
        device.vendor_id = Some("1234".into());
        device.product_id = Some("0x007d".into());
        let summaries = summarize_devices([&device], std::iter::empty::<&DeviceInfo>());
        assert!(summaries.is_empty());
    }

    #[test]
    fn merges_usb_ids_with_different_hex_formatting() {
        let mut input = test_device("Elgato Wave XLR input", Some("elgato.wave-xlr"));
        input.alsa_card = None;
        input.vendor_id = Some("0x0FD9".into());
        input.product_id = Some("0x007D".into());

        let mut output = test_device("Elgato Wave XLR output", Some("elgato.wave-xlr"));
        output.alsa_card = None;
        output.vendor_id = Some("0fd9".into());
        output.product_id = Some("007d".into());

        let summaries = summarize_devices([&input], [&output]);
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].controls_supported);
    }

    #[test]
    fn maps_headphone_db_to_alsa_range() {
        assert_eq!(fw_hp_to_alsa(hp_db_to_raw(0.0)), 120);
        assert_eq!(fw_hp_to_alsa(hp_db_to_raw(-60.0)), 0);
        assert_eq!(fw_hp_to_alsa(hp_db_to_raw(-12.0)), 96);
    }
}
