use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wavelinux_model::EffectInstance;

pub const AUDIO_RUNTIME_ENV: &str = "WAVELINUX_AUDIO_RUNTIME";
pub const DSP_PROVIDER_ENV: &str = "WAVELINUX_DSP_PROVIDER";
pub const DSP_FORCE_PROVIDER_FAIL_ENV: &str = "WAVELINUX_DSP_FORCE_PROVIDER_FAIL";
pub const DSP_CHANNEL_CONFIG_REVISION: &str = "wavelinux5-dsp-channel-v1";

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
            .unwrap_or(Self::PipewireFilterChain)
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
    Cpu,
}

impl DspProviderPreference {
    pub fn parse(value: &str) -> Option<Self> {
        match normalize_token(value).as_str() {
            "auto" => Some(Self::Auto),
            "cuda" | "nvidia" => Some(Self::Cuda),
            "openvino" | "intel" => Some(Self::OpenVino),
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
    PortableCpu,
    PureCpu,
}

impl DspProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::OpenVino => "openvino",
            Self::PortableCpu => "portable_cpu",
            Self::PureCpu => "pure_cpu",
        }
    }

    fn accelerated(self) -> bool {
        matches!(self, Self::Cuda | Self::OpenVino)
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
    pub portable_cpu_available: bool,
    pub portable_cpu_detail: String,
}

impl ProviderProbeInputs {
    pub fn detect() -> Self {
        let forced = forced_provider_failures();
        let cuda_detected = command_available("nvidia-smi")
            || readable_any(&[
                "/proc/driver/nvidia/version",
                "/usr/lib/x86_64-linux-gnu/libcuda.so.1",
                "/usr/lib64/libcuda.so.1",
                "/usr/lib/libcuda.so.1",
            ]);
        let openvino_detected = std::env::var_os("OPENVINO_DIR").is_some()
            || readable_any(&[
                "/usr/lib/x86_64-linux-gnu/libopenvino.so",
                "/usr/lib64/libopenvino.so",
                "/usr/lib/libopenvino.so",
            ]);

        let cuda_available = cuda_detected && !forced.contains(&DspProvider::Cuda);
        let openvino_available = openvino_detected && !forced.contains(&DspProvider::OpenVino);
        let portable_cpu_available = !forced.contains(&DspProvider::PortableCpu);

        Self {
            cuda_available,
            cuda_detail: if forced.contains(&DspProvider::Cuda) {
                "forced unavailable by WAVELINUX_DSP_FORCE_PROVIDER_FAIL".into()
            } else if cuda_detected {
                "NVIDIA CUDA runtime probe succeeded".into()
            } else {
                "nvidia-smi/libcuda probe did not find CUDA".into()
            },
            openvino_available,
            openvino_detail: if forced.contains(&DspProvider::OpenVino) {
                "forced unavailable by WAVELINUX_DSP_FORCE_PROVIDER_FAIL".into()
            } else if openvino_detected {
                "OpenVINO runtime probe succeeded".into()
            } else {
                "OPENVINO_DIR/libopenvino probe did not find OpenVINO".into()
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
        .filter(|probe| !probe.available)
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
    let fallback_active = selected != first_choice
        || (matches!(runtime, AudioRuntimeMode::DspAccelerated) && !selected.accelerated());
    let fallback_count = u32::from(fallback_active);

    DspBackendStatus {
        runtime,
        effective_runtime: runtime,
        requested_provider,
        selected_provider: Some(selected),
        accelerated: selected.accelerated(),
        fallback_active,
        fallback_count,
        runtime_fallback_reason: None,
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
        (AudioRuntimeMode::DspAccelerated, DspProviderPreference::Auto) => vec![
            DspProvider::Cuda,
            DspProvider::OpenVino,
            DspProvider::PortableCpu,
            DspProvider::PureCpu,
        ],
        _ => vec![
            DspProvider::Cuda,
            DspProvider::OpenVino,
            DspProvider::PortableCpu,
            DspProvider::PureCpu,
        ],
    }
}

fn provider_available(provider: DspProvider, inputs: &ProviderProbeInputs) -> bool {
    match provider {
        DspProvider::Cuda => inputs.cuda_available,
        DspProvider::OpenVino => inputs.openvino_available,
        DspProvider::PortableCpu => inputs.portable_cpu_available,
        DspProvider::PureCpu => true,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DspChannelConfig {
    pub revision: String,
    pub channel_id: String,
    pub channel_name: String,
    pub graph_prefix: String,
    pub property_prefix: String,
    pub app_name: String,
    pub input_node_name: String,
    pub output_node_name: String,
    pub sample_rate_hz: u32,
    pub latency_frames: u32,
    pub effects: Vec<EffectInstance>,
}

impl DspChannelConfig {
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
            channel_id: channel_id.into(),
            channel_name: channel_name.into(),
            graph_prefix: graph_prefix.into(),
            property_prefix: property_prefix.into(),
            app_name: app_name.into(),
            input_node_name: input_node_name.into(),
            output_node_name: output_node_name.into(),
            sample_rate_hz: 48_000,
            latency_frames: 256,
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

pub fn native_dsp_effect_supported(effect_id: &str) -> bool {
    matches!(
        effect_id,
        "deepfilternet" | "highpass" | "eq" | "compressor" | "gate" | "limiter"
    )
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

pub fn process_effect_chain_interleaved_stereo(
    effects: &[EffectInstance],
    sample_rate_hz: u32,
    interleaved: &mut [f32],
) -> ChainMetrics {
    DspChain::new(effects, sample_rate_hz).process_interleaved_stereo(interleaved)
}

#[derive(Debug, Clone)]
pub struct DspChain {
    nodes: Vec<DspNode>,
    sample_rate_hz: u32,
    fallback_count: u32,
}

impl DspChain {
    pub fn new(effects: &[EffectInstance], sample_rate_hz: u32) -> Self {
        let mut fallback_count = 0_u32;
        let nodes = effects
            .iter()
            .filter(|effect| !effect.bypassed)
            .filter_map(|effect| match DspNode::new(effect, sample_rate_hz) {
                Some(node) => Some(node),
                None => {
                    fallback_count = fallback_count.saturating_add(1);
                    None
                }
            })
            .collect();
        Self {
            nodes,
            sample_rate_hz,
            fallback_count,
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn process_interleaved_stereo(&mut self, interleaved: &mut [f32]) -> ChainMetrics {
        let started = Instant::now();
        let mut frame_timings = Vec::new();
        for node in &mut self.nodes {
            let effect_started = Instant::now();
            node.process(self.sample_rate_hz, interleaved);
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
            fallback_count: self.fallback_count,
        }
    }
}

#[derive(Debug, Clone)]
enum DspNode {
    Highpass(HighpassNode),
    Eq(EqNode),
    Compressor(CompressorNode),
    Gate(GateNode),
    Limiter(LimiterNode),
    DeepFilterNet(DeepFilterNetNode),
}

impl DspNode {
    fn new(effect: &EffectInstance, sample_rate_hz: u32) -> Option<Self> {
        match effect.effect_id.as_str() {
            "highpass" => Some(Self::Highpass(HighpassNode::new(effect, sample_rate_hz))),
            "eq" => Some(Self::Eq(EqNode::new(effect, sample_rate_hz))),
            "compressor" => Some(Self::Compressor(CompressorNode::new(
                effect,
                sample_rate_hz,
            ))),
            "gate" => Some(Self::Gate(GateNode::new(effect, sample_rate_hz))),
            "limiter" => Some(Self::Limiter(LimiterNode::new(effect))),
            "deepfilternet" => Some(Self::DeepFilterNet(DeepFilterNetNode::new(effect))),
            _ => None,
        }
    }

    fn process(&mut self, sample_rate_hz: u32, data: &mut [f32]) {
        match self {
            Self::Highpass(node) => node.process(data),
            Self::Eq(node) => node.process(data),
            Self::Compressor(node) => node.process(data),
            Self::Gate(node) => node.process(data),
            Self::Limiter(node) => node.process(data),
            Self::DeepFilterNet(node) => node.process(sample_rate_hz, data),
        }
    }
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
        for frame in data.chunks_exact_mut(2) {
            for (ch, sample) in frame.iter_mut().enumerate().take(2) {
                let x = *sample;
                let y = self.alpha * (self.prev_y[ch] + x - self.prev_x[ch]);
                *sample = y;
                self.prev_x[ch] = x;
                self.prev_y[ch] = y;
            }
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
        for (freq_key, gain_key, q) in [
            ("low_freq_hz", "low_gain_db", 0.8),
            ("mid_freq_hz", "mid_gain_db", 1.0),
            ("high_freq_hz", "high_gain_db", 0.7),
        ] {
            let gain = param(effect, gain_key, 0.0).clamp(-12.0, 12.0);
            if gain.abs() < 0.01 {
                continue;
            }
            let freq = param(effect, freq_key, 1000.0).clamp(20.0, sample_rate_hz as f32 * 0.45);
            bands.push([
                Biquad::peaking(sample_rate_hz as f32, freq, q, gain),
                Biquad::peaking(sample_rate_hz as f32, freq, q, gain),
            ]);
        }
        Self { bands }
    }

    fn process(&mut self, data: &mut [f32]) {
        for band in &mut self.bands {
            for frame in data.chunks_exact_mut(2) {
                frame[0] = band[0].process(frame[0]);
                frame[1] = band[1].process(frame[1]);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct CompressorNode {
    threshold_db: f32,
    ratio: f32,
    makeup: f32,
    attack: f32,
    release: f32,
    gain: f32,
}

impl CompressorNode {
    fn new(effect: &EffectInstance, sample_rate_hz: u32) -> Self {
        Self {
            threshold_db: param(effect, "threshold_db", -20.0).clamp(-60.0, 0.0),
            ratio: param(effect, "ratio", 4.0).clamp(1.0, 20.0),
            makeup: db_to_amp(param(effect, "makeup_gain_db", 0.0).clamp(0.0, 24.0)),
            attack: smoothing_coeff(param(effect, "attack_ms", 5.0), sample_rate_hz),
            release: smoothing_coeff(param(effect, "release_ms", 100.0), sample_rate_hz),
            gain: 1.0,
        }
    }

    fn process(&mut self, data: &mut [f32]) {
        for frame in data.chunks_exact_mut(2) {
            let level = frame[0].abs().max(frame[1].abs()).max(1.0e-9);
            let level_db = amp_to_db(level);
            let target_gain = if level_db > self.threshold_db {
                let compressed_db = self.threshold_db + (level_db - self.threshold_db) / self.ratio;
                db_to_amp(compressed_db - level_db)
            } else {
                1.0
            };
            let coeff = if target_gain < self.gain {
                self.attack
            } else {
                self.release
            };
            self.gain = coeff * self.gain + (1.0 - coeff) * target_gain;
            frame[0] *= self.gain * self.makeup;
            frame[1] *= self.gain * self.makeup;
        }
    }
}

#[derive(Debug, Clone)]
struct GateNode {
    threshold_db: f32,
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
            threshold_db: param(effect, "threshold_db", -35.0).clamp(-90.0, 0.0),
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
        for frame in data.chunks_exact_mut(2) {
            let level = frame[0].abs().max(frame[1].abs()).max(1.0e-9);
            let open = amp_to_db(level) >= self.threshold_db;
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

#[derive(Debug, Clone)]
struct DeepFilterNetNode {
    input_trim: f32,
    output_makeup: f32,
    threshold: f32,
    attenuation: f32,
}

impl DeepFilterNetNode {
    fn new(effect: &EffectInstance) -> Self {
        Self {
            input_trim: db_to_amp(param(effect, "input_trim_db", -6.0).clamp(-24.0, 0.0)),
            output_makeup: db_to_amp(param(effect, "output_makeup_db", 6.0).clamp(0.0, 18.0)),
            threshold: db_to_amp(param(effect, "min_processing_threshold_db", -15.0)),
            attenuation: db_to_amp(-param(effect, "attenuation_limit_db", 18.0).clamp(0.0, 100.0)),
        }
    }

    fn process(&self, sample_rate_hz: u32, data: &mut [f32]) {
        for sample in data.iter_mut() {
            *sample *= self.input_trim;
        }

        #[cfg(feature = "deep-filter")]
        deep_filter_identity_pass(sample_rate_hz, data);

        for frame in data.chunks_exact_mut(2) {
            let level = frame[0].abs().max(frame[1].abs());
            if level < self.threshold {
                frame[0] *= self.attenuation;
                frame[1] *= self.attenuation;
            }
            frame[0] *= self.output_makeup;
            frame[1] *= self.output_makeup;
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
            "deepfilternet" => apply_deepfilternet_cpu(effect, sample_rate_hz, interleaved),
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
                ("low_freq_hz", 120.0),
                ("low_gain_db", -2.0),
                ("mid_freq_hz", 1200.0),
                ("mid_gain_db", 2.5),
                ("high_freq_hz", 6500.0),
                ("high_gain_db", 1.5),
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
    for (freq_key, gain_key, q) in [
        ("low_freq_hz", "low_gain_db", 0.8),
        ("mid_freq_hz", "mid_gain_db", 1.0),
        ("high_freq_hz", "high_gain_db", 0.7),
    ] {
        let gain = param(effect, gain_key, 0.0).clamp(-12.0, 12.0);
        if gain.abs() < 0.01 {
            continue;
        }
        let freq = param(effect, freq_key, 1000.0).clamp(20.0, sample_rate_hz as f32 * 0.45);
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

#[cfg(test)]
fn apply_deepfilternet_cpu(effect: &EffectInstance, sample_rate_hz: u32, data: &mut [f32]) {
    let input_trim = db_to_amp(param(effect, "input_trim_db", -6.0).clamp(-24.0, 0.0));
    let output_makeup = db_to_amp(param(effect, "output_makeup_db", 6.0).clamp(0.0, 18.0));
    for sample in data.iter_mut() {
        *sample *= input_trim;
    }

    #[cfg(feature = "deep-filter")]
    deep_filter_identity_pass(sample_rate_hz, data);

    let threshold = db_to_amp(param(effect, "min_processing_threshold_db", -15.0));
    let attenuation = db_to_amp(-param(effect, "attenuation_limit_db", 18.0).clamp(0.0, 100.0));
    for frame in data.chunks_exact_mut(2) {
        let level = frame[0].abs().max(frame[1].abs());
        if level < threshold {
            frame[0] *= attenuation;
            frame[1] *= attenuation;
        }
        frame[0] *= output_makeup;
        frame[1] *= output_makeup;
    }
}

#[cfg(feature = "deep-filter")]
fn deep_filter_identity_pass(sample_rate_hz: u32, data: &mut [f32]) {
    let frame_size = (sample_rate_hz / 100).max(160) as usize;
    let fft_size = frame_size * 2;
    let mut left_state = df::DFState::new(sample_rate_hz as usize, fft_size, frame_size, 32, 1);
    let mut right_state = df::DFState::new(sample_rate_hz as usize, fft_size, frame_size, 32, 1);
    let mut left_in = vec![0.0_f32; frame_size];
    let mut right_in = vec![0.0_f32; frame_size];
    let mut left_out = vec![0.0_f32; frame_size];
    let mut right_out = vec![0.0_f32; frame_size];
    let frames = frame_count(data);
    let mut offset = 0;
    while offset < frames {
        left_in.fill(0.0);
        right_in.fill(0.0);
        let count = frame_size.min(frames - offset);
        for idx in 0..count {
            left_in[idx] = data[(offset + idx) * 2];
            right_in[idx] = data[(offset + idx) * 2 + 1];
        }
        left_state.process_frame(&left_in, &mut left_out);
        right_state.process_frame(&right_in, &mut right_out);
        for idx in 0..count {
            data[(offset + idx) * 2] = left_out[idx];
            data[(offset + idx) * 2 + 1] = right_out[idx];
        }
        offset += count;
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
            "portable_cpu" | "simd_cpu" => Some(DspProvider::PortableCpu),
            "pure_cpu" | "cpu" => Some(DspProvider::PureCpu),
            _ => None,
        })
        .collect()
}

fn readable_any(paths: &[&str]) -> bool {
    paths.iter().any(|path| Path::new(path).exists())
}

fn command_available(command: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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

fn percentile(mut values: Vec<f32>, percentile: f32) -> f32 {
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

    #[test]
    fn dsp_channel_config_tracks_wavelinux5_namespace() {
        let config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux5",
            "wavelinux5",
            "WaveLinux5",
            "wavelinux5_fx_hardware_in_input",
            "wavelinux5-mic",
            vec![effect("highpass", &[("frequency_hz", 90.0)])],
        );

        assert_eq!(config.revision, DSP_CHANNEL_CONFIG_REVISION);
        assert_eq!(config.input_node_name, "wavelinux5_fx_hardware_in_input");
        assert_eq!(config.output_node_name, "wavelinux5-mic");
        assert!(config.unsupported_active_effects().is_empty());
    }

    #[test]
    fn dsp_channel_config_reports_unsupported_native_effects() {
        let config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux5",
            "wavelinux5",
            "WaveLinux5",
            "wavelinux5_fx_hardware_in_input",
            "wavelinux5-mic",
            vec![effect("rnnoise", &[])],
        );

        assert_eq!(config.unsupported_active_effects(), vec!["rnnoise"]);
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
    fn eq_gain_changes_signal_without_nan() {
        let mut data = sine(4800, 1000.0, 48_000, 0.2);
        let before = rms(&data);
        apply_eq(
            &effect(
                "eq",
                &[
                    ("low_freq_hz", 120.0),
                    ("low_gain_db", 0.0),
                    ("mid_freq_hz", 1000.0),
                    ("mid_gain_db", 6.0),
                    ("high_freq_hz", 6000.0),
                    ("high_gain_db", 0.0),
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
            portable_cpu_available: true,
            portable_cpu_detail: "simd".into(),
        };
        let status = select_provider(
            AudioRuntimeMode::DspAccelerated,
            DspProviderPreference::Cuda,
            &inputs,
        );
        assert_eq!(status.effective_runtime, AudioRuntimeMode::DspAccelerated);
        assert_eq!(status.selected_provider, Some(DspProvider::PortableCpu));
        assert!(status.fallback_active);
        assert_eq!(status.fallback_count, 1);
    }

    #[test]
    fn provider_selection_prefers_cuda_when_available() {
        let inputs = ProviderProbeInputs {
            cuda_available: true,
            cuda_detail: "ok".into(),
            openvino_available: true,
            openvino_detail: "ok".into(),
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
    fn runtime_fallback_records_effective_runtime() {
        let inputs = ProviderProbeInputs {
            cuda_available: true,
            cuda_detail: "ok".into(),
            openvino_available: true,
            openvino_detail: "ok".into(),
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
}
