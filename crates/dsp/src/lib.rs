use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nnnoiseless::{DenoiseFeatures, DenoiseState};
use serde::{Deserialize, Serialize};
use wavelinux_model::EffectInstance;

pub const AUDIO_RUNTIME_ENV: &str = "WAVELINUX_AUDIO_RUNTIME";
pub const DSP_PROVIDER_ENV: &str = "WAVELINUX_DSP_PROVIDER";
pub const DSP_FORCE_PROVIDER_FAIL_ENV: &str = "WAVELINUX_DSP_FORCE_PROVIDER_FAIL";
pub const DSP_CHANNEL_CONFIG_REVISION: &str = "wavelinux6-audio-core-channel-v3";
pub const CORE_CONTROL_PROTOCOL_VERSION: u16 = 3;
pub const MAX_MIX_OUTPUT_TARGETS: usize = 4;
pub const CONTROL_DIRECTORY_NAME: &str = "control";
pub const MIX_CONTROL_SOCKET_FILE: &str = "wavelinux6-audio-core.sock";
pub const METER_STREAM_PROTOCOL_VERSION: u16 = 1;

/// Convert an adaptive buffer target into a PipeWire scheduling request.
/// Zero releases the stream-level quantum override at the low-latency level.
pub fn adaptive_pipewire_quantum_frames(target_msec: u16) -> u32 {
    match target_msec {
        0..=28 => 0,
        29..=40 => 512,
        _ => 1024,
    }
}

pub fn valid_pipewire_quantum_frames(quantum_frames: u32) -> bool {
    quantum_frames == 0
        || (quantum_frames.is_power_of_two() && (64..=8192).contains(&quantum_frames))
}

pub const METER_STREAM_SOCKET_FILE: &str = "wavelinux6-meters.sock";
pub const METER_STREAM_RATE_HZ: u16 = 30;
pub const METER_STREAM_MAX_SLOTS: usize = 64;
const METER_STREAM_HEADER_MAGIC: [u8; 8] = *b"WLMTR001";
const METER_STREAM_FRAME_MAGIC: [u8; 4] = *b"WLMF";
const METER_STREAM_HEADER_BYTES: usize = 24;
const METER_STREAM_SLOT_ID_BYTES: usize = 64;
const METER_STREAM_DESCRIPTOR_BYTES: usize = 4 + METER_STREAM_SLOT_ID_BYTES;
const METER_STREAM_FRAME_HEADER_BYTES: usize = 24;
const METER_STREAM_SAMPLE_BYTES: usize = 16;

pub fn control_directory(runtime_root: &Path) -> PathBuf {
    runtime_root.join(CONTROL_DIRECTORY_NAME)
}

pub fn channel_control_socket(
    runtime_root: &Path,
    graph_prefix: &str,
    channel_id: &str,
) -> PathBuf {
    control_directory(runtime_root).join(format!(
        "{}-chain-{}.sock",
        safe_control_path_component(graph_prefix, "wavelinux6"),
        safe_control_path_component(channel_id, "channel")
    ))
}

pub fn mix_control_socket(runtime_root: &Path) -> PathBuf {
    control_directory(runtime_root).join(MIX_CONTROL_SOCKET_FILE)
}

pub fn meter_stream_socket(runtime_root: &Path) -> PathBuf {
    control_directory(runtime_root).join(METER_STREAM_SOCKET_FILE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MeterStreamSlotKind {
    Channel = 1,
    Mix = 2,
}

impl MeterStreamSlotKind {
    fn from_byte(value: u8) -> io::Result<Self> {
        match value {
            1 => Ok(Self::Channel),
            2 => Ok(Self::Mix),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown meter slot kind {value}"),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterStreamSlot {
    pub kind: MeterStreamSlotKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MeterStreamSample {
    pub peak_left: f32,
    pub peak_right: f32,
    pub rms_left: f32,
    pub rms_right: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeterStreamFrame {
    pub sequence: u64,
    pub monotonic_nanos: u64,
    pub samples: Vec<MeterStreamSample>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterStreamHeader {
    pub rate_hz: u16,
    pub slots: Vec<MeterStreamSlot>,
}

impl MeterStreamHeader {
    pub fn frame_bytes(&self) -> usize {
        METER_STREAM_FRAME_HEADER_BYTES + self.slots.len() * METER_STREAM_SAMPLE_BYTES
    }
}

pub fn encode_meter_stream_header(slots: &[MeterStreamSlot]) -> io::Result<Vec<u8>> {
    validate_meter_stream_slots(slots)?;
    let frame_bytes = METER_STREAM_FRAME_HEADER_BYTES
        .checked_add(slots.len() * METER_STREAM_SAMPLE_BYTES)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "meter frame is too large"))?;
    let mut bytes =
        Vec::with_capacity(METER_STREAM_HEADER_BYTES + slots.len() * METER_STREAM_DESCRIPTOR_BYTES);
    bytes.extend_from_slice(&METER_STREAM_HEADER_MAGIC);
    bytes.extend_from_slice(&METER_STREAM_PROTOCOL_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(METER_STREAM_HEADER_BYTES as u16).to_le_bytes());
    bytes.extend_from_slice(&(METER_STREAM_DESCRIPTOR_BYTES as u16).to_le_bytes());
    bytes.extend_from_slice(&(METER_STREAM_SAMPLE_BYTES as u16).to_le_bytes());
    bytes.extend_from_slice(&(slots.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&METER_STREAM_RATE_HZ.to_le_bytes());
    bytes.extend_from_slice(&(frame_bytes as u32).to_le_bytes());
    for slot in slots {
        bytes.push(slot.kind as u8);
        bytes.extend_from_slice(&[0; 3]);
        let id = slot.id.as_bytes();
        bytes.extend_from_slice(id);
        bytes.resize(bytes.len() + METER_STREAM_SLOT_ID_BYTES - id.len(), 0);
    }
    Ok(bytes)
}

pub fn read_meter_stream_header(reader: &mut impl Read) -> io::Result<MeterStreamHeader> {
    let mut prefix = [0_u8; METER_STREAM_HEADER_BYTES];
    reader.read_exact(&mut prefix)?;
    if prefix[..8] != METER_STREAM_HEADER_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid meter stream header magic",
        ));
    }
    let version = read_u16(&prefix, 8)?;
    if version != METER_STREAM_PROTOCOL_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "unsupported meter stream protocol {version}; expected {METER_STREAM_PROTOCOL_VERSION}"
            ),
        ));
    }
    let header_bytes = read_u16(&prefix, 10)? as usize;
    let descriptor_bytes = read_u16(&prefix, 12)? as usize;
    let sample_bytes = read_u16(&prefix, 14)? as usize;
    let slot_count = read_u16(&prefix, 16)? as usize;
    let rate_hz = read_u16(&prefix, 18)?;
    let frame_bytes = read_u32(&prefix, 20)? as usize;
    if header_bytes != METER_STREAM_HEADER_BYTES
        || descriptor_bytes != METER_STREAM_DESCRIPTOR_BYTES
        || sample_bytes != METER_STREAM_SAMPLE_BYTES
        || slot_count > METER_STREAM_MAX_SLOTS
        || rate_hz == 0
        || frame_bytes != METER_STREAM_FRAME_HEADER_BYTES + slot_count * METER_STREAM_SAMPLE_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid meter stream dimensions",
        ));
    }

    let mut descriptor = [0_u8; METER_STREAM_DESCRIPTOR_BYTES];
    let mut slots = Vec::with_capacity(slot_count);
    for _ in 0..slot_count {
        reader.read_exact(&mut descriptor)?;
        let kind = MeterStreamSlotKind::from_byte(descriptor[0])?;
        let id_bytes = &descriptor[4..];
        let id_len = id_bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(id_bytes.len());
        let id = std::str::from_utf8(&id_bytes[..id_len])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid meter slot id"))?
            .to_string();
        if id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "empty meter slot id",
            ));
        }
        slots.push(MeterStreamSlot { kind, id });
    }
    validate_meter_stream_slots(&slots)?;
    Ok(MeterStreamHeader { rate_hz, slots })
}

pub fn encode_meter_stream_frame_into(
    sequence: u64,
    monotonic_nanos: u64,
    samples: &[MeterStreamSample],
    bytes: &mut Vec<u8>,
) -> io::Result<()> {
    if samples.len() > METER_STREAM_MAX_SLOTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many meter samples",
        ));
    }
    bytes.clear();
    bytes.reserve(METER_STREAM_FRAME_HEADER_BYTES + samples.len() * METER_STREAM_SAMPLE_BYTES);
    bytes.extend_from_slice(&METER_STREAM_FRAME_MAGIC);
    bytes.extend_from_slice(&METER_STREAM_PROTOCOL_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(samples.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&sequence.to_le_bytes());
    bytes.extend_from_slice(&monotonic_nanos.to_le_bytes());
    for sample in samples {
        for value in [
            sample.peak_left,
            sample.peak_right,
            sample.rms_left,
            sample.rms_right,
        ] {
            bytes.extend_from_slice(&finite_meter_protocol_value(value).to_le_bytes());
        }
    }
    Ok(())
}

pub fn read_meter_stream_frame(
    reader: &mut impl Read,
    expected_slots: usize,
    bytes: &mut Vec<u8>,
) -> io::Result<MeterStreamFrame> {
    if expected_slots > METER_STREAM_MAX_SLOTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many expected meter slots",
        ));
    }
    let frame_bytes = METER_STREAM_FRAME_HEADER_BYTES + expected_slots * METER_STREAM_SAMPLE_BYTES;
    bytes.resize(frame_bytes, 0);
    reader.read_exact(bytes)?;
    if bytes[..4] != METER_STREAM_FRAME_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid meter frame magic",
        ));
    }
    let version = read_u16(bytes, 4)?;
    let slot_count = read_u16(bytes, 6)? as usize;
    if version != METER_STREAM_PROTOCOL_VERSION || slot_count != expected_slots {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "meter frame does not match negotiated stream",
        ));
    }
    let sequence = read_u64(bytes, 8)?;
    let monotonic_nanos = read_u64(bytes, 16)?;
    let mut samples = Vec::with_capacity(slot_count);
    for index in 0..slot_count {
        let offset = METER_STREAM_FRAME_HEADER_BYTES + index * METER_STREAM_SAMPLE_BYTES;
        samples.push(MeterStreamSample {
            peak_left: read_f32(bytes, offset)?,
            peak_right: read_f32(bytes, offset + 4)?,
            rms_left: read_f32(bytes, offset + 8)?,
            rms_right: read_f32(bytes, offset + 12)?,
        });
    }
    Ok(MeterStreamFrame {
        sequence,
        monotonic_nanos,
        samples,
    })
}

fn validate_meter_stream_slots(slots: &[MeterStreamSlot]) -> io::Result<()> {
    if slots.is_empty() || slots.len() > METER_STREAM_MAX_SLOTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "meter stream slot count is out of range",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for slot in slots {
        let bytes = slot.id.as_bytes();
        if bytes.is_empty() || bytes.len() > METER_STREAM_SLOT_ID_BYTES || bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid meter slot id {}", slot.id),
            ));
        }
        if !ids.insert(slot.id.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("duplicate meter slot id {}", slot.id),
            ));
        }
    }
    Ok(())
}

fn finite_meter_protocol_value(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> io::Result<u16> {
    let value = bytes.get(offset..offset + 2).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "truncated meter protocol u16")
    })?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let value = bytes.get(offset..offset + 4).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "truncated meter protocol u32")
    })?;
    Ok(u32::from_le_bytes(
        value.try_into().expect("four-byte slice"),
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> io::Result<u64> {
    let value = bytes.get(offset..offset + 8).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "truncated meter protocol u64")
    })?;
    Ok(u64::from_le_bytes(
        value.try_into().expect("eight-byte slice"),
    ))
}

fn read_f32(bytes: &[u8], offset: usize) -> io::Result<f32> {
    let value = bytes.get(offset..offset + 4).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "truncated meter protocol f32")
    })?;
    Ok(finite_meter_protocol_value(f32::from_le_bytes(
        value.try_into().expect("four-byte slice"),
    )))
}

fn safe_control_path_component(value: &str, fallback: &str) -> String {
    let mut safe = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            safe.push(ch);
        } else if !safe.ends_with('-') {
            safe.push('-');
        }
    }
    let safe = safe.trim_matches('-');
    if safe.is_empty() {
        fallback.into()
    } else {
        safe.into()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AudioRuntimeMode {
    PipewireFilterChain,
    DspCpu,
    DspAuto,
    DspAccelerated,
}

impl AudioRuntimeMode {
    pub fn parse(value: &str) -> Option<Self> {
        match normalize_token(value).as_str() {
            "pipewire_filter_chain" | "filter_chain" | "pipewire" => {
                Some(Self::PipewireFilterChain)
            }
            "dsp_cpu" | "cpu" => Some(Self::DspCpu),
            "dsp_auto" | "auto" => Some(Self::DspAuto),
            "dsp_accelerated" | "accelerated" | "gpu" => Some(Self::DspAccelerated),
            _ => None,
        }
    }

    pub fn from_env() -> Self {
        std::env::var(AUDIO_RUNTIME_ENV)
            .ok()
            .and_then(|value| Self::parse(&value))
            .unwrap_or(Self::DspCpu)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PipewireFilterChain => "pipewire_filter_chain",
            Self::DspCpu => "dsp_cpu",
            Self::DspAuto => "dsp_auto",
            Self::DspAccelerated => "dsp_accelerated",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DspProviderPreference {
    Auto,
    Cuda,
    #[serde(rename = "openvino")]
    OpenVino,
    #[serde(rename = "migraphx")]
    MiGraphX,
    Cpu,
}

impl DspProviderPreference {
    pub fn parse(value: &str) -> Option<Self> {
        match normalize_token(value).as_str() {
            "auto" => Some(Self::Auto),
            "cuda" | "nvidia" => Some(Self::Cuda),
            "openvino" | "intel" => Some(Self::OpenVino),
            "migraphx" | "amd" | "rocm" => Some(Self::MiGraphX),
            "cpu" | "portable_cpu" | "pure_cpu" => Some(Self::Cpu),
            _ => None,
        }
    }

    pub fn from_env() -> Self {
        std::env::var(DSP_PROVIDER_ENV)
            .ok()
            .and_then(|value| Self::parse(&value))
            .unwrap_or(Self::Auto)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cuda => "cuda",
            Self::OpenVino => "openvino",
            Self::MiGraphX => "migraphx",
            Self::Cpu => "cpu",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DspProvider {
    Cuda,
    #[serde(rename = "openvino")]
    OpenVino,
    #[serde(rename = "migraphx")]
    MiGraphX,
    PortableCpu,
    PureCpu,
}

impl DspProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::OpenVino => "openvino",
            Self::MiGraphX => "migraphx",
            Self::PortableCpu => "portable_cpu",
            Self::PureCpu => "pure_cpu",
        }
    }

    fn accelerated(self) -> bool {
        matches!(self, Self::Cuda | Self::OpenVino | Self::MiGraphX)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderProbe {
    pub provider: DspProvider,
    pub available: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderProbeInputs {
    pub cuda_available: bool,
    pub cuda_detail: String,
    pub openvino_available: bool,
    pub openvino_detail: String,
    pub migraphx_available: bool,
    pub migraphx_detail: String,
    pub portable_cpu_available: bool,
    pub portable_cpu_detail: String,
}

impl ProviderProbeInputs {
    pub fn detect() -> Self {
        let forced = forced_provider_failures();
        let cuda_pack = wavelinux_accelerator::probe_provider_pack(
            wavelinux_accelerator::AcceleratorProvider::Cuda,
        );
        let openvino_pack = wavelinux_accelerator::probe_provider_pack(
            wavelinux_accelerator::AcceleratorProvider::OpenVino,
        );
        let migraphx_pack = wavelinux_accelerator::probe_provider_pack(
            wavelinux_accelerator::AcceleratorProvider::MiGraphX,
        );

        // A provider is eligible only when its pack passed the complete
        // machine-local workload gate. Driver discovery alone is never enough.
        let cuda_available = cuda_pack.qualified && !forced.contains(&DspProvider::Cuda);
        let openvino_available =
            openvino_pack.qualified && !forced.contains(&DspProvider::OpenVino);
        let migraphx_available =
            migraphx_pack.qualified && !forced.contains(&DspProvider::MiGraphX);
        let portable_cpu_available = !forced.contains(&DspProvider::PortableCpu);

        Self {
            cuda_available,
            cuda_detail: if forced.contains(&DspProvider::Cuda) {
                "forced unavailable by WAVELINUX_DSP_FORCE_PROVIDER_FAIL".into()
            } else {
                cuda_pack.detail
            },
            openvino_available,
            openvino_detail: if forced.contains(&DspProvider::OpenVino) {
                "forced unavailable by WAVELINUX_DSP_FORCE_PROVIDER_FAIL".into()
            } else {
                openvino_pack.detail
            },
            migraphx_available,
            migraphx_detail: if forced.contains(&DspProvider::MiGraphX) {
                "forced unavailable by WAVELINUX_DSP_FORCE_PROVIDER_FAIL".into()
            } else {
                migraphx_pack.detail
            },
            portable_cpu_available,
            portable_cpu_detail: if portable_cpu_available {
                portable_cpu_detail()
            } else {
                "forced unavailable by WAVELINUX_DSP_FORCE_PROVIDER_FAIL".into()
            },
        }
    }

    fn probes(&self) -> Vec<ProviderProbe> {
        vec![
            ProviderProbe {
                provider: DspProvider::Cuda,
                available: self.cuda_available,
                detail: self.cuda_detail.clone(),
            },
            ProviderProbe {
                provider: DspProvider::OpenVino,
                available: self.openvino_available,
                detail: self.openvino_detail.clone(),
            },
            ProviderProbe {
                provider: DspProvider::MiGraphX,
                available: self.migraphx_available,
                detail: self.migraphx_detail.clone(),
            },
            ProviderProbe {
                provider: DspProvider::PortableCpu,
                available: self.portable_cpu_available,
                detail: self.portable_cpu_detail.clone(),
            },
            ProviderProbe {
                provider: DspProvider::PureCpu,
                available: true,
                detail: "scalar CPU fallback is always available".into(),
            },
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DspBackendStatus {
    pub runtime: AudioRuntimeMode,
    pub effective_runtime: AudioRuntimeMode,
    pub requested_provider: DspProviderPreference,
    pub selected_provider: Option<DspProvider>,
    pub accelerated: bool,
    pub fallback_active: bool,
    pub fallback_count: u32,
    pub runtime_fallback_reason: Option<String>,
    pub provider_probe_failures: Vec<String>,
    pub probes: Vec<ProviderProbe>,
}

impl DspBackendStatus {
    pub fn with_runtime_fallback(
        mut self,
        effective_runtime: AudioRuntimeMode,
        reason: impl Into<String>,
    ) -> Self {
        if self.effective_runtime != effective_runtime {
            self.fallback_count = self.fallback_count.saturating_add(1);
        }
        self.effective_runtime = effective_runtime;
        self.fallback_active = true;
        self.accelerated = false;
        self.runtime_fallback_reason = Some(reason.into());
        self
    }
}

pub fn probe_backend_from_env() -> DspBackendStatus {
    select_provider(
        AudioRuntimeMode::from_env(),
        DspProviderPreference::from_env(),
        &ProviderProbeInputs::detect(),
    )
}

pub fn select_provider(
    runtime: AudioRuntimeMode,
    requested_provider: DspProviderPreference,
    inputs: &ProviderProbeInputs,
) -> DspBackendStatus {
    let probes = inputs.probes();
    let provider_probe_failures = probes
        .iter()
        .filter(|probe| {
            !probe.available
                && match probe.provider {
                    DspProvider::Cuda => {
                        runtime == AudioRuntimeMode::DspAccelerated
                            || requested_provider == DspProviderPreference::Cuda
                    }
                    DspProvider::OpenVino => {
                        runtime == AudioRuntimeMode::DspAccelerated
                            || requested_provider == DspProviderPreference::OpenVino
                    }
                    DspProvider::MiGraphX => {
                        runtime == AudioRuntimeMode::DspAccelerated
                            || requested_provider == DspProviderPreference::MiGraphX
                    }
                    DspProvider::PortableCpu | DspProvider::PureCpu => true,
                }
        })
        .map(|probe| format!("{}: {}", probe.provider.as_str(), probe.detail))
        .collect::<Vec<_>>();

    if runtime == AudioRuntimeMode::PipewireFilterChain {
        return DspBackendStatus {
            runtime,
            effective_runtime: runtime,
            requested_provider,
            selected_provider: None,
            accelerated: false,
            fallback_active: false,
            fallback_count: 0,
            runtime_fallback_reason: None,
            provider_probe_failures,
            probes,
        };
    }

    let candidates = provider_candidates(runtime, requested_provider);
    let selected_provider = candidates
        .iter()
        .copied()
        .find(|provider| provider_available(*provider, inputs))
        .or(Some(DspProvider::PureCpu));
    let selected = selected_provider.expect("pure CPU provider is always available");
    let first_choice = candidates.first().copied().unwrap_or(DspProvider::PureCpu);
    let accelerated = selected.accelerated();
    let explicit_provider_missed = match requested_provider {
        DspProviderPreference::Cuda => selected != DspProvider::Cuda,
        DspProviderPreference::OpenVino => selected != DspProvider::OpenVino,
        DspProviderPreference::MiGraphX => selected != DspProvider::MiGraphX,
        DspProviderPreference::Cpu => selected != first_choice,
        DspProviderPreference::Auto => false,
    };
    let accelerated_runtime_missed = runtime == AudioRuntimeMode::DspAccelerated && !accelerated;
    let cpu_provider_fallback = runtime == AudioRuntimeMode::DspCpu && selected != first_choice;
    let fallback_active =
        explicit_provider_missed || accelerated_runtime_missed || cpu_provider_fallback;
    let fallback_count = u32::from(fallback_active);
    let effective_runtime = if accelerated {
        runtime
    } else {
        AudioRuntimeMode::DspCpu
    };
    let runtime_fallback_reason = if accelerated_runtime_missed {
        Some(format!(
            "no qualified accelerated provider is available; using {}",
            selected.as_str()
        ))
    } else if explicit_provider_missed {
        Some(format!(
            "requested {} provider is unavailable; using {}",
            requested_provider.as_str(),
            selected.as_str()
        ))
    } else if cpu_provider_fallback {
        Some(format!(
            "portable CPU provider is unavailable; using {}",
            selected.as_str()
        ))
    } else {
        None
    };

    DspBackendStatus {
        runtime,
        effective_runtime,
        requested_provider,
        selected_provider: Some(selected),
        accelerated,
        fallback_active,
        fallback_count,
        runtime_fallback_reason,
        provider_probe_failures,
        probes,
    }
}

fn provider_candidates(
    runtime: AudioRuntimeMode,
    requested_provider: DspProviderPreference,
) -> Vec<DspProvider> {
    match (runtime, requested_provider) {
        (AudioRuntimeMode::DspCpu, _) | (_, DspProviderPreference::Cpu) => {
            vec![DspProvider::PortableCpu, DspProvider::PureCpu]
        }
        (_, DspProviderPreference::Cuda) => {
            vec![
                DspProvider::Cuda,
                DspProvider::PortableCpu,
                DspProvider::PureCpu,
            ]
        }
        (_, DspProviderPreference::OpenVino) => vec![
            DspProvider::OpenVino,
            DspProvider::PortableCpu,
            DspProvider::PureCpu,
        ],
        (_, DspProviderPreference::MiGraphX) => vec![
            DspProvider::MiGraphX,
            DspProvider::PortableCpu,
            DspProvider::PureCpu,
        ],
        (AudioRuntimeMode::DspAccelerated, DspProviderPreference::Auto) => vec![
            DspProvider::Cuda,
            DspProvider::OpenVino,
            DspProvider::MiGraphX,
            DspProvider::PortableCpu,
            DspProvider::PureCpu,
        ],
        _ => vec![
            DspProvider::Cuda,
            DspProvider::OpenVino,
            DspProvider::MiGraphX,
            DspProvider::PortableCpu,
            DspProvider::PureCpu,
        ],
    }
}

fn provider_available(provider: DspProvider, inputs: &ProviderProbeInputs) -> bool {
    match provider {
        DspProvider::Cuda => inputs.cuda_available,
        DspProvider::OpenVino => inputs.openvino_available,
        DspProvider::MiGraphX => inputs.migraphx_available,
        DspProvider::PortableCpu => inputs.portable_cpu_available,
        DspProvider::PureCpu => true,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DspChannelConfig {
    pub revision: String,
    #[serde(default = "default_chain_generation")]
    pub generation: u64,
    pub channel_id: String,
    pub channel_name: String,
    pub graph_prefix: String,
    pub property_prefix: String,
    pub app_name: String,
    pub input_node_name: String,
    pub output_node_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_target_node_name: Option<String>,
    #[serde(default)]
    pub input_target_capable: bool,
    #[serde(default)]
    pub input_mode: DspInputMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_role: Option<String>,
    pub sample_rate_hz: u32,
    #[serde(default = "default_input_channels")]
    pub input_channels: u8,
    pub latency_frames: u32,
    #[serde(default)]
    pub adaptive_latency: DspAdaptiveLatencyConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_socket_path: Option<String>,
    pub effects: Vec<EffectInstance>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DspInputMode {
    #[default]
    Stereo,
    MonoLeft,
    MonoRight,
    SumMono,
    SwapLr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DspAdaptiveLatencyConfig {
    pub enabled: bool,
    pub min_msec: u16,
    pub max_msec: u16,
    pub levels_msec: Vec<u16>,
}

impl Default for DspAdaptiveLatencyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_msec: 28,
            max_msec: 120,
            levels_msec: vec![28, 40, 60, 80, 100, 120],
        }
    }
}

impl DspChannelConfig {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel_id: impl Into<String>,
        channel_name: impl Into<String>,
        graph_prefix: impl Into<String>,
        property_prefix: impl Into<String>,
        app_name: impl Into<String>,
        input_node_name: impl Into<String>,
        output_node_name: impl Into<String>,
        effects: Vec<EffectInstance>,
    ) -> Self {
        Self {
            revision: DSP_CHANNEL_CONFIG_REVISION.into(),
            generation: default_chain_generation(),
            channel_id: channel_id.into(),
            channel_name: channel_name.into(),
            graph_prefix: graph_prefix.into(),
            property_prefix: property_prefix.into(),
            app_name: app_name.into(),
            input_node_name: input_node_name.into(),
            output_node_name: output_node_name.into(),
            input_target_node_name: None,
            input_target_capable: false,
            input_mode: DspInputMode::Stereo,
            input_role: None,
            output_role: None,
            sample_rate_hz: 48_000,
            input_channels: 2,
            latency_frames: 256,
            adaptive_latency: DspAdaptiveLatencyConfig::default(),
            control_socket_path: None,
            effects,
        }
    }

    pub fn active_effects(&self) -> Vec<EffectInstance> {
        self.effects
            .iter()
            .filter(|effect| !effect.bypassed)
            .cloned()
            .collect()
    }

    pub fn unsupported_active_effects(&self) -> Vec<String> {
        self.effects
            .iter()
            .filter(|effect| !effect.bypassed)
            .filter(|effect| !native_dsp_effect_supported(&effect.effect_id))
            .map(|effect| effect.effect_id.clone())
            .collect()
    }
}

fn default_chain_generation() -> u64 {
    1
}

pub fn native_dsp_effect_supported(effect_id: &str) -> bool {
    matches!(
        effect_id,
        "rnnoise" | "highpass" | "eq" | "compressor" | "gate" | "karaoke_stage" | "limiter"
    )
}

fn default_input_channels() -> u8 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DspCoreManifest {
    pub protocol_version: u16,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_root: Option<String>,
    pub channels: Vec<DspChannelConfig>,
    #[serde(default)]
    pub mixes: Vec<DspMixConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_socket_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DspMixConfig {
    pub mix_id: String,
    pub mix_name: String,
    pub graph_prefix: String,
    pub property_prefix: String,
    pub app_name: String,
    pub output_node_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_target_node_names: Vec<String>,
    pub sample_rate_hz: u32,
    pub latency_frames: u32,
    #[serde(default)]
    pub pipewire_quantum_frames: u32,
    #[serde(default)]
    pub adaptive_latency: DspAdaptiveLatencyConfig,
    #[serde(default = "default_unit_gain")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub buses: Vec<DspMixBusConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DspMixBusConfig {
    pub channel_id: String,
    #[serde(default = "default_unit_gain")]
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub enabled: bool,
}

fn default_unit_gain() -> f32 {
    1.0
}

impl DspCoreManifest {
    pub fn new(revision: impl Into<String>, channels: Vec<DspChannelConfig>) -> Self {
        Self {
            protocol_version: CORE_CONTROL_PROTOCOL_VERSION,
            revision: revision.into(),
            runtime_root: None,
            channels,
            mixes: Vec::new(),
            control_socket_path: None,
        }
    }

    pub fn with_mixes(
        mut self,
        mixes: Vec<DspMixConfig>,
        control_socket_path: Option<String>,
    ) -> Self {
        self.mixes = mixes;
        self.control_socket_path = control_socket_path;
        self
    }

    pub fn with_runtime_root(mut self, runtime_root: impl Into<String>) -> Self {
        self.runtime_root = Some(runtime_root.into());
        self
    }

    pub fn resolve_control_socket_paths(&mut self) -> Result<(), String> {
        let Some(runtime_root) = self.runtime_root.as_deref() else {
            return Ok(());
        };
        if runtime_root.trim().is_empty() {
            return Err("audio-core runtime root is empty".into());
        }
        let runtime_root = Path::new(runtime_root);
        for channel in &mut self.channels {
            channel.control_socket_path = Some(
                channel_control_socket(runtime_root, &channel.graph_prefix, &channel.channel_id)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        self.control_socket_path = Some(
            mix_control_socket(runtime_root)
                .to_string_lossy()
                .into_owned(),
        );
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != CORE_CONTROL_PROTOCOL_VERSION {
            return Err(format!(
                "unsupported audio-core protocol {}; expected {}",
                self.protocol_version, CORE_CONTROL_PROTOCOL_VERSION
            ));
        }
        if self.channels.is_empty() {
            return Err("audio-core manifest has no channels".into());
        }
        let mut channel_ids = std::collections::BTreeSet::new();
        let mut node_names = std::collections::BTreeSet::new();
        for channel in &self.channels {
            if channel.generation == 0 {
                return Err(format!(
                    "audio-core channel {} has generation zero",
                    channel.channel_id
                ));
            }
            if !channel_ids.insert(channel.channel_id.as_str()) {
                return Err(format!("duplicate channel id {}", channel.channel_id));
            }
            for node_name in [&channel.input_node_name, &channel.output_node_name] {
                if !node_names.insert(node_name.as_str()) {
                    return Err(format!("duplicate core node name {node_name}"));
                }
            }
            if channel
                .input_target_node_name
                .as_deref()
                .is_some_and(str::is_empty)
            {
                return Err(format!(
                    "channel {} has an empty input target",
                    channel.channel_id
                ));
            }
        }
        let mut mix_ids = std::collections::BTreeSet::new();
        for mix in &self.mixes {
            if !mix_ids.insert(mix.mix_id.as_str()) {
                return Err(format!("duplicate mix id {}", mix.mix_id));
            }
            if !node_names.insert(mix.output_node_name.as_str()) {
                return Err(format!("duplicate core node name {}", mix.output_node_name));
            }
            if mix.sample_rate_hz == 0 {
                return Err(format!("mix {} has a zero sample rate", mix.mix_id));
            }
            if !valid_pipewire_quantum_frames(mix.pipewire_quantum_frames) {
                return Err(format!(
                    "mix {} has invalid PipeWire quantum {}",
                    mix.mix_id, mix.pipewire_quantum_frames
                ));
            }
            if mix.output_target_node_names.iter().any(String::is_empty) {
                return Err(format!("mix {} has an empty output target", mix.mix_id));
            }
            if mix.output_target_node_names.len() > MAX_MIX_OUTPUT_TARGETS {
                return Err(format!(
                    "mix {} has {} output targets; at most {} are supported",
                    mix.mix_id,
                    mix.output_target_node_names.len(),
                    MAX_MIX_OUTPUT_TARGETS
                ));
            }
            let mut bus_channels = std::collections::BTreeSet::new();
            for bus in &mix.buses {
                if !channel_ids.contains(bus.channel_id.as_str()) {
                    return Err(format!(
                        "mix {} references unknown channel {}",
                        mix.mix_id, bus.channel_id
                    ));
                }
                if !bus_channels.insert(bus.channel_id.as_str()) {
                    return Err(format!(
                        "mix {} has duplicate bus for {}",
                        mix.mix_id, bus.channel_id
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Hash only endpoint topology. Live targets, levels, latency, and effect
/// parameters are intentionally excluded because the running core applies
/// those without replacing its PipeWire nodes.
pub fn core_topology_revision(manifest: &DspCoreManifest) -> String {
    let mut topology = format!("protocol:{}|", manifest.protocol_version);
    topology.push_str(
        &manifest
            .channels
            .iter()
            .map(|channel| {
                format!(
                    "{}:{}:{}:{}:{}:{:?}:{}:{:?}:{}:{}",
                    channel.channel_id,
                    channel.channel_name,
                    channel.input_node_name,
                    channel.output_node_name,
                    channel.input_channels,
                    channel.input_mode,
                    channel.input_target_capable,
                    channel.input_role,
                    channel.sample_rate_hz,
                    channel.property_prefix,
                )
            })
            .collect::<Vec<_>>()
            .join("|"),
    );
    for mix in &manifest.mixes {
        topology.push('|');
        topology.push_str(&format!(
            "mix:{}:{}:{}:{}:{}",
            mix.mix_id,
            mix.mix_name,
            mix.output_node_name,
            mix.sample_rate_hz,
            mix.buses
                .iter()
                .map(|bus| bus.channel_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ));
    }

    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in topology.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChainMetrics {
    pub frames: usize,
    pub channels: usize,
    pub elapsed_micros: u128,
    pub p95_frame_micros: f32,
    pub peak: f32,
    pub rms: f32,
    pub underruns: u32,
    pub fallback_count: u32,
}

#[derive(Debug, Clone)]
pub struct DspAccelerationConfig {
    pack: wavelinux_accelerator::QualifiedProviderPack,
    runtime_directory: PathBuf,
    block_timeout: Duration,
}

impl DspAccelerationConfig {
    pub fn new(
        pack: wavelinux_accelerator::QualifiedProviderPack,
        runtime_directory: impl Into<PathBuf>,
        block_timeout: Duration,
    ) -> Result<Self, String> {
        if block_timeout.is_zero() {
            return Err("accelerator block timeout must be greater than zero".into());
        }
        Ok(Self {
            pack,
            runtime_directory: runtime_directory.into(),
            block_timeout,
        })
    }

    pub fn provider(&self) -> wavelinux_accelerator::AcceleratorProvider {
        self.pack.resolved().manifest.provider
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DspAccelerationMetrics {
    #[serde(rename = "accelerator_provider")]
    pub provider: Option<String>,
    #[serde(rename = "accelerator_active_states")]
    pub active_states: u32,
    #[serde(rename = "accelerator_provider_pids")]
    pub provider_pids: Vec<u32>,
    #[serde(rename = "accelerator_provider_blocks")]
    pub provider_blocks: u64,
    #[serde(rename = "accelerator_fallback_blocks")]
    pub fallback_blocks: u64,
    #[serde(rename = "accelerator_deadline_misses")]
    pub deadline_misses: u64,
    #[serde(rename = "accelerator_invalid_results")]
    pub invalid_results: u64,
    #[serde(rename = "accelerator_stale_results")]
    pub stale_results: u64,
    #[serde(rename = "accelerator_disabled_states")]
    pub disabled_states: u32,
    #[serde(rename = "accelerator_startup_failures")]
    pub startup_failures: Vec<String>,
    #[serde(rename = "accelerator_last_failure")]
    pub last_failure: Option<String>,
}

pub fn process_effect_chain_interleaved_stereo(
    effects: &[EffectInstance],
    sample_rate_hz: u32,
    interleaved: &mut [f32],
) -> ChainMetrics {
    DspChain::new(effects, sample_rate_hz).process_interleaved_stereo(interleaved)
}

#[derive(Debug)]
pub struct DspChain {
    nodes: Vec<DspNode>,
    sample_rate_hz: u32,
    fallback_count: u32,
    initialization_failures: Vec<String>,
    accelerator_provider: Option<String>,
    acceleration_startup_failures: Vec<String>,
    validation_blocks_remaining: u16,
}

const REALTIME_VALIDATION_BLOCKS: u16 = 256;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RealtimeProcessStatus {
    pub non_finite_samples: u32,
    pub effect_mask: u64,
}

impl RealtimeProcessStatus {
    fn record(&mut self, effect_mask: u64, non_finite_samples: usize) {
        if non_finite_samples == 0 {
            return;
        }
        self.non_finite_samples = self
            .non_finite_samples
            .saturating_add(non_finite_samples.min(u32::MAX as usize) as u32);
        self.effect_mask |= effect_mask;
    }

    pub fn merge(&mut self, other: Self) {
        self.non_finite_samples = self
            .non_finite_samples
            .saturating_add(other.non_finite_samples);
        self.effect_mask |= other.effect_mask;
    }
}

impl DspChain {
    pub fn new(effects: &[EffectInstance], sample_rate_hz: u32) -> Self {
        Self::new_with_channels(effects, sample_rate_hz, 2)
    }

    pub fn new_with_channels(
        effects: &[EffectInstance],
        sample_rate_hz: u32,
        input_channels: u8,
    ) -> Self {
        Self::new_with_channels_and_acceleration(effects, sample_rate_hz, input_channels, None)
    }

    pub fn new_with_channels_and_acceleration(
        effects: &[EffectInstance],
        sample_rate_hz: u32,
        input_channels: u8,
        acceleration: Option<&DspAccelerationConfig>,
    ) -> Self {
        let mut fallback_count = 0_u32;
        let mut initialization_failures = Vec::new();
        let mut acceleration_startup_failures = Vec::new();
        let accelerator_provider = acceleration
            .filter(|_| {
                effects
                    .iter()
                    .any(|effect| !effect.bypassed && effect.effect_id == "rnnoise")
            })
            .map(|config| config.provider().as_str().to_string());
        let mut nodes = Vec::new();
        for effect in effects.iter().filter(|effect| !effect.bypassed) {
            match DspNode::new(effect, sample_rate_hz, input_channels, acceleration) {
                Ok(node) => {
                    if let Some(detail) = node.acceleration_startup_failure() {
                        acceleration_startup_failures
                            .push(format!("{}: {detail}", effect.effect_id));
                        fallback_count = fallback_count.saturating_add(1);
                    }
                    nodes.push(node);
                }
                Err(detail) => {
                    initialization_failures.push(format!("{}: {detail}", effect.effect_id));
                    fallback_count = fallback_count.saturating_add(1);
                }
            }
        }
        Self {
            nodes,
            sample_rate_hz,
            fallback_count,
            initialization_failures,
            accelerator_provider,
            acceleration_startup_failures,
            validation_blocks_remaining: REALTIME_VALIDATION_BLOCKS,
        }
    }

    pub fn initialization_failures(&self) -> &[String] {
        &self.initialization_failures
    }

    pub fn is_fully_initialized(&self) -> bool {
        self.initialization_failures.is_empty()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn acceleration_metrics(&self) -> DspAccelerationMetrics {
        let mut metrics = DspAccelerationMetrics {
            provider: self.accelerator_provider.clone(),
            startup_failures: self.acceleration_startup_failures.clone(),
            ..DspAccelerationMetrics::default()
        };
        for node in &self.nodes {
            node.add_acceleration_metrics(&mut metrics);
        }
        metrics.provider_pids.sort_unstable();
        metrics.provider_pids.dedup();
        metrics
    }

    /// Process one realtime block without benchmark instrumentation.
    ///
    /// The audio core records callback-level timing itself. Avoiding per-node
    /// clocks plus extra peak/RMS passes keeps the callback proportional to the
    /// enabled DSP work only.
    pub fn process_realtime_interleaved_stereo(
        &mut self,
        interleaved: &mut [f32],
    ) -> RealtimeProcessStatus {
        self.process_uninstrumented_interleaved_stereo(interleaved)
    }

    /// Process one block on the audio worker. Accelerated chains may wait for
    /// an isolated provider deadline and must use this non-callback boundary.
    pub fn process_worker_interleaved_stereo(
        &mut self,
        interleaved: &mut [f32],
    ) -> RealtimeProcessStatus {
        self.process_uninstrumented_interleaved_stereo(interleaved)
    }

    fn process_uninstrumented_interleaved_stereo(
        &mut self,
        interleaved: &mut [f32],
    ) -> RealtimeProcessStatus {
        let validate_node_outputs = self.validation_blocks_remaining > 0;
        self.validation_blocks_remaining = self.validation_blocks_remaining.saturating_sub(1);
        let mut status = RealtimeProcessStatus::default();
        for node in &mut self.nodes {
            node.process(self.sample_rate_hz, interleaved);
            if validate_node_outputs {
                status.record(
                    node.diagnostic_mask(),
                    silence_non_finite_samples(interleaved),
                );
            }
        }
        status
    }

    pub fn process_interleaved_stereo(&mut self, interleaved: &mut [f32]) -> ChainMetrics {
        let started = Instant::now();
        // The RT path has a bounded effect count. Keep timing samples on the
        // stack so processing never allocates in a PipeWire callback.
        let mut frame_timings = [0.0_f32; 16];
        let mut timing_count = 0_usize;
        for node in &mut self.nodes {
            let effect_started = Instant::now();
            node.process(self.sample_rate_hz, interleaved);
            let per_frame = effect_started.elapsed().as_secs_f64() * 1_000_000.0
                / frame_count(interleaved).max(1) as f64;
            if let Some(slot) = frame_timings.get_mut(timing_count) {
                *slot = per_frame as f32;
                timing_count += 1;
            }
        }

        ChainMetrics {
            frames: frame_count(interleaved),
            channels: 2,
            elapsed_micros: started.elapsed().as_micros(),
            p95_frame_micros: percentile_in_place(&mut frame_timings[..timing_count], 0.95),
            peak: peak(interleaved),
            rms: rms(interleaved),
            underruns: 0,
            fallback_count: self.fallback_count,
        }
    }
}

#[derive(Debug)]
enum DspNode {
    RnNoise(RnNoiseNode),
    Highpass(HighpassNode),
    Eq(EqNode),
    Compressor(CompressorNode),
    Gate(GateNode),
    KaraokeStage(KaraokeStageNode),
    Limiter(LimiterNode),
}

impl DspNode {
    fn new(
        effect: &EffectInstance,
        sample_rate_hz: u32,
        input_channels: u8,
        acceleration: Option<&DspAccelerationConfig>,
    ) -> Result<Self, String> {
        match effect.effect_id.as_str() {
            "rnnoise" => RnNoiseNode::new(effect, input_channels, acceleration).map(Self::RnNoise),
            "highpass" => Ok(Self::Highpass(HighpassNode::new(effect, sample_rate_hz))),
            "eq" => Ok(Self::Eq(EqNode::new(effect, sample_rate_hz))),
            "compressor" => Ok(Self::Compressor(CompressorNode::new(
                effect,
                sample_rate_hz,
            ))),
            "gate" => Ok(Self::Gate(GateNode::new(effect, sample_rate_hz))),
            "karaoke_stage" => Ok(Self::KaraokeStage(KaraokeStageNode::new(
                effect,
                sample_rate_hz,
            ))),
            "limiter" => Ok(Self::Limiter(LimiterNode::new(effect))),
            other => Err(format!("unsupported native effect {other}")),
        }
    }

    fn process(&mut self, _sample_rate_hz: u32, data: &mut [f32]) {
        match self {
            Self::RnNoise(node) => node.process(data),
            Self::Highpass(node) => node.process(data),
            Self::Eq(node) => node.process(data),
            Self::Compressor(node) => node.process(data),
            Self::Gate(node) => node.process(data),
            Self::KaraokeStage(node) => node.process(data),
            Self::Limiter(node) => node.process(data),
        }
    }

    fn diagnostic_mask(&self) -> u64 {
        match self {
            Self::RnNoise(_) => 1 << 0,
            Self::Highpass(_) => 1 << 1,
            Self::Eq(_) => 1 << 2,
            Self::Compressor(_) => 1 << 3,
            Self::Gate(_) => 1 << 4,
            Self::KaraokeStage(_) => 1 << 5,
            Self::Limiter(_) => 1 << 6,
        }
    }

    fn acceleration_startup_failure(&self) -> Option<&str> {
        match self {
            Self::RnNoise(node) => node.acceleration_startup_failure.as_deref(),
            _ => None,
        }
    }

    fn add_acceleration_metrics(&self, metrics: &mut DspAccelerationMetrics) {
        if let Self::RnNoise(node) = self {
            node.add_acceleration_metrics(metrics);
        }
    }
}

fn silence_non_finite_samples(samples: &mut [f32]) -> usize {
    let mut replaced = 0_usize;
    for sample in samples {
        if !sample.is_finite() {
            *sample = 0.0;
            replaced = replaced.saturating_add(1);
        }
    }
    replaced
}

type ExplicitDenoiseStateSlots = [Option<Box<ExplicitDenoiseState>>; 2];

struct RnNoiseNode {
    states: ExplicitDenoiseStateSlots,
    acceleration_startup_failure: Option<String>,
    state_count: usize,
    frame_size: usize,
    input_fill: usize,
    output_index: usize,
    output_available: bool,
    input_left: Box<[f32]>,
    input_right: Box<[f32]>,
    output_left: Box<[f32]>,
    output_right: Box<[f32]>,
    vad_threshold: f32,
    minimum_voice_level_db: f32,
    hold_blocks: usize,
    hold_remaining: usize,
    dry_mix: f32,
    wet_envelope: f32,
}

impl std::fmt::Debug for RnNoiseNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RnNoiseNode")
            .field("state_count", &self.state_count)
            .field("frame_size", &self.frame_size)
            .field("input_fill", &self.input_fill)
            .field("output_index", &self.output_index)
            .field("output_available", &self.output_available)
            .finish_non_exhaustive()
    }
}

impl RnNoiseNode {
    fn new(
        effect: &EffectInstance,
        input_channels: u8,
        acceleration: Option<&DspAccelerationConfig>,
    ) -> Result<Self, String> {
        let state_count = if input_channels <= 1 { 1 } else { 2 };
        let frame_size = DenoiseState::FRAME_SIZE;
        let frame_msec = frame_size as f32 / 48_000.0 * 1000.0;
        let hold_blocks = (param(effect, "hold_ms", 200.0).max(0.0) / frame_msec).ceil() as usize;
        let (states, acceleration_startup_failure) =
            build_explicit_denoise_states(state_count, acceleration)?;
        Ok(Self {
            states,
            acceleration_startup_failure,
            state_count,
            frame_size,
            input_fill: 0,
            output_index: 0,
            output_available: false,
            input_left: vec![0.0; frame_size].into_boxed_slice(),
            input_right: vec![0.0; frame_size].into_boxed_slice(),
            output_left: vec![0.0; frame_size].into_boxed_slice(),
            output_right: vec![0.0; frame_size].into_boxed_slice(),
            vad_threshold: param(effect, "vad_threshold", 25.0).clamp(0.0, 99.0) / 100.0,
            minimum_voice_level_db: param(effect, "minimum_voice_level_db", -70.0)
                .clamp(-70.0, -20.0),
            hold_blocks,
            hold_remaining: 0,
            dry_mix: param(effect, "dry_mix", 0.1).clamp(0.0, 1.0),
            wet_envelope: 0.0,
        })
    }

    fn add_acceleration_metrics(&self, metrics: &mut DspAccelerationMetrics) {
        for state in self.states.iter().flatten() {
            state.add_acceleration_metrics(metrics);
        }
    }

    fn process(&mut self, data: &mut [f32]) {
        for frame in data.as_chunks_mut::<2>().0 {
            let output = if self.output_available {
                let output = [
                    self.output_left[self.output_index],
                    self.output_right[self.output_index],
                ];
                self.output_index += 1;
                if self.output_index >= self.frame_size {
                    self.output_index = 0;
                    self.output_available = false;
                }
                output
            } else {
                [0.0, 0.0]
            };

            self.input_left[self.input_fill] = rnnoise_pcm_sample(frame[0]);
            self.input_right[self.input_fill] = rnnoise_pcm_sample(frame[1]);
            self.input_fill += 1;
            if self.input_fill >= self.frame_size {
                self.process_complete_frame();
                self.input_fill = 0;
            }
            frame.copy_from_slice(&output);
        }
    }

    fn process_complete_frame(&mut self) {
        let input_level_db =
            rnnoise_input_level_db(&self.input_left, &self.input_right, self.state_count);
        if !rnnoise_should_process_frame(
            input_level_db,
            self.minimum_voice_level_db,
            self.hold_remaining,
            self.wet_envelope,
        ) {
            for index in 0..self.frame_size {
                self.output_left[index] = self.input_left[index] * self.dry_mix / 32_768.0;
                self.output_right[index] = self.input_right[index] * self.dry_mix / 32_768.0;
            }
            self.output_index = 0;
            self.output_available = true;
            return;
        }
        let left_probability = self.states[0]
            .as_mut()
            .expect("left RNNoise state is initialized")
            .process_frame(&mut self.output_left, &self.input_left);
        let right_probability = if self.state_count == 2 {
            self.states[1]
                .as_mut()
                .expect("right RNNoise state is initialized for stereo input")
                .process_frame(&mut self.output_right, &self.input_right)
        } else {
            self.output_right.copy_from_slice(&self.output_left);
            left_probability
        };
        let voice_active = rnnoise_voice_is_near(
            left_probability.max(right_probability),
            input_level_db,
            self.vad_threshold,
            self.minimum_voice_level_db,
        );
        if voice_active {
            self.hold_remaining = self.hold_blocks;
        } else {
            self.hold_remaining = self.hold_remaining.saturating_sub(1);
        }
        let target_wet_gain = if voice_active || self.hold_remaining > 0 {
            1.0 - self.dry_mix
        } else {
            0.0
        };
        let wet_step = (target_wet_gain - self.wet_envelope) / self.frame_size.max(1) as f32;
        for index in 0..self.frame_size {
            self.wet_envelope += wet_step;
            self.output_left[index] = (self.output_left[index] * self.wet_envelope
                + self.input_left[index] * self.dry_mix)
                / 32_768.0;
            self.output_right[index] = (self.output_right[index] * self.wet_envelope
                + self.input_right[index] * self.dry_mix)
                / 32_768.0;
        }
        self.wet_envelope = target_wet_gain;
        self.output_index = 0;
        self.output_available = true;
    }
}

fn build_explicit_denoise_states(
    state_count: usize,
    acceleration: Option<&DspAccelerationConfig>,
) -> Result<(ExplicitDenoiseStateSlots, Option<String>), String> {
    if let Some(acceleration) = acceleration {
        let mut states: ExplicitDenoiseStateSlots = [None, None];
        for state in states.iter_mut().take(state_count) {
            match ExplicitDenoiseState::new_provider(acceleration) {
                Ok(provider_state) => *state = Some(provider_state),
                Err(error) => {
                    return Ok((
                        build_cpu_denoise_states(state_count)?,
                        Some(format!(
                            "{} provider startup failed ({error}); using exact CPU neural stage",
                            acceleration.provider().as_str()
                        )),
                    ));
                }
            }
        }
        return Ok((states, None));
    }
    Ok((build_cpu_denoise_states(state_count)?, None))
}

fn build_cpu_denoise_states(state_count: usize) -> Result<ExplicitDenoiseStateSlots, String> {
    Ok([
        Some(ExplicitDenoiseState::new()?),
        if state_count == 2 {
            Some(ExplicitDenoiseState::new()?)
        } else {
            None
        },
    ])
}

struct ExplicitDenoiseState {
    features: DenoiseFeatures,
    neural: RnNoiseNeuralStage,
    previous_gain: [f32; nnnoiseless::NB_BANDS],
}

enum RnNoiseNeuralStage {
    Cpu(Box<wavelinux_accelerator::rnnoise::CpuNeuralStage>),
    Provider {
        stage: Box<wavelinux_accelerator::rnnoise::ProviderBackedNeuralStage>,
        timeout: Duration,
    },
}

impl ExplicitDenoiseState {
    fn new() -> Result<Box<Self>, String> {
        Ok(Box::new(Self {
            features: DenoiseFeatures::new(),
            neural: RnNoiseNeuralStage::Cpu(Box::new(
                wavelinux_accelerator::rnnoise::CpuNeuralStage::new()?,
            )),
            previous_gain: [0.0; nnnoiseless::NB_BANDS],
        }))
    }

    fn new_provider(acceleration: &DspAccelerationConfig) -> Result<Box<Self>, String> {
        Ok(Box::new(Self {
            features: DenoiseFeatures::new(),
            neural: RnNoiseNeuralStage::Provider {
                stage: Box::new(
                    wavelinux_accelerator::rnnoise::ProviderBackedNeuralStage::spawn(
                        &acceleration.pack,
                        &acceleration.runtime_directory,
                    )?,
                ),
                timeout: acceleration.block_timeout,
            },
            previous_gain: [0.0; nnnoiseless::NB_BANDS],
        }))
    }

    fn process_frame(&mut self, output: &mut [f32], input: &[f32]) -> f32 {
        self.features.shift_and_filter_input(input);
        if self.features.compute_frame_features() {
            self.features.synthesize_unmodified(output);
            return 0.0;
        }

        let mut features = [0.0_f32; wavelinux_accelerator::RNNOISE_FEATURE_COUNT];
        features.copy_from_slice(self.features.features());
        let neural = match &mut self.neural {
            RnNoiseNeuralStage::Cpu(stage) => stage.process(&features),
            RnNoiseNeuralStage::Provider { stage, timeout } => stage.process(&features, *timeout),
        };
        self.features.apply_denoise_gains_and_synthesize(
            output,
            &neural.gains,
            &mut self.previous_gain,
        );
        neural.vad_probability
    }

    fn add_acceleration_metrics(&self, metrics: &mut DspAccelerationMetrics) {
        let RnNoiseNeuralStage::Provider { stage, .. } = &self.neural else {
            return;
        };
        let state = stage.metrics();
        metrics.provider = Some(stage.provider().as_str().to_string());
        metrics.provider_pids.push(stage.provider_pid());
        metrics.provider_blocks = metrics
            .provider_blocks
            .saturating_add(state.provider_blocks);
        metrics.fallback_blocks = metrics
            .fallback_blocks
            .saturating_add(state.fallback_blocks);
        metrics.deadline_misses = metrics
            .deadline_misses
            .saturating_add(state.deadline_misses);
        metrics.invalid_results = metrics
            .invalid_results
            .saturating_add(state.invalid_results);
        metrics.stale_results = metrics.stale_results.saturating_add(state.stale_results);
        if state.provider_disabled {
            metrics.disabled_states = metrics.disabled_states.saturating_add(1);
        } else {
            metrics.active_states = metrics.active_states.saturating_add(1);
        }
        if state.last_failure.is_some() {
            metrics.last_failure = state.last_failure.clone();
        }
    }
}

fn rnnoise_pcm_sample(sample: f32) -> f32 {
    if !sample.is_finite() {
        return 0.0;
    }
    (sample * 32_768.0).clamp(-32_768.0, 32_767.0)
}

fn rnnoise_input_level_db(left: &[f32], right: &[f32], channel_count: usize) -> f32 {
    let left_energy =
        left.iter().map(|sample| sample * sample).sum::<f32>() / left.len().max(1) as f32;
    let energy = if channel_count > 1 {
        let right_energy =
            right.iter().map(|sample| sample * sample).sum::<f32>() / right.len().max(1) as f32;
        left_energy.max(right_energy)
    } else {
        left_energy
    };
    amp_to_db(energy.sqrt() / 32_768.0)
}

fn rnnoise_voice_is_near(
    speech_probability: f32,
    input_level_db: f32,
    vad_threshold: f32,
    minimum_voice_level_db: f32,
) -> bool {
    speech_probability >= vad_threshold && input_level_db >= minimum_voice_level_db
}

fn rnnoise_should_process_frame(
    input_level_db: f32,
    minimum_voice_level_db: f32,
    hold_remaining: usize,
    wet_envelope: f32,
) -> bool {
    input_level_db >= minimum_voice_level_db || hold_remaining > 0 || wet_envelope > 1.0e-4
}

#[derive(Debug, Clone)]
struct HighpassNode {
    alpha: f32,
    prev_x: [f32; 2],
    prev_y: [f32; 2],
}

impl HighpassNode {
    fn new(effect: &EffectInstance, sample_rate_hz: u32) -> Self {
        let cutoff = param(effect, "frequency_hz", 80.0).clamp(20.0, 500.0);
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
        let dt = 1.0 / sample_rate_hz.max(1) as f32;
        Self {
            alpha: rc / (rc + dt),
            prev_x: [0.0; 2],
            prev_y: [0.0; 2],
        }
    }

    fn process(&mut self, data: &mut [f32]) {
        for frame in data.as_chunks_mut::<2>().0 {
            for (ch, sample) in frame.iter_mut().enumerate().take(2) {
                let x = *sample;
                let y = self.alpha * (self.prev_y[ch] + x - self.prev_x[ch]);
                *sample = y;
                self.prev_x[ch] = x;
                self.prev_y[ch] = y;
            }
        }
        for channel in 0..2 {
            self.prev_x[channel] = flush_denormal(self.prev_x[channel]);
            self.prev_y[channel] = flush_denormal(self.prev_y[channel]);
        }
    }
}

#[derive(Debug, Clone)]
struct EqNode {
    bands: Vec<[Biquad; 2]>,
}

impl EqNode {
    fn new(effect: &EffectInstance, sample_rate_hz: u32) -> Self {
        let mut bands = Vec::new();
        for (freq, gain_key, q) in graphic_eq_bands() {
            let gain = param(effect, gain_key, 0.0).clamp(-12.0, 12.0);
            if gain.abs() < 0.01 {
                continue;
            }
            let freq = freq.clamp(20.0, sample_rate_hz as f32 * 0.45);
            bands.push([
                Biquad::peaking(sample_rate_hz as f32, freq, q, gain),
                Biquad::peaking(sample_rate_hz as f32, freq, q, gain),
            ]);
        }
        Self { bands }
    }

    fn process(&mut self, data: &mut [f32]) {
        for band in &mut self.bands {
            for frame in data.as_chunks_mut::<2>().0 {
                frame[0] = band[0].process(frame[0]);
                frame[1] = band[1].process(frame[1]);
            }
            band[0].flush_denormals();
            band[1].flush_denormals();
        }
    }
}

fn graphic_eq_bands() -> [(f32, &'static str, f32); 8] {
    [
        (63.0, "band_63_gain_db", 0.9),
        (125.0, "band_125_gain_db", 1.0),
        (250.0, "band_250_gain_db", 1.0),
        (500.0, "band_500_gain_db", 1.0),
        (1000.0, "band_1k_gain_db", 1.0),
        (2000.0, "band_2k_gain_db", 1.0),
        (4000.0, "band_4k_gain_db", 1.0),
        (8000.0, "band_8k_gain_db", 0.9),
    ]
}

#[derive(Debug, Clone)]
struct CompressorNode {
    threshold_amp: f32,
    gain_exponent: f32,
    makeup: f32,
    attack: f32,
    release: f32,
    gain: f32,
}

impl CompressorNode {
    fn new(effect: &EffectInstance, sample_rate_hz: u32) -> Self {
        let threshold_db = param(effect, "threshold_db", -20.0).clamp(-60.0, 0.0);
        let ratio = param(effect, "ratio", 4.0).clamp(1.0, 20.0);
        Self {
            threshold_amp: db_to_amp(threshold_db),
            gain_exponent: ratio.recip() - 1.0,
            makeup: db_to_amp(param(effect, "makeup_gain_db", 0.0).clamp(0.0, 24.0)),
            attack: smoothing_coeff(param(effect, "attack_ms", 5.0), sample_rate_hz),
            release: smoothing_coeff(param(effect, "release_ms", 100.0), sample_rate_hz),
            gain: 1.0,
        }
    }

    fn process(&mut self, data: &mut [f32]) {
        for frame in data.as_chunks_mut::<2>().0 {
            let level = frame[0].abs().max(frame[1].abs());
            let target_gain = if level > self.threshold_amp {
                (level / self.threshold_amp).powf(self.gain_exponent)
            } else {
                1.0
            };
            let coeff = if target_gain < self.gain {
                self.attack
            } else {
                self.release
            };
            self.gain = coeff * self.gain + (1.0 - coeff) * target_gain;
            let output_gain = self.gain * self.makeup;
            frame[0] *= output_gain;
            frame[1] *= output_gain;
        }
    }
}

#[derive(Debug, Clone)]
struct GateNode {
    threshold_amp: f32,
    range: f32,
    attack: f32,
    release: f32,
    hold_frames: usize,
    gain: f32,
    hold: usize,
}

impl GateNode {
    fn new(effect: &EffectInstance, sample_rate_hz: u32) -> Self {
        Self {
            threshold_amp: db_to_amp(param(effect, "threshold_db", -35.0).clamp(-90.0, 0.0)),
            range: db_to_amp(param(effect, "range_db", -60.0).clamp(-90.0, 0.0)),
            attack: smoothing_coeff(param(effect, "attack_ms", 2.5), sample_rate_hz),
            release: smoothing_coeff(param(effect, "release_ms", 160.0), sample_rate_hz),
            hold_frames: (param(effect, "hold_ms", 80.0).max(0.0) * sample_rate_hz as f32 / 1000.0)
                as usize,
            gain: 1.0,
            hold: 0,
        }
    }

    fn process(&mut self, data: &mut [f32]) {
        for frame in data.as_chunks_mut::<2>().0 {
            let level = frame[0].abs().max(frame[1].abs());
            let open = level >= self.threshold_amp;
            if open {
                self.hold = self.hold_frames;
            } else {
                self.hold = self.hold.saturating_sub(1);
            }
            let target = if open || self.hold > 0 {
                1.0
            } else {
                self.range
            };
            let coeff = if target > self.gain {
                self.attack
            } else {
                self.release
            };
            self.gain = coeff * self.gain + (1.0 - coeff) * target;
            frame[0] *= self.gain;
            frame[1] *= self.gain;
        }
    }
}

#[derive(Debug)]
struct VariableDelay {
    samples: Box<[f32]>,
    write_index: usize,
}

impl VariableDelay {
    fn new(capacity: usize) -> Self {
        Self {
            samples: vec![0.0; capacity.max(2)].into_boxed_slice(),
            write_index: 0,
        }
    }

    fn push(&mut self, sample: f32) {
        self.samples[self.write_index] = sample;
        self.write_index = (self.write_index + 1) % self.samples.len();
    }

    fn tap(&self, delay_samples: f32) -> f32 {
        let length = self.samples.len() as f32;
        let position =
            (self.write_index as f32 - delay_samples.clamp(1.0, length - 1.0)).rem_euclid(length);
        let first = position.floor() as usize % self.samples.len();
        let second = (first + 1) % self.samples.len();
        let fraction = position - position.floor();
        self.samples[first] * (1.0 - fraction) + self.samples[second] * fraction
    }
}

#[derive(Debug)]
struct FeedbackDelay {
    samples: Box<[f32]>,
    index: usize,
    feedback: f32,
}

impl FeedbackDelay {
    fn new(delay_frames: usize, sample_rate_hz: u32, decay_seconds: f32) -> Self {
        let delay_seconds = delay_frames.max(1) as f32 / sample_rate_hz.max(1) as f32;
        let feedback = 10.0_f32
            .powf(-3.0 * delay_seconds / decay_seconds.max(0.1))
            .clamp(0.0, 0.98);
        Self {
            samples: vec![0.0; delay_frames.max(2)].into_boxed_slice(),
            index: 0,
            feedback,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.samples[self.index];
        self.samples[self.index] = input + delayed * self.feedback;
        self.index = (self.index + 1) % self.samples.len();
        delayed
    }
}

#[derive(Debug)]
struct StereoRoom {
    left: Vec<FeedbackDelay>,
    right: Vec<FeedbackDelay>,
}

impl StereoRoom {
    fn new(sample_rate_hz: u32, room_size_m: f32, decay_seconds: f32) -> Self {
        let room_scale = (room_size_m / 38.0).sqrt().clamp(0.55, 1.8);
        let frames = |seconds: f32| {
            (seconds * room_scale * sample_rate_hz.max(1) as f32)
                .round()
                .max(2.0) as usize
        };
        Self {
            left: vec![
                FeedbackDelay::new(frames(0.0297), sample_rate_hz, decay_seconds),
                FeedbackDelay::new(frames(0.0371), sample_rate_hz, decay_seconds),
                FeedbackDelay::new(frames(0.0411), sample_rate_hz, decay_seconds),
            ],
            right: vec![
                FeedbackDelay::new(frames(0.0313), sample_rate_hz, decay_seconds),
                FeedbackDelay::new(frames(0.0397), sample_rate_hz, decay_seconds),
                FeedbackDelay::new(frames(0.0437), sample_rate_hz, decay_seconds),
            ],
        }
    }

    fn process(&mut self, input: f32) -> [f32; 2] {
        let left = self
            .left
            .iter_mut()
            .map(|delay| delay.process(input))
            .sum::<f32>()
            / self.left.len().max(1) as f32;
        let right = self
            .right
            .iter_mut()
            .map(|delay| delay.process(input))
            .sum::<f32>()
            / self.right.len().max(1) as f32;
        [left, right]
    }
}

#[derive(Debug)]
struct KaraokeStageNode {
    highpass: [Biquad; 2],
    lowpass: [Biquad; 2],
    tone_gain: f32,
    dry_mix: f32,
    double_mix: f32,
    base_delays: [f32; 2],
    modulation_depth: f32,
    modulation_phase: f32,
    modulation_step: f32,
    delays: [VariableDelay; 2],
    room: StereoRoom,
    room_gain: f32,
}

impl KaraokeStageNode {
    fn new(effect: &EffectInstance, sample_rate_hz: u32) -> Self {
        let sample_rate = sample_rate_hz.max(1) as f32;
        let highpass_hz = param(effect, "tone_highpass_hz", 40.0).clamp(20.0, sample_rate * 0.4);
        let lowpass_hz = param(effect, "tone_lowpass_hz", 16_000.0)
            .clamp(highpass_hz + 20.0, sample_rate * 0.48);
        let delay_msec = param(effect, "double_delay_ms", 28.0).clamp(8.0, 80.0);
        let detune_cents = param(effect, "detune_cents", 7.0).clamp(0.0, 25.0);
        let pitch_delta = 2.0_f32.powf(detune_cents / 1200.0) - 1.0;
        let modulation_hz = 0.7_f32;
        let modulation_depth = (pitch_delta * sample_rate
            / (2.0 * std::f32::consts::PI * modulation_hz))
            .clamp(0.0, sample_rate * 0.012);
        let delay_capacity = (sample_rate * 0.14).ceil() as usize;
        Self {
            highpass: [
                Biquad::highpass(sample_rate, highpass_hz, 0.707),
                Biquad::highpass(sample_rate, highpass_hz, 0.707),
            ],
            lowpass: [
                Biquad::lowpass(sample_rate, lowpass_hz, 0.707),
                Biquad::lowpass(sample_rate, lowpass_hz, 0.707),
            ],
            tone_gain: db_to_amp(param(effect, "tone_gain_db", 0.0).clamp(-12.0, 12.0)),
            dry_mix: param(effect, "dry_mix", 0.78).clamp(0.0, 1.0),
            double_mix: param(effect, "double_mix", 0.22).clamp(0.0, 1.0),
            base_delays: [
                delay_msec * sample_rate / 1000.0,
                (delay_msec + 12.0) * sample_rate / 1000.0,
            ],
            modulation_depth,
            modulation_phase: 0.0,
            modulation_step: 2.0 * std::f32::consts::PI * modulation_hz / sample_rate,
            delays: [
                VariableDelay::new(delay_capacity),
                VariableDelay::new(delay_capacity),
            ],
            room: StereoRoom::new(
                sample_rate_hz,
                param(effect, "room_size_m", 38.0).clamp(1.0, 120.0),
                param(effect, "reverb_time_s", 2.4).clamp(0.1, 8.0),
            ),
            room_gain: db_to_amp(param(effect, "room_level_db", -17.0).clamp(-70.0, 0.0)),
        }
    }

    fn process(&mut self, data: &mut [f32]) {
        for frame in data.as_chunks_mut::<2>().0 {
            let mut tone = [0.0_f32; 2];
            for channel in 0..2 {
                tone[channel] = self.lowpass[channel]
                    .process(self.highpass[channel].process(frame[channel]))
                    * self.tone_gain;
            }
            let modulation = self.modulation_phase.sin() * self.modulation_depth;
            let doubled = [
                self.delays[0].tap(self.base_delays[0] + modulation),
                self.delays[1].tap(self.base_delays[1] - modulation),
            ];
            self.delays[0].push(tone[0]);
            self.delays[1].push(tone[1]);
            self.modulation_phase = (self.modulation_phase + self.modulation_step)
                .rem_euclid(2.0 * std::f32::consts::PI);

            let room_input = (tone[0] + tone[1]) * 0.5;
            let room = self.room.process(room_input);
            frame[0] =
                tone[0] * self.dry_mix + doubled[1] * self.double_mix + room[0] * self.room_gain;
            frame[1] =
                tone[1] * self.dry_mix + doubled[0] * self.double_mix + room[1] * self.room_gain;
        }
    }
}

#[derive(Debug, Clone)]
struct LimiterNode {
    input_gain: f32,
    ceiling: f32,
}

impl LimiterNode {
    fn new(effect: &EffectInstance) -> Self {
        Self {
            input_gain: db_to_amp(param(effect, "input_gain_db", 0.0).clamp(-20.0, 20.0)),
            ceiling: db_to_amp(param(effect, "ceiling_db", -1.0).clamp(-20.0, 0.0)),
        }
    }

    fn process(&self, data: &mut [f32]) {
        for sample in data {
            *sample = (*sample * self.input_gain).clamp(-self.ceiling, self.ceiling);
        }
    }
}

#[cfg(test)]
fn process_effect_chain_interleaved_stereo_once(
    effects: &[EffectInstance],
    sample_rate_hz: u32,
    interleaved: &mut [f32],
) -> ChainMetrics {
    let started = Instant::now();
    let mut frame_timings = Vec::new();
    for effect in effects.iter().filter(|effect| !effect.bypassed) {
        let effect_started = Instant::now();
        match effect.effect_id.as_str() {
            "highpass" => apply_highpass(effect, sample_rate_hz, interleaved),
            "eq" => apply_eq(effect, sample_rate_hz, interleaved),
            "compressor" => apply_compressor(effect, sample_rate_hz, interleaved),
            "gate" => apply_gate(effect, sample_rate_hz, interleaved),
            "limiter" => apply_limiter(effect, interleaved),
            _ => {}
        }
        let per_frame = effect_started.elapsed().as_secs_f64() * 1_000_000.0
            / frame_count(interleaved).max(1) as f64;
        frame_timings.push(per_frame as f32);
    }

    ChainMetrics {
        frames: frame_count(interleaved),
        channels: 2,
        elapsed_micros: started.elapsed().as_micros(),
        p95_frame_micros: percentile(frame_timings, 0.95),
        peak: peak(interleaved),
        rms: rms(interleaved),
        underruns: 0,
        fallback_count: 0,
    }
}

pub fn fixture_effect_chain() -> Vec<EffectInstance> {
    vec![
        effect("highpass", &[("frequency_hz", 80.0)]),
        effect(
            "eq",
            &[
                ("band_63_gain_db", -4.0),
                ("band_125_gain_db", -2.0),
                ("band_250_gain_db", -1.0),
                ("band_500_gain_db", 0.0),
                ("band_1k_gain_db", 1.0),
                ("band_2k_gain_db", 2.5),
                ("band_4k_gain_db", 2.0),
                ("band_8k_gain_db", 1.0),
            ],
        ),
        effect(
            "compressor",
            &[
                ("threshold_db", -18.0),
                ("ratio", 4.0),
                ("attack_ms", 4.0),
                ("release_ms", 90.0),
                ("makeup_gain_db", 3.0),
            ],
        ),
        effect(
            "gate",
            &[
                ("threshold_db", -55.0),
                ("range_db", -18.0),
                ("attack_ms", 3.0),
                ("hold_ms", 80.0),
                ("release_ms", 180.0),
            ],
        ),
        effect("limiter", &[("input_gain_db", 0.0), ("ceiling_db", -1.0)]),
    ]
}

pub fn generated_stereo_fixture(frames: usize, sample_rate_hz: u32) -> Vec<f32> {
    let mut data = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let t = frame as f32 / sample_rate_hz as f32;
        let rumble = (2.0 * std::f32::consts::PI * 35.0 * t).sin() * 0.06;
        let voice = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.22
            + (2.0 * std::f32::consts::PI * 880.0 * t).sin() * 0.08;
        let transient = if frame % 4096 < 48 { 0.7 } else { 0.0 };
        let noise = pseudo_noise(frame) * 0.018;
        let left = (rumble + voice + transient + noise).clamp(-0.98, 0.98);
        let right = (rumble * 0.9 + voice * 0.96 + transient + noise * 0.7).clamp(-0.98, 0.98);
        data.push(left);
        data.push(right);
    }
    data
}

#[cfg(test)]
fn apply_highpass(effect: &EffectInstance, sample_rate_hz: u32, data: &mut [f32]) {
    let cutoff = param(effect, "frequency_hz", 80.0).clamp(20.0, 500.0);
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff);
    let dt = 1.0 / sample_rate_hz.max(1) as f32;
    let alpha = rc / (rc + dt);
    let mut prev_x = [0.0_f32; 2];
    let mut prev_y = [0.0_f32; 2];

    for frame in data.chunks_exact_mut(2) {
        for ch in 0..2 {
            let x = frame[ch];
            let y = alpha * (prev_y[ch] + x - prev_x[ch]);
            frame[ch] = y;
            prev_x[ch] = x;
            prev_y[ch] = y;
        }
    }
}

#[cfg(test)]
fn apply_eq(effect: &EffectInstance, sample_rate_hz: u32, data: &mut [f32]) {
    for (freq, gain_key, q) in graphic_eq_bands() {
        let gain = param(effect, gain_key, 0.0).clamp(-12.0, 12.0);
        if gain.abs() < 0.01 {
            continue;
        }
        let freq = freq.clamp(20.0, sample_rate_hz as f32 * 0.45);
        let mut left = Biquad::peaking(sample_rate_hz as f32, freq, q, gain);
        let mut right = Biquad::peaking(sample_rate_hz as f32, freq, q, gain);
        for frame in data.chunks_exact_mut(2) {
            frame[0] = left.process(frame[0]);
            frame[1] = right.process(frame[1]);
        }
    }
}

#[cfg(test)]
fn apply_compressor(effect: &EffectInstance, sample_rate_hz: u32, data: &mut [f32]) {
    let threshold_db = param(effect, "threshold_db", -20.0).clamp(-60.0, 0.0);
    let ratio = param(effect, "ratio", 4.0).clamp(1.0, 20.0);
    let makeup = db_to_amp(param(effect, "makeup_gain_db", 0.0).clamp(0.0, 24.0));
    let attack = smoothing_coeff(param(effect, "attack_ms", 5.0), sample_rate_hz);
    let release = smoothing_coeff(param(effect, "release_ms", 100.0), sample_rate_hz);
    let mut gain = 1.0_f32;

    for frame in data.chunks_exact_mut(2) {
        let level = frame[0].abs().max(frame[1].abs()).max(1.0e-9);
        let level_db = amp_to_db(level);
        let target_gain = if level_db > threshold_db {
            let compressed_db = threshold_db + (level_db - threshold_db) / ratio;
            db_to_amp(compressed_db - level_db)
        } else {
            1.0
        };
        let coeff = if target_gain < gain { attack } else { release };
        gain = coeff * gain + (1.0 - coeff) * target_gain;
        frame[0] *= gain * makeup;
        frame[1] *= gain * makeup;
    }
}

#[cfg(test)]
fn apply_gate(effect: &EffectInstance, sample_rate_hz: u32, data: &mut [f32]) {
    let threshold_db = param(effect, "threshold_db", -35.0).clamp(-90.0, 0.0);
    let range = db_to_amp(param(effect, "range_db", -60.0).clamp(-90.0, 0.0));
    let attack = smoothing_coeff(param(effect, "attack_ms", 2.5), sample_rate_hz);
    let release = smoothing_coeff(param(effect, "release_ms", 160.0), sample_rate_hz);
    let hold_frames =
        (param(effect, "hold_ms", 80.0).max(0.0) * sample_rate_hz as f32 / 1000.0) as usize;
    let mut gain = 1.0_f32;
    let mut hold = 0_usize;

    for frame in data.chunks_exact_mut(2) {
        let level = frame[0].abs().max(frame[1].abs()).max(1.0e-9);
        let open = amp_to_db(level) >= threshold_db;
        if open {
            hold = hold_frames;
        } else {
            hold = hold.saturating_sub(1);
        }
        let target = if open || hold > 0 { 1.0 } else { range };
        let coeff = if target > gain { attack } else { release };
        gain = coeff * gain + (1.0 - coeff) * target;
        frame[0] *= gain;
        frame[1] *= gain;
    }
}

#[cfg(test)]
fn apply_limiter(effect: &EffectInstance, data: &mut [f32]) {
    let input_gain = db_to_amp(param(effect, "input_gain_db", 0.0).clamp(-20.0, 20.0));
    let ceiling = db_to_amp(param(effect, "ceiling_db", -1.0).clamp(-20.0, 0.0));
    for sample in data {
        *sample = (*sample * input_gain).clamp(-ceiling, ceiling);
    }
}

#[derive(Debug, Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn lowpass(sample_rate_hz: f32, frequency_hz: f32, q: f32) -> Self {
        let omega = 2.0 * std::f32::consts::PI * frequency_hz / sample_rate_hz.max(1.0);
        let alpha = omega.sin() / (2.0 * q.max(0.1));
        let cos = omega.cos();
        let b0 = (1.0 - cos) * 0.5;
        let b1 = 1.0 - cos;
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn highpass(sample_rate_hz: f32, frequency_hz: f32, q: f32) -> Self {
        let omega = 2.0 * std::f32::consts::PI * frequency_hz / sample_rate_hz.max(1.0);
        let alpha = omega.sin() / (2.0 * q.max(0.1));
        let cos = omega.cos();
        let b0 = (1.0 + cos) * 0.5;
        let b1 = -(1.0 + cos);
        let b2 = b0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn peaking(sample_rate_hz: f32, frequency_hz: f32, q: f32, gain_db: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let omega = 2.0 * std::f32::consts::PI * frequency_hz / sample_rate_hz.max(1.0);
        let alpha = omega.sin() / (2.0 * q.max(0.1));
        let cos = omega.cos();
        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha / a;
        Self::normalized(b0, b1, b2, a0, a1, a2)
    }

    fn normalized(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }

    fn flush_denormals(&mut self) {
        self.z1 = flush_denormal(self.z1);
        self.z2 = flush_denormal(self.z2);
    }
}

fn flush_denormal(value: f32) -> f32 {
    if value.abs() < 1.0e-20 {
        0.0
    } else {
        value
    }
}

fn effect(id: &str, params: &[(&str, f32)]) -> EffectInstance {
    let mut effect = EffectInstance::new(id);
    effect.instance_id = id.into();
    effect.params = params
        .iter()
        .map(|(key, value)| ((*key).to_string(), *value))
        .collect();
    effect
}

fn param(effect: &EffectInstance, key: &str, default: f32) -> f32 {
    effect.params.get(key).copied().unwrap_or(default)
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn forced_provider_failures() -> Vec<DspProvider> {
    std::env::var(DSP_FORCE_PROVIDER_FAIL_ENV)
        .unwrap_or_default()
        .split(',')
        .filter_map(|value| match normalize_token(value).as_str() {
            "cuda" | "nvidia" => Some(DspProvider::Cuda),
            "openvino" | "intel" => Some(DspProvider::OpenVino),
            "migraphx" | "amd" | "rocm" => Some(DspProvider::MiGraphX),
            "portable_cpu" | "simd_cpu" => Some(DspProvider::PortableCpu),
            "pure_cpu" | "cpu" => Some(DspProvider::PureCpu),
            _ => None,
        })
        .collect()
}

fn portable_cpu_detail() -> String {
    let mut features = Vec::new();
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("sse2") {
            features.push("sse2");
        }
        if std::is_x86_feature_detected!("avx2") {
            features.push("avx2");
        }
        if std::is_x86_feature_detected!("fma") {
            features.push("fma");
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        features.push("neon");
    }
    if features.is_empty() {
        "portable scalar CPU path available".into()
    } else {
        format!("portable CPU path available ({})", features.join(","))
    }
}

fn db_to_amp(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

fn amp_to_db(amp: f32) -> f32 {
    20.0 * amp.max(1.0e-9).log10()
}

fn smoothing_coeff(ms: f32, sample_rate_hz: u32) -> f32 {
    let seconds = (ms.max(0.01) / 1000.0).max(1.0e-6);
    (-1.0 / (seconds * sample_rate_hz.max(1) as f32)).exp()
}

fn frame_count(interleaved: &[f32]) -> usize {
    interleaved.len() / 2
}

fn peak(data: &[f32]) -> f32 {
    data.iter()
        .fold(0.0_f32, |acc, sample| acc.max(sample.abs()))
}

fn rms(data: &[f32]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    (data.iter().map(|sample| sample * sample).sum::<f32>() / data.len() as f32).sqrt()
}

#[cfg(test)]
fn percentile(mut values: Vec<f32>, percentile: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    let idx = ((values.len() - 1) as f32 * percentile.clamp(0.0, 1.0)).round() as usize;
    values[idx]
}

fn percentile_in_place(values: &mut [f32], percentile: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    let idx = ((values.len() - 1) as f32 * percentile.clamp(0.0, 1.0)).round() as usize;
    values[idx]
}

fn pseudo_noise(frame: usize) -> f32 {
    let mut x = frame as u32 ^ 0x9e37_79b9;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
}

pub fn benchmark_fixture(frames: usize, sample_rate_hz: u32) -> ChainMetrics {
    let mut fixture = generated_stereo_fixture(frames, sample_rate_hz);
    let effects = fixture_effect_chain();
    process_effect_chain_interleaved_stereo(&effects, sample_rate_hz, &mut fixture)
}

pub fn human_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{:.2}s", duration.as_secs_f64())
    } else {
        format!("{:.2}ms", duration.as_secs_f64() * 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, hz: f32, sample_rate_hz: u32, amp: f32) -> Vec<f32> {
        let mut data = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let sample = (2.0 * std::f32::consts::PI * hz * frame as f32 / sample_rate_hz as f32)
                .sin()
                * amp;
            data.push(sample);
            data.push(sample);
        }
        data
    }

    fn voice_like(frames: usize, sample_rate_hz: u32, peak_amp: f32) -> Vec<f32> {
        let mut data = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let time = frame as f32 / sample_rate_hz as f32;
            let envelope = 0.7 + 0.3 * (2.0 * std::f32::consts::PI * 3.1 * time).sin().abs();
            let sample = peak_amp
                * envelope
                * ((2.0 * std::f32::consts::PI * 130.0 * time).sin() * 0.6
                    + (2.0 * std::f32::consts::PI * 260.0 * time).sin() * 0.25
                    + (2.0 * std::f32::consts::PI * 1040.0 * time).sin() * 0.15);
            data.push(sample);
            data.push(sample);
        }
        data
    }

    #[test]
    fn dsp_channel_config_tracks_wavelinux6_namespace() {
        let config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux6",
            "wavelinux6_fx_hardware_in_input",
            "wavelinux6-mic",
            vec![effect("highpass", &[("frequency_hz", 90.0)])],
        );

        assert_eq!(config.revision, DSP_CHANNEL_CONFIG_REVISION);
        assert_eq!(config.input_node_name, "wavelinux6_fx_hardware_in_input");
        assert_eq!(config.output_node_name, "wavelinux6-mic");
        assert!(config.unsupported_active_effects().is_empty());
    }

    #[test]
    fn dsp_channel_config_reports_unsupported_native_effects() {
        let config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux6",
            "wavelinux6_fx_hardware_in_input",
            "wavelinux6-mic",
            vec![effect("convolver", &[])],
        );

        assert_eq!(config.unsupported_active_effects(), vec!["convolver"]);
    }

    #[test]
    fn dsp_channel_config_defaults_adaptive_latency_for_legacy_json() {
        let raw = r#"{
            "revision": "1",
            "channel_id": "hardware_in",
            "channel_name": "Input",
            "graph_prefix": "wavelinux5",
            "property_prefix": "wavelinux5",
            "app_name": "WaveLinux5",
            "input_node_name": "wavelinux5_fx_hardware_in_input",
            "output_node_name": "wavelinux5-mic",
            "sample_rate_hz": 48000,
            "latency_frames": 256,
            "effects": []
        }"#;
        let config: DspChannelConfig = serde_json::from_str(raw).unwrap();
        assert!(config.adaptive_latency.enabled);
        assert_eq!(config.adaptive_latency.max_msec, 120);
        assert_eq!(config.control_socket_path, None);
    }

    #[test]
    fn core_manifest_validates_native_mix_bus_channels() {
        let channel = DspChannelConfig::new(
            "music",
            "Music",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_music",
            "wavelinux6_fx_music_source",
            Vec::new(),
        );
        let mix = DspMixConfig {
            mix_id: "stream".into(),
            mix_name: "Stream".into(),
            graph_prefix: "wavelinux6".into(),
            property_prefix: "wavelinux6".into(),
            app_name: "WaveLinux 6".into(),
            output_node_name: "wavelinux6_mix_stream_source".into(),
            output_target_node_names: Vec::new(),
            sample_rate_hz: 48_000,
            latency_frames: 1_344,
            pipewire_quantum_frames: 0,
            adaptive_latency: DspAdaptiveLatencyConfig::default(),
            volume: 1.0,
            muted: false,
            buses: vec![DspMixBusConfig {
                channel_id: "music".into(),
                volume: 0.75,
                muted: false,
                enabled: true,
            }],
        };
        let manifest = DspCoreManifest::new("test", vec![channel.clone()]).with_mixes(
            vec![mix.clone()],
            Some("/run/user/1000/wavelinux6/control/wavelinux6-audio-core.sock".into()),
        );
        assert!(manifest.validate().is_ok());

        let mut invalid_quantum_mix = mix.clone();
        invalid_quantum_mix.pipewire_quantum_frames = 300;
        let invalid_quantum = DspCoreManifest::new("test", vec![channel.clone()])
            .with_mixes(vec![invalid_quantum_mix], None);
        assert!(invalid_quantum
            .validate()
            .unwrap_err()
            .contains("invalid PipeWire quantum"));

        let mut invalid_mix = mix;
        invalid_mix.buses[0].channel_id = "missing".into();
        let invalid =
            DspCoreManifest::new("test", vec![channel]).with_mixes(vec![invalid_mix], None);
        assert!(invalid
            .validate()
            .unwrap_err()
            .contains("unknown channel missing"));
    }

    #[test]
    fn stateful_dsp_chain_keeps_filter_state_between_buffers() {
        let effects = vec![effect("highpass", &[("frequency_hz", 120.0)])];
        let mut stateful = DspChain::new(&effects, 48_000);
        let mut first = vec![0.5_f32; 512 * 2];
        let mut second = vec![0.5_f32; 512 * 2];

        stateful.process_interleaved_stereo(&mut first);
        stateful.process_interleaved_stereo(&mut second);

        let mut stateless_second = vec![0.5_f32; 512 * 2];
        process_effect_chain_interleaved_stereo_once(&effects, 48_000, &mut stateless_second);
        assert!(rms(&second) < rms(&stateless_second) * 0.5);
    }

    #[test]
    fn startup_validation_identifies_and_contains_the_first_invalid_effect() {
        let effects = vec![effect("highpass", &[("frequency_hz", 80.0)])];
        let mut chain = DspChain::new(&effects, 48_000);
        let mut block = vec![0.0_f32; 128 * 2];
        block[0] = f32::NAN;

        let status = chain.process_realtime_interleaved_stereo(&mut block);

        assert_eq!(status.effect_mask, 1 << 1);
        assert!(status.non_finite_samples > 0);
        assert!(block.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn mono_rnnoise_uses_one_state_and_keeps_public_stereo_channels_equal() {
        let effects = vec![effect(
            "rnnoise",
            &[
                ("vad_threshold", 25.0),
                ("hold_ms", 200.0),
                ("minimum_voice_level_db", -70.0),
                ("dry_mix", 0.1),
            ],
        )];
        let mut chain = DspChain::new_with_channels(&effects, 48_000, 1);
        assert!(
            chain.is_fully_initialized(),
            "{:?}",
            chain.initialization_failures()
        );
        let mut data = sine(1440, 220.0, 48_000, 0.25);

        chain.process_interleaved_stereo(&mut data);

        assert!(data[960..].iter().any(|sample| sample.abs() > 1.0e-6));
        assert!(data
            .chunks_exact(2)
            .all(|frame| (frame[0] - frame[1]).abs() < 1.0e-6));
    }

    #[test]
    fn explicit_rnnoise_state_matches_upstream_frame_processing() {
        let mut upstream = DenoiseState::new();
        let mut explicit = ExplicitDenoiseState::new().unwrap();
        let mut maximum_sample_error = 0.0_f32;
        let mut maximum_vad_error = 0.0_f32;
        for frame_index in 0..32 {
            let input = (0..DenoiseState::FRAME_SIZE)
                .map(|sample| {
                    let absolute = frame_index * DenoiseState::FRAME_SIZE + sample;
                    let voice = (absolute as f32 * 2.0 * std::f32::consts::PI * 173.0 / 48_000.0)
                        .sin()
                        * 8_000.0;
                    let noise = ((absolute * 17 % 101) as f32 - 50.0) * 18.0;
                    voice + noise
                })
                .collect::<Vec<_>>();
            let mut expected = vec![0.0_f32; DenoiseState::FRAME_SIZE];
            let mut actual = vec![0.0_f32; DenoiseState::FRAME_SIZE];
            let expected_vad = upstream.process_frame(&mut expected, &input);
            let actual_vad = explicit.process_frame(&mut actual, &input);
            maximum_vad_error = maximum_vad_error.max((expected_vad - actual_vad).abs());
            maximum_sample_error = maximum_sample_error.max(
                expected
                    .iter()
                    .zip(actual)
                    .map(|(expected, actual)| (expected - actual).abs())
                    .fold(0.0, f32::max),
            );
        }

        assert!(maximum_vad_error <= 1.0e-4, "VAD error {maximum_vad_error}");
        assert!(
            maximum_sample_error <= 8.0,
            "PCM sample error {maximum_sample_error}"
        );
    }

    #[test]
    fn tuned_voice_chain_remains_finite_across_many_realtime_blocks() {
        let effects = vec![
            effect("highpass", &[("frequency_hz", 80.0)]),
            effect(
                "rnnoise",
                &[
                    ("vad_threshold", 67.0),
                    ("hold_ms", 145.0),
                    ("minimum_voice_level_db", -40.8),
                    ("dry_mix", 0.02),
                ],
            ),
            effect(
                "eq",
                &[
                    ("band_63_gain_db", 5.5),
                    ("band_125_gain_db", 2.0),
                    ("band_250_gain_db", -1.0),
                    ("band_500_gain_db", 0.0),
                    ("band_1k_gain_db", 1.0),
                    ("band_2k_gain_db", 2.5),
                    ("band_4k_gain_db", -4.5),
                    ("band_8k_gain_db", 1.0),
                ],
            ),
            effect(
                "gate",
                &[
                    ("threshold_db", -56.0),
                    ("range_db", -37.0),
                    ("attack_ms", 5.5),
                    ("hold_ms", 120.0),
                    ("release_ms", 225.0),
                ],
            ),
            effect(
                "compressor",
                &[
                    ("threshold_db", -25.5),
                    ("ratio", 5.4),
                    ("attack_ms", 3.5),
                    ("release_ms", 80.0),
                    ("makeup_gain_db", 3.0),
                ],
            ),
            effect("limiter", &[("input_gain_db", 1.0), ("ceiling_db", -0.6)]),
        ];
        let mut chain = DspChain::new_with_channels(&effects, 48_000, 1);
        assert!(
            chain.is_fully_initialized(),
            "{:?}",
            chain.initialization_failures()
        );

        let mut priming_silence = vec![0.0_f32; 960 * 2];
        let prime_status = chain.process_realtime_interleaved_stereo(&mut priming_silence);
        assert_eq!(prime_status, RealtimeProcessStatus::default());

        for block_index in 0..500 {
            let amplitude = if block_index < 80 {
                0.005
            } else if block_index % 40 < 12 {
                0.12
            } else {
                0.0008
            };
            let mut block = sine(128, 220.0, 48_000, amplitude);
            let status = chain.process_realtime_interleaved_stereo(&mut block);
            assert!(
                block.iter().all(|sample| sample.is_finite()),
                "non-finite sample in block {block_index}"
            );
            assert_eq!(
                status,
                RealtimeProcessStatus::default(),
                "invalid effect output in block {block_index}"
            );
        }
    }

    #[test]
    fn rnnoise_near_field_gate_requires_probability_and_input_level() {
        assert!(rnnoise_voice_is_near(0.90, -34.0, 0.85, -42.0));
        assert!(!rnnoise_voice_is_near(0.90, -50.0, 0.85, -42.0));
        assert!(!rnnoise_voice_is_near(0.70, -34.0, 0.85, -42.0));
    }

    #[test]
    fn rnnoise_skips_settled_background_but_finishes_open_gate() {
        assert!(!rnnoise_should_process_frame(-50.0, -42.0, 0, 0.0));
        assert!(rnnoise_should_process_frame(-35.0, -42.0, 0, 0.0));
        assert!(rnnoise_should_process_frame(-50.0, -42.0, 2, 0.0));
        assert!(rnnoise_should_process_frame(-50.0, -42.0, 0, 0.2));
    }

    #[test]
    fn rnnoise_input_level_uses_the_louder_channel() {
        let quiet = vec![327.68_f32; 480];
        let loud = vec![3_276.8_f32; 480];

        assert!((rnnoise_input_level_db(&quiet, &loud, 2) + 20.0).abs() < 0.1);
        assert!((rnnoise_input_level_db(&quiet, &loud, 1) + 40.0).abs() < 0.1);
    }

    #[test]
    fn rnnoise_input_is_bounded_to_its_signed_pcm_contract() {
        assert_eq!(rnnoise_pcm_sample(-2.0), -32_768.0);
        assert_eq!(rnnoise_pcm_sample(2.0), 32_767.0);
        assert_eq!(rnnoise_pcm_sample(0.5), 16_384.0);
        assert_eq!(rnnoise_pcm_sample(f32::NAN), 0.0);
    }

    #[test]
    fn karaoke_stage_processes_without_external_plugins_or_non_finite_samples() {
        let effects = vec![effect(
            "karaoke_stage",
            &[
                ("dry_mix", 0.78),
                ("tone_highpass_hz", 40.0),
                ("tone_lowpass_hz", 16_000.0),
                ("tone_gain_db", 0.0),
                ("double_mix", 0.22),
                ("double_delay_ms", 28.0),
                ("detune_cents", 7.0),
                ("room_size_m", 38.0),
                ("reverb_time_s", 2.4),
                ("room_level_db", -17.0),
            ],
        )];
        let mut chain = DspChain::new(&effects, 48_000);
        let mut data = sine(4800, 220.0, 48_000, 0.2);
        let original = data.clone();

        chain.process_interleaved_stereo(&mut data);

        assert!(chain.is_fully_initialized());
        assert!(data.iter().all(|sample| sample.is_finite()));
        assert_ne!(data, original);
    }

    #[test]
    fn highpass_reduces_low_frequency_energy() {
        let mut data = sine(4800, 30.0, 48_000, 0.5);
        let before = rms(&data);
        apply_highpass(
            &effect("highpass", &[("frequency_hz", 120.0)]),
            48_000,
            &mut data,
        );
        assert!(rms(&data) < before * 0.5);
    }

    #[test]
    fn filter_state_flushes_subnormal_values_between_blocks() {
        assert_eq!(flush_denormal(1.0e-30), 0.0);
        assert_eq!(flush_denormal(-1.0e-30), 0.0);
        assert_eq!(flush_denormal(1.0e-10), 1.0e-10);

        let mut filter = Biquad::peaking(48_000.0, 63.0, 0.9, 6.0);
        let _ = filter.process(1.0);
        for _ in 0..2_000 {
            for _ in 0..256 {
                let _ = filter.process(0.0);
            }
            filter.flush_denormals();
        }

        assert_eq!(filter.z1, 0.0);
        assert_eq!(filter.z2, 0.0);
    }

    #[test]
    fn eq_gain_changes_signal_without_nan() {
        let mut data = sine(4800, 1000.0, 48_000, 0.2);
        let before = rms(&data);
        apply_eq(
            &effect(
                "eq",
                &[
                    ("band_63_gain_db", 0.0),
                    ("band_125_gain_db", 0.0),
                    ("band_250_gain_db", 0.0),
                    ("band_500_gain_db", 0.0),
                    ("band_1k_gain_db", 6.0),
                    ("band_2k_gain_db", 0.0),
                    ("band_4k_gain_db", 0.0),
                    ("band_8k_gain_db", 0.0),
                ],
            ),
            48_000,
            &mut data,
        );
        assert!(data.iter().all(|sample| sample.is_finite()));
        assert!(rms(&data) > before * 1.2);
    }

    #[test]
    fn compressor_reduces_loud_signal_after_makeup_accounted() {
        let mut data = sine(4800, 440.0, 48_000, 0.8);
        let before = peak(&data);
        apply_compressor(
            &effect(
                "compressor",
                &[
                    ("threshold_db", -24.0),
                    ("ratio", 8.0),
                    ("attack_ms", 1.5),
                    ("release_ms", 80.0),
                    ("makeup_gain_db", 0.0),
                ],
            ),
            48_000,
            &mut data,
        );
        assert!(peak(&data) < before);
    }

    #[test]
    fn compressor_linear_domain_matches_decibel_reference() {
        let compressor = effect(
            "compressor",
            &[
                ("threshold_db", -24.0),
                ("ratio", 6.0),
                ("attack_ms", 3.0),
                ("release_ms", 120.0),
                ("makeup_gain_db", 4.0),
            ],
        );
        let mut reference = generated_stereo_fixture(12_000, 48_000);
        let mut optimized = reference.clone();

        apply_compressor(&compressor, 48_000, &mut reference);
        DspChain::new(&[compressor], 48_000).process_realtime_interleaved_stereo(&mut optimized);

        let max_difference = reference
            .iter()
            .zip(&optimized)
            .map(|(left, right)| (left - right).abs())
            .fold(0.0_f32, f32::max);
        assert!(max_difference < 2.0e-6, "max difference {max_difference}");
    }

    #[test]
    fn gate_attenuates_quiet_signal() {
        let mut data = sine(4800, 440.0, 48_000, 0.001);
        let before = rms(&data);
        apply_gate(
            &effect(
                "gate",
                &[
                    ("threshold_db", -35.0),
                    ("range_db", -40.0),
                    ("attack_ms", 1.0),
                    ("hold_ms", 0.0),
                    ("release_ms", 10.0),
                ],
            ),
            48_000,
            &mut data,
        );
        assert!(rms(&data) < before * 0.5);
    }

    #[test]
    fn nearby_voice_gate_profile_rejects_far_speech_and_opens_for_close_speech() {
        let gate = effect(
            "gate",
            &[
                ("threshold_db", -20.0),
                ("range_db", -90.0),
                ("attack_ms", 1.0),
                ("hold_ms", 60.0),
                ("release_ms", 100.0),
            ],
        );
        let mut chain = DspChain::new(&[gate], 48_000);

        let mut far_speech = voice_like(48_000, 48_000, db_to_amp(-30.0));
        let far_input_rms = rms(&far_speech);
        chain.process_realtime_interleaved_stereo(&mut far_speech);
        let far_tail = &far_speech[far_speech.len() - 9_600 * 2..];
        assert!(
            rms(far_tail) < far_input_rms * 0.001,
            "far speech remained audible at {} dBFS RMS",
            amp_to_db(rms(far_tail))
        );

        let mut close_speech = voice_like(9_600, 48_000, db_to_amp(-10.0));
        let close_tail_start = close_speech.len() - 2_400 * 2;
        let close_input_rms = rms(&close_speech[close_tail_start..]);
        chain.process_realtime_interleaved_stereo(&mut close_speech);
        assert!(
            rms(&close_speech[close_tail_start..]) > close_input_rms * 0.95,
            "close speech did not reopen the gate"
        );
    }

    #[test]
    fn gate_linear_threshold_matches_decibel_reference() {
        let gate = effect(
            "gate",
            &[
                ("threshold_db", -42.0),
                ("range_db", -35.0),
                ("attack_ms", 2.0),
                ("hold_ms", 50.0),
                ("release_ms", 140.0),
            ],
        );
        let mut reference = generated_stereo_fixture(12_000, 48_000);
        let mut optimized = reference.clone();

        apply_gate(&gate, 48_000, &mut reference);
        DspChain::new(&[gate], 48_000).process_realtime_interleaved_stereo(&mut optimized);

        assert_eq!(reference, optimized);
    }

    #[test]
    fn limiter_enforces_ceiling() {
        let mut data = vec![1.5, -1.5, 0.8, -0.8];
        apply_limiter(
            &effect("limiter", &[("input_gain_db", 6.0), ("ceiling_db", -6.0)]),
            &mut data,
        );
        assert!(peak(&data) <= db_to_amp(-6.0) + 1.0e-6);
    }

    #[test]
    fn provider_selection_falls_back_to_cpu() {
        let inputs = ProviderProbeInputs {
            cuda_available: false,
            cuda_detail: "missing".into(),
            openvino_available: false,
            openvino_detail: "missing".into(),
            migraphx_available: false,
            migraphx_detail: "missing".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let status = select_provider(
            AudioRuntimeMode::DspAccelerated,
            DspProviderPreference::Cuda,
            &inputs,
        );
        assert_eq!(status.effective_runtime, AudioRuntimeMode::DspCpu);
        assert_eq!(status.selected_provider, Some(DspProvider::PortableCpu));
        assert!(status.fallback_active);
        assert_eq!(status.fallback_count, 1);
        assert!(!status.accelerated);
        assert!(status.runtime_fallback_reason.is_some());
    }

    #[test]
    fn automatic_runtime_uses_cpu_without_reporting_an_acceleration_failure() {
        let inputs = ProviderProbeInputs {
            cuda_available: false,
            cuda_detail: "provider pack unavailable".into(),
            openvino_available: false,
            openvino_detail: "provider pack unavailable".into(),
            migraphx_available: false,
            migraphx_detail: "provider pack unavailable".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };

        let status = select_provider(
            AudioRuntimeMode::DspAuto,
            DspProviderPreference::Auto,
            &inputs,
        );

        assert_eq!(status.effective_runtime, AudioRuntimeMode::DspCpu);
        assert_eq!(status.selected_provider, Some(DspProvider::PortableCpu));
        assert!(!status.accelerated);
        assert!(!status.fallback_active);
        assert!(status.provider_probe_failures.is_empty());
    }

    #[test]
    fn host_runtime_probe_never_enables_an_unqualified_accelerator() {
        let inputs = ProviderProbeInputs::detect();
        for (available, provider) in [
            (
                inputs.cuda_available,
                wavelinux_accelerator::AcceleratorProvider::Cuda,
            ),
            (
                inputs.openvino_available,
                wavelinux_accelerator::AcceleratorProvider::OpenVino,
            ),
            (
                inputs.migraphx_available,
                wavelinux_accelerator::AcceleratorProvider::MiGraphX,
            ),
        ] {
            if available {
                assert!(wavelinux_accelerator::probe_provider_pack(provider).qualified);
            }
        }
    }

    #[test]
    fn cpu_chain_reports_no_live_accelerator_state() {
        let chain = DspChain::new_with_channels(&[effect("rnnoise", &[])], 48_000, 1);
        assert_eq!(
            chain.acceleration_metrics(),
            DspAccelerationMetrics::default()
        );
    }

    #[test]
    fn provider_selection_prefers_cuda_when_available() {
        let inputs = ProviderProbeInputs {
            cuda_available: true,
            cuda_detail: "ok".into(),
            openvino_available: true,
            openvino_detail: "ok".into(),
            migraphx_available: true,
            migraphx_detail: "ok".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let status = select_provider(
            AudioRuntimeMode::DspAuto,
            DspProviderPreference::Auto,
            &inputs,
        );
        assert_eq!(status.effective_runtime, AudioRuntimeMode::DspAuto);
        assert_eq!(status.selected_provider, Some(DspProvider::Cuda));
        assert!(status.accelerated);
        assert!(!status.fallback_active);
    }

    #[test]
    fn provider_selection_honors_migraphx_preference() {
        let inputs = ProviderProbeInputs {
            cuda_available: true,
            cuda_detail: "ok".into(),
            openvino_available: true,
            openvino_detail: "ok".into(),
            migraphx_available: true,
            migraphx_detail: "ok".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let status = select_provider(
            AudioRuntimeMode::DspAuto,
            DspProviderPreference::MiGraphX,
            &inputs,
        );

        assert_eq!(status.selected_provider, Some(DspProvider::MiGraphX));
        assert!(status.accelerated);
        assert!(!status.fallback_active);
    }

    #[test]
    fn runtime_fallback_records_effective_runtime() {
        let inputs = ProviderProbeInputs {
            cuda_available: true,
            cuda_detail: "ok".into(),
            openvino_available: true,
            openvino_detail: "ok".into(),
            migraphx_available: true,
            migraphx_detail: "ok".into(),
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let status = select_provider(
            AudioRuntimeMode::DspAuto,
            DspProviderPreference::Auto,
            &inputs,
        )
        .with_runtime_fallback(
            AudioRuntimeMode::PipewireFilterChain,
            "live helper graph unavailable",
        );

        assert_eq!(status.runtime, AudioRuntimeMode::DspAuto);
        assert_eq!(
            status.effective_runtime,
            AudioRuntimeMode::PipewireFilterChain
        );
        assert!(status.fallback_active);
        assert_eq!(status.fallback_count, 1);
        assert_eq!(
            status.runtime_fallback_reason.as_deref(),
            Some("live helper graph unavailable")
        );
        assert!(!status.accelerated);
    }

    #[test]
    fn canonical_control_paths_cover_every_standard_channel_and_mix() {
        let runtime = Path::new("/run/user/1000/wavelinux6");
        for channel_id in ["hardware_in", "music", "game", "browser", "chat", "system"] {
            assert_eq!(
                channel_control_socket(runtime, "wavelinux6", channel_id),
                runtime
                    .join("control")
                    .join(format!("wavelinux6-chain-{channel_id}.sock"))
            );
        }
        assert_eq!(
            mix_control_socket(runtime),
            runtime.join("control").join("wavelinux6-audio-core.sock")
        );
    }

    #[test]
    fn manifest_resolves_server_paths_from_its_runtime_root() {
        let runtime = "/run/user/1000/wavelinux6";
        let channel = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_fx_hardware_in_input",
            "wavelinux6-mic",
            Vec::new(),
        );
        let mut manifest =
            DspCoreManifest::new("test", vec![channel]).with_runtime_root(runtime.to_string());

        manifest.resolve_control_socket_paths().unwrap();

        assert_eq!(
            manifest.channels[0].control_socket_path.as_deref(),
            Some("/run/user/1000/wavelinux6/control/wavelinux6-chain-hardware_in.sock")
        );
        assert_eq!(
            manifest.control_socket_path.as_deref(),
            Some("/run/user/1000/wavelinux6/control/wavelinux6-audio-core.sock")
        );
        manifest.validate().unwrap();
    }

    #[test]
    fn meter_stream_protocol_round_trips_header_and_frame() {
        let slots = vec![
            MeterStreamSlot {
                kind: MeterStreamSlotKind::Channel,
                id: "hardware_in".into(),
            },
            MeterStreamSlot {
                kind: MeterStreamSlotKind::Mix,
                id: "stream".into(),
            },
        ];
        let header_bytes = encode_meter_stream_header(&slots).unwrap();
        let header = read_meter_stream_header(&mut std::io::Cursor::new(header_bytes)).unwrap();
        assert_eq!(header.slots, slots);
        assert_eq!(header.rate_hz, METER_STREAM_RATE_HZ);

        let samples = vec![
            MeterStreamSample {
                peak_left: 0.75,
                peak_right: 0.5,
                rms_left: 0.25,
                rms_right: 0.125,
            },
            MeterStreamSample {
                peak_left: f32::INFINITY,
                peak_right: -0.5,
                rms_left: 2.0,
                rms_right: 0.0,
            },
        ];
        let mut encoded = Vec::new();
        encode_meter_stream_frame_into(17, 42, &samples, &mut encoded).unwrap();
        let mut scratch = Vec::new();
        let frame = read_meter_stream_frame(
            &mut std::io::Cursor::new(encoded),
            slots.len(),
            &mut scratch,
        )
        .unwrap();
        assert_eq!(frame.sequence, 17);
        assert_eq!(frame.monotonic_nanos, 42);
        assert_eq!(frame.samples[0], samples[0]);
        assert_eq!(frame.samples[1].peak_left, 0.0);
        assert_eq!(frame.samples[1].peak_right, 0.0);
        assert_eq!(frame.samples[1].rms_left, 1.0);
    }

    #[test]
    fn adaptive_latency_requests_larger_pipewire_quanta_only_under_pressure() {
        assert_eq!(adaptive_pipewire_quantum_frames(28), 0);
        assert_eq!(adaptive_pipewire_quantum_frames(40), 512);
        assert_eq!(adaptive_pipewire_quantum_frames(60), 1024);
        assert_eq!(adaptive_pipewire_quantum_frames(120), 1024);
    }
}
