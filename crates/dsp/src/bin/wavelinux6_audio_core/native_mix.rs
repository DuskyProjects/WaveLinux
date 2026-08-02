use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::mem;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::{properties::properties, spa};
use serde::Deserialize;
use spa::pod::Pod;
use wavelinux_dsp::{
    encode_meter_stream_frame_into, encode_meter_stream_header, DspMixBusConfig, DspMixConfig,
    MeterStreamSample, MeterStreamSlot, MeterStreamSlotKind, CORE_CONTROL_PROTOCOL_VERSION,
    MAX_MIX_OUTPUT_TARGETS, METER_STREAM_RATE_HZ,
};

use super::{
    audio_format_pod_bytes, msec_to_frames, playback_render_frames, LatencyTransition, NativeMeter,
    NativeMeterSnapshot, NativeShared, RealtimeTimingStats, CONTROL_MAINLOOP_POLL_INTERVAL,
    LATENCY_CROSSFADE_MSEC, MAX_NATIVE_CALLBACK_FRAMES, TERMINATE,
};

const GAIN_SMOOTH_MSEC: f32 = 5.0;
const EXACT_MIX_METER_MAX_AGE_MICROS: u64 = 250_000;
const OUTPUT_TARGET_CROSSFADE_MSEC: u64 = 20;
const OUTPUT_TARGET_PRIME_TIMEOUT: Duration = Duration::from_millis(750);
const MIX_OUTPUT_SLOT_COUNT: usize = MAX_MIX_OUTPUT_TARGETS * 2;

#[derive(Debug)]
struct AtomicGain(AtomicU32);

impl AtomicGain {
    fn new(value: f32) -> Self {
        Self(AtomicU32::new(normalized_gain(value).to_bits()))
    }

    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn store(&self, value: f32) {
        self.0
            .store(normalized_gain(value).to_bits(), Ordering::Relaxed);
    }
}

fn normalized_gain(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 2.0)
    } else {
        1.0
    }
}

#[derive(Debug)]
struct NativeMixBusShared {
    channel_id: String,
    volume: AtomicGain,
    muted: AtomicBool,
    enabled: AtomicBool,
}

impl NativeMixBusShared {
    fn new(config: &DspMixBusConfig) -> Self {
        Self {
            channel_id: config.channel_id.clone(),
            volume: AtomicGain::new(config.volume),
            muted: AtomicBool::new(config.muted),
            enabled: AtomicBool::new(config.enabled),
        }
    }

    fn target_gain(&self) -> f32 {
        if self.enabled.load(Ordering::Relaxed) && !self.muted.load(Ordering::Relaxed) {
            self.volume.load()
        } else {
            0.0
        }
    }
}

#[derive(Debug, Default)]
struct NativeMixStats {
    rendered_frames: AtomicU64,
    underrun_frames: AtomicU64,
    non_finite_blocks: AtomicU64,
    non_finite_samples: AtomicU64,
    process_calls: AtomicU64,
    last_process_micros: AtomicU64,
    max_process_micros: AtomicU64,
    rate_correction_bits: AtomicU64,
    pipewire_rate_match_callbacks: AtomicU64,
    software_rate_match_callbacks: AtomicU64,
    rt_callback_timing: RealtimeTimingStats,
}

#[derive(Debug)]
struct PendingOutputTargets {
    generation: u64,
    targets: Vec<String>,
}

#[derive(Debug)]
struct OutputTargetControl {
    submitted_generation: AtomicU64,
    applied_generation: AtomicU64,
    current_targets: Mutex<Vec<String>>,
    pending: Mutex<Option<PendingOutputTargets>>,
    last_error: Mutex<Option<String>>,
}

impl OutputTargetControl {
    fn new(initial_targets: Vec<String>) -> Self {
        Self {
            submitted_generation: AtomicU64::new(1),
            applied_generation: AtomicU64::new(1),
            current_targets: Mutex::new(initial_targets),
            pending: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    fn queue(
        &self,
        targets: Vec<String>,
        requested_generation: Option<u64>,
    ) -> Result<u64, String> {
        let targets = normalize_output_targets(targets)?;
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "output target queue lock poisoned".to_string())?;
        if let Some(existing) = pending
            .as_ref()
            .filter(|pending| pending.targets == targets)
        {
            return Ok(existing.generation);
        }
        let current_matches = *self
            .current_targets
            .lock()
            .map_err(|_| "output target state lock poisoned".to_string())?
            == targets;
        if current_matches && pending.is_none() {
            if let Ok(mut error) = self.last_error.lock() {
                *error = None;
            }
            return Ok(self.applied_generation.load(Ordering::Acquire));
        }
        let submitted = self.submitted_generation.load(Ordering::Acquire);
        let generation = requested_generation.unwrap_or_else(|| submitted.saturating_add(1));
        if generation <= submitted {
            return Err(format!(
                "stale route generation {generation}; latest submitted generation is {submitted}"
            ));
        }
        *pending = Some(PendingOutputTargets {
            generation,
            targets,
        });
        self.submitted_generation
            .store(generation, Ordering::Release);
        if let Ok(mut error) = self.last_error.lock() {
            *error = None;
        }
        Ok(generation)
    }

    fn take_pending(&self) -> Option<PendingOutputTargets> {
        self.pending.lock().ok()?.take()
    }

    fn acknowledge(&self, request: &PendingOutputTargets) {
        if let Ok(mut current) = self.current_targets.lock() {
            *current = request.targets.clone();
        }
        if let Ok(mut error) = self.last_error.lock() {
            *error = None;
        }
        self.applied_generation
            .store(request.generation, Ordering::Release);
    }

    fn reject(&self, error: String) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error);
        }
    }

    fn current_targets(&self) -> Vec<String> {
        self.current_targets
            .lock()
            .map(|targets| targets.clone())
            .unwrap_or_default()
    }

    fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok()?.clone()
    }
}

fn normalize_output_targets(targets: Vec<String>) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for target in targets {
        let target = target.trim();
        if target.is_empty() {
            return Err("output target must not be empty".into());
        }
        if !normalized.iter().any(|existing| existing == target) {
            normalized.push(target.to_string());
        }
    }
    if normalized.len() > MAX_MIX_OUTPUT_TARGETS {
        return Err(format!(
            "{} output targets requested; at most {} are supported",
            normalized.len(),
            MAX_MIX_OUTPUT_TARGETS
        ));
    }
    Ok(normalized)
}

#[derive(Debug)]
pub(super) struct NativeMixShared {
    mix_id: String,
    sample_rate_hz: u32,
    render_quantum_frames: usize,
    volume: AtomicGain,
    muted: AtomicBool,
    target_latency_msec: AtomicUsize,
    target_latency_frames: AtomicUsize,
    requested_quantum_frames: AtomicUsize,
    applied_quantum_frames: AtomicUsize,
    applied_target_latency_frames: AtomicUsize,
    current_buffer_frames: AtomicU64,
    min_latency_msec: u16,
    max_latency_msec: u16,
    buses: Vec<Arc<NativeMixBusShared>>,
    meter: NativeMeter,
    stats: NativeMixStats,
    output_target_control: OutputTargetControl,
}

impl NativeMixShared {
    fn new(config: &DspMixConfig) -> Self {
        let min_msec = config
            .adaptive_latency
            .min_msec
            .min(config.adaptive_latency.max_msec)
            .max(5);
        let max_msec = config.adaptive_latency.max_msec.max(min_msec).min(500);
        let configured_msec =
            frames_to_msec(config.latency_frames, config.sample_rate_hz).clamp(min_msec, max_msec);
        Self {
            mix_id: config.mix_id.clone(),
            sample_rate_hz: config.sample_rate_hz,
            render_quantum_frames: config.latency_frames.max(1) as usize,
            volume: AtomicGain::new(config.volume),
            muted: AtomicBool::new(config.muted),
            target_latency_msec: AtomicUsize::new(configured_msec as usize),
            target_latency_frames: AtomicUsize::new(msec_to_frames(
                configured_msec,
                config.sample_rate_hz,
            )),
            requested_quantum_frames: AtomicUsize::new(config.pipewire_quantum_frames.max(
                wavelinux_dsp::adaptive_pipewire_quantum_frames(configured_msec),
            ) as usize),
            applied_quantum_frames: AtomicUsize::new(usize::MAX),
            applied_target_latency_frames: AtomicUsize::new(usize::MAX),
            current_buffer_frames: AtomicU64::new(0),
            min_latency_msec: min_msec,
            max_latency_msec: max_msec,
            buses: config
                .buses
                .iter()
                .map(|bus| Arc::new(NativeMixBusShared::new(bus)))
                .collect(),
            meter: NativeMeter::default(),
            stats: NativeMixStats::default(),
            output_target_control: OutputTargetControl::new(
                config.output_target_node_names.clone(),
            ),
        }
    }

    fn target_master_gain(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            self.volume.load()
        }
    }

    fn set_target_latency(&self, target_msec: u16) {
        let target_msec = target_msec.clamp(self.min_latency_msec, self.max_latency_msec);
        self.target_latency_msec
            .store(target_msec as usize, Ordering::Relaxed);
        self.target_latency_frames.store(
            msec_to_frames(target_msec, self.sample_rate_hz),
            Ordering::Relaxed,
        );
        self.requested_quantum_frames.store(
            wavelinux_dsp::adaptive_pipewire_quantum_frames(target_msec) as usize,
            Ordering::Release,
        );
    }

    fn set_requested_quantum(&self, quantum_frames: u32) -> Result<(), String> {
        if !wavelinux_dsp::valid_pipewire_quantum_frames(quantum_frames) {
            return Err(format!(
                "pipewire quantum must be zero or a power of two between 64 and 8192; got {quantum_frames}"
            ));
        }
        self.requested_quantum_frames
            .store(quantum_frames as usize, Ordering::Release);
        Ok(())
    }

    fn bus(&self, channel_id: &str) -> Option<&NativeMixBusShared> {
        self.buses
            .iter()
            .find(|bus| bus.channel_id == channel_id)
            .map(Arc::as_ref)
    }
}

fn frames_to_msec(frames: u32, sample_rate_hz: u32) -> u16 {
    if sample_rate_hz == 0 {
        return 28;
    }
    ((u64::from(frames) * 1000) / u64::from(sample_rate_hz)).clamp(5, 500) as u16
}

struct NativeMixInput {
    shared: Arc<NativeShared>,
    bus: Arc<NativeMixBusShared>,
    current_gain: f32,
    last_frame: [f32; 2],
    read_sequence: Option<u64>,
    read_fraction: f64,
    rate_correction: f64,
    applied_target_frames: usize,
    transition: Option<LatencyTransition>,
}

impl NativeMixInput {
    fn new(shared: Arc<NativeShared>, bus: Arc<NativeMixBusShared>) -> Self {
        Self {
            shared,
            bus,
            current_gain: 0.0,
            last_frame: [0.0, 0.0],
            read_sequence: None,
            read_fraction: 0.0,
            rate_correction: 1.0,
            applied_target_frames: 0,
            transition: None,
        }
    }

    fn reset(&mut self) {
        self.read_sequence = None;
        self.read_fraction = 0.0;
        self.rate_correction = 1.0;
        self.applied_target_frames = 0;
        self.transition = None;
    }

    fn render(&mut self, target_frames: usize, underruns: &mut u64) -> [f32; 2] {
        if !self.shared.capture_streaming.load(Ordering::Acquire) {
            self.reset();
            self.last_frame[0] *= 0.98;
            self.last_frame[1] *= 0.98;
            return self.last_frame;
        }

        let latest = self.shared.history.write_sequence();
        if self.read_sequence.is_none() {
            if latest < target_frames as u64 {
                return [0.0, 0.0];
            }
            self.read_sequence = Some(latest - target_frames as u64);
            self.read_fraction = 0.0;
            self.applied_target_frames = target_frames;
        }

        if target_frames != self.applied_target_frames && self.transition.is_none() {
            let from_sequence = self.read_sequence.unwrap_or_default();
            let to_sequence = self.shared.history.aligned_latency_sequence(
                from_sequence,
                latest.saturating_sub(target_frames as u64),
                self.read_fraction,
            );
            if self.shared.history.get(from_sequence).is_some()
                && self.shared.history.get(to_sequence).is_some()
            {
                self.transition = Some(LatencyTransition {
                    from_sequence,
                    to_sequence,
                    from_fraction: self.read_fraction,
                    to_fraction: self.read_fraction,
                    progress_frames: 0,
                    total_frames: (self.shared.sample_rate_hz as usize * LATENCY_CROSSFADE_MSEC
                        / 1000)
                        .max(1),
                });
            } else {
                self.read_sequence = Some(to_sequence);
                self.read_fraction = 0.0;
                self.applied_target_frames = target_frames;
            }
        }

        let frame = if let Some(mut transition) = self.transition {
            let from_sequence = transition
                .from_sequence
                .saturating_add(transition.progress_frames as u64);
            let to_sequence = transition
                .to_sequence
                .saturating_add(transition.progress_frames as u64);
            match (
                self.shared
                    .history
                    .get_interpolated(from_sequence, transition.from_fraction),
                self.shared
                    .history
                    .get_interpolated(to_sequence, transition.to_fraction),
            ) {
                (Some(from), Some(to)) => {
                    let phase = (transition.progress_frames + 1) as f32
                        / transition.total_frames.max(1) as f32;
                    let old_gain = (phase * std::f32::consts::FRAC_PI_2).cos();
                    let new_gain = (phase * std::f32::consts::FRAC_PI_2).sin();
                    let normalization = (old_gain + new_gain).max(1.0);
                    let mixed = [
                        (from[0] * old_gain + to[0] * new_gain) / normalization,
                        (from[1] * old_gain + to[1] * new_gain) / normalization,
                    ];
                    transition.progress_frames += 1;
                    if transition.progress_frames >= transition.total_frames {
                        self.read_sequence = Some(
                            transition
                                .to_sequence
                                .saturating_add(transition.total_frames as u64),
                        );
                        self.read_fraction = transition.to_fraction;
                        self.applied_target_frames = target_frames;
                        self.transition = None;
                    } else {
                        self.transition = Some(transition);
                    }
                    mixed
                }
                _ => {
                    *underruns = underruns.saturating_add(1);
                    self.transition = None;
                    self.read_sequence = Some(latest.saturating_sub(target_frames as u64));
                    self.read_fraction = 0.0;
                    self.rate_correction = 1.0;
                    [self.last_frame[0] * 0.98, self.last_frame[1] * 0.98]
                }
            }
        } else {
            let sequence = self.read_sequence.unwrap_or_default();
            match self.shared.history.get(sequence) {
                Some(first) => {
                    let frame = if self.read_fraction <= f64::EPSILON {
                        first
                    } else {
                        let second = self
                            .shared
                            .history
                            .get(sequence.saturating_add(1))
                            .unwrap_or(first);
                        let fraction = self.read_fraction as f32;
                        [
                            first[0] + (second[0] - first[0]) * fraction,
                            first[1] + (second[1] - first[1]) * fraction,
                        ]
                    };
                    self.read_fraction += self.rate_correction;
                    let advance = self.read_fraction.floor() as u64;
                    self.read_fraction -= advance as f64;
                    self.read_sequence = Some(sequence.saturating_add(advance));
                    frame
                }
                None => {
                    *underruns = underruns.saturating_add(1);
                    self.read_sequence = Some(latest.saturating_sub(target_frames as u64));
                    self.read_fraction = 0.0;
                    self.rate_correction = 1.0;
                    [self.last_frame[0] * 0.98, self.last_frame[1] * 0.98]
                }
            }
        };
        self.last_frame = frame;
        frame
    }
}

struct NativeMixPlaybackData {
    shared: Arc<NativeMixShared>,
    inputs: Vec<NativeMixInput>,
    current_master_gain: f32,
    endpoint_gain: Option<Arc<AtomicGain>>,
    endpoint_status: Option<Arc<OutputEndpointStatus>>,
    current_endpoint_phase: f32,
    endpoint_fade_step: f32,
    gain_smoothing: f32,
    rate_match: *mut spa::sys::spa_io_rate_match,
    rate_correction: f64,
}

#[derive(Debug, Default)]
struct OutputEndpointStatus {
    connected: AtomicBool,
    streaming: AtomicBool,
    failed: AtomicBool,
    processed_frames: AtomicU64,
}

impl OutputEndpointStatus {
    fn reset(&self) {
        self.connected.store(false, Ordering::Release);
        self.streaming.store(false, Ordering::Release);
        self.failed.store(false, Ordering::Release);
        self.processed_frames.store(0, Ordering::Release);
    }

    fn observe_state(&self, state: &pw::stream::StreamState) {
        self.connected.store(
            matches!(
                state,
                pw::stream::StreamState::Paused | pw::stream::StreamState::Streaming
            ),
            Ordering::Release,
        );
        self.streaming.store(
            matches!(state, pw::stream::StreamState::Streaming),
            Ordering::Release,
        );
        self.failed.store(
            matches!(state, pw::stream::StreamState::Error(_)),
            Ordering::Release,
        );
    }
}

struct NativeMixOutputSlot {
    stream_index: usize,
    gain: Arc<AtomicGain>,
    status: Arc<OutputEndpointStatus>,
    target: Option<String>,
    disconnect_at: Option<Instant>,
}

#[derive(Debug)]
enum OutputTargetTransitionState {
    Priming { started_at: Instant },
    Fading { finish_at: Instant },
}

#[derive(Debug)]
struct StagedOutputTargets {
    request: PendingOutputTargets,
    prime_slots: Vec<(usize, u64)>,
    state: OutputTargetTransitionState,
}

pub(super) struct NativeMixRuntime<'core> {
    _listeners: Vec<pw::stream::StreamListener<NativeMixPlaybackData>>,
    _streams: Vec<pw::stream::StreamBox<'core>>,
    pub(super) shared: Arc<NativeMixShared>,
    output_slots: Vec<NativeMixOutputSlot>,
    staged_output_targets: Option<StagedOutputTargets>,
    config: DspMixConfig,
}

struct NativeMixStreamSpec {
    stream_name: String,
    kind: String,
    target: Option<String>,
    endpoint_gain: Option<Arc<AtomicGain>>,
    endpoint_status: Option<Arc<OutputEndpointStatus>>,
    properties: pw::properties::PropertiesBox,
}

fn native_mix_inputs(
    shared: &Arc<NativeMixShared>,
    channels: &BTreeMap<String, Arc<NativeShared>>,
    mix_id: &str,
) -> Result<Vec<NativeMixInput>, String> {
    shared
        .buses
        .iter()
        .map(|bus| {
            let channel = channels.get(&bus.channel_id).ok_or_else(|| {
                format!(
                    "native mix {mix_id} references missing channel {}",
                    bus.channel_id
                )
            })?;
            Ok(NativeMixInput::new(Arc::clone(channel), Arc::clone(bus)))
        })
        .collect()
}

fn native_mix_public_source_properties(config: &DspMixConfig) -> pw::properties::PropertiesBox {
    let mut props = native_mix_base_properties(
        config,
        &config.output_node_name,
        &format!("{} {}", config.app_name, config.mix_name),
        "Audio/Source",
        "mix_source",
        &format!("{}-audio-core-mix-{}", config.graph_prefix, config.mix_id),
    );
    props.insert(*pw::keys::NODE_VIRTUAL, "true");
    props
}

fn native_mix_output_slot_properties(
    config: &DspMixConfig,
    target: Option<&str>,
    index: usize,
) -> pw::properties::PropertiesBox {
    let node_name = format!(
        "{}-output-target-{}-{index}",
        config.graph_prefix, config.mix_id
    );
    let mut props = native_mix_base_properties(
        config,
        &node_name,
        &format!("{} {} output", config.app_name, config.mix_name),
        "Stream/Output/Audio",
        "mix_output_target",
        &format!(
            "{}-audio-core-mix-{}-target-{index}",
            config.graph_prefix, config.mix_id
        ),
    );
    props.insert("node.dont-fallback", "true");
    props.insert("node.linger", "true");
    props.insert("node.hidden", "true");
    if let Some(target) = target {
        props.insert(*pw::keys::TARGET_OBJECT, target);
        props.insert(format!("{}.target_node", config.property_prefix), target);
    }
    props
}

fn native_mix_base_properties(
    config: &DspMixConfig,
    node_name: &str,
    description: &str,
    media_class: &str,
    role: &str,
    restore_id: &str,
) -> pw::properties::PropertiesBox {
    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_CLASS => media_class,
        *pw::keys::NODE_NAME => node_name,
        *pw::keys::NODE_DESCRIPTION => description,
        *pw::keys::NODE_NICK => description,
        *pw::keys::MEDIA_NAME => description,
    };
    props.insert("application.name", config.app_name.clone());
    props.insert("audio.rate", config.sample_rate_hz.to_string());
    props.insert("audio.channels", "2");
    props.insert("audio.position", "FL,FR");
    props.insert(
        "node.latency",
        format!("{}/{}", config.latency_frames, config.sample_rate_hz),
    );
    props.insert(*pw::keys::NODE_PAUSE_ON_IDLE, "true");
    props.insert("node.dont-move", "true");
    props.insert("state.restore-props", "false");
    props.insert("state.restore-target", "false");
    props.insert("module-stream-restore.id", restore_id);
    props.insert(format!("{}.managed", config.property_prefix), "1");
    props.insert(format!("{}.role", config.property_prefix), role);
    props.insert(
        format!("{}.mix_id", config.property_prefix),
        config.mix_id.clone(),
    );
    props
}

pub(super) fn prepare_native_mix<'core>(
    core: &'core pw::core::Core,
    config: DspMixConfig,
    channels: &BTreeMap<String, Arc<NativeShared>>,
) -> Result<NativeMixRuntime<'core>, String> {
    let shared = Arc::new(NativeMixShared::new(&config));
    let mut specs = vec![NativeMixStreamSpec {
        stream_name: format!("{}-mix-{}", config.graph_prefix, config.mix_id),
        kind: "public_source".into(),
        target: None,
        endpoint_gain: None,
        endpoint_status: None,
        properties: native_mix_public_source_properties(&config),
    }];
    specs.extend((0..MIX_OUTPUT_SLOT_COUNT).map(|index| {
        let target = config.output_target_node_names.get(index).cloned();
        let endpoint_gain = Arc::new(AtomicGain::new(if target.is_some() { 1.0 } else { 0.0 }));
        let endpoint_status = Arc::new(OutputEndpointStatus::default());
        NativeMixStreamSpec {
            stream_name: format!(
                "{}-mix-output-{}-{index}",
                config.graph_prefix, config.mix_id
            ),
            kind: "output_target".into(),
            properties: native_mix_output_slot_properties(&config, target.as_deref(), index),
            target,
            endpoint_gain: Some(endpoint_gain),
            endpoint_status: Some(endpoint_status),
        }
    }));

    let mut streams = Vec::with_capacity(specs.len());
    let mut listeners = Vec::with_capacity(specs.len());
    let mut output_slots = Vec::with_capacity(MIX_OUTPUT_SLOT_COUNT);
    for spec in specs {
        let stream_index = streams.len();
        let stream = pw::stream::StreamBox::new(core, &spec.stream_name, spec.properties)
            .map_err(|err| format!("PipeWire native mix stream creation failed: {err}"))?;
        let initial_endpoint_gain = spec
            .endpoint_gain
            .as_ref()
            .map(|gain| gain.load())
            .unwrap_or(1.0);
        let endpoint_status = spec.endpoint_status.as_ref().map(Arc::clone);
        let data = NativeMixPlaybackData {
            shared: Arc::clone(&shared),
            inputs: native_mix_inputs(&shared, channels, &config.mix_id)?,
            current_master_gain: 0.0,
            endpoint_gain: spec.endpoint_gain.as_ref().map(Arc::clone),
            endpoint_status: endpoint_status.as_ref().map(Arc::clone),
            current_endpoint_phase: initial_endpoint_gain,
            endpoint_fade_step: 1.0
                / (config.sample_rate_hz as f32 * OUTPUT_TARGET_CROSSFADE_MSEC as f32 / 1000.0)
                    .max(1.0),
            gain_smoothing: 1.0
                - (-1.0 / (config.sample_rate_hz as f32 * GAIN_SMOOTH_MSEC / 1000.0).max(1.0))
                    .exp(),
            rate_match: std::ptr::null_mut(),
            rate_correction: 1.0,
        };
        let mix_id = config.mix_id.clone();
        let kind = spec.kind;
        let target_label = spec.target.clone().unwrap_or_else(|| "<clients>".into());
        let listener = stream
            .add_local_listener_with_user_data(data)
            .state_changed(move |_, user_data, old, new| {
                if let Some(status) = user_data.endpoint_status.as_ref() {
                    status.observe_state(&new);
                }
                eprintln!(
                    "wavelinux6-audio-core native_mix_state mix_id={} kind={} target={} {:?}->{:?}",
                    mix_id, kind, target_label, old, new
                );
            })
            .io_changed(|_, user_data, id, area, size| {
                if id == spa::sys::SPA_IO_RateMatch
                    && !area.is_null()
                    && size as usize >= mem::size_of::<spa::sys::spa_io_rate_match>()
                {
                    user_data.rate_match = area.cast();
                } else if id == spa::sys::SPA_IO_RateMatch {
                    user_data.rate_match = std::ptr::null_mut();
                }
            })
            .process(process_mix_buffer)
            .register()
            .map_err(|err| format!("PipeWire native mix listener failed: {err}"))?;
        let format = audio_format_pod_bytes(config.sample_rate_hz)?;
        let mut params = [Pod::from_bytes(&format)
            .ok_or_else(|| "native mix format pod was invalid".to_string())?];
        if spec.endpoint_gain.is_none() || spec.target.is_some() {
            stream
                .connect(
                    spa::utils::Direction::Output,
                    None,
                    super::native_stream_flags(),
                    &mut params,
                )
                .map_err(|err| format!("PipeWire native mix connect failed: {err}"))?;
        }
        if let (Some(gain), Some(status)) = (spec.endpoint_gain, endpoint_status) {
            output_slots.push(NativeMixOutputSlot {
                stream_index,
                gain,
                status,
                target: spec.target,
                disconnect_at: None,
            });
        }
        listeners.push(listener);
        streams.push(stream);
    }

    Ok(NativeMixRuntime {
        _listeners: listeners,
        _streams: streams,
        shared,
        output_slots,
        staged_output_targets: None,
        config,
    })
}

impl NativeMixRuntime<'_> {
    pub(super) fn apply_pending_latency_quantum(&mut self) {
        let requested = self.shared.requested_quantum_frames.load(Ordering::Acquire);
        let target_frames = self.shared.target_latency_frames.load(Ordering::Acquire);
        if !output_stream_timing_update_pending(
            self.shared.applied_quantum_frames.load(Ordering::Acquire),
            self.shared
                .applied_target_latency_frames
                .load(Ordering::Acquire),
            requested,
            target_frames,
        ) {
            return;
        }

        let mut failed = false;
        for slot in &self.output_slots {
            let stream = &self._streams[slot.stream_index];
            if let Err(error) = update_output_stream_latency_properties(
                stream,
                requested,
                target_frames,
                self.config.sample_rate_hz,
            ) {
                failed = true;
                eprintln!(
                    "wavelinux6-audio-core output_quantum_failed mix_id={} quantum_frames={} error={}",
                    self.shared.mix_id, requested, error
                );
            }
        }
        if !failed {
            self.shared
                .applied_quantum_frames
                .store(requested, Ordering::Release);
            self.shared
                .applied_target_latency_frames
                .store(target_frames, Ordering::Release);
            eprintln!(
                "wavelinux6-audio-core output_quantum_applied mix_id={} quantum_frames={} target_latency_msec={}",
                self.shared.mix_id,
                requested,
                self.shared.target_latency_msec.load(Ordering::Relaxed),
            );
        }
    }

    pub(super) fn apply_pending_output_targets(&mut self) {
        if let Some(request) = self.shared.output_target_control.take_pending() {
            self.finish_or_cancel_staged_output_targets();
            match self.prepare_output_targets(&request.targets) {
                Ok(prime_slots) => {
                    eprintln!(
                        "wavelinux6-audio-core output_targets_priming mix_id={} generation={} targets={} prime_slots={}",
                        self.shared.mix_id,
                        request.generation,
                        display_targets(&request.targets),
                        prime_slots.len(),
                    );
                    self.staged_output_targets = Some(StagedOutputTargets {
                        request,
                        prime_slots,
                        state: OutputTargetTransitionState::Priming {
                            started_at: Instant::now(),
                        },
                    });
                }
                Err(error) => self.reject_output_targets(&request, error),
            }
            return;
        }

        let Some(mut staged) = self.staged_output_targets.take() else {
            return;
        };
        match staged.state {
            OutputTargetTransitionState::Priming { started_at } => {
                match self.output_prime_ready(&staged.prime_slots) {
                    Ok(true) => {
                        let finish_at = self.begin_output_target_fade(&staged.request.targets);
                        staged.state = OutputTargetTransitionState::Fading { finish_at };
                        self.staged_output_targets = Some(staged);
                    }
                    Ok(false) if started_at.elapsed() < OUTPUT_TARGET_PRIME_TIMEOUT => {
                        self.staged_output_targets = Some(staged);
                    }
                    Ok(false) => {
                        self.rollback_primed_output_slots(&staged.prime_slots);
                        self.reject_output_targets(
                            &staged.request,
                            format!(
                                "output targets did not become ready within {} ms",
                                OUTPUT_TARGET_PRIME_TIMEOUT.as_millis()
                            ),
                        );
                    }
                    Err(error) => {
                        self.rollback_primed_output_slots(&staged.prime_slots);
                        self.reject_output_targets(&staged.request, error);
                    }
                }
            }
            OutputTargetTransitionState::Fading { finish_at } => {
                if Instant::now() < finish_at {
                    self.staged_output_targets = Some(staged);
                    return;
                }
                let previous = self.shared.output_target_control.current_targets();
                self.shared
                    .output_target_control
                    .acknowledge(&staged.request);
                eprintln!(
                    "wavelinux6-audio-core output_targets_applied mix_id={} generation={} previous={} targets={}",
                    self.shared.mix_id,
                    staged.request.generation,
                    display_targets(&previous),
                    display_targets(&staged.request.targets),
                );
            }
        }
    }

    pub(super) fn reap_retired_output_targets(&mut self) {
        let now = Instant::now();
        for slot in &mut self.output_slots {
            if slot
                .disconnect_at
                .is_none_or(|disconnect_at| now < disconnect_at)
            {
                continue;
            }
            let stream = &self._streams[slot.stream_index];
            match stream.disconnect() {
                Ok(()) => {
                    eprintln!(
                        "wavelinux6-audio-core output_target_retired mix_id={} target={}",
                        self.shared.mix_id,
                        slot.target.as_deref().unwrap_or("<none>")
                    );
                    slot.target = None;
                    slot.disconnect_at = None;
                    slot.status.reset();
                }
                Err(error) => {
                    self.shared.output_target_control.reject(format!(
                        "failed to retire output target {}: {error}",
                        slot.target.as_deref().unwrap_or("<none>")
                    ));
                    slot.disconnect_at = Some(now + Duration::from_millis(50));
                }
            }
        }
    }

    fn prepare_output_targets(&mut self, targets: &[String]) -> Result<Vec<(usize, u64)>, String> {
        let current = self.shared.output_target_control.current_targets();
        let missing = targets
            .iter()
            .filter(|target| {
                !self
                    .output_slots
                    .iter()
                    .any(|slot| slot.target.as_deref() == Some(target.as_str()))
            })
            .cloned()
            .collect::<Vec<_>>();
        let free_slots = self
            .output_slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.target.is_none().then_some(index))
            .collect::<Vec<_>>();
        if missing.len() > free_slots.len() {
            return Err(format!(
                "no free output slots for {} target changes",
                missing.len()
            ));
        }

        let mut connected: Vec<usize> = Vec::new();
        for (target, slot_index) in missing.iter().zip(free_slots) {
            if let Err(error) = self.connect_output_slot(slot_index, target) {
                for connected_index in connected {
                    let slot = &mut self.output_slots[connected_index];
                    slot.gain.store(0.0);
                    let _ = self._streams[slot.stream_index].disconnect();
                    slot.target = None;
                    slot.disconnect_at = None;
                }
                return Err(error);
            }
            connected.push(slot_index);
        }

        let mut prime_slots = Vec::new();
        for target in targets {
            let Some((slot_index, slot)) = self
                .output_slots
                .iter_mut()
                .enumerate()
                .find(|(_, slot)| slot.target.as_deref() == Some(target.as_str()))
            else {
                self.rollback_primed_output_slots(
                    &connected
                        .into_iter()
                        .map(|index| (index, 0))
                        .collect::<Vec<_>>(),
                );
                return Err(format!(
                    "prepared output target {target} has no stream slot"
                ));
            };
            slot.disconnect_at = None;
            if !current.contains(target) {
                slot.gain.store(0.0);
                prime_slots.push((
                    slot_index,
                    slot.status.processed_frames.load(Ordering::Acquire),
                ));
            }
        }
        Ok(prime_slots)
    }

    fn connect_output_slot(&mut self, slot_index: usize, target: &str) -> Result<(), String> {
        let slot = self
            .output_slots
            .get_mut(slot_index)
            .ok_or_else(|| format!("invalid output slot {slot_index}"))?;
        let stream = &self._streams[slot.stream_index];
        slot.gain.store(0.0);
        slot.status.reset();
        super::update_stream_target_properties(stream, &self.config.property_prefix, target)?;
        let format = super::audio_format_pod_bytes(self.config.sample_rate_hz)?;
        let mut params = [Pod::from_bytes(&format)
            .ok_or_else(|| "native mix format pod was invalid".to_string())?];
        stream
            .connect(
                spa::utils::Direction::Output,
                None,
                super::native_stream_flags(),
                &mut params,
            )
            .map_err(|error| format!("could not connect output target {target}: {error}"))?;
        slot.target = Some(target.to_string());
        slot.disconnect_at = None;
        Ok(())
    }

    fn output_prime_ready(&self, prime_slots: &[(usize, u64)]) -> Result<bool, String> {
        let prime_frames =
            (u64::from(self.config.sample_rate_hz) * OUTPUT_TARGET_CROSSFADE_MSEC / 1000).max(1);
        for (slot_index, baseline) in prime_slots {
            let slot = self
                .output_slots
                .get(*slot_index)
                .ok_or_else(|| format!("invalid priming output slot {slot_index}"))?;
            if slot.status.failed.load(Ordering::Acquire) {
                return Err(format!(
                    "output target {} entered an error state while priming",
                    slot.target.as_deref().unwrap_or("<none>")
                ));
            }
            if !slot.status.connected.load(Ordering::Acquire)
                || !slot.status.streaming.load(Ordering::Acquire)
            {
                return Ok(false);
            }
            if slot
                .status
                .processed_frames
                .load(Ordering::Acquire)
                .saturating_sub(*baseline)
                < prime_frames
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn begin_output_target_fade(&mut self, targets: &[String]) -> Instant {
        let finish_at = Instant::now() + Duration::from_millis(OUTPUT_TARGET_CROSSFADE_MSEC);
        let retire_at = finish_at + Duration::from_millis(10);
        for slot in &mut self.output_slots {
            let retained = slot
                .target
                .as_ref()
                .is_some_and(|target| targets.contains(target));
            if retained {
                slot.disconnect_at = None;
                slot.gain.store(1.0);
            } else if slot.target.is_some() {
                slot.gain.store(0.0);
                slot.disconnect_at = Some(retire_at);
            }
        }
        finish_at
    }

    fn finish_or_cancel_staged_output_targets(&mut self) {
        let Some(staged) = self.staged_output_targets.take() else {
            return;
        };
        match staged.state {
            OutputTargetTransitionState::Priming { .. } => {
                self.rollback_primed_output_slots(&staged.prime_slots);
            }
            OutputTargetTransitionState::Fading { .. } => {
                self.shared
                    .output_target_control
                    .acknowledge(&staged.request);
            }
        }
    }

    fn rollback_primed_output_slots(&mut self, prime_slots: &[(usize, u64)]) {
        let current = self.shared.output_target_control.current_targets();
        for (slot_index, _) in prime_slots {
            let Some(slot) = self.output_slots.get_mut(*slot_index) else {
                continue;
            };
            if slot
                .target
                .as_ref()
                .is_some_and(|target| current.contains(target))
            {
                slot.gain.store(1.0);
                slot.disconnect_at = None;
                continue;
            }
            slot.gain.store(0.0);
            let _ = self._streams[slot.stream_index].disconnect();
            slot.target = None;
            slot.disconnect_at = None;
            slot.status.reset();
        }
    }

    fn reject_output_targets(&self, request: &PendingOutputTargets, error: String) {
        self.shared.output_target_control.reject(error.clone());
        eprintln!(
            "wavelinux6-audio-core output_targets_failed mix_id={} generation={} targets={} error={}",
            self.shared.mix_id,
            request.generation,
            display_targets(&request.targets),
            error,
        );
    }
}

fn output_stream_timing_update_pending(
    applied_quantum_frames: usize,
    applied_target_latency_frames: usize,
    requested_quantum_frames: usize,
    requested_target_latency_frames: usize,
) -> bool {
    applied_quantum_frames != requested_quantum_frames
        || applied_target_latency_frames != requested_target_latency_frames
}

fn update_output_stream_latency_properties(
    stream: &pw::stream::Stream,
    quantum_frames: usize,
    target_latency_frames: usize,
    sample_rate_hz: u32,
) -> Result<(), String> {
    let mut props = properties! {
        *pw::keys::NODE_FORCE_QUANTUM => quantum_frames.to_string(),
    };
    props.insert(
        *pw::keys::NODE_LATENCY,
        format!("{}/{}", target_latency_frames.max(1), sample_rate_hz.max(1)),
    );
    let result = unsafe {
        pw::sys::pw_stream_update_properties(stream.as_raw_ptr(), props.dict().as_raw_ptr())
    };
    if result < 0 {
        Err(format!(
            "PipeWire latency property update failed with status {result}"
        ))
    } else {
        Ok(())
    }
}

fn display_targets(targets: &[String]) -> String {
    if targets.is_empty() {
        "<none>".into()
    } else {
        targets.join(",")
    }
}

fn process_mix_buffer(stream: &pw::stream::Stream, data: &mut NativeMixPlaybackData) {
    let started = Instant::now();
    process_mix_buffer_inner(stream, data);
    data.shared
        .stats
        .rt_callback_timing
        .record(started.elapsed());
}

fn process_mix_buffer_inner(stream: &pw::stream::Stream, data: &mut NativeMixPlaybackData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    update_mix_rate_match(data);
    let rate_match_frames = if data.rate_match.is_null() {
        0
    } else {
        unsafe { (*data.rate_match).size as usize }
    };
    let requested_frames = if rate_match_frames > 0 {
        rate_match_frames
    } else {
        buffer.requested() as usize
    };
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }
    let output = &mut datas[0];
    let Some(bytes) = output.data() else {
        return;
    };
    let stride = mem::size_of::<f32>() * 2;
    let frames = playback_render_frames(
        requested_frames,
        bytes.len(),
        stride,
        data.shared
            .render_quantum_frames
            .min(MAX_NATIVE_CALLBACK_FRAMES),
    );
    let target_frames = data
        .shared
        .target_latency_frames
        .load(Ordering::Relaxed)
        .max(1);
    let started = Instant::now();
    let mut underruns = 0_u64;
    let mut non_finite_samples = 0_u64;
    let mut peak_left = 0.0_f32;
    let mut peak_right = 0.0_f32;
    let mut square_sum_left = 0.0_f32;
    let mut square_sum_right = 0.0_f32;
    for frame_index in 0..frames {
        let mut mixed = [0.0_f32, 0.0_f32];
        for input in &mut data.inputs {
            let target_gain = input.bus.target_gain();
            input.current_gain += (target_gain - input.current_gain) * data.gain_smoothing;
            if target_gain <= 1.0e-5 && input.current_gain <= 1.0e-5 {
                input.reset();
                continue;
            }
            let rendered = input.render(target_frames, &mut underruns);
            let (frame, replaced) = finite_stereo_or_silence(rendered);
            non_finite_samples = non_finite_samples.saturating_add(replaced as u64);
            input.last_frame = frame;
            mixed[0] += frame[0] * input.current_gain;
            mixed[1] += frame[1] * input.current_gain;
        }
        data.current_master_gain +=
            (data.shared.target_master_gain() - data.current_master_gain) * data.gain_smoothing;
        let endpoint_target = data
            .endpoint_gain
            .as_ref()
            .map(|gain| gain.load())
            .unwrap_or(1.0);
        let endpoint_delta = (endpoint_target - data.current_endpoint_phase)
            .clamp(-data.endpoint_fade_step, data.endpoint_fade_step);
        data.current_endpoint_phase =
            (data.current_endpoint_phase + endpoint_delta).clamp(0.0, 1.0);
        let endpoint_output_gain =
            (data.current_endpoint_phase * std::f32::consts::FRAC_PI_2).sin();
        let output_gain = data.current_master_gain * endpoint_output_gain;
        mixed[0] = (mixed[0] * output_gain).clamp(-1.0, 1.0);
        mixed[1] = (mixed[1] * output_gain).clamp(-1.0, 1.0);
        let (mixed, replaced) = finite_stereo_or_silence(mixed);
        non_finite_samples = non_finite_samples.saturating_add(replaced as u64);
        peak_left = peak_left.max(mixed[0].abs());
        peak_right = peak_right.max(mixed[1].abs());
        square_sum_left += mixed[0] * mixed[0];
        square_sum_right += mixed[1] * mixed[1];
        for (channel, sample) in mixed.iter().enumerate() {
            let start = frame_index * stride + channel * mem::size_of::<f32>();
            bytes[start..start + mem::size_of::<f32>()].copy_from_slice(&sample.to_le_bytes());
        }
    }
    let chunk = output.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = stride as _;
    *chunk.size_mut() = (frames * stride) as _;
    let rms_scale = 1.0 / frames.max(1) as f32;
    data.shared.meter.publish(
        peak_left,
        peak_right,
        (square_sum_left * rms_scale).sqrt(),
        (square_sum_right * rms_scale).sqrt(),
        frames,
    );
    let elapsed = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
    data.shared
        .stats
        .rendered_frames
        .fetch_add(frames as u64, Ordering::Relaxed);
    data.shared
        .stats
        .underrun_frames
        .fetch_add(underruns, Ordering::Relaxed);
    if non_finite_samples > 0 {
        data.shared
            .stats
            .non_finite_blocks
            .fetch_add(1, Ordering::Relaxed);
        data.shared
            .stats
            .non_finite_samples
            .fetch_add(non_finite_samples, Ordering::Relaxed);
    }
    data.shared
        .stats
        .process_calls
        .fetch_add(1, Ordering::Relaxed);
    data.shared
        .stats
        .last_process_micros
        .store(elapsed, Ordering::Relaxed);
    data.shared
        .stats
        .max_process_micros
        .fetch_max(elapsed, Ordering::Relaxed);
    if let Some(status) = data.endpoint_status.as_ref() {
        status
            .processed_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }
    let current_buffer_frames = minimum_active_mix_fill(&data.inputs).unwrap_or(0);
    data.shared
        .current_buffer_frames
        .store(current_buffer_frames, Ordering::Relaxed);
}

fn update_mix_rate_match(data: &mut NativeMixPlaybackData) {
    data.rate_correction = update_software_mix_rates(
        &mut data.inputs,
        data.shared.target_latency_frames.load(Ordering::Relaxed),
    );
    data.shared
        .stats
        .software_rate_match_callbacks
        .fetch_add(1, Ordering::Relaxed);

    if data.rate_match.is_null() {
        data.shared
            .stats
            .rate_correction_bits
            .store(data.rate_correction.to_bits(), Ordering::Relaxed);
        return;
    }
    unsafe {
        (*data.rate_match).flags |= spa::sys::SPA_IO_RATE_MATCH_FLAG_ACTIVE;
        // Each mix input has an independent producer clock. Keep the output
        // stream synchronized to its PipeWire graph while fractional readers
        // correct each input independently.
        (*data.rate_match).rate = 1.0;
    }
    data.shared
        .stats
        .rate_correction_bits
        .store(data.rate_correction.to_bits(), Ordering::Relaxed);
    data.shared
        .stats
        .pipewire_rate_match_callbacks
        .fetch_add(1, Ordering::Relaxed);
}

fn update_software_mix_rates(inputs: &mut [NativeMixInput], target_frames: usize) -> f64 {
    let target = target_frames.max(1) as f64;
    let mut reported_correction = 1.0_f64;
    for input in inputs {
        let active = input.shared.capture_streaming.load(Ordering::Acquire)
            && (input.bus.target_gain() > 1.0e-5 || input.current_gain > 1.0e-5);
        let desired = if active && input.transition.is_none() {
            input.read_sequence.map_or(1.0, |sequence| {
                let fill = input
                    .shared
                    .history
                    .write_sequence()
                    .saturating_sub(sequence) as f64
                    - input.read_fraction;
                desired_software_read_rate(fill, target)
            })
        } else {
            1.0
        };
        input.rate_correction += (desired - input.rate_correction) * 0.02;
        if active && (input.rate_correction - 1.0).abs() > (reported_correction - 1.0).abs() {
            reported_correction = input.rate_correction;
        }
    }
    reported_correction
}

fn desired_software_read_rate(fill_frames: f64, target_frames: f64) -> f64 {
    let target_frames = target_frames.max(1.0);
    (1.0 + ((fill_frames - target_frames) / target_frames) * 0.002).clamp(0.997, 1.003)
}

fn minimum_active_mix_fill(inputs: &[NativeMixInput]) -> Option<u64> {
    let mut minimum = None;
    for input in inputs {
        if !input.shared.capture_streaming.load(Ordering::Acquire)
            || (input.bus.target_gain() <= 1.0e-5 && input.current_gain <= 1.0e-5)
        {
            continue;
        }
        if input.transition.is_some() {
            return None;
        }
        let sequence = input.read_sequence?;
        let fill = input
            .shared
            .history
            .write_sequence()
            .saturating_sub(sequence);
        minimum = Some(minimum.map_or(fill, |current: u64| current.min(fill)));
    }
    minimum
}

fn current_mix_rate_correction(shared: &NativeMixShared) -> f64 {
    let bits = shared.stats.rate_correction_bits.load(Ordering::Relaxed);
    if bits == 0 {
        1.0
    } else {
        f64::from_bits(bits)
    }
}

fn finite_stereo_or_silence(frame: [f32; 2]) -> ([f32; 2], usize) {
    let replaced = frame.iter().filter(|sample| !sample.is_finite()).count();
    if replaced == 0 {
        (frame, 0)
    } else {
        ([0.0, 0.0], replaced)
    }
}

#[derive(Debug)]
pub(super) struct NativeMixRegistry {
    mixes: Vec<Arc<NativeMixShared>>,
    channels: BTreeMap<String, Arc<NativeShared>>,
    meter_subscribers: AtomicUsize,
    meter_connections: AtomicU64,
    meter_frames: AtomicU64,
    meter_disconnects: AtomicU64,
}

impl NativeMixRegistry {
    pub(super) fn new(
        mixes: &[NativeMixRuntime<'_>],
        channels: &BTreeMap<String, Arc<NativeShared>>,
    ) -> Self {
        Self {
            mixes: mixes.iter().map(|mix| Arc::clone(&mix.shared)).collect(),
            channels: channels
                .iter()
                .map(|(channel_id, shared)| (channel_id.clone(), Arc::clone(shared)))
                .collect(),
            meter_subscribers: AtomicUsize::new(0),
            meter_connections: AtomicU64::new(0),
            meter_frames: AtomicU64::new(0),
            meter_disconnects: AtomicU64::new(0),
        }
    }

    fn mix(&self, mix_id: &str) -> Option<&NativeMixShared> {
        self.mixes
            .iter()
            .find(|mix| mix.mix_id == mix_id)
            .map(Arc::as_ref)
    }

    fn meter_response(&self, request_id: Option<String>) -> serde_json::Value {
        let channels = self
            .channels
            .iter()
            .map(|(channel_id, shared)| meter_json(channel_id, shared.meter.snapshot()))
            .collect::<Vec<_>>();
        let mixes = self
            .mixes
            .iter()
            .map(|mix| {
                let exact = mix.meter.snapshot();
                let snapshot =
                    if exact.frames > 0 && exact.age_micros <= EXACT_MIX_METER_MAX_AGE_MICROS {
                        exact
                    } else {
                        self.estimated_mix_meter(mix)
                    };
                meter_json(&mix.mix_id, snapshot)
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
            "ok": true,
            "request_id": request_id,
            "channels": channels,
            "mixes": mixes,
            "meter_stream_protocol": wavelinux_dsp::METER_STREAM_PROTOCOL_VERSION,
            "meter_stream_subscribers": self.meter_subscribers.load(Ordering::Relaxed),
            "meter_stream_connections": self.meter_connections.load(Ordering::Relaxed),
            "meter_stream_frames": self.meter_frames.load(Ordering::Relaxed),
            "meter_stream_disconnects": self.meter_disconnects.load(Ordering::Relaxed),
        })
    }

    fn meter_slots(&self) -> Vec<MeterStreamSlot> {
        self.channels
            .keys()
            .map(|id| MeterStreamSlot {
                kind: MeterStreamSlotKind::Channel,
                id: id.clone(),
            })
            .chain(self.mixes.iter().map(|mix| MeterStreamSlot {
                kind: MeterStreamSlotKind::Mix,
                id: mix.mix_id.clone(),
            }))
            .collect()
    }

    fn meter_samples(&self) -> Vec<MeterStreamSample> {
        self.channels
            .values()
            .map(|shared| meter_stream_sample(shared.meter.snapshot()))
            .chain(self.mixes.iter().map(|mix| {
                let exact = mix.meter.snapshot();
                let snapshot =
                    if exact.frames > 0 && exact.age_micros <= EXACT_MIX_METER_MAX_AGE_MICROS {
                        exact
                    } else {
                        self.estimated_mix_meter(mix)
                    };
                meter_stream_sample(snapshot)
            }))
            .collect()
    }

    fn estimated_mix_meter(&self, mix: &NativeMixShared) -> NativeMeterSnapshot {
        let mut peak_left = 0.0_f32;
        let mut peak_right = 0.0_f32;
        let mut rms_left = 0.0_f32;
        let mut rms_right = 0.0_f32;
        let mut frames = 0_u64;
        let mut youngest_age = u64::MAX;
        for bus in &mix.buses {
            let Some(channel) = self.channels.get(&bus.channel_id) else {
                continue;
            };
            let snapshot = channel.meter.snapshot();
            let gain = bus.target_gain();
            peak_left += snapshot.peak_left * gain;
            peak_right += snapshot.peak_right * gain;
            rms_left += snapshot.rms_left * gain;
            rms_right += snapshot.rms_right * gain;
            frames = frames.max(snapshot.frames);
            youngest_age = youngest_age.min(snapshot.age_micros);
        }
        let master = mix.target_master_gain();
        NativeMeterSnapshot {
            peak_left: (peak_left * master).clamp(0.0, 1.0),
            peak_right: (peak_right * master).clamp(0.0, 1.0),
            rms_left: (rms_left * master).clamp(0.0, 1.0),
            rms_right: (rms_right * master).clamp(0.0, 1.0),
            age_micros: if youngest_age == u64::MAX {
                0
            } else {
                youngest_age
            },
            frames,
        }
    }
}

fn meter_json(id: &str, snapshot: NativeMeterSnapshot) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "peak_left": snapshot.peak_left,
        "peak_right": snapshot.peak_right,
        "rms_left": snapshot.rms_left,
        "rms_right": snapshot.rms_right,
        "age_micros": snapshot.age_micros,
        "frames": snapshot.frames,
    })
}

fn meter_stream_sample(snapshot: NativeMeterSnapshot) -> MeterStreamSample {
    MeterStreamSample {
        peak_left: snapshot.peak_left,
        peak_right: snapshot.peak_right,
        rms_left: snapshot.rms_left,
        rms_right: snapshot.rms_right,
    }
}

#[derive(Debug, Deserialize)]
struct MixControlCommand {
    #[serde(default = "default_control_protocol_version")]
    protocol_version: u16,
    command: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    route_id: Option<String>,
    #[serde(default)]
    mix_id: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    volume: Option<f32>,
    #[serde(default)]
    muted: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    target_msec: Option<u16>,
    #[serde(default)]
    pipewire_quantum_frames: Option<u32>,
    #[serde(default)]
    target_node_names: Option<Vec<String>>,
    #[serde(default)]
    route_generation: Option<u64>,
}

fn default_control_protocol_version() -> u16 {
    CORE_CONTROL_PROTOCOL_VERSION
}

pub(super) fn start_meter_stream_socket(socket_path: String, registry: Arc<NativeMixRegistry>) {
    thread::spawn(move || {
        let path = PathBuf::from(socket_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&path);
        let Ok(listener) = UnixListener::bind(&path) else {
            eprintln!(
                "wavelinux6-audio-core meter_stream_socket_failed path={}",
                path.display()
            );
            return;
        };
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let _ = listener.set_nonblocking(true);
        let slots = registry.meter_slots();
        let header = match encode_meter_stream_header(&slots) {
            Ok(header) => Arc::new(header),
            Err(error) => {
                eprintln!(
                    "wavelinux6-audio-core meter_stream_header_failed path={} error={}",
                    path.display(),
                    error
                );
                let _ = std::fs::remove_file(&path);
                return;
            }
        };
        eprintln!(
            "wavelinux6-audio-core meter_stream_socket path={} protocol={} rate_hz={} slots={}",
            path.display(),
            wavelinux_dsp::METER_STREAM_PROTOCOL_VERSION,
            METER_STREAM_RATE_HZ,
            slots.len()
        );

        while !TERMINATE.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let previous = registry.meter_subscribers.fetch_add(1, Ordering::AcqRel);
                    if previous >= 4 {
                        registry.meter_subscribers.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    registry.meter_connections.fetch_add(1, Ordering::Relaxed);
                    let client_registry = Arc::clone(&registry);
                    let client_header = Arc::clone(&header);
                    if thread::Builder::new()
                        .name("wavelinux6-meter-client".into())
                        .spawn(move || {
                            serve_meter_stream_client(stream, client_registry, &client_header)
                        })
                        .is_err()
                    {
                        registry.meter_subscribers.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    eprintln!("wavelinux6-audio-core meter_stream_accept_error {error}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
        let _ = std::fs::remove_file(path);
    });
}

fn serve_meter_stream_client(
    mut stream: std::os::unix::net::UnixStream,
    registry: Arc<NativeMixRegistry>,
    header: &[u8],
) {
    let result = (|| -> std::io::Result<()> {
        stream.set_write_timeout(Some(Duration::from_millis(250)))?;
        stream.write_all(header)?;
        let started = Instant::now();
        let interval = Duration::from_nanos(1_000_000_000 / u64::from(METER_STREAM_RATE_HZ));
        let mut next_tick = Instant::now();
        let mut sequence = 0_u64;
        let mut bytes = Vec::new();
        while !TERMINATE.load(Ordering::SeqCst) {
            sequence = sequence.wrapping_add(1).max(1);
            let samples = registry.meter_samples();
            encode_meter_stream_frame_into(
                sequence,
                started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
                &samples,
                &mut bytes,
            )?;
            stream.write_all(&bytes)?;
            registry.meter_frames.fetch_add(1, Ordering::Relaxed);
            next_tick += interval;
            let now = Instant::now();
            if next_tick > now {
                thread::sleep(next_tick.duration_since(now));
            } else if now.duration_since(next_tick) > interval {
                next_tick = now;
            }
        }
        Ok(())
    })();
    registry.meter_subscribers.fetch_sub(1, Ordering::AcqRel);
    registry.meter_disconnects.fetch_add(1, Ordering::Relaxed);
    if let Err(error) = result {
        if !matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
        ) {
            eprintln!("wavelinux6-audio-core meter_stream_client_error {error}");
        }
    }
}

pub(super) fn start_mix_control_socket(socket_path: String, registry: Arc<NativeMixRegistry>) {
    thread::spawn(move || {
        let path = PathBuf::from(socket_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&path);
        let Ok(listener) = UnixListener::bind(&path) else {
            eprintln!(
                "wavelinux6-audio-core mix_control_socket_failed path={}",
                path.display()
            );
            return;
        };
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        eprintln!(
            "wavelinux6-audio-core mix_control_socket path={} protocol={}",
            path.display(),
            CORE_CONTROL_PROTOCOL_VERSION
        );
        while !TERMINATE.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let timeout = Some(Duration::from_secs(1));
                    let _ = stream.set_read_timeout(timeout);
                    let _ = stream.set_write_timeout(timeout);
                    let mut payload = String::new();
                    let response =
                        match Read::take(&mut stream, 1024 * 1024).read_to_string(&mut payload) {
                            Ok(_) => match serde_json::from_str::<MixControlCommand>(&payload) {
                                Ok(command) => handle_mix_control(&registry, command),
                                Err(err) => control_error(None, format!("invalid command: {err}")),
                            },
                            Err(err) => control_error(None, format!("read failed: {err}")),
                        };
                    let _ = stream.write_all(response.to_string().as_bytes());
                    let _ = stream.write_all(b"\n");
                }
                Err(err) => {
                    eprintln!("wavelinux6-audio-core mix_control_accept_error {err}");
                    thread::sleep(CONTROL_MAINLOOP_POLL_INTERVAL);
                }
            }
        }
        let _ = std::fs::remove_file(path);
    });
}

fn handle_mix_control(
    registry: &NativeMixRegistry,
    command: MixControlCommand,
) -> serde_json::Value {
    if command.protocol_version != CORE_CONTROL_PROTOCOL_VERSION {
        return control_error(
            command.request_id,
            format!(
                "unsupported protocol {}; expected {}",
                command.protocol_version, CORE_CONTROL_PROTOCOL_VERSION
            ),
        );
    }
    if command.command == "get_meters" {
        return registry.meter_response(command.request_id);
    }
    let mix_id = command.mix_id.as_deref().or(command.route_id.as_deref());
    let Some(mix_id) = mix_id else {
        return control_error(command.request_id, "mix_id is required");
    };
    let Some(mix) = registry.mix(mix_id) else {
        return control_error(command.request_id, format!("unknown mix {mix_id}"));
    };

    match command.command.as_str() {
        "set_output_targets" => {
            let Some(targets) = command.target_node_names else {
                return control_error(command.request_id, "target_node_names is required");
            };
            match mix
                .output_target_control
                .queue(targets.clone(), command.route_generation)
            {
                Ok(generation) => serde_json::json!({
                    "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                    "ok": true,
                    "request_id": command.request_id,
                    "route_id": mix_id,
                    "route_generation": generation,
                    "target_node_names": targets,
                    "operation": "output_targets_queued",
                }),
                Err(error) => control_error(command.request_id, error),
            }
        }
        "set_mix_bus" => {
            let Some(channel_id) = command.channel_id.as_deref() else {
                return control_error(command.request_id, "channel_id is required");
            };
            let Some(bus) = mix.bus(channel_id) else {
                return control_error(
                    command.request_id,
                    format!("mix {mix_id} has no bus for {channel_id}"),
                );
            };
            if let Some(volume) = command.volume {
                bus.volume.store(volume);
            }
            if let Some(muted) = command.muted {
                bus.muted.store(muted, Ordering::Relaxed);
            }
            if let Some(enabled) = command.enabled {
                bus.enabled.store(enabled, Ordering::Relaxed);
            }
            serde_json::json!({
                "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                "ok": true,
                "request_id": command.request_id,
                "mix_id": mix_id,
                "channel_id": channel_id,
                "volume": bus.volume.load(),
                "muted": bus.muted.load(Ordering::Relaxed),
                "enabled": bus.enabled.load(Ordering::Relaxed),
            })
        }
        "set_mix_master" => {
            if let Some(volume) = command.volume {
                mix.volume.store(volume);
            }
            if let Some(muted) = command.muted {
                mix.muted.store(muted, Ordering::Relaxed);
            }
            serde_json::json!({
                "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                "ok": true,
                "request_id": command.request_id,
                "mix_id": mix_id,
                "volume": mix.volume.load(),
                "muted": mix.muted.load(Ordering::Relaxed),
            })
        }
        "set_target_latency" => {
            let Some(target_msec) = command.target_msec else {
                return control_error(command.request_id, "target_msec is required");
            };
            mix.set_target_latency(target_msec);
            if let Some(quantum_frames) = command.pipewire_quantum_frames {
                if let Err(error) = mix.set_requested_quantum(quantum_frames) {
                    return control_error(command.request_id, error);
                }
            }
            serde_json::json!({
                "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                "ok": true,
                "request_id": command.request_id,
                "route_id": mix_id,
                "target_msec": mix.target_latency_msec.load(Ordering::Relaxed),
                "pipewire_quantum_frames": mix.requested_quantum_frames.load(Ordering::Relaxed),
            })
        }
        "get_diagnostics" => serde_json::json!({
            "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
            "ok": true,
            "request_id": command.request_id,
            "route_id": mix_id,
            "sample_rate_hz": mix.sample_rate_hz,
            "target_latency_msec": mix.target_latency_msec.load(Ordering::Relaxed),
            "pipewire_quantum_frames": mix.applied_quantum_frames.load(Ordering::Relaxed),
            "current_buffer_frames": mix.current_buffer_frames.load(Ordering::Relaxed),
            "rate_correction": current_mix_rate_correction(mix),
            "pipewire_rate_match_callbacks": mix.stats.pipewire_rate_match_callbacks.load(Ordering::Relaxed),
            "software_rate_match_callbacks": mix.stats.software_rate_match_callbacks.load(Ordering::Relaxed),
            "rendered_frames": mix.stats.rendered_frames.load(Ordering::Relaxed),
            "underrun_frames": mix.stats.underrun_frames.load(Ordering::Relaxed),
            "non_finite_blocks": mix.stats.non_finite_blocks.load(Ordering::Relaxed),
            "non_finite_samples": mix.stats.non_finite_samples.load(Ordering::Relaxed),
            "non_finite_effect_mask": 0,
            "chain_recoveries": 0,
            "last_process_micros": mix.stats.last_process_micros.load(Ordering::Relaxed),
            "max_process_micros": mix.stats.max_process_micros.load(Ordering::Relaxed),
            "rt_callback_count": mix.stats.rt_callback_timing.count(),
            "rt_callback_p99_micros": mix.stats.rt_callback_timing.p99_micros(),
            "rt_callback_max_micros": mix.stats.rt_callback_timing.max_micros(),
            "submitted_route_generation": mix.output_target_control.submitted_generation.load(Ordering::Acquire),
            "applied_route_generation": mix.output_target_control.applied_generation.load(Ordering::Acquire),
            "output_target_node_names": mix.output_target_control.current_targets(),
            "route_target_error": mix.output_target_control.last_error(),
        }),
        _ => control_error(
            command.request_id,
            format!("unsupported command {}", command.command),
        ),
    }
}

fn control_error(request_id: Option<String>, error: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
        "ok": false,
        "request_id": request_id,
        "error": error.into(),
    })
}

pub(super) fn log_mix_stats(shared: &NativeMixShared) {
    eprintln!(
        "wavelinux6-audio-core native_mix_stats mix_id={} rendered_frames={} underrun_frames={} non_finite_blocks={} non_finite_samples={} process_calls={} last_process_us={} max_process_us={} buffered_frames={} target_latency_msec={} rate_correction={:.9} pipewire_rate_match_callbacks={} software_rate_match_callbacks={}",
        shared.mix_id,
        shared.stats.rendered_frames.load(Ordering::Relaxed),
        shared.stats.underrun_frames.load(Ordering::Relaxed),
        shared.stats.non_finite_blocks.load(Ordering::Relaxed),
        shared.stats.non_finite_samples.load(Ordering::Relaxed),
        shared.stats.process_calls.load(Ordering::Relaxed),
        shared.stats.last_process_micros.load(Ordering::Relaxed),
        shared.stats.max_process_micros.load(Ordering::Relaxed),
        shared.current_buffer_frames.load(Ordering::Relaxed),
        shared.target_latency_msec.load(Ordering::Relaxed),
        current_mix_rate_correction(shared),
        shared.stats.pipewire_rate_match_callbacks.load(Ordering::Relaxed),
        shared.stats.software_rate_match_callbacks.load(Ordering::Relaxed),
    );
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use wavelinux_dsp::{DspAdaptiveLatencyConfig, DspChannelConfig};

    fn mix_config() -> DspMixConfig {
        DspMixConfig {
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
            volume: 0.8,
            muted: false,
            buses: vec![DspMixBusConfig {
                channel_id: "music".into(),
                volume: 0.5,
                muted: false,
                enabled: true,
            }],
        }
    }

    #[test]
    fn atomic_gain_rejects_non_finite_values() {
        let gain = AtomicGain::new(0.5);
        gain.store(f32::NAN);
        assert_eq!(gain.load(), 1.0);
        gain.store(9.0);
        assert_eq!(gain.load(), 2.0);
    }

    #[test]
    fn native_mix_nodes_pause_processing_when_unlinked() {
        let props = native_mix_public_source_properties(&mix_config());

        assert_eq!(props.get(*pw::keys::NODE_PAUSE_ON_IDLE), Some("true"));
    }

    #[test]
    fn disabled_or_muted_bus_has_zero_target_gain() {
        let config = DspMixBusConfig {
            channel_id: "music".into(),
            volume: 0.75,
            muted: false,
            enabled: true,
        };
        let bus = NativeMixBusShared::new(&config);
        assert_eq!(bus.target_gain(), 0.75);
        bus.muted.store(true, Ordering::Relaxed);
        assert_eq!(bus.target_gain(), 0.0);
        bus.muted.store(false, Ordering::Relaxed);
        bus.enabled.store(false, Ordering::Relaxed);
        assert_eq!(bus.target_gain(), 0.0);
    }

    #[test]
    fn invalid_mix_input_silences_only_that_frame() {
        assert_eq!(finite_stereo_or_silence([f32::NAN, 0.5]), ([0.0, 0.0], 1));
        assert_eq!(finite_stereo_or_silence([0.25, -0.5]), ([0.25, -0.5], 0));
    }

    #[test]
    fn output_target_updates_are_deduplicated_and_latest_wins() {
        let control = OutputTargetControl::new(vec!["alsa_output.old".into()]);

        let generation = control
            .queue(
                vec!["alsa_output.usb".into(), "alsa_output.usb".into()],
                None,
            )
            .unwrap();
        assert_eq!(generation, 2);
        assert_eq!(
            control
                .queue(vec!["bluez_output.headphones".into()], None)
                .unwrap(),
            3
        );
        let pending = control.take_pending().expect("latest output targets");
        assert_eq!(pending.generation, 3);
        assert_eq!(pending.targets, vec!["bluez_output.headphones"]);
        control.acknowledge(&pending);
        assert_eq!(control.current_targets(), vec!["bluez_output.headphones"]);
    }

    #[test]
    fn output_endpoint_status_tracks_priming_readiness() {
        let status = OutputEndpointStatus::default();
        status.observe_state(&pw::stream::StreamState::Connecting);
        assert!(!status.connected.load(Ordering::Acquire));

        status.observe_state(&pw::stream::StreamState::Paused);
        assert!(status.connected.load(Ordering::Acquire));
        assert!(!status.streaming.load(Ordering::Acquire));

        status.observe_state(&pw::stream::StreamState::Streaming);
        assert!(status.connected.load(Ordering::Acquire));
        assert!(status.streaming.load(Ordering::Acquire));

        status.observe_state(&pw::stream::StreamState::Error("sink lost".into()));
        assert!(status.failed.load(Ordering::Acquire));
    }

    #[test]
    fn output_stream_latency_updates_when_learned_quantum_is_unchanged() {
        assert!(output_stream_timing_update_pending(512, 1_920, 512, 1_344));
        assert!(!output_stream_timing_update_pending(512, 1_344, 512, 1_344));
    }

    #[test]
    fn native_mix_starts_with_persisted_pipewire_quantum_floor() {
        let mut config = mix_config();
        config.pipewire_quantum_frames = 512;
        let mix = NativeMixShared::new(&config);

        assert_eq!(mix.requested_quantum_frames.load(Ordering::Relaxed), 512);
    }

    fn buffered_mix_input(
        channel_id: &str,
        write_frames: usize,
        read_sequence: Option<u64>,
        enabled: bool,
        current_gain: f32,
    ) -> NativeMixInput {
        let channel_config = DspChannelConfig::new(
            channel_id,
            channel_id,
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            format!("wavelinux6_channel_{channel_id}"),
            format!("wavelinux6_fx_{channel_id}_source"),
            Vec::new(),
        );
        let channel = Arc::new(NativeShared::new(&channel_config, None));
        for _ in 0..write_frames {
            channel.history.push([0.0, 0.0]);
        }
        channel.capture_streaming.store(true, Ordering::Release);
        let bus = Arc::new(NativeMixBusShared::new(&DspMixBusConfig {
            channel_id: channel_id.into(),
            volume: 1.0,
            muted: false,
            enabled,
        }));
        let mut input = NativeMixInput::new(channel, bus);
        input.read_sequence = read_sequence;
        input.current_gain = current_gain;
        input
    }

    #[test]
    fn rate_match_tracks_the_closest_active_input_to_underrun() {
        let active_a = buffered_mix_input("music", 200, Some(150), true, 1.0);
        let active_b = buffered_mix_input("browser", 200, Some(180), true, 1.0);
        let inactive = buffered_mix_input("game", 200, Some(199), false, 0.0);

        assert_eq!(
            minimum_active_mix_fill(&[active_a, active_b, inactive]),
            Some(20)
        );
    }

    #[test]
    fn idle_or_uninitialized_inputs_do_not_distort_rate_feedback() {
        let active = buffered_mix_input("music", 200, Some(160), true, 1.0);
        let idle = buffered_mix_input("browser", 200, None, true, 1.0);
        idle.shared
            .capture_streaming
            .store(false, Ordering::Release);

        assert_eq!(minimum_active_mix_fill(&[active, idle]), Some(40));

        let active = buffered_mix_input("music", 200, Some(160), true, 1.0);
        let uninitialized = buffered_mix_input("browser", 200, None, true, 1.0);
        assert_eq!(minimum_active_mix_fill(&[active, uninitialized]), None);
    }

    #[test]
    fn software_rate_correction_is_directional_and_bounded() {
        assert!(desired_software_read_rate(1_600.0, 1_344.0) > 1.0);
        assert!(desired_software_read_rate(1_000.0, 1_344.0) < 1.0);
        assert_eq!(desired_software_read_rate(1_344.0, 1_344.0), 1.0);
        assert_eq!(desired_software_read_rate(100_000.0, 1.0), 1.003);
        assert_eq!(desired_software_read_rate(0.0, 1.0), 0.998);
    }

    #[test]
    fn fractional_mix_reader_absorbs_clock_drift_without_skipping_blocks() {
        let mut input = buffered_mix_input("music", 4_000, Some(1_000), true, 1.0);
        input.applied_target_frames = 1_344;
        input.rate_correction = 0.997;
        let mut underruns = 0;

        for _ in 0..1_000 {
            input.render(1_344, &mut underruns);
        }

        assert_eq!(underruns, 0);
        let read_position = input.read_sequence.unwrap_or_default() as f64 + input.read_fraction;
        assert!((read_position - 1_997.0).abs() < 1.0e-9);
    }

    #[test]
    fn latency_crossfade_preserves_fractional_read_phase() {
        let mut input = buffered_mix_input("music", 4_000, Some(3_000), true, 1.0);
        input.applied_target_frames = 1_344;
        input.read_fraction = 0.75;
        let mut underruns = 0;

        input.render(1_920, &mut underruns);

        let transition = input.transition.expect("latency transition");
        assert_eq!(underruns, 0);
        assert_eq!(transition.from_fraction, 0.75);
        assert_eq!(transition.to_fraction, 0.75);
        assert_eq!(input.read_fraction, 0.75);
    }

    #[test]
    fn estimated_mix_meter_applies_bus_and_master_gain_once() {
        let channel_config = DspChannelConfig::new(
            "music",
            "Music",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_music",
            "wavelinux6_fx_music_source",
            Vec::new(),
        );
        let channel = Arc::new(NativeShared::new(&channel_config, None));
        channel.meter.publish(0.5, 0.25, 0.3, 0.15, 256);
        let mix = Arc::new(NativeMixShared::new(&mix_config()));
        let registry = NativeMixRegistry {
            mixes: vec![Arc::clone(&mix)],
            channels: BTreeMap::from([("music".into(), channel)]),
            meter_subscribers: AtomicUsize::new(0),
            meter_connections: AtomicU64::new(0),
            meter_frames: AtomicU64::new(0),
            meter_disconnects: AtomicU64::new(0),
        };

        let snapshot = registry.estimated_mix_meter(&mix);
        assert!((snapshot.peak_left - 0.2).abs() < 0.000_001);
        assert!((snapshot.peak_right - 0.1).abs() < 0.000_001);
        assert!((snapshot.rms_left - 0.12).abs() < 0.000_001);
        assert!((snapshot.rms_right - 0.06).abs() < 0.000_001);
        assert_eq!(snapshot.frames, 256);

        mix.buses[0].muted.store(true, Ordering::Relaxed);
        let muted_bus = registry.estimated_mix_meter(&mix);
        assert_eq!(muted_bus.peak_left, 0.0);
        assert_eq!(muted_bus.peak_right, 0.0);

        mix.buses[0].muted.store(false, Ordering::Relaxed);
        mix.muted.store(true, Ordering::Relaxed);
        let muted_mix = registry.estimated_mix_meter(&mix);
        assert_eq!(muted_mix.peak_left, 0.0);
        assert_eq!(muted_mix.peak_right, 0.0);
    }
}
