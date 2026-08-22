use std::env;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::mem;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use pipewire as pw;
use pw::{properties::properties, spa};
use serde::Deserialize;
use serde::Serialize;
use spa::pod::Pod;
use wavelinux_dsp::{
    benchmark_fixture, human_duration, native_dsp_effect_supported, probe_backend_from_env,
    AudioRuntimeMode, ChainMetrics, DspAccelerationConfig, DspAccelerationMetrics,
    DspBackendStatus, DspChain, DspChannelConfig, DspCoreManifest, DspInputMode, DspProvider,
    RealtimeProcessStatus, AUDIO_RUNTIME_ENV, CORE_CONTROL_PROTOCOL_VERSION,
};

#[path = "wavelinux6_audio_core/native_mix.rs"]
mod native_mix;

const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_FRAMES: usize = DEFAULT_SAMPLE_RATE_HZ as usize * 5;
const FILTER_CHAIN_PIPEWIRE_ENV: &str = "WAVELINUX_FILTER_CHAIN_PIPEWIRE";
const MAX_NATIVE_CALLBACK_FRAMES: usize = 16_384;
const DSP_WORKER_BLOCK_FRAMES: usize = 480;
const DSP_WORKER_ACTIVE_IDLE: Duration = Duration::from_micros(500);
const DSP_WORKER_INACTIVE_IDLE: Duration = Duration::from_millis(5);
const DSP_ACCELERATOR_BLOCK_TIMEOUT: Duration = Duration::from_millis(4);
const DSP_ACCELERATOR_METRICS_INTERVAL_BLOCKS: u8 = 30;
const LATENCY_CROSSFADE_MSEC: usize = 20;
const LATENCY_ALIGNMENT_SEARCH_FRAMES: u64 = 96;
const LATENCY_ALIGNMENT_COMPARE_FRAMES: u64 = 24;
const CHAIN_CROSSFADE_MSEC: usize = 20;
const INPUT_TARGET_PRIME_MSEC: u64 = 20;
const INPUT_TARGET_TRANSITION_TIMEOUT: Duration = Duration::from_millis(750);
const RECENTER_INTERVAL_MSEC: usize = 100;
const RECENTER_THRESHOLD_FRAMES: u64 = 256;
const CONTROL_MAINLOOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const NATIVE_METER_STALE_AFTER: Duration = Duration::from_millis(120);
const NATIVE_METER_RELEASE_PER_SECOND: f32 = 0.08;
const RT_CALLBACK_BUCKET_MICROS: u64 = 25;
const RT_CALLBACK_BUCKETS: usize = 256;
const EMPTY_SEQUENCE: u64 = u64::MAX;
const WRITING_SEQUENCE: u64 = u64::MAX - 1;
static TERMINATE: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
struct PersistentCoreLock {
    _file: File,
}

fn acquire_persistent_core_lock(
    runtime_root: &Path,
    topology_revision: &str,
) -> Result<PersistentCoreLock, String> {
    std::fs::create_dir_all(runtime_root).map_err(|err| {
        format!(
            "failed to create audio-core runtime directory {}: {err}",
            runtime_root.display()
        )
    })?;
    let lock_path = runtime_root.join("wavelinux6-audio-core.lock");
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| format!("failed to open {}: {err}", lock_path.display()))?;
    let started = Instant::now();
    loop {
        let lock_result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if lock_result == 0 {
            break;
        }
        if started.elapsed() >= Duration::from_secs(2) {
            return Err(format!(
                "another WaveLinux 6 audio core owns {}",
                lock_path.display()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
    file.set_len(0)
        .map_err(|err| format!("failed to reset {}: {err}", lock_path.display()))?;
    writeln!(
        file,
        "pid={} topology_revision={topology_revision}",
        process::id()
    )
    .map_err(|err| format!("failed to write {}: {err}", lock_path.display()))?;
    file.sync_data()
        .map_err(|err| format!("failed to sync {}: {err}", lock_path.display()))?;
    let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
    Ok(PersistentCoreLock { _file: file })
}

#[derive(Debug, Serialize)]
struct ProbeReport {
    helper: &'static str,
    status: DspBackendStatus,
}

#[derive(Debug, Serialize)]
struct BenchReport {
    helper: &'static str,
    status: DspBackendStatus,
    sample_rate_hz: u32,
    metrics: ChainMetrics,
    elapsed: String,
}

fn main() {
    install_process_panic_hook();
    let args = env::args().skip(1).collect::<Vec<_>>();
    let result = if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        Ok(())
    } else if args.iter().any(|arg| arg == "--run-native") {
        run_native_graph(&args)
    } else if args.iter().any(|arg| arg == "--run-core") {
        run_persistent_core(&args)
    } else if args.iter().any(|arg| arg == "--run-filter-chain") {
        run_filter_chain_bridge(&args)
    } else if args.iter().any(|arg| arg == "--bench-fixture") {
        run_bench(&args)
    } else {
        run_probe()
    };

    if let Err(err) = result {
        eprintln!("wavelinux6-audio-core: {err}");
        process::exit(2);
    }
}

fn run_native_graph(args: &[String]) -> Result<(), String> {
    install_signal_handlers();
    let config_path = value_after(args, "--config")
        .map(PathBuf::from)
        .ok_or_else(|| "--run-native requires --config".to_string())?;
    let config: DspChannelConfig = serde_json::from_str(
        &std::fs::read_to_string(&config_path)
            .map_err(|err| format!("failed to read native DSP config: {err}"))?,
    )
    .map_err(|err| format!("failed to parse native DSP config: {err}"))?;
    if config
        .unsupported_active_effects()
        .iter()
        .any(|effect_id| !native_dsp_effect_supported(effect_id))
    {
        return Err(format!(
            "native DSP config contains unsupported effects: {}",
            config.unsupported_active_effects().join(",")
        ));
    }

    let mut status = probe_backend_from_env();
    let runtime_root = config_runtime_root(&config);
    let acceleration = resolve_acceleration_config(&mut status, runtime_root.as_deref());
    eprintln!(
        "wavelinux6-audio-core native_start channel_id={} runtime={} provider={} input={} output={} config={}",
        config.channel_id,
        status.runtime.as_str(),
        status
            .selected_provider
            .map(|provider| provider.as_str())
            .unwrap_or("cpu"),
        config.input_node_name,
        config.output_node_name,
        config_path.display()
    );
    eprintln!(
        "wavelinux6-audio-core backend_status={}",
        serde_json::to_string(&status).map_err(|err| err.to_string())?
    );

    run_pipewire_native_graph(config, status, acceleration)
}

fn run_persistent_core(args: &[String]) -> Result<(), String> {
    install_signal_handlers();
    let manifest_path = value_after(args, "--manifest")
        .map(PathBuf::from)
        .ok_or_else(|| "--run-core requires --manifest".to_string())?;
    let mut manifest: DspCoreManifest = serde_json::from_str(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|err| format!("failed to read audio-core manifest: {err}"))?,
    )
    .map_err(|err| format!("failed to parse audio-core manifest: {err}"))?;
    manifest.resolve_control_socket_paths()?;
    manifest.validate()?;
    let topology_revision = wavelinux_dsp::core_topology_revision(&manifest);
    let runtime_root = manifest
        .runtime_root
        .as_deref()
        .map(Path::new)
        .ok_or_else(|| "persistent audio-core manifest has no runtime root".to_string())?;
    let _instance_lock = acquire_persistent_core_lock(runtime_root, &topology_revision)?;
    let mut status = probe_backend_from_env();
    let runtime_root = manifest.runtime_root.as_deref().map(PathBuf::from);
    let acceleration = resolve_acceleration_config(&mut status, runtime_root.as_deref());
    eprintln!(
        "wavelinux6-audio-core core_start revision={} channels={} mixes={} runtime={} provider={} manifest={}",
        manifest.revision,
        manifest.channels.len(),
        manifest.mixes.len(),
        status.runtime.as_str(),
        status
            .selected_provider
            .map(|provider| provider.as_str())
            .unwrap_or("cpu"),
        manifest_path.display()
    );
    eprintln!(
        "wavelinux6-audio-core backend_status={}",
        serde_json::to_string(&status).map_err(|err| err.to_string())?
    );

    for config in &manifest.channels {
        eprintln!(
            "wavelinux6-audio-core native_start channel_id={} input={} output={} input_target={}",
            config.channel_id,
            config.input_node_name,
            config.output_node_name,
            config.input_target_node_name.as_deref().unwrap_or("<none>")
        );
    }
    let meter_socket_path = manifest.runtime_root.as_deref().map(|runtime_root| {
        wavelinux_dsp::meter_stream_socket(std::path::Path::new(runtime_root))
            .to_string_lossy()
            .into_owned()
    });
    let result = run_pipewire_native_core(
        manifest.channels,
        manifest.mixes,
        manifest.control_socket_path,
        meter_socket_path,
        topology_revision,
        status,
        acceleration,
    );
    eprintln!(
        "wavelinux6-audio-core core_stop failures={}",
        usize::from(result.is_err())
    );
    result
}

fn config_runtime_root(config: &DspChannelConfig) -> Option<PathBuf> {
    config
        .control_socket_path
        .as_deref()
        .and_then(|path| Path::new(path).parent())
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .map(|path| path.join("wavelinux6"))
        })
}

fn resolve_acceleration_config(
    status: &mut DspBackendStatus,
    runtime_root: Option<&Path>,
) -> Option<DspAccelerationConfig> {
    if !status.accelerated {
        return None;
    }
    let provider = match status.selected_provider {
        Some(DspProvider::Cuda) => wavelinux_accelerator::AcceleratorProvider::Cuda,
        Some(DspProvider::OpenVino) => wavelinux_accelerator::AcceleratorProvider::OpenVino,
        Some(DspProvider::MiGraphX) => wavelinux_accelerator::AcceleratorProvider::MiGraphX,
        _ => return None,
    };
    let Some(runtime_root) = runtime_root else {
        downgrade_acceleration_status(
            status,
            format!(
                "qualified {} provider has no private runtime directory; using CPU",
                provider.as_str()
            ),
        );
        return None;
    };
    let result = wavelinux_accelerator::load_qualified_provider_pack(provider)
        .map_err(|error| error.to_string())
        .and_then(|pack| {
            DspAccelerationConfig::new(
                pack,
                runtime_root.join("accelerators").join(provider.as_str()),
                DSP_ACCELERATOR_BLOCK_TIMEOUT,
            )
        });
    match result {
        Ok(config) => {
            eprintln!(
                "wavelinux6-audio-core accelerator_eligible provider={} deadline_msec={} runtime_dir={}",
                provider.as_str(),
                DSP_ACCELERATOR_BLOCK_TIMEOUT.as_millis(),
                runtime_root
                    .join("accelerators")
                    .join(provider.as_str())
                    .display()
            );
            Some(config)
        }
        Err(error) => {
            downgrade_acceleration_status(
                status,
                format!(
                    "qualified {} provider could not be loaded ({error}); using CPU",
                    provider.as_str()
                ),
            );
            None
        }
    }
}

fn downgrade_acceleration_status(status: &mut DspBackendStatus, reason: String) {
    let cpu_provider = status
        .probes
        .iter()
        .find(|probe| probe.provider == DspProvider::PortableCpu && probe.available)
        .map(|_| DspProvider::PortableCpu)
        .unwrap_or(DspProvider::PureCpu);
    let mut fallback = status
        .clone()
        .with_runtime_fallback(AudioRuntimeMode::DspCpu, reason.clone());
    fallback.selected_provider = Some(cpu_provider);
    fallback.provider_probe_failures.push(reason);
    *status = fallback;
}

fn run_probe() -> Result<(), String> {
    let report = ProbeReport {
        helper: "wavelinux6-audio-core",
        status: probe_backend_from_env(),
    };
    print_json(&report)
}

#[derive(Debug)]
struct AudioSlot {
    sequence: AtomicU64,
    stereo: AtomicU64,
}

impl Default for AudioSlot {
    fn default() -> Self {
        Self {
            sequence: AtomicU64::new(EMPTY_SEQUENCE),
            stereo: AtomicU64::new(pack_stereo([0.0, 0.0])),
        }
    }
}

/// Fixed-capacity single-producer audio history with independent readers.
///
/// The producer publishes monotonically numbered stereo frames. Public channel
/// and native mix streams can each read stable taps during latency transitions
/// without moving or rebuilding PipeWire nodes. Atomic samples avoid a Rust
/// data race if a severely delayed reader reaches a slot while it is overwritten.
#[derive(Debug)]
struct FixedAudioHistory {
    slots: Box<[AudioSlot]>,
    write_sequence: AtomicU64,
}

impl FixedAudioHistory {
    fn new(capacity_frames: usize) -> Self {
        let capacity_frames = capacity_frames.max(2).next_power_of_two();
        let slots = (0..capacity_frames)
            .map(|_| AudioSlot::default())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            write_sequence: AtomicU64::new(0),
        }
    }

    fn capacity(&self) -> u64 {
        self.slots.len() as u64
    }

    fn write_sequence(&self) -> u64 {
        self.write_sequence.load(Ordering::Acquire)
    }

    fn push(&self, frame: [f32; 2]) {
        let sequence = self.write_sequence.load(Ordering::Relaxed);
        let slot = &self.slots[sequence as usize & (self.slots.len() - 1)];
        slot.sequence.store(WRITING_SEQUENCE, Ordering::Release);
        slot.stereo.store(pack_stereo(frame), Ordering::Relaxed);
        slot.sequence.store(sequence, Ordering::Release);
        self.write_sequence
            .store(sequence.wrapping_add(1), Ordering::Release);
    }

    fn skip(&self, frames: u64) {
        if frames > 0 {
            self.write_sequence.fetch_add(frames, Ordering::AcqRel);
        }
    }

    fn get(&self, sequence: u64) -> Option<[f32; 2]> {
        let latest = self.write_sequence();
        if sequence >= latest || latest.saturating_sub(sequence) > self.capacity() {
            return None;
        }
        let slot = &self.slots[sequence as usize & (self.slots.len() - 1)];
        if slot.sequence.load(Ordering::Acquire) != sequence {
            return None;
        }
        let frame = unpack_stereo(slot.stereo.load(Ordering::Relaxed));
        (slot.sequence.load(Ordering::Acquire) == sequence).then_some(frame)
    }

    fn get_interpolated(&self, sequence: u64, fraction: f64) -> Option<[f32; 2]> {
        let first = self.get(sequence)?;
        let fraction = fraction.clamp(0.0, 1.0);
        if fraction <= f64::EPSILON {
            return Some(first);
        }
        let second = self.get(sequence.saturating_add(1)).unwrap_or(first);
        let fraction = fraction as f32;
        Some([
            first[0] + (second[0] - first[0]) * fraction,
            first[1] + (second[1] - first[1]) * fraction,
        ])
    }

    fn aligned_latency_sequence(
        &self,
        reference_sequence: u64,
        desired_sequence: u64,
        fraction: f64,
    ) -> u64 {
        let latest = self.write_sequence();
        let oldest = latest.saturating_sub(self.capacity());
        let lower = desired_sequence
            .saturating_sub(LATENCY_ALIGNMENT_SEARCH_FRAMES)
            .max(oldest);
        let upper = desired_sequence
            .saturating_add(LATENCY_ALIGNMENT_SEARCH_FRAMES)
            .min(latest.saturating_sub(LATENCY_ALIGNMENT_COMPARE_FRAMES));
        if lower > upper {
            return desired_sequence;
        }

        let mut reference_sum = 0.0_f64;
        let mut reference_square_sum = 0.0_f64;
        for offset in 0..LATENCY_ALIGNMENT_COMPARE_FRAMES {
            let Some(reference) =
                self.get_interpolated(reference_sequence.saturating_add(offset), fraction)
            else {
                return desired_sequence;
            };
            for sample in reference {
                let sample = f64::from(sample);
                reference_sum += sample;
                reference_square_sum += sample * sample;
            }
        }
        let sample_count = (LATENCY_ALIGNMENT_COMPARE_FRAMES * 2) as f64;
        let reference_variance =
            reference_square_sum - reference_sum * reference_sum / sample_count;
        if reference_variance <= 1.0e-12 || !reference_variance.is_finite() {
            return desired_sequence;
        }

        let mut best_sequence = desired_sequence;
        let mut best_score = f64::INFINITY;
        for candidate in lower..=upper {
            let mut candidate_sum = 0.0_f64;
            let mut candidate_square_sum = 0.0_f64;
            let mut cross_sum = 0.0_f64;
            let mut valid = true;
            for offset in 0..LATENCY_ALIGNMENT_COMPARE_FRAMES {
                let Some(reference) =
                    self.get_interpolated(reference_sequence.saturating_add(offset), fraction)
                else {
                    valid = false;
                    break;
                };
                let Some(sample) =
                    self.get_interpolated(candidate.saturating_add(offset), fraction)
                else {
                    valid = false;
                    break;
                };
                for channel in 0..2 {
                    let reference = f64::from(reference[channel]);
                    let sample = f64::from(sample[channel]);
                    candidate_sum += sample;
                    candidate_square_sum += sample * sample;
                    cross_sum += reference * sample;
                }
            }
            if !valid {
                continue;
            }
            let distance = candidate.abs_diff(desired_sequence);
            let candidate_variance =
                candidate_square_sum - candidate_sum * candidate_sum / sample_count;
            if candidate_variance <= 1.0e-12 || !candidate_variance.is_finite() {
                continue;
            }
            let covariance = cross_sum - reference_sum * candidate_sum / sample_count;
            let correlation =
                (covariance / (reference_variance * candidate_variance).sqrt()).clamp(-1.0, 1.0);
            let score = 1.0 - correlation + distance as f64 * 1.0e-6;
            if score < best_score {
                best_score = score;
                best_sequence = candidate;
            }
        }
        best_sequence
    }
}

fn pack_stereo(frame: [f32; 2]) -> u64 {
    u64::from(frame[0].to_bits()) | (u64::from(frame[1].to_bits()) << 32)
}

fn unpack_stereo(packed: u64) -> [f32; 2] {
    [
        f32::from_bits(packed as u32),
        f32::from_bits((packed >> 32) as u32),
    ]
}

#[derive(Debug, Clone, Copy, Default)]
struct NativeMeterSnapshot {
    peak_left: f32,
    peak_right: f32,
    rms_left: f32,
    rms_right: f32,
    age_micros: u64,
    frames: u64,
}

#[derive(Debug)]
struct NativeMeter {
    peak_left: AtomicU32,
    peak_right: AtomicU32,
    rms_left: AtomicU32,
    rms_right: AtomicU32,
    frames: AtomicU64,
    updated_micros: AtomicU64,
    clock_started_at: Instant,
}

impl Default for NativeMeter {
    fn default() -> Self {
        Self {
            peak_left: AtomicU32::new(0.0_f32.to_bits()),
            peak_right: AtomicU32::new(0.0_f32.to_bits()),
            rms_left: AtomicU32::new(0.0_f32.to_bits()),
            rms_right: AtomicU32::new(0.0_f32.to_bits()),
            frames: AtomicU64::new(0),
            updated_micros: AtomicU64::new(0),
            clock_started_at: Instant::now(),
        }
    }
}

impl NativeMeter {
    fn publish(
        &self,
        peak_left: f32,
        peak_right: f32,
        rms_left: f32,
        rms_right: f32,
        frames: usize,
    ) {
        self.peak_left
            .store(finite_meter_peak(peak_left).to_bits(), Ordering::Relaxed);
        self.peak_right
            .store(finite_meter_peak(peak_right).to_bits(), Ordering::Relaxed);
        self.rms_left
            .store(finite_meter_peak(rms_left).to_bits(), Ordering::Relaxed);
        self.rms_right
            .store(finite_meter_peak(rms_right).to_bits(), Ordering::Relaxed);
        self.frames.fetch_add(frames as u64, Ordering::Relaxed);
        let updated_micros = duration_micros(self.clock_started_at.elapsed()).saturating_add(1);
        self.updated_micros.store(updated_micros, Ordering::Release);
    }

    fn snapshot(&self) -> NativeMeterSnapshot {
        let updated_micros = self.updated_micros.load(Ordering::Acquire);
        if updated_micros == 0 {
            return NativeMeterSnapshot::default();
        }
        let now_micros = duration_micros(self.clock_started_at.elapsed());
        let age_micros = now_micros.saturating_sub(updated_micros.saturating_sub(1));
        let release = native_meter_release(age_micros);
        NativeMeterSnapshot {
            peak_left: finite_meter_peak(f32::from_bits(self.peak_left.load(Ordering::Relaxed)))
                * release,
            peak_right: finite_meter_peak(f32::from_bits(self.peak_right.load(Ordering::Relaxed)))
                * release,
            rms_left: finite_meter_peak(f32::from_bits(self.rms_left.load(Ordering::Relaxed)))
                * release,
            rms_right: finite_meter_peak(f32::from_bits(self.rms_right.load(Ordering::Relaxed)))
                * release,
            age_micros,
            frames: self.frames.load(Ordering::Relaxed),
        }
    }
}

fn native_meter_release(age_micros: u64) -> f32 {
    let stale_micros = age_micros.saturating_sub(duration_micros(NATIVE_METER_STALE_AFTER));
    if stale_micros == 0 {
        1.0
    } else {
        NATIVE_METER_RELEASE_PER_SECOND.powf(stale_micros as f32 / 1_000_000.0)
    }
}

fn finite_meter_peak(value: f32) -> f32 {
    if value.is_finite() {
        value.abs().clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

#[derive(Debug, Default)]
struct NativeStats {
    captured_frames: AtomicU64,
    rendered_frames: AtomicU64,
    dropped_frames: AtomicU64,
    underrun_frames: AtomicU64,
    capture_callbacks: AtomicU64,
    worker_blocks: AtomicU64,
    worker_overrun_frames: AtomicU64,
    process_calls: AtomicU64,
    last_process_micros: AtomicU64,
    max_process_micros: AtomicU64,
    chain_swaps: AtomicU64,
    non_finite_blocks: AtomicU64,
    non_finite_samples: AtomicU64,
    non_finite_effect_mask: AtomicU64,
    chain_recoveries: AtomicU64,
    rate_correction_bits: AtomicU64,
    rt_callback_timing: RealtimeTimingStats,
}

#[derive(Debug)]
struct RealtimeTimingStats {
    count: AtomicU64,
    max_micros: AtomicU64,
    histogram: [AtomicU64; RT_CALLBACK_BUCKETS],
}

impl Default for RealtimeTimingStats {
    fn default() -> Self {
        Self {
            count: AtomicU64::new(0),
            max_micros: AtomicU64::new(0),
            histogram: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl RealtimeTimingStats {
    fn record(&self, elapsed: Duration) {
        let micros = duration_micros(elapsed);
        let bucket = micros
            .saturating_sub(1)
            .checked_div(RT_CALLBACK_BUCKET_MICROS)
            .unwrap_or(0)
            .min((RT_CALLBACK_BUCKETS - 1) as u64) as usize;
        self.histogram[bucket].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
        self.max_micros.fetch_max(micros, Ordering::Relaxed);
    }

    fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    fn max_micros(&self) -> u64 {
        self.max_micros.load(Ordering::Relaxed)
    }

    fn p99_micros(&self) -> u64 {
        let count = self.count();
        if count == 0 {
            return 0;
        }
        let target = count.saturating_mul(99).saturating_add(99) / 100;
        let mut cumulative = 0_u64;
        for (index, bucket) in self.histogram.iter().enumerate() {
            cumulative = cumulative.saturating_add(bucket.load(Ordering::Relaxed));
            if cumulative >= target {
                return (index as u64 + 1).saturating_mul(RT_CALLBACK_BUCKET_MICROS);
            }
        }
        (RT_CALLBACK_BUCKETS as u64).saturating_mul(RT_CALLBACK_BUCKET_MICROS)
    }
}

#[derive(Debug)]
struct PreparedChain {
    chain: DspChain,
    generation: u64,
    input_mode: DspInputMode,
}

#[derive(Debug, Default)]
struct ChainSwapControl {
    pending: AtomicPtr<PreparedChain>,
    submitted_generation: AtomicU64,
    acknowledged_generation: AtomicU64,
    replacements: AtomicU64,
    retired_overflows: AtomicU64,
}

impl ChainSwapControl {
    fn next_generation(&self) -> u64 {
        self.submitted_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn reserve_generation(&self, generation: u64) -> bool {
        let mut current = self.submitted_generation.load(Ordering::Acquire);
        while generation > current {
            match self.submitted_generation.compare_exchange_weak(
                current,
                generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(next) => current = next,
            }
        }
        false
    }

    fn submit(&self, chain: DspChain, generation: u64, input_mode: DspInputMode) {
        let prepared = Box::into_raw(Box::new(PreparedChain {
            chain,
            generation,
            input_mode,
        }));
        let replaced = self.pending.swap(prepared, Ordering::AcqRel);
        if !replaced.is_null() {
            self.replacements.fetch_add(1, Ordering::Relaxed);
            unsafe { drop(Box::from_raw(replaced)) };
        }
    }

    fn take_pending(&self) -> Option<Box<PreparedChain>> {
        let pending = self.pending.swap(std::ptr::null_mut(), Ordering::AcqRel);
        (!pending.is_null()).then(|| unsafe { Box::from_raw(pending) })
    }

    fn acknowledge(&self, generation: u64) {
        self.acknowledged_generation
            .store(generation, Ordering::Release);
    }
}

impl Drop for ChainSwapControl {
    fn drop(&mut self) {
        let pending = *self.pending.get_mut();
        if !pending.is_null() {
            unsafe { drop(Box::from_raw(pending)) };
        }
        *self.pending.get_mut() = std::ptr::null_mut();
    }
}

#[derive(Debug)]
struct PendingInputTarget {
    generation: u64,
    target: Option<String>,
}

#[derive(Debug)]
enum InputTargetTransitionState {
    Priming {
        started_at: Instant,
        baseline_frames: u64,
    },
    Switching {
        started_at: Instant,
        boundary_sequence: u64,
        baseline_frames: u64,
    },
}

#[derive(Debug)]
struct StagedInputTarget {
    request: PendingInputTarget,
    previous: Option<String>,
    state: InputTargetTransitionState,
}

#[derive(Debug)]
struct InputTargetControl {
    submitted_generation: AtomicU64,
    applied_generation: AtomicU64,
    current_target: Mutex<Option<String>>,
    pending: Mutex<Option<PendingInputTarget>>,
    last_error: Mutex<Option<String>>,
}

impl InputTargetControl {
    fn new(initial_target: Option<String>) -> Self {
        Self {
            submitted_generation: AtomicU64::new(1),
            applied_generation: AtomicU64::new(1),
            current_target: Mutex::new(initial_target),
            pending: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }

    fn queue(
        &self,
        target: Option<String>,
        requested_generation: Option<u64>,
    ) -> Result<u64, String> {
        let target = match target {
            Some(target) => {
                let target = target.trim().to_string();
                if target.is_empty() {
                    return Err("input target must not be empty".into());
                }
                Some(target)
            }
            None => None,
        };
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| "input target queue lock poisoned".to_string())?;
        if let Some(existing) = pending.as_ref().filter(|pending| pending.target == target) {
            return Ok(existing.generation);
        }
        let current_matches = *self
            .current_target
            .lock()
            .map_err(|_| "input target state lock poisoned".to_string())?
            == target;
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
        *pending = Some(PendingInputTarget { generation, target });
        self.submitted_generation
            .store(generation, Ordering::Release);
        if let Ok(mut error) = self.last_error.lock() {
            *error = None;
        }
        Ok(generation)
    }

    fn take_pending(&self) -> Option<PendingInputTarget> {
        self.pending.lock().ok()?.take()
    }

    fn acknowledge(&self, request: &PendingInputTarget) {
        if let Ok(mut current) = self.current_target.lock() {
            *current = request.target.clone();
        }
        if let Ok(mut error) = self.last_error.lock() {
            *error = None;
        }
        self.applied_generation
            .store(request.generation, Ordering::Release);
    }

    fn reject(&self, _request: &PendingInputTarget, error: String) {
        if let Ok(mut last_error) = self.last_error.lock() {
            *last_error = Some(error);
        }
    }

    fn current_target(&self) -> Option<String> {
        self.current_target.lock().ok()?.clone()
    }

    fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok()?.clone()
    }
}

#[derive(Debug)]
struct NativeShared {
    channel_id: String,
    core_topology_revision: String,
    raw_history: FixedAudioHistory,
    history: FixedAudioHistory,
    meter: NativeMeter,
    stats: NativeStats,
    capture_streaming: AtomicBool,
    sample_rate_hz: u32,
    render_quantum_frames: usize,
    target_latency_msec: AtomicUsize,
    target_latency_frames: AtomicUsize,
    current_buffer_frames: AtomicU64,
    worker_read_sequence: AtomicU64,
    worker_running: AtomicBool,
    acceleration_config: Option<DspAccelerationConfig>,
    acceleration_metrics: Mutex<DspAccelerationMetrics>,
    last_latency_reason: Mutex<String>,
    chain_control: Arc<ChainSwapControl>,
    chain_config: Mutex<DspChannelConfig>,
    input_target_control: InputTargetControl,
    input_route_change_sequence: AtomicU64,
    recovery_requested: AtomicBool,
    recovery_in_progress: AtomicBool,
}

impl NativeShared {
    fn new(
        config: &DspChannelConfig,
        acceleration_config: Option<DspAccelerationConfig>,
        core_topology_revision: &str,
    ) -> Self {
        let adaptive = &config.adaptive_latency;
        let max_msec = adaptive.max_msec.max(adaptive.min_msec).max(28);
        let min_msec = adaptive.min_msec.min(max_msec).max(5);
        let target_frames = msec_to_frames(min_msec, config.sample_rate_hz);
        let capacity_frames = msec_to_frames(max_msec.saturating_mul(2), config.sample_rate_hz)
            .max(config.latency_frames.max(256) as usize * 8);
        let chain_control = Arc::new(ChainSwapControl::default());
        chain_control
            .submitted_generation
            .store(config.generation, Ordering::Relaxed);
        chain_control
            .acknowledged_generation
            .store(config.generation, Ordering::Relaxed);
        Self {
            channel_id: config.channel_id.clone(),
            core_topology_revision: core_topology_revision.to_string(),
            raw_history: FixedAudioHistory::new(
                capacity_frames.max(MAX_NATIVE_CALLBACK_FRAMES.saturating_mul(2)),
            ),
            history: FixedAudioHistory::new(capacity_frames),
            meter: NativeMeter::default(),
            stats: NativeStats::default(),
            capture_streaming: AtomicBool::new(false),
            sample_rate_hz: config.sample_rate_hz,
            render_quantum_frames: config.latency_frames.max(1) as usize,
            target_latency_msec: AtomicUsize::new(min_msec as usize),
            target_latency_frames: AtomicUsize::new(target_frames),
            current_buffer_frames: AtomicU64::new(0),
            worker_read_sequence: AtomicU64::new(0),
            worker_running: AtomicBool::new(false),
            acceleration_config,
            acceleration_metrics: Mutex::new(DspAccelerationMetrics::default()),
            last_latency_reason: Mutex::new("initial".into()),
            chain_control,
            chain_config: Mutex::new(config.clone()),
            input_target_control: InputTargetControl::new(config.input_target_node_name.clone()),
            input_route_change_sequence: AtomicU64::new(0),
            recovery_requested: AtomicBool::new(false),
            recovery_in_progress: AtomicBool::new(false),
        }
    }

    fn set_target_latency(&self, target_msec: u16, reason: &str) {
        let target_msec = target_msec.clamp(5, 500);
        self.target_latency_msec
            .store(target_msec as usize, Ordering::Relaxed);
        self.target_latency_frames.store(
            msec_to_frames(target_msec, self.sample_rate_hz),
            Ordering::Relaxed,
        );
        if let Ok(mut last_reason) = self.last_latency_reason.lock() {
            *last_reason = reason.to_string();
        }
    }
}

struct NativeCaptureData {
    format: spa::param::audio::AudioInfoRaw,
    shared: Arc<NativeShared>,
    scratch: Box<[f32]>,
    endpoint_status: Arc<InputEndpointStatus>,
}

struct DspWorkerData {
    active_chain: Box<PreparedChain>,
    shadow_chain: Option<Box<PreparedChain>>,
    chain_crossfade_progress: usize,
    chain_crossfade_total: usize,
    shared: Arc<NativeShared>,
    read_sequence: u64,
    raw_scratch: Box<[f32]>,
    active_scratch: Box<[f32]>,
    active_dry_scratch: Box<[f32]>,
    shadow_scratch: Box<[f32]>,
    shadow_dry_scratch: Box<[f32]>,
    acceleration_metrics_countdown: u8,
}

struct DspWorkerHandle {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl DspWorkerHandle {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for DspWorkerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Default)]
struct InputEndpointStatus {
    connected: AtomicBool,
    streaming: AtomicBool,
    failed: AtomicBool,
    processed_frames: AtomicU64,
}

impl InputEndpointStatus {
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

#[derive(Default)]
struct NativeDiscardCaptureData {
    endpoint_status: Option<Arc<InputEndpointStatus>>,
}

#[derive(Debug, Clone, Copy)]
struct LatencyTransition {
    from_sequence: u64,
    to_sequence: u64,
    from_fraction: f64,
    to_fraction: f64,
    progress_frames: usize,
    total_frames: usize,
}

#[derive(Debug, Clone, Copy)]
struct DiscontinuityRecovery {
    from_frame: [f32; 2],
    to_sequence: u64,
    progress_frames: usize,
    total_frames: usize,
}

struct NativePlaybackData {
    shared: Arc<NativeShared>,
    last_frame: [f32; 2],
    read_sequence: Option<u64>,
    observed_input_route_generation: u64,
    applied_target_frames: usize,
    frames_since_recenter: usize,
    transition: Option<LatencyTransition>,
    recovery: Option<DiscontinuityRecovery>,
    rate_match: *mut spa::sys::spa_io_rate_match,
    rate_correction: f64,
}

struct NativeChannelRuntime<'core> {
    // Listeners must be dropped before the streams they are registered on.
    _capture_listener: pw::stream::StreamListener<NativeCaptureData>,
    _prime_capture_listener: Option<pw::stream::StreamListener<NativeDiscardCaptureData>>,
    _compatibility_listener: Option<pw::stream::StreamListener<NativeDiscardCaptureData>>,
    _playback_listener: pw::stream::StreamListener<NativePlaybackData>,
    capture_stream: pw::stream::StreamBox<'core>,
    prime_capture_stream: Option<pw::stream::StreamBox<'core>>,
    _compatibility_stream: Option<pw::stream::StreamBox<'core>>,
    _playback_stream: pw::stream::StreamBox<'core>,
    shared: Arc<NativeShared>,
    channel_id: String,
    capture_uses_target: bool,
    sample_rate_hz: u32,
    property_prefix: String,
    main_input_status: Arc<InputEndpointStatus>,
    prime_input_status: Option<Arc<InputEndpointStatus>>,
    staged_input_target: Option<StagedInputTarget>,
    dsp_worker: DspWorkerHandle,
}

impl NativeChannelRuntime<'_> {
    fn apply_pending_input_target(&mut self) {
        if let Some(request) = self.shared.input_target_control.take_pending() {
            self.cancel_staged_input_target();
            if !self.capture_uses_target {
                self.reject_input_target(
                    &request,
                    "channel uses a public sink and cannot accept a hardware input target".into(),
                );
                return;
            }

            let previous = self.shared.input_target_control.current_target();
            if previous == request.target {
                self.shared.input_target_control.acknowledge(&request);
                return;
            }
            let Some(target) = request.target.as_deref() else {
                self.disconnect_prime_input();
                self.disconnect_main_input();
                self.shared
                    .input_route_change_sequence
                    .store(self.shared.raw_history.write_sequence(), Ordering::Release);
                self.shared.input_target_control.acknowledge(&request);
                eprintln!(
                    "wavelinux6-audio-core input_target_applied channel_id={} generation={} previous={} target=<none>",
                    self.channel_id,
                    request.generation,
                    previous.as_deref().unwrap_or("<none>"),
                );
                return;
            };
            match self.connect_prime_input_target(target) {
                Ok(baseline_frames) => {
                    eprintln!(
                        "wavelinux6-audio-core input_target_priming channel_id={} generation={} previous={} target={}",
                        self.channel_id,
                        request.generation,
                        previous.as_deref().unwrap_or("<none>"),
                        target,
                    );
                    self.staged_input_target = Some(StagedInputTarget {
                        request,
                        previous,
                        state: InputTargetTransitionState::Priming {
                            started_at: Instant::now(),
                            baseline_frames,
                        },
                    });
                }
                Err(error) => self.reject_input_target(&request, error),
            }
            return;
        }

        let Some(mut staged) = self.staged_input_target.take() else {
            return;
        };
        match staged.state {
            InputTargetTransitionState::Priming {
                started_at,
                baseline_frames,
            } => match self.input_endpoint_ready(
                self.prime_input_status.as_deref(),
                baseline_frames,
                "prime input",
            ) {
                Ok(true) => {
                    let boundary_sequence = self.shared.raw_history.write_sequence();
                    self.shared
                        .capture_streaming
                        .store(false, Ordering::Release);
                    let target = staged
                        .request
                        .target
                        .as_deref()
                        .expect("priming requires an input target");
                    match self.reconnect_capture_target(target) {
                        Ok(()) => {
                            staged.state = InputTargetTransitionState::Switching {
                                started_at: Instant::now(),
                                boundary_sequence,
                                baseline_frames: self
                                    .main_input_status
                                    .processed_frames
                                    .load(Ordering::Acquire),
                            };
                            self.staged_input_target = Some(staged);
                        }
                        Err(error) => self.fail_input_transition(staged, error, true),
                    }
                }
                Ok(false) if started_at.elapsed() < INPUT_TARGET_TRANSITION_TIMEOUT => {
                    self.staged_input_target = Some(staged);
                }
                Ok(false) => self.fail_input_transition(
                    staged,
                    format!(
                        "input target did not prime within {} ms",
                        INPUT_TARGET_TRANSITION_TIMEOUT.as_millis()
                    ),
                    false,
                ),
                Err(error) => self.fail_input_transition(staged, error, false),
            },
            InputTargetTransitionState::Switching {
                started_at,
                boundary_sequence,
                baseline_frames,
            } => match self.input_endpoint_ready(
                Some(self.main_input_status.as_ref()),
                baseline_frames,
                "main input",
            ) {
                Ok(true) => {
                    self.disconnect_prime_input();
                    self.shared
                        .input_route_change_sequence
                        .store(boundary_sequence, Ordering::Release);
                    self.shared
                        .input_target_control
                        .acknowledge(&staged.request);
                    eprintln!(
                        "wavelinux6-audio-core input_target_applied channel_id={} generation={} previous={} target={}",
                        self.channel_id,
                        staged.request.generation,
                        staged.previous.as_deref().unwrap_or("<none>"),
                        display_input_target(staged.request.target.as_deref()),
                    );
                }
                Ok(false) if started_at.elapsed() < INPUT_TARGET_TRANSITION_TIMEOUT => {
                    self.staged_input_target = Some(staged);
                }
                Ok(false) => self.fail_input_transition(
                    staged,
                    format!(
                        "input target did not activate within {} ms",
                        INPUT_TARGET_TRANSITION_TIMEOUT.as_millis()
                    ),
                    true,
                ),
                Err(error) => self.fail_input_transition(staged, error, true),
            },
        }
    }

    fn reconnect_capture_target(&self, target: &str) -> Result<(), String> {
        // Disconnect is idempotent for route recovery: a failed connection
        // leaves the stream unconnected before the previous target is restored.
        let _ = self.capture_stream.disconnect();
        self.main_input_status.reset();
        update_stream_target_properties(&self.capture_stream, &self.property_prefix, target)?;
        let format = audio_format_pod_bytes(self.sample_rate_hz)?;
        let mut params = [Pod::from_bytes(&format)
            .ok_or_else(|| "native DSP capture format pod was invalid".to_string())?];
        self.capture_stream
            .connect(
                spa::utils::Direction::Input,
                None,
                native_stream_flags(),
                &mut params,
            )
            .map_err(|err| format!("capture reconnect failed: {err}"))
    }

    fn connect_prime_input_target(&self, target: &str) -> Result<u64, String> {
        let stream = self
            .prime_capture_stream
            .as_ref()
            .ok_or_else(|| "input target prime stream is unavailable".to_string())?;
        let status = self
            .prime_input_status
            .as_ref()
            .ok_or_else(|| "input target prime status is unavailable".to_string())?;
        let _ = stream.disconnect();
        status.reset();
        update_stream_target_properties(stream, &self.property_prefix, target)?;
        let format = audio_format_pod_bytes(self.sample_rate_hz)?;
        let mut params = [Pod::from_bytes(&format)
            .ok_or_else(|| "native DSP prime capture format pod was invalid".to_string())?];
        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                native_stream_flags(),
                &mut params,
            )
            .map_err(|err| format!("prime capture connect failed: {err}"))?;
        Ok(status.processed_frames.load(Ordering::Acquire))
    }

    fn input_endpoint_ready(
        &self,
        status: Option<&InputEndpointStatus>,
        baseline_frames: u64,
        label: &str,
    ) -> Result<bool, String> {
        let status = status.ok_or_else(|| format!("{label} status is unavailable"))?;
        if status.failed.load(Ordering::Acquire) {
            return Err(format!("{label} entered an error state"));
        }
        if !status.connected.load(Ordering::Acquire) || !status.streaming.load(Ordering::Acquire) {
            return Ok(false);
        }
        let prime_frames = (u64::from(self.sample_rate_hz) * INPUT_TARGET_PRIME_MSEC / 1000).max(1);
        if status
            .processed_frames
            .load(Ordering::Acquire)
            .saturating_sub(baseline_frames)
            < prime_frames
        {
            return Ok(false);
        }
        Ok(true)
    }

    fn disconnect_main_input(&self) {
        self.shared
            .capture_streaming
            .store(false, Ordering::Release);
        let _ = self.capture_stream.disconnect();
        self.main_input_status.reset();
    }

    fn disconnect_prime_input(&self) {
        if let Some(stream) = self.prime_capture_stream.as_ref() {
            let _ = stream.disconnect();
        }
        if let Some(status) = self.prime_input_status.as_ref() {
            status.reset();
        }
    }

    fn cancel_staged_input_target(&mut self) {
        let Some(staged) = self.staged_input_target.take() else {
            return;
        };
        let main_changed = matches!(staged.state, InputTargetTransitionState::Switching { .. });
        self.disconnect_prime_input();
        if main_changed {
            self.restore_input_target(staged.previous.as_deref());
        }
    }

    fn fail_input_transition(&self, staged: StagedInputTarget, error: String, main_changed: bool) {
        self.disconnect_prime_input();
        if main_changed {
            self.restore_input_target(staged.previous.as_deref());
        }
        self.reject_input_target(&staged.request, error);
    }

    fn restore_input_target(&self, target: Option<&str>) {
        match target {
            Some(target) => {
                let _ = self.reconnect_capture_target(target);
            }
            None => self.disconnect_main_input(),
        }
    }

    fn reject_input_target(&self, request: &PendingInputTarget, error: String) {
        self.shared
            .input_target_control
            .reject(request, error.clone());
        eprintln!(
            "wavelinux6-audio-core input_target_failed channel_id={} generation={} target={} error={}",
            self.channel_id,
            request.generation,
            display_input_target(request.target.as_deref()),
            error,
        );
    }
}

fn display_input_target(target: Option<&str>) -> &str {
    target.unwrap_or("<none>")
}

fn update_stream_target_properties(
    stream: &pw::stream::Stream,
    property_prefix: &str,
    target: &str,
) -> Result<(), String> {
    let mut props = properties! {
        *pw::keys::TARGET_OBJECT => target,
    };
    props.insert(format!("{}.target_node", property_prefix), target);
    let result = unsafe {
        pw::sys::pw_stream_update_properties(stream.as_raw_ptr(), props.dict().as_raw_ptr())
    };
    if result < 0 {
        Err(format!(
            "PipeWire property update failed with status {result}"
        ))
    } else {
        Ok(())
    }
}

fn native_stream_flags() -> pw::stream::StreamFlags {
    pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS
}

fn run_pipewire_native_graph(
    config: DspChannelConfig,
    status: DspBackendStatus,
    acceleration: Option<DspAccelerationConfig>,
) -> Result<(), String> {
    run_pipewire_native_core(
        vec![config],
        Vec::new(),
        None,
        None,
        String::new(),
        status,
        acceleration,
    )
}

fn run_pipewire_native_core(
    configs: Vec<DspChannelConfig>,
    mix_configs: Vec<wavelinux_dsp::DspMixConfig>,
    control_socket_path: Option<String>,
    meter_socket_path: Option<String>,
    core_topology_revision: String,
    _status: DspBackendStatus,
    acceleration: Option<DspAccelerationConfig>,
) -> Result<(), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|err| format!("PipeWire native DSP mainloop creation failed: {err}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|err| format!("PipeWire native DSP context creation failed: {err}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|err| format!("PipeWire native DSP core connection failed: {err}"))?;
    let mut channels = configs
        .into_iter()
        .map(|config| {
            prepare_native_channel(&core, config, acceleration.clone(), &core_topology_revision)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let channel_map = channels
        .iter()
        .map(|channel| (channel.channel_id.clone(), Arc::clone(&channel.shared)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut mixes = mix_configs
        .into_iter()
        .map(|config| native_mix::prepare_native_mix(&core, config, &channel_map))
        .collect::<Result<Vec<_>, _>>()?;
    let registry = Arc::new(native_mix::NativeMixRegistry::new(&mixes, &channel_map));
    if let Some(socket_path) = control_socket_path.filter(|_| !mixes.is_empty()) {
        native_mix::start_mix_control_socket(socket_path, Arc::clone(&registry));
    }
    if let Some(socket_path) = meter_socket_path {
        native_mix::start_meter_stream_socket(socket_path, Arc::clone(&registry));
    }

    let mut last_log = Instant::now();
    while !TERMINATE.load(Ordering::SeqCst) {
        mainloop.loop_().iterate(CONTROL_MAINLOOP_POLL_INTERVAL);
        for channel in &mut channels {
            channel.apply_pending_input_target();
            recover_poisoned_chain(&channel.shared);
        }
        for mix in &mut mixes {
            mix.apply_pending_latency_quantum();
            mix.apply_pending_output_targets();
            mix.reap_retired_output_targets();
        }
        if last_log.elapsed() >= Duration::from_secs(30) {
            for channel in &channels {
                log_native_stats(&channel.shared);
            }
            for mix in &mixes {
                native_mix::log_mix_stats(&mix.shared);
            }
            last_log = Instant::now();
        }
    }
    for channel in &mut channels {
        channel.dsp_worker.stop();
    }
    for channel in &channels {
        log_native_stats(&channel.shared);
        eprintln!(
            "wavelinux6-audio-core native_stop channel_id={}",
            channel.channel_id
        );
    }
    for mix in &mixes {
        native_mix::log_mix_stats(&mix.shared);
    }
    Ok(())
}

fn prepare_native_channel<'core>(
    core: &'core pw::core::Core,
    config: DspChannelConfig,
    acceleration: Option<DspAccelerationConfig>,
    core_topology_revision: &str,
) -> Result<NativeChannelRuntime<'core>, String> {
    let chain = build_replacement_chain(&config, acceleration.as_ref())
        .map_err(|error| format!("native DSP chain initialization failed: {error}"))?;
    let shared = Arc::new(NativeShared::new(
        &config,
        acceleration,
        core_topology_revision,
    ));
    let dsp_worker = start_dsp_worker(
        Arc::clone(&shared),
        PreparedChain {
            chain,
            generation: config.generation,
            input_mode: config.input_mode,
        },
    )?;
    start_latency_control_socket(&config, Arc::clone(&shared));

    let mut public_capture_props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_CLASS => "Audio/Sink",
        *pw::keys::NODE_NAME => config.input_node_name.clone(),
        *pw::keys::NODE_DESCRIPTION => format!("{} FX {} Input", config.app_name, config.channel_name),
        *pw::keys::NODE_NICK => format!("{} FX Input", config.app_name),
        *pw::keys::MEDIA_NAME => format!("{} FX {} Input", config.app_name, config.channel_name),
        *pw::keys::NODE_VIRTUAL => "true",
    };
    public_capture_props.insert(
        "module-stream-restore.id",
        native_stream_restore_id(&config, "capture"),
    );
    let input_role = config.input_role.as_deref().unwrap_or("effect_input");
    insert_common_native_props(&mut public_capture_props, &config, input_role);

    let capture_uses_target = channel_uses_input_target(&config);
    apply_capture_idle_policy(&mut public_capture_props, capture_uses_target);
    let capture_props = if capture_uses_target {
        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_CLASS => "Stream/Input/Audio",
            *pw::keys::NODE_NAME => native_input_target_node_name(&config),
            *pw::keys::NODE_DESCRIPTION => format!("{} {} hardware input", config.app_name, config.channel_name),
            *pw::keys::NODE_NICK => format!("{} hardware input", config.channel_name),
            *pw::keys::MEDIA_NAME => format!("{} {} hardware input", config.app_name, config.channel_name),
            *pw::keys::NODE_VIRTUAL => "true",
        };
        props.insert(
            "module-stream-restore.id",
            native_stream_restore_id(&config, "input-target"),
        );
        props.insert("node.dont-fallback", "true");
        props.insert("node.linger", "true");
        props.insert("node.hidden", "true");
        if let Some(target) = config.input_target_node_name.as_deref() {
            props.insert(*pw::keys::TARGET_OBJECT, target);
            props.insert(format!("{}.target_node", config.property_prefix), target);
        }
        insert_common_native_props(&mut props, &config, "input_target");
        props
    } else {
        public_capture_props.to_owned()
    };
    let prime_capture_props = capture_uses_target.then(|| {
        let mut props = capture_props.to_owned();
        props.insert(
            *pw::keys::NODE_NAME,
            format!(
                "{}-input-target-prime-{}",
                config.graph_prefix, config.channel_id
            ),
        );
        props.insert(
            *pw::keys::NODE_DESCRIPTION,
            format!(
                "{} {} input transition",
                config.app_name, config.channel_name
            ),
        );
        props.insert(
            "module-stream-restore.id",
            native_stream_restore_id(&config, "input-target-prime"),
        );
        insert_common_native_props(&mut props, &config, "input_target_prime");
        props
    });

    let mut playback_props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_CLASS => "Audio/Source",
        *pw::keys::NODE_NAME => config.output_node_name.clone(),
        *pw::keys::NODE_DESCRIPTION => format!("{} FX {} Output", config.app_name, config.channel_name),
        *pw::keys::NODE_NICK => format!("{} FX Output", config.app_name),
        *pw::keys::MEDIA_NAME => format!("{} FX {} Output", config.app_name, config.channel_name),
        *pw::keys::NODE_VIRTUAL => "true",
    };
    playback_props.insert(
        "module-stream-restore.id",
        native_stream_restore_id(&config, "playback"),
    );
    let output_role = config.output_role.as_deref().unwrap_or("effect_output");
    insert_common_native_props(&mut playback_props, &config, output_role);

    let capture_stream = pw::stream::StreamBox::new(
        core,
        &format!("{}-dsp-capture-{}", config.graph_prefix, config.channel_id),
        capture_props,
    )
    .map_err(|err| format!("PipeWire native DSP capture stream creation failed: {err}"))?;
    let prime_capture_stream = prime_capture_props
        .map(|props| {
            pw::stream::StreamBox::new(
                core,
                &format!(
                    "{}-dsp-capture-prime-{}",
                    config.graph_prefix, config.channel_id
                ),
                props,
            )
        })
        .transpose()
        .map_err(|err| format!("PipeWire native prime capture stream creation failed: {err}"))?;
    let compatibility_stream = capture_uses_target
        .then(|| {
            pw::stream::StreamBox::new(
                core,
                &format!("{}-channel-sink-{}", config.graph_prefix, config.channel_id),
                public_capture_props,
            )
        })
        .transpose()
        .map_err(|err| format!("PipeWire native compatibility sink creation failed: {err}"))?;
    let playback_stream = pw::stream::StreamBox::new(
        core,
        &format!("{}-dsp-playback-{}", config.graph_prefix, config.channel_id),
        playback_props,
    )
    .map_err(|err| format!("PipeWire native DSP playback stream creation failed: {err}"))?;

    let main_input_status = Arc::new(InputEndpointStatus::default());
    let prime_input_status = capture_uses_target.then(|| Arc::new(InputEndpointStatus::default()));
    let capture_data = NativeCaptureData {
        format: Default::default(),
        shared: Arc::clone(&shared),
        scratch: vec![0.0; MAX_NATIVE_CALLBACK_FRAMES * 2].into_boxed_slice(),
        endpoint_status: Arc::clone(&main_input_status),
    };
    let playback_data = NativePlaybackData {
        shared: Arc::clone(&shared),
        last_frame: [0.0, 0.0],
        read_sequence: None,
        observed_input_route_generation: shared
            .input_target_control
            .applied_generation
            .load(Ordering::Acquire),
        applied_target_frames: 0,
        frames_since_recenter: 0,
        transition: None,
        recovery: None,
        rate_match: std::ptr::null_mut(),
        rate_correction: 1.0,
    };

    let capture_channel_id = config.channel_id.clone();
    let capture_state_kind = if capture_uses_target {
        "input_target"
    } else {
        "channel_sink"
    };
    let capture_listener = capture_stream
        .add_local_listener_with_user_data(capture_data)
        .state_changed(move |_, user_data, old, new| {
            user_data.endpoint_status.observe_state(&new);
            user_data.shared.capture_streaming.store(
                matches!(&new, pw::stream::StreamState::Streaming),
                Ordering::Release,
            );
            eprintln!(
                "wavelinux6-audio-core native_capture_state channel_id={} kind={} {:?}->{:?}",
                capture_channel_id, capture_state_kind, old, new
            );
        })
        .param_changed(|_, user_data, id, param| {
            parse_audio_format_param(id, param, &mut user_data.format);
        })
        .process(|stream, user_data| {
            process_capture_buffer(stream, user_data);
        })
        .register()
        .map_err(|err| format!("PipeWire native DSP capture listener failed: {err}"))?;

    let prime_capture_listener = prime_capture_stream
        .as_ref()
        .zip(prime_input_status.as_ref())
        .map(|(stream, status)| {
            let channel_id = config.channel_id.clone();
            stream
                .add_local_listener_with_user_data(NativeDiscardCaptureData {
                    endpoint_status: Some(Arc::clone(status)),
                })
                .state_changed(move |_, user_data, old, new| {
                    if let Some(status) = user_data.endpoint_status.as_ref() {
                        status.observe_state(&new);
                    }
                    eprintln!(
                        "wavelinux6-audio-core native_capture_state channel_id={} kind=input_target_prime {:?}->{:?}",
                        channel_id, old, new
                    );
                })
                .process(process_discard_capture_buffer)
                .register()
        })
        .transpose()
        .map_err(|err| format!("PipeWire native prime capture listener failed: {err}"))?;

    let compatibility_listener = compatibility_stream
        .as_ref()
        .map(|stream| {
            let channel_id = config.channel_id.clone();
            stream
                .add_local_listener_with_user_data(NativeDiscardCaptureData::default())
                .state_changed(move |_, _, old, new| {
                    eprintln!(
                        "wavelinux6-audio-core native_compatibility_sink_state channel_id={} {:?}->{:?}",
                        channel_id, old, new
                    );
                })
                .process(process_discard_capture_buffer)
                .register()
        })
        .transpose()
        .map_err(|err| format!("PipeWire native compatibility sink listener failed: {err}"))?;

    let playback_channel_id = config.channel_id.clone();
    let playback_listener = playback_stream
        .add_local_listener_with_user_data(playback_data)
        .state_changed(move |_, _, old, new| {
            eprintln!(
                "wavelinux6-audio-core native_playback_state channel_id={} {:?}->{:?}",
                playback_channel_id, old, new
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
        .process(|stream, user_data| {
            process_playback_buffer(stream, user_data);
        })
        .register()
        .map_err(|err| format!("PipeWire native DSP playback listener failed: {err}"))?;

    let capture_format = audio_format_pod_bytes(config.sample_rate_hz)?;
    let playback_format = audio_format_pod_bytes(config.sample_rate_hz)?;
    let mut capture_params = [Pod::from_bytes(&capture_format)
        .ok_or_else(|| "native DSP capture format pod was invalid".to_string())?];
    let mut playback_params = [Pod::from_bytes(&playback_format)
        .ok_or_else(|| "native DSP playback format pod was invalid".to_string())?];
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;
    capture_stream
        .connect(
            spa::utils::Direction::Input,
            None,
            flags,
            &mut capture_params,
        )
        .map_err(|err| format!("PipeWire native DSP capture connect failed: {err}"))?;
    if let Some(stream) = compatibility_stream.as_ref() {
        let compatibility_format = audio_format_pod_bytes(config.sample_rate_hz)?;
        let mut compatibility_params = [Pod::from_bytes(&compatibility_format)
            .ok_or_else(|| "native compatibility sink format pod was invalid".to_string())?];
        stream
            .connect(
                spa::utils::Direction::Input,
                None,
                flags,
                &mut compatibility_params,
            )
            .map_err(|err| format!("PipeWire native compatibility sink connect failed: {err}"))?;
    }
    playback_stream
        .connect(
            spa::utils::Direction::Output,
            None,
            flags,
            &mut playback_params,
        )
        .map_err(|err| format!("PipeWire native DSP playback connect failed: {err}"))?;

    Ok(NativeChannelRuntime {
        _capture_listener: capture_listener,
        _prime_capture_listener: prime_capture_listener,
        _compatibility_listener: compatibility_listener,
        _playback_listener: playback_listener,
        capture_stream,
        prime_capture_stream,
        _compatibility_stream: compatibility_stream,
        _playback_stream: playback_stream,
        shared,
        channel_id: config.channel_id,
        capture_uses_target,
        sample_rate_hz: config.sample_rate_hz,
        property_prefix: config.property_prefix,
        main_input_status,
        prime_input_status,
        staged_input_target: None,
        dsp_worker,
    })
}

fn native_stream_restore_id(config: &DspChannelConfig, direction: &str) -> String {
    format!(
        "{}-audio-core-{}-{direction}",
        config.graph_prefix, config.channel_id
    )
}

fn native_input_target_node_name(config: &DspChannelConfig) -> String {
    format!("{}-input-target-{}", config.graph_prefix, config.channel_id)
}

fn channel_uses_input_target(config: &DspChannelConfig) -> bool {
    config.input_target_node_name.is_some() || config.input_target_capable
}

fn insert_common_native_props(
    props: &mut pw::properties::PropertiesBox,
    config: &DspChannelConfig,
    role: &str,
) {
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
    props.insert(format!("{}.managed", config.property_prefix), "1");
    props.insert(format!("{}.role", config.property_prefix), role);
    props.insert(
        format!("{}.channel_id", config.property_prefix),
        config.channel_id.clone(),
    );
    props.insert(
        format!("{}.effect_config_revision", config.property_prefix),
        config.revision.clone(),
    );
}

fn apply_capture_idle_policy(props: &mut pw::properties::PropertiesBox, capture_uses_target: bool) {
    if capture_uses_target {
        return;
    }

    // Application buses keep a silent clock while idle. This prevents a
    // browser or game resuming from activating a new node in an established
    // real-time graph; the DSP worker already skips quickly over idle input.
    props.insert(*pw::keys::NODE_PAUSE_ON_IDLE, "false");
    props.insert("node.always-process", "true");
}

fn parse_audio_format_param(
    id: u32,
    param: Option<&spa::pod::Pod>,
    format: &mut spa::param::audio::AudioInfoRaw,
) {
    let Some(param) = param else {
        return;
    };
    if id != spa::param::ParamType::Format.as_raw() {
        return;
    }
    let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param) else {
        return;
    };
    if media_type != spa::param::format::MediaType::Audio
        || media_subtype != spa::param::format::MediaSubtype::Raw
    {
        return;
    }
    let _ = format.parse(param);
}

fn process_capture_buffer(stream: &pw::stream::Stream, user_data: &mut NativeCaptureData) {
    let started = Instant::now();
    process_capture_buffer_inner(stream, user_data);
    user_data
        .shared
        .stats
        .rt_callback_timing
        .record(started.elapsed());
}

fn process_capture_buffer_inner(stream: &pw::stream::Stream, user_data: &mut NativeCaptureData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }
    let data = &mut datas[0];
    let chunk = data.chunk();
    let offset = chunk.offset() as usize;
    let size = chunk.size() as usize;
    let channels = user_data.format.channels().max(1) as usize;
    let Some(bytes) = data.data() else {
        return;
    };
    let Some(end) = offset.checked_add(size) else {
        return;
    };
    if end > bytes.len() {
        return;
    }
    let sample_bytes = mem::size_of::<f32>();
    let frame_bytes = channels.saturating_mul(sample_bytes);
    if frame_bytes == 0 {
        return;
    }

    let input = &bytes[offset..end];
    let available_frames = input.len() / frame_bytes;
    let scratch_frames = user_data.scratch.len() / 2;
    let mut input_frame = 0_usize;
    while input_frame < available_frames {
        let chunk_frames = (available_frames - input_frame).min(scratch_frames);
        let byte_start = input_frame * frame_bytes;
        let byte_end = byte_start + chunk_frames * frame_bytes;
        let decoded = decode_interleaved_stereo_into(
            &input[byte_start..byte_end],
            channels,
            &mut user_data.scratch[..chunk_frames * 2],
        );
        if decoded == 0 {
            break;
        }
        for frame in user_data.scratch[..decoded * 2].as_chunks::<2>().0 {
            user_data.shared.raw_history.push([frame[0], frame[1]]);
        }
        user_data
            .shared
            .stats
            .captured_frames
            .fetch_add(decoded as u64, Ordering::Relaxed);
        user_data
            .endpoint_status
            .processed_frames
            .fetch_add(decoded as u64, Ordering::Relaxed);
        input_frame += decoded;
    }
    user_data
        .shared
        .stats
        .capture_callbacks
        .fetch_add(1, Ordering::Relaxed);
}

fn process_discard_capture_buffer(
    stream: &pw::stream::Stream,
    user_data: &mut NativeDiscardCaptureData,
) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let frames = buffer
        .datas_mut()
        .first()
        .map(|data| data.chunk().size() as u64 / (mem::size_of::<f32>() as u64 * 2))
        .unwrap_or(0);
    if let Some(status) = user_data.endpoint_status.as_ref() {
        status.processed_frames.fetch_add(frames, Ordering::Relaxed);
    }
}

fn apply_dsp_input_mode(samples: &mut [f32], mode: DspInputMode) {
    for frame in samples.as_chunks_mut::<2>().0 {
        let left = frame[0];
        let right = frame[1];
        match mode {
            DspInputMode::Stereo => {}
            DspInputMode::MonoLeft => frame.copy_from_slice(&[left, left]),
            DspInputMode::MonoRight => frame.copy_from_slice(&[right, right]),
            DspInputMode::SumMono => {
                let mono = (left + right) * 0.5;
                frame.copy_from_slice(&[mono, mono]);
            }
            DspInputMode::SwapLr => frame.copy_from_slice(&[right, left]),
        }
    }
}

fn sanitize_non_finite_in_place(samples: &mut [f32]) -> usize {
    let mut replaced = 0_usize;
    for sample in samples {
        if !sample.is_finite() {
            *sample = 0.0;
            replaced = replaced.saturating_add(1);
        }
    }
    replaced
}

fn replace_non_finite_with_dry(processed: &mut [f32], dry: &[f32]) -> usize {
    let mut replaced = 0_usize;
    for (index, sample) in processed.iter_mut().enumerate() {
        if !sample.is_finite() {
            *sample = dry
                .get(index)
                .copied()
                .filter(|value| value.is_finite())
                .unwrap_or(0.0);
            replaced = replaced.saturating_add(1);
        }
    }
    replaced
}

fn start_dsp_worker(
    shared: Arc<NativeShared>,
    active_chain: PreparedChain,
) -> Result<DspWorkerHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker_shared = Arc::clone(&shared);
    let thread_name = format!("wl6-dsp-{}", shared.channel_id);
    let join = thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            worker_shared.worker_running.store(true, Ordering::Release);
            let mut data = DspWorkerData {
                active_chain: Box::new(active_chain),
                shadow_chain: None,
                chain_crossfade_progress: 0,
                chain_crossfade_total: (worker_shared.sample_rate_hz as usize
                    * CHAIN_CROSSFADE_MSEC
                    / 1000)
                    .max(1),
                shared: Arc::clone(&worker_shared),
                read_sequence: 0,
                raw_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
                active_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
                active_dry_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
                shadow_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
                shadow_dry_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
                acceleration_metrics_countdown: DSP_ACCELERATOR_METRICS_INTERVAL_BLOCKS,
            };
            publish_active_acceleration_metrics(&data);
            while !worker_stop.load(Ordering::Acquire) && !TERMINATE.load(Ordering::Acquire) {
                let frames = process_available_dsp_frames(&mut data);
                if frames == 0 {
                    let idle = if worker_shared.capture_streaming.load(Ordering::Acquire) {
                        DSP_WORKER_ACTIVE_IDLE
                    } else {
                        DSP_WORKER_INACTIVE_IDLE
                    };
                    thread::sleep(idle);
                }
            }
            worker_shared.worker_running.store(false, Ordering::Release);
        })
        .map_err(|error| format!("failed to start DSP worker: {error}"))?;
    Ok(DspWorkerHandle {
        stop,
        join: Some(join),
    })
}

fn process_available_dsp_frames(data: &mut DspWorkerData) -> usize {
    let latest = data.shared.raw_history.write_sequence();
    let capacity = data.shared.raw_history.capacity();
    if latest.saturating_sub(data.read_sequence) > capacity {
        let recovered_sequence = latest.saturating_sub(capacity);
        let skipped = recovered_sequence.saturating_sub(data.read_sequence);
        data.read_sequence = recovered_sequence;
        data.shared.history.skip(skipped);
        data.shared
            .stats
            .dropped_frames
            .fetch_add(skipped, Ordering::Relaxed);
        data.shared
            .stats
            .worker_overrun_frames
            .fetch_add(skipped, Ordering::Relaxed);
    }

    let available = latest.saturating_sub(data.read_sequence) as usize;
    let requested = available.min(DSP_WORKER_BLOCK_FRAMES);
    if requested == 0 {
        data.shared
            .worker_read_sequence
            .store(data.read_sequence, Ordering::Release);
        return 0;
    }

    let mut frames = 0_usize;
    for index in 0..requested {
        let Some(frame) = data
            .shared
            .raw_history
            .get(data.read_sequence.saturating_add(index as u64))
        else {
            break;
        };
        data.raw_scratch[index * 2] = frame[0];
        data.raw_scratch[index * 2 + 1] = frame[1];
        frames += 1;
    }
    if frames == 0 {
        return 0;
    }

    process_dsp_worker_block(data, frames);
    data.read_sequence = data.read_sequence.saturating_add(frames as u64);
    data.shared
        .worker_read_sequence
        .store(data.read_sequence, Ordering::Release);
    frames
}

fn process_dsp_worker_block(data: &mut DspWorkerData, frames: usize) {
    if data.shadow_chain.is_none() {
        if let Some(pending) = data.shared.chain_control.take_pending() {
            data.shadow_chain = Some(pending);
            data.chain_crossfade_progress = 0;
        }
    }

    let samples = frames * 2;
    let started = Instant::now();
    let input_non_finite = sanitize_non_finite_in_place(&mut data.raw_scratch[..samples]);
    let mut status = process_prepared_chain(
        &mut data.active_chain,
        &data.raw_scratch[..samples],
        &mut data.active_scratch[..samples],
        &mut data.active_dry_scratch[..samples],
    );

    if let Some(shadow) = data.shadow_chain.as_mut() {
        let shadow_status = process_prepared_chain(
            shadow,
            &data.raw_scratch[..samples],
            &mut data.shadow_scratch[..samples],
            &mut data.shadow_dry_scratch[..samples],
        );
        status.merge(shadow_status);
        for frame in 0..frames {
            let phase = (data.chain_crossfade_progress + 1) as f32
                / data.chain_crossfade_total.max(1) as f32;
            let phase = phase.clamp(0.0, 1.0);
            let old_gain = (phase * std::f32::consts::FRAC_PI_2).cos();
            let new_gain = (phase * std::f32::consts::FRAC_PI_2).sin();
            let normalization = (old_gain + new_gain).max(1.0);
            for channel in 0..2 {
                let index = frame * 2 + channel;
                data.active_scratch[index] = (data.active_scratch[index] * old_gain
                    + data.shadow_scratch[index] * new_gain)
                    / normalization;
            }
            data.chain_crossfade_progress = data.chain_crossfade_progress.saturating_add(1);
        }
    }

    let output_non_finite = replace_non_finite_with_dry(
        &mut data.active_scratch[..samples],
        &data.active_dry_scratch[..samples],
    );
    record_dsp_integrity(data, input_non_finite, output_non_finite, status);
    publish_dsp_block(data, frames);

    if data.shadow_chain.is_some() && data.chain_crossfade_progress >= data.chain_crossfade_total {
        let shadow = data
            .shadow_chain
            .take()
            .expect("shadow chain checked above");
        let generation = shadow.generation;
        data.active_chain = shadow;
        data.shared.chain_control.acknowledge(generation);
        data.shared
            .recovery_in_progress
            .store(false, Ordering::Release);
        data.shared
            .stats
            .chain_swaps
            .fetch_add(1, Ordering::Relaxed);
        data.acceleration_metrics_countdown = 0;
    }

    let elapsed = duration_micros(started.elapsed());
    data.shared
        .stats
        .worker_blocks
        .fetch_add(1, Ordering::Relaxed);
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
    if data.acceleration_metrics_countdown == 0 {
        publish_active_acceleration_metrics(data);
        data.acceleration_metrics_countdown = DSP_ACCELERATOR_METRICS_INTERVAL_BLOCKS;
    } else {
        data.acceleration_metrics_countdown = data.acceleration_metrics_countdown.saturating_sub(1);
    }
}

fn publish_active_acceleration_metrics(data: &DspWorkerData) {
    let metrics = data.active_chain.chain.acceleration_metrics();
    match data.shared.acceleration_metrics.lock() {
        Ok(mut current) => *current = metrics,
        Err(poisoned) => *poisoned.into_inner() = metrics,
    }
}

fn process_prepared_chain(
    prepared: &mut PreparedChain,
    raw: &[f32],
    processed: &mut [f32],
    dry: &mut [f32],
) -> RealtimeProcessStatus {
    processed.copy_from_slice(raw);
    apply_dsp_input_mode(processed, prepared.input_mode);
    dry.copy_from_slice(processed);
    let status = prepared.chain.process_worker_interleaved_stereo(processed);
    if status.non_finite_samples > 0 {
        processed.copy_from_slice(dry);
    }
    status
}

fn record_dsp_integrity(
    data: &DspWorkerData,
    input_non_finite: usize,
    output_non_finite: usize,
    status: RealtimeProcessStatus,
) {
    let internal_non_finite = status.non_finite_samples as usize;
    let non_finite_samples = input_non_finite
        .saturating_add(internal_non_finite)
        .saturating_add(output_non_finite);
    if status.non_finite_samples > 0 {
        data.shared
            .stats
            .non_finite_effect_mask
            .fetch_or(status.effect_mask, Ordering::Relaxed);
    }
    if non_finite_samples > 0 {
        data.shared
            .stats
            .non_finite_blocks
            .fetch_add(1, Ordering::Relaxed);
        data.shared
            .stats
            .non_finite_samples
            .fetch_add(non_finite_samples as u64, Ordering::Relaxed);
    }
    if (internal_non_finite > 0 || output_non_finite > 0)
        && !data.shared.recovery_in_progress.load(Ordering::Acquire)
    {
        data.shared
            .recovery_requested
            .store(true, Ordering::Release);
    }
}

fn publish_dsp_block(data: &DspWorkerData, frames: usize) {
    let mut peak_left = 0.0_f32;
    let mut peak_right = 0.0_f32;
    let mut square_sum_left = 0.0_f32;
    let mut square_sum_right = 0.0_f32;
    for frame in data.active_scratch[..frames * 2].as_chunks::<2>().0 {
        peak_left = peak_left.max(frame[0].abs());
        peak_right = peak_right.max(frame[1].abs());
        square_sum_left += frame[0] * frame[0];
        square_sum_right += frame[1] * frame[1];
        data.shared.history.push([frame[0], frame[1]]);
    }
    let rms_scale = 1.0 / frames.max(1) as f32;
    data.shared.meter.publish(
        peak_left,
        peak_right,
        (square_sum_left * rms_scale).sqrt(),
        (square_sum_right * rms_scale).sqrt(),
        frames,
    );
}

fn process_playback_buffer(stream: &pw::stream::Stream, user_data: &mut NativePlaybackData) {
    let started = Instant::now();
    process_playback_buffer_inner(stream, user_data);
    user_data
        .shared
        .stats
        .rt_callback_timing
        .record(started.elapsed());
}

fn process_playback_buffer_inner(stream: &pw::stream::Stream, user_data: &mut NativePlaybackData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    update_pipewire_rate_match(user_data);
    let rate_match_frames = if user_data.rate_match.is_null() {
        0
    } else {
        unsafe { (*user_data.rate_match).size as usize }
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
    let data = &mut datas[0];
    let Some(bytes) = data.data() else {
        return;
    };
    let stride = mem::size_of::<f32>() * 2;
    let frames = playback_render_frames(
        requested_frames,
        bytes.len(),
        stride,
        user_data.shared.render_quantum_frames,
    );
    if !user_data.shared.capture_streaming.load(Ordering::Acquire)
        && user_data
            .last_frame
            .iter()
            .all(|sample| sample.abs() <= 1.0e-8)
    {
        reset_idle_playback_state(user_data);
        bytes[..frames * stride].fill(0);
        let chunk = data.chunk_mut();
        *chunk.offset_mut() = 0;
        *chunk.stride_mut() = stride as _;
        *chunk.size_mut() = (frames * stride) as _;
        user_data
            .shared
            .stats
            .rendered_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
        return;
    }
    let mut rendered = 0_u64;
    let mut underrun = 0_u64;
    for frame_index in 0..frames {
        let output_frame = render_native_frame(user_data, &mut underrun);
        for (channel, sample) in output_frame.iter().enumerate() {
            let start = frame_index * stride + channel * mem::size_of::<f32>();
            bytes[start..start + mem::size_of::<f32>()].copy_from_slice(&sample.to_le_bytes());
        }
        user_data.last_frame = output_frame;
        rendered = rendered.saturating_add(1);
    }
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = stride as _;
    *chunk.size_mut() = (frames * stride) as _;
    user_data
        .shared
        .stats
        .rendered_frames
        .fetch_add(rendered, Ordering::Relaxed);
    user_data
        .shared
        .stats
        .underrun_frames
        .fetch_add(underrun, Ordering::Relaxed);
}

fn update_pipewire_rate_match(user_data: &mut NativePlaybackData) {
    if user_data.rate_match.is_null() {
        user_data.rate_correction = 1.0;
        user_data
            .shared
            .stats
            .rate_correction_bits
            .store(1.0_f64.to_bits(), Ordering::Relaxed);
        return;
    }
    let latest = user_data.shared.history.write_sequence();
    let current = user_data.read_sequence.unwrap_or(latest);
    let fill = latest.saturating_sub(current) as f64;
    let target = user_data
        .shared
        .target_latency_frames
        .load(Ordering::Relaxed)
        .max(1) as f64;
    let desired = if user_data.transition.is_some()
        || user_data.recovery.is_some()
        || user_data.read_sequence.is_none()
    {
        1.0
    } else {
        // A 0.3% bound is enough to absorb independent device clocks without
        // producing an audible pitch change. For a node-to-graph stream,
        // PipeWire requests more node samples when this correction is below
        // 1.0, so a buffer above target must lower the correction.
        desired_rate_correction(fill, target)
    };
    user_data.rate_correction += (desired - user_data.rate_correction) * 0.02;
    unsafe {
        (*user_data.rate_match).flags |= spa::sys::SPA_IO_RATE_MATCH_FLAG_ACTIVE;
        (*user_data.rate_match).rate = user_data.rate_correction;
    }
    user_data
        .shared
        .stats
        .rate_correction_bits
        .store(user_data.rate_correction.to_bits(), Ordering::Relaxed);
}

fn desired_rate_correction(fill_frames: f64, target_frames: f64) -> f64 {
    let target_frames = target_frames.max(1.0);
    (1.0 - ((fill_frames - target_frames) / target_frames) * 0.002).clamp(0.997, 1.003)
}

fn render_native_frame(user_data: &mut NativePlaybackData, underruns: &mut u64) -> [f32; 2] {
    let latest = user_data.shared.history.write_sequence();
    let target_frames = user_data
        .shared
        .target_latency_frames
        .load(Ordering::Relaxed)
        .max(1);

    if !user_data.shared.capture_streaming.load(Ordering::Acquire) {
        reset_idle_playback_state(user_data);
        return [
            user_data.last_frame[0] * 0.995,
            user_data.last_frame[1] * 0.995,
        ];
    }

    begin_input_route_transition_if_ready(user_data, latest, target_frames);

    if user_data.read_sequence.is_none() {
        if latest < target_frames as u64 {
            user_data
                .shared
                .current_buffer_frames
                .store(latest, Ordering::Relaxed);
            return [0.0, 0.0];
        }
        user_data.read_sequence = Some(latest - target_frames as u64);
        user_data.applied_target_frames = target_frames;
    }

    recover_overwritten_history(user_data, latest, target_frames);

    let recenter_interval =
        (user_data.shared.sample_rate_hz as usize * RECENTER_INTERVAL_MSEC / 1000).max(1);
    user_data.frames_since_recenter = user_data.frames_since_recenter.saturating_add(1);
    let target_changed = target_frames != user_data.applied_target_frames;
    let should_recenter =
        user_data.rate_match.is_null() && user_data.frames_since_recenter >= recenter_interval;
    if user_data.transition.is_none() && (target_changed || should_recenter) {
        let current = user_data.read_sequence.unwrap_or_default();
        let desired = latest.saturating_sub(target_frames as u64);
        let current_lag = latest.saturating_sub(current);
        let drift = current_lag.abs_diff(target_frames as u64);
        if target_changed || drift > RECENTER_THRESHOLD_FRAMES {
            begin_latency_transition(user_data, current, desired);
        }
        if should_recenter {
            user_data.frames_since_recenter = 0;
        }
    }

    let output = if let Some(mut recovery) = user_data.recovery {
        let to_sequence = recovery
            .to_sequence
            .saturating_add(recovery.progress_frames as u64);
        match user_data.shared.history.get(to_sequence) {
            Some(to) => {
                let phase =
                    (recovery.progress_frames + 1) as f32 / recovery.total_frames.max(1) as f32;
                let from_gain = (phase * std::f32::consts::FRAC_PI_2).cos();
                let to_gain = (phase * std::f32::consts::FRAC_PI_2).sin();
                let normalization = (from_gain + to_gain).max(1.0);
                let mixed = [
                    (recovery.from_frame[0] * from_gain + to[0] * to_gain) / normalization,
                    (recovery.from_frame[1] * from_gain + to[1] * to_gain) / normalization,
                ];
                recovery.progress_frames += 1;
                if recovery.progress_frames >= recovery.total_frames {
                    user_data.read_sequence = Some(
                        recovery
                            .to_sequence
                            .saturating_add(recovery.total_frames as u64),
                    );
                    user_data.recovery = None;
                } else {
                    user_data.recovery = Some(recovery);
                }
                mixed
            }
            None => {
                *underruns = underruns.saturating_add(1);
                user_data.recovery = None;
                user_data.read_sequence = Some(to_sequence);
                [
                    user_data.last_frame[0] * 0.995,
                    user_data.last_frame[1] * 0.995,
                ]
            }
        }
    } else if let Some(mut transition) = user_data.transition {
        let from_sequence = transition
            .from_sequence
            .saturating_add(transition.progress_frames as u64);
        let to_sequence = transition
            .to_sequence
            .saturating_add(transition.progress_frames as u64);
        match (
            user_data.shared.history.get(from_sequence),
            user_data.shared.history.get(to_sequence),
        ) {
            (Some(from), Some(to)) => {
                let phase =
                    (transition.progress_frames + 1) as f32 / transition.total_frames.max(1) as f32;
                let from_gain = (phase * std::f32::consts::FRAC_PI_2).cos();
                let to_gain = (phase * std::f32::consts::FRAC_PI_2).sin();
                let normalization = (from_gain + to_gain).max(1.0);
                let mixed = [
                    (from[0] * from_gain + to[0] * to_gain) / normalization,
                    (from[1] * from_gain + to[1] * to_gain) / normalization,
                ];
                transition.progress_frames += 1;
                if transition.progress_frames >= transition.total_frames {
                    user_data.read_sequence = Some(
                        transition
                            .to_sequence
                            .saturating_add(transition.total_frames as u64),
                    );
                    user_data.applied_target_frames = target_frames;
                    user_data.transition = None;
                } else {
                    user_data.transition = Some(transition);
                }
                mixed
            }
            _ => {
                *underruns = underruns.saturating_add(1);
                user_data.transition = None;
                [
                    user_data.last_frame[0] * 0.995,
                    user_data.last_frame[1] * 0.995,
                ]
            }
        }
    } else {
        let sequence = user_data.read_sequence.unwrap_or_default();
        match user_data.shared.history.get(sequence) {
            Some(frame) => {
                user_data.read_sequence = Some(sequence.saturating_add(1));
                frame
            }
            None => {
                *underruns = underruns.saturating_add(1);
                [
                    user_data.last_frame[0] * 0.995,
                    user_data.last_frame[1] * 0.995,
                ]
            }
        }
    };

    let current_sequence = user_data
        .recovery
        .map(|recovery| {
            recovery
                .to_sequence
                .saturating_add(recovery.progress_frames as u64)
        })
        .or(user_data.read_sequence)
        .unwrap_or(latest);
    user_data.shared.current_buffer_frames.store(
        latest
            .saturating_sub(current_sequence)
            .min(user_data.shared.history.capacity()),
        Ordering::Relaxed,
    );
    output
}

fn begin_input_route_transition_if_ready(
    user_data: &mut NativePlaybackData,
    latest: u64,
    target_frames: usize,
) {
    let generation = user_data
        .shared
        .input_target_control
        .applied_generation
        .load(Ordering::Acquire);
    if generation == user_data.observed_input_route_generation {
        return;
    }
    let boundary = user_data
        .shared
        .input_route_change_sequence
        .load(Ordering::Acquire);
    if latest < boundary.saturating_add(target_frames as u64) {
        return;
    }
    let total_frames = (user_data.shared.sample_rate_hz as usize * LATENCY_CROSSFADE_MSEC / 1000)
        .max(1)
        .min(target_frames.max(1));
    user_data.transition = None;
    user_data.recovery = Some(DiscontinuityRecovery {
        from_frame: user_data.last_frame,
        to_sequence: boundary,
        progress_frames: 0,
        total_frames,
    });
    user_data.read_sequence = Some(boundary);
    user_data.applied_target_frames = target_frames;
    user_data.frames_since_recenter = 0;
    user_data.observed_input_route_generation = generation;
}

fn reset_idle_playback_state(user_data: &mut NativePlaybackData) {
    user_data.read_sequence = None;
    user_data.transition = None;
    user_data.recovery = None;
    user_data.applied_target_frames = user_data
        .shared
        .target_latency_frames
        .load(Ordering::Relaxed)
        .max(1);
    user_data.frames_since_recenter = 0;
    user_data
        .shared
        .current_buffer_frames
        .store(0, Ordering::Relaxed);
}

fn recover_overwritten_history(
    user_data: &mut NativePlaybackData,
    latest: u64,
    target_frames: usize,
) {
    let Some(current_sequence) = user_data.read_sequence else {
        return;
    };
    if latest.saturating_sub(current_sequence) <= user_data.shared.history.capacity() {
        return;
    }

    let desired_sequence = latest.saturating_sub(target_frames as u64);
    if user_data.shared.history.get(desired_sequence).is_none() {
        return;
    }
    let dropped = desired_sequence.saturating_sub(current_sequence);
    user_data
        .shared
        .stats
        .dropped_frames
        .fetch_add(dropped, Ordering::Relaxed);
    user_data.transition = None;
    user_data.recovery = Some(DiscontinuityRecovery {
        from_frame: user_data.last_frame,
        to_sequence: desired_sequence,
        progress_frames: 0,
        total_frames: (user_data.shared.sample_rate_hz as usize * LATENCY_CROSSFADE_MSEC / 1000)
            .max(1)
            .min(target_frames.max(1)),
    });
    user_data.read_sequence = Some(desired_sequence);
    user_data.applied_target_frames = target_frames;
    user_data.frames_since_recenter = 0;
}

fn begin_latency_transition(
    user_data: &mut NativePlaybackData,
    current_sequence: u64,
    desired_sequence: u64,
) {
    let desired_sequence =
        user_data
            .shared
            .history
            .aligned_latency_sequence(current_sequence, desired_sequence, 0.0);
    if current_sequence == desired_sequence
        || user_data.shared.history.get(current_sequence).is_none()
        || user_data.shared.history.get(desired_sequence).is_none()
    {
        return;
    }
    let total_frames =
        (user_data.shared.sample_rate_hz as usize * LATENCY_CROSSFADE_MSEC / 1000).max(1);
    user_data.transition = Some(LatencyTransition {
        from_sequence: current_sequence,
        to_sequence: desired_sequence,
        from_fraction: 0.0,
        to_fraction: 0.0,
        progress_frames: 0,
        total_frames,
    });
}

fn playback_render_frames(
    requested_frames: usize,
    buffer_bytes: usize,
    stride: usize,
    fallback_frames: usize,
) -> usize {
    if stride == 0 {
        return 0;
    }
    let capacity_frames = buffer_bytes / stride;
    let target_frames = if requested_frames > 0 {
        requested_frames
    } else {
        fallback_frames.max(1)
    };
    target_frames.min(capacity_frames)
}

#[derive(Debug, Deserialize)]
struct CoreControlCommand {
    #[serde(default = "default_control_protocol_version")]
    protocol_version: u16,
    command: String,
    #[serde(default)]
    route_id: Option<String>,
    #[serde(default)]
    target_msec: Option<u16>,
    #[serde(default)]
    target_node_name: Option<String>,
    #[serde(default)]
    route_generation: Option<u64>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    config_path: Option<String>,
    #[serde(default)]
    config_revision: Option<String>,
    #[serde(default)]
    desired_generation: Option<u64>,
    #[serde(default)]
    request_id: Option<String>,
}

fn default_control_protocol_version() -> u16 {
    CORE_CONTROL_PROTOCOL_VERSION
}

fn start_latency_control_socket(config: &DspChannelConfig, shared: Arc<NativeShared>) {
    let Some(socket_path) = config.control_socket_path.clone() else {
        return;
    };
    let channel_id = config.channel_id.clone();
    thread::spawn(move || {
        let path = PathBuf::from(&socket_path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::remove_file(&path);
        let Ok(listener) = UnixListener::bind(&path) else {
            eprintln!(
                "wavelinux6-audio-core adaptive_latency_socket_failed path={}",
                path.display()
            );
            return;
        };
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        eprintln!(
            "wavelinux6-audio-core control_socket path={} channel_id={} protocol={}",
            path.display(),
            channel_id,
            CORE_CONTROL_PROTOCOL_VERSION
        );
        while !TERMINATE.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    let timeout = Some(Duration::from_secs(1));
                    let _ = stream.set_read_timeout(timeout);
                    let _ = stream.set_write_timeout(timeout);
                    let mut payload = String::new();
                    let read_result =
                        Read::take(&mut stream, 1024 * 1024).read_to_string(&mut payload);
                    let response = if let Err(err) = read_result {
                        control_error(None, format!("read failed: {err}"))
                    } else {
                        match serde_json::from_str::<CoreControlCommand>(&payload) {
                            Ok(command) => {
                                handle_core_control_command(&channel_id, &shared, command)
                            }
                            Err(err) => control_error(None, format!("invalid command: {err}")),
                        }
                    };
                    let _ = stream.write_all(response.to_string().as_bytes());
                    let _ = stream.write_all(b"\n");
                }
                Err(err) => {
                    eprintln!("wavelinux6-audio-core adaptive_latency_accept_error {err}");
                    thread::sleep(Duration::from_millis(250));
                }
            }
        }
        let _ = std::fs::remove_file(&path);
    });
}

fn handle_core_control_command(
    channel_id: &str,
    shared: &NativeShared,
    command: CoreControlCommand,
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
    if command
        .route_id
        .as_deref()
        .is_some_and(|route| route != channel_id)
    {
        return control_error(command.request_id, "route_id does not match this core node");
    }

    match command.command.as_str() {
        "set_input_target" => {
            let Some(target) = command.target_node_name else {
                return control_error(command.request_id, "target_node_name is required");
            };
            match shared
                .input_target_control
                .queue(Some(target.clone()), command.route_generation)
            {
                Ok(generation) => serde_json::json!({
                    "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                    "ok": true,
                    "request_id": command.request_id,
                    "route_id": channel_id,
                    "route_generation": generation,
                    "target_node_name": target,
                    "operation": "input_target_queued",
                }),
                Err(error) => control_error(command.request_id, error),
            }
        }
        "clear_input_target" => {
            match shared
                .input_target_control
                .queue(None, command.route_generation)
            {
                Ok(generation) => serde_json::json!({
                    "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                    "ok": true,
                    "request_id": command.request_id,
                    "route_id": channel_id,
                    "route_generation": generation,
                    "target_node_name": null,
                    "operation": "input_target_clear_queued",
                }),
                Err(error) => control_error(command.request_id, error),
            }
        }
        "set_target_latency" => {
            let Some(target_msec) = command.target_msec else {
                return control_error(command.request_id, "target_msec is required");
            };
            shared.set_target_latency(
                target_msec,
                command
                    .reason
                    .as_deref()
                    .unwrap_or("adaptive_latency_control"),
            );
            serde_json::json!({
                "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                "ok": true,
                "request_id": command.request_id,
                "route_id": channel_id,
                "core_topology_revision": shared.core_topology_revision.as_str(),
                "target_msec": target_msec,
            })
        }
        "swap_chain" | "set_parameters" => {
            let Some(config_path) = command.config_path.as_deref() else {
                return control_error(command.request_id, "config_path is required");
            };
            match prepare_replacement_chain(
                PathBuf::from(config_path),
                channel_id,
                shared.acceleration_config.as_ref(),
            ) {
                Ok((chain, config)) => {
                    if command
                        .desired_generation
                        .is_some_and(|generation| generation != config.generation)
                    {
                        return control_error(
                            command.request_id,
                            format!(
                                "requested generation {:?} does not match config generation {}",
                                command.desired_generation, config.generation
                            ),
                        );
                    }
                    let generation = if let Some(generation) = command.desired_generation {
                        if generation == 0 {
                            return control_error(
                                command.request_id,
                                "desired_generation must be greater than zero",
                            );
                        }
                        if !shared.chain_control.reserve_generation(generation) {
                            return control_error(
                                command.request_id,
                                format!(
                                    "stale generation {generation}; latest submitted generation is {}",
                                    shared
                                        .chain_control
                                        .submitted_generation
                                        .load(Ordering::Acquire)
                                ),
                            );
                        }
                        generation
                    } else {
                        shared.chain_control.next_generation()
                    };
                    let input_mode = config.input_mode;
                    match shared.chain_config.lock() {
                        Ok(mut current) => *current = config,
                        Err(poisoned) => *poisoned.into_inner() = config,
                    }
                    shared.recovery_in_progress.store(true, Ordering::Release);
                    shared.chain_control.submit(chain, generation, input_mode);
                    serde_json::json!({
                        "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                        "ok": true,
                        "request_id": command.request_id,
                        "route_id": channel_id,
                        "config_revision": command.config_revision,
                        "graph_revision": generation,
                        "operation": "chain_swap_queued",
                    })
                }
                Err(err) => control_error(command.request_id, err),
            }
        }
        "get_diagnostics" => {
            let acceleration = shared
                .acceleration_metrics
                .lock()
                .map(|metrics| metrics.clone())
                .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
            let mut response = serde_json::json!({
                "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                "ok": true,
                "request_id": command.request_id,
                "route_id": channel_id,
                "core_topology_revision": shared.core_topology_revision.as_str(),
                "sample_rate_hz": shared.sample_rate_hz,
                "target_latency_msec": shared.target_latency_msec.load(Ordering::Relaxed),
                "current_buffer_frames": shared.current_buffer_frames.load(Ordering::Relaxed),
                "captured_frames": shared.stats.captured_frames.load(Ordering::Relaxed),
                "rendered_frames": shared.stats.rendered_frames.load(Ordering::Relaxed),
                "dropped_frames": shared.stats.dropped_frames.load(Ordering::Relaxed),
                "underrun_frames": shared.stats.underrun_frames.load(Ordering::Relaxed),
                "capture_callbacks": shared.stats.capture_callbacks.load(Ordering::Relaxed),
                "worker_running": shared.worker_running.load(Ordering::Acquire),
                "worker_blocks": shared.stats.worker_blocks.load(Ordering::Relaxed),
                "worker_queue_frames": shared.raw_history.write_sequence()
                    .saturating_sub(shared.worker_read_sequence.load(Ordering::Acquire))
                    .min(shared.raw_history.capacity()),
                "worker_queue_capacity_frames": shared.raw_history.capacity(),
                "worker_overrun_frames": shared.stats.worker_overrun_frames.load(Ordering::Relaxed),
                "last_process_micros": shared.stats.last_process_micros.load(Ordering::Relaxed),
                "max_process_micros": shared.stats.max_process_micros.load(Ordering::Relaxed),
                "rt_callback_count": shared.stats.rt_callback_timing.count(),
                "rt_callback_p99_micros": shared.stats.rt_callback_timing.p99_micros(),
                "rt_callback_max_micros": shared.stats.rt_callback_timing.max_micros(),
                "chain_swaps": shared.stats.chain_swaps.load(Ordering::Relaxed),
                "non_finite_blocks": shared.stats.non_finite_blocks.load(Ordering::Relaxed),
                "non_finite_samples": shared.stats.non_finite_samples.load(Ordering::Relaxed),
                "non_finite_effect_mask": shared.stats.non_finite_effect_mask.load(Ordering::Relaxed),
                "chain_recoveries": shared.stats.chain_recoveries.load(Ordering::Relaxed),
                "chain_swap_replacements": shared.chain_control.replacements.load(Ordering::Relaxed),
                "retired_chain_overflows": shared.chain_control.retired_overflows.load(Ordering::Relaxed),
                "submitted_generation": shared.chain_control.submitted_generation.load(Ordering::Acquire),
                "acknowledged_generation": shared.chain_control.acknowledged_generation.load(Ordering::Acquire),
                "submitted_route_generation": shared.input_target_control.submitted_generation.load(Ordering::Acquire),
                "applied_route_generation": shared.input_target_control.applied_generation.load(Ordering::Acquire),
                "input_target_node_name": shared.input_target_control.current_target(),
                "route_target_error": shared.input_target_control.last_error(),
                "rate_correction": current_rate_correction(shared),
            });
            if let (Some(response), Ok(serde_json::Value::Object(acceleration))) =
                (response.as_object_mut(), serde_json::to_value(acceleration))
            {
                response.extend(acceleration);
            }
            response
        }
        "shutdown" => {
            TERMINATE.store(true, Ordering::SeqCst);
            serde_json::json!({
                "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
                "ok": true,
                "request_id": command.request_id,
                "route_id": channel_id,
                "operation": "shutdown_requested",
            })
        }
        _ => control_error(
            command.request_id,
            format!("unsupported command {}", command.command),
        ),
    }
}

fn prepare_replacement_chain(
    path: PathBuf,
    channel_id: &str,
    acceleration: Option<&DspAccelerationConfig>,
) -> Result<(DspChain, DspChannelConfig), String> {
    let raw = std::fs::read_to_string(&path)
        .map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    let config: DspChannelConfig = serde_json::from_str(&raw)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))?;
    if config.channel_id != channel_id {
        return Err(format!(
            "replacement channel {} does not match {channel_id}",
            config.channel_id
        ));
    }
    let chain = build_replacement_chain(&config, acceleration)?;
    Ok((chain, config))
}

fn build_replacement_chain(
    config: &DspChannelConfig,
    acceleration: Option<&DspAccelerationConfig>,
) -> Result<DspChain, String> {
    let mut chain = DspChain::new_with_channels_and_acceleration(
        &config.active_effects(),
        config.sample_rate_hz,
        config.input_channels,
        acceleration,
    );
    if !chain.is_fully_initialized() {
        return Err(format!(
            "replacement chain initialization failed: {}",
            chain.initialization_failures().join("; ")
        ));
    }
    // Prime stateful filters and RNNoise outside the RT callback. The shadow
    // chain receives real input only after this preparation is complete.
    let mut silence = vec![0.0_f32; 960 * 2];
    let prime_status = chain.process_worker_interleaved_stereo(&mut silence);
    if prime_status.non_finite_samples > 0 {
        return Err(format!(
            "replacement chain produced {} non-finite samples while priming (effect mask {:#x})",
            prime_status.non_finite_samples, prime_status.effect_mask
        ));
    }
    let acceleration_metrics = chain.acceleration_metrics();
    if !acceleration_metrics.startup_failures.is_empty() {
        eprintln!(
            "wavelinux6-audio-core accelerator_startup_fallback channel_id={} provider={} failures={}",
            config.channel_id,
            acceleration_metrics.provider.as_deref().unwrap_or("unknown"),
            acceleration_metrics.startup_failures.join("; ")
        );
    }
    Ok(chain)
}

fn recover_poisoned_chain(shared: &NativeShared) {
    if !shared.recovery_requested.swap(false, Ordering::AcqRel) {
        return;
    }
    if shared.recovery_in_progress.swap(true, Ordering::AcqRel) {
        return;
    }
    let config = match shared.chain_config.lock() {
        Ok(config) => config.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let chain = match build_replacement_chain(&config, shared.acceleration_config.as_ref()) {
        Ok(chain) => chain,
        Err(error) => {
            shared.recovery_in_progress.store(false, Ordering::Release);
            eprintln!(
                "wavelinux6-audio-core chain_recovery_failed channel_id={} error={}",
                shared.channel_id, error
            );
            return;
        }
    };
    let config_is_current = match shared.chain_config.lock() {
        Ok(current) => *current == config,
        Err(poisoned) => *poisoned.into_inner() == config,
    };
    if !config_is_current {
        shared.recovery_in_progress.store(false, Ordering::Release);
        return;
    }
    // Recovery rebuilds the same desired configuration. It must not consume a
    // user-visible generation or make the engine's next update appear stale.
    let generation = config.generation;
    shared
        .chain_control
        .submit(chain, generation, config.input_mode);
    shared
        .stats
        .chain_recoveries
        .fetch_add(1, Ordering::Relaxed);
    eprintln!(
        "wavelinux6-audio-core chain_recovery_queued channel_id={} generation={} revision={}",
        shared.channel_id, generation, config.revision
    );
}

fn control_error(request_id: Option<String>, error: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "protocol_version": CORE_CONTROL_PROTOCOL_VERSION,
        "ok": false,
        "request_id": request_id,
        "error": error.into(),
    })
}

fn msec_to_frames(msec: u16, sample_rate_hz: u32) -> usize {
    ((u64::from(msec) * u64::from(sample_rate_hz)) / 1000)
        .max(1)
        .min(usize::MAX as u64) as usize
}

fn decode_interleaved_stereo_into(bytes: &[u8], channels: usize, out: &mut [f32]) -> usize {
    let sample_size = mem::size_of::<f32>();
    if channels == 0 || bytes.len() < sample_size || out.len() < 2 {
        return 0;
    }
    let frames = (bytes.len() / (channels * sample_size)).min(out.len() / 2);
    for frame in 0..frames {
        let base = frame * channels * sample_size;
        let left = read_f32le(bytes, base).unwrap_or(0.0);
        let right = if channels > 1 {
            read_f32le(bytes, base + sample_size).unwrap_or(left)
        } else {
            left
        };
        out[frame * 2] = left;
        out[frame * 2 + 1] = right;
    }
    frames
}

fn read_f32le(bytes: &[u8], offset: usize) -> Option<f32> {
    let end = offset.checked_add(mem::size_of::<f32>())?;
    let chunk = bytes.get(offset..end)?;
    Some(f32::from_le_bytes(chunk.try_into().ok()?))
}

fn audio_format_pod_bytes(sample_rate_hz: u32) -> Result<Vec<u8>, String> {
    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(sample_rate_hz);
    audio_info.set_channels(2);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    audio_info.set_position(position);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    Ok(spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|err| err.to_string())?
    .0
    .into_inner())
}

fn log_native_stats(shared: &NativeShared) {
    let stats = &shared.stats;
    let target_msec = shared.target_latency_msec.load(Ordering::Relaxed);
    let reason = shared
        .last_latency_reason
        .lock()
        .map(|reason| reason.clone())
        .unwrap_or_else(|_| "unknown".into());
    eprintln!(
        "wavelinux6-audio-core native_stats channel_id={} captured_frames={} rendered_frames={} dropped_frames={} underrun_frames={} capture_callbacks={} worker_running={} worker_blocks={} worker_queue_frames={} worker_queue_capacity_frames={} worker_overrun_frames={} process_calls={} last_process_us={} max_process_us={} buffered_frames={} target_latency_msec={} rate_correction={:.8} chain_swaps={} non_finite_blocks={} non_finite_samples={} non_finite_effect_mask={:#x} chain_recoveries={} chain_swap_replacements={} retired_chain_overflows={} acknowledged_generation={} reason={}",
        shared.channel_id,
        stats.captured_frames.load(Ordering::Relaxed),
        stats.rendered_frames.load(Ordering::Relaxed),
        stats.dropped_frames.load(Ordering::Relaxed),
        stats.underrun_frames.load(Ordering::Relaxed),
        stats.capture_callbacks.load(Ordering::Relaxed),
        shared.worker_running.load(Ordering::Acquire),
        stats.worker_blocks.load(Ordering::Relaxed),
        shared
            .raw_history
            .write_sequence()
            .saturating_sub(shared.worker_read_sequence.load(Ordering::Acquire))
            .min(shared.raw_history.capacity()),
        shared.raw_history.capacity(),
        stats.worker_overrun_frames.load(Ordering::Relaxed),
        stats.process_calls.load(Ordering::Relaxed),
        stats.last_process_micros.load(Ordering::Relaxed),
        stats.max_process_micros.load(Ordering::Relaxed),
        shared.current_buffer_frames.load(Ordering::Relaxed),
        target_msec,
        current_rate_correction(shared),
        stats.chain_swaps.load(Ordering::Relaxed),
        stats.non_finite_blocks.load(Ordering::Relaxed),
        stats.non_finite_samples.load(Ordering::Relaxed),
        stats.non_finite_effect_mask.load(Ordering::Relaxed),
        stats.chain_recoveries.load(Ordering::Relaxed),
        shared.chain_control.replacements.load(Ordering::Relaxed),
        shared.chain_control.retired_overflows.load(Ordering::Relaxed),
        shared
            .chain_control
            .acknowledged_generation
            .load(Ordering::Relaxed),
        reason
    );
    let acceleration = shared
        .acceleration_metrics
        .lock()
        .map(|metrics| metrics.clone())
        .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
    if acceleration.provider.is_some() || !acceleration.startup_failures.is_empty() {
        eprintln!(
            "wavelinux6-audio-core accelerator_stats channel_id={} provider={} active_states={} pids={:?} provider_blocks={} fallback_blocks={} deadline_misses={} invalid_results={} stale_results={} disabled_states={} startup_failures={:?} last_failure={}",
            shared.channel_id,
            acceleration.provider.as_deref().unwrap_or("cpu"),
            acceleration.active_states,
            acceleration.provider_pids,
            acceleration.provider_blocks,
            acceleration.fallback_blocks,
            acceleration.deadline_misses,
            acceleration.invalid_results,
            acceleration.stale_results,
            acceleration.disabled_states,
            acceleration.startup_failures,
            acceleration.last_failure.as_deref().unwrap_or("none")
        );
    }
}

fn current_rate_correction(shared: &NativeShared) -> f64 {
    let bits = shared.stats.rate_correction_bits.load(Ordering::Relaxed);
    if bits == 0 {
        1.0
    } else {
        f64::from_bits(bits)
    }
}

fn run_filter_chain_bridge(args: &[String]) -> Result<(), String> {
    install_signal_handlers();
    let channel_id = value_after(args, "--channel-id")
        .ok_or_else(|| "--run-filter-chain requires --channel-id".to_string())?;
    let config = value_after(args, "--config")
        .map(PathBuf::from)
        .ok_or_else(|| "--run-filter-chain requires --config".to_string())?;
    let adaptive_bridge_config = value_after(args, "--adaptive-bridge-config").map(PathBuf::from);
    if !config.is_file() {
        return Err(format!(
            "PipeWire filter-chain config is missing: {}",
            config.display()
        ));
    }
    if let Some(path) = &adaptive_bridge_config {
        if !path.is_file() {
            return Err(format!(
                "adaptive bridge config is missing: {}",
                path.display()
            ));
        }
    }

    let status = probe_backend_from_env();
    let pipewire_program = env::var(FILTER_CHAIN_PIPEWIRE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "pipewire".into());
    eprintln!(
        "wavelinux6-audio-core bridge_start channel_id={} runtime={} provider={} effective=pipewire_filter_chain pipewire={} config={}",
        channel_id,
        status.runtime.as_str(),
        status
            .selected_provider
            .map(|provider| provider.as_str())
            .unwrap_or("pipewire_filter_chain"),
        pipewire_program,
        config.display()
    );
    eprintln!(
        "wavelinux6-audio-core backend_status={}",
        serde_json::to_string(&status).map_err(|err| err.to_string())?
    );

    let mut child = Command::new(&pipewire_program)
        .arg("-c")
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => "pipewire command was not found".into(),
            _ => format!("failed to start pipewire filter-chain bridge: {err}"),
        })?;
    let child_pid = child.id();
    eprintln!("wavelinux6-audio-core bridge_child pid={child_pid}");

    if let Some(bridge_config_path) = adaptive_bridge_config {
        let bridge_config: DspChannelConfig = serde_json::from_str(
            &std::fs::read_to_string(&bridge_config_path)
                .map_err(|err| format!("failed to read adaptive bridge config: {err}"))?,
        )
        .map_err(|err| format!("failed to parse adaptive bridge config: {err}"))?;
        eprintln!(
            "wavelinux6-audio-core adaptive_bridge_start channel_id={} input={} output={} config={}",
            bridge_config.channel_id,
            bridge_config.input_node_name,
            bridge_config.output_node_name,
            bridge_config_path.display()
        );
        thread::spawn(move || loop {
            if TERMINATE.load(Ordering::SeqCst) {
                eprintln!("wavelinux6-audio-core bridge_stop child_pid={child_pid}");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!("wavelinux6-audio-core bridge_child_exit status={status}");
                    TERMINATE.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(err) => {
                    eprintln!("wavelinux6-audio-core bridge_child_wait_error {err}");
                    TERMINATE.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
        let result = run_pipewire_native_graph(bridge_config, status, None);
        TERMINATE.store(true, Ordering::SeqCst);
        return result;
    }

    loop {
        if TERMINATE.load(Ordering::SeqCst) {
            eprintln!("wavelinux6-audio-core bridge_stop child_pid={child_pid}");
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                eprintln!("wavelinux6-audio-core bridge_child_exit status={status}");
                return Ok(());
            }
            Ok(Some(status)) => {
                return Err(format!("pipewire filter-chain bridge exited with {status}"));
            }
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(err) => {
                return Err(format!(
                    "failed to monitor pipewire filter-chain bridge: {err}"
                ))
            }
        }
    }
}

fn run_bench(args: &[String]) -> Result<(), String> {
    if env::var_os(AUDIO_RUNTIME_ENV).is_none() {
        env::set_var(AUDIO_RUNTIME_ENV, AudioRuntimeMode::DspAuto.as_str());
    }
    let sample_rate_hz = value_after(args, "--sample-rate")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(DEFAULT_SAMPLE_RATE_HZ);
    let frames = value_after(args, "--frames")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(DEFAULT_FRAMES);
    let status = probe_backend_from_env();
    let metrics = benchmark_fixture(frames, sample_rate_hz);
    let elapsed = human_duration(std::time::Duration::from_micros(
        metrics.elapsed_micros.min(u64::MAX as u128) as u64,
    ));
    let report = BenchReport {
        helper: "wavelinux6-audio-core",
        status,
        sample_rate_hz,
        metrics,
        elapsed,
    };
    print_json(&report)
}

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
}

fn print_json<T: Serialize>(value: &T) -> Result<(), String> {
    serde_json::to_writer_pretty(std::io::stdout(), value).map_err(|err| err.to_string())?;
    println!();
    Ok(())
}

fn print_help() {
    println!(
        "wavelinux6-audio-core\n\
         \n\
         Usage:\n\
           wavelinux6-audio-core --probe\n\
           wavelinux6-audio-core --run-native --config PATH\n\
           wavelinux6-audio-core --run-filter-chain --channel-id ID --config PATH\n\
           wavelinux6-audio-core --bench-fixture [--frames N] [--sample-rate HZ]\n\
         \n\
         Environment:\n\
           WAVELINUX_AUDIO_RUNTIME=pipewire_filter_chain|dsp_cpu|dsp_auto|dsp_accelerated\n\
           WAVELINUX_DSP_PROVIDER=auto|cuda|openvino|migraphx|cpu\n\
           WAVELINUX_FILTER_CHAIN_PIPEWIRE=/usr/bin/pipewire"
    );
}

#[cfg(unix)]
fn install_signal_handlers() {
    unsafe extern "C" fn handle_signal(_signal: i32) {
        TERMINATE.store(true, Ordering::SeqCst);
    }

    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handle_signal as *const () as usize;
        action.sa_flags = 0;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
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
        eprintln!(
            "wavelinux6-crash process=wavelinux6-audio-core pid={} thread={} location={} payload={} backtrace=\n{}",
            process::id(),
            thread_name,
            location,
            payload,
            std::backtrace::Backtrace::force_capture(),
        );
    }));
}

#[cfg(not(unix))]
fn install_signal_handlers() {}

#[cfg(test)]
mod tests {
    use std::sync::{atomic::Ordering, Arc};
    use std::time::Duration;

    use super::{
        acquire_persistent_core_lock, apply_capture_idle_policy, apply_dsp_input_mode,
        begin_latency_transition, channel_uses_input_target, decode_interleaved_stereo_into,
        desired_rate_correction, duration_micros, insert_common_native_props, native_meter_release,
        native_stream_restore_id, playback_render_frames, recover_poisoned_chain,
        render_native_frame, replace_non_finite_with_dry, sanitize_non_finite_in_place,
        ChainSwapControl, DspWorkerData, FixedAudioHistory, InputEndpointStatus,
        InputTargetControl, NativeMeter, NativePlaybackData, NativeShared, PreparedChain,
        RealtimeTimingStats, CHAIN_CROSSFADE_MSEC, DSP_WORKER_BLOCK_FRAMES, LATENCY_CROSSFADE_MSEC,
        NATIVE_METER_STALE_AFTER,
    };
    use pipewire as pw;
    use wavelinux_dsp::{DspChain, DspChannelConfig, DspInputMode};

    fn test_worker_data(config: &DspChannelConfig) -> DspWorkerData {
        let shared = Arc::new(NativeShared::new(config, None, ""));
        let chain = DspChain::new_with_channels(&[], config.sample_rate_hz, config.input_channels);
        assert!(chain.is_fully_initialized());
        DspWorkerData {
            active_chain: Box::new(PreparedChain {
                chain,
                generation: config.generation,
                input_mode: config.input_mode,
            }),
            shadow_chain: None,
            chain_crossfade_progress: 0,
            chain_crossfade_total: (config.sample_rate_hz as usize * CHAIN_CROSSFADE_MSEC / 1000)
                .max(1),
            shared,
            read_sequence: 0,
            raw_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
            active_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
            active_dry_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
            shadow_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
            shadow_dry_scratch: vec![0.0; DSP_WORKER_BLOCK_FRAMES * 2].into_boxed_slice(),
            acceleration_metrics_countdown: 0,
        }
    }

    #[test]
    fn persistent_core_lock_rejects_a_second_owner() {
        let runtime = tempfile::tempdir().unwrap();
        let first = acquire_persistent_core_lock(runtime.path(), "topology-a").unwrap();
        let duplicate = acquire_persistent_core_lock(runtime.path(), "topology-a").unwrap_err();
        assert!(duplicate.contains("another WaveLinux 6 audio core owns"));

        drop(first);
        acquire_persistent_core_lock(runtime.path(), "topology-b").unwrap();
    }

    #[test]
    fn playback_render_frames_honors_requested_count() {
        assert_eq!(playback_render_frames(128, 4096, 8, 256), 128);
    }

    #[test]
    fn playback_render_frames_falls_back_to_configured_quantum() {
        assert_eq!(playback_render_frames(0, 4096, 8, 256), 256);
    }

    #[test]
    fn playback_render_frames_caps_at_buffer_capacity() {
        assert_eq!(playback_render_frames(1024, 512, 8, 256), 64);
        assert_eq!(playback_render_frames(0, 512, 8, 256), 64);
    }

    #[test]
    fn native_meter_clamps_invalid_samples_and_counts_frames() {
        let meter = NativeMeter::default();
        meter.publish(-0.25, f32::INFINITY, 0.125, f32::NAN, 128);

        let snapshot = meter.snapshot();
        assert!((snapshot.peak_left - 0.25).abs() < f32::EPSILON);
        assert_eq!(snapshot.peak_right, 0.0);
        assert!((snapshot.rms_left - 0.125).abs() < f32::EPSILON);
        assert_eq!(snapshot.rms_right, 0.0);
        assert_eq!(snapshot.frames, 128);

        meter.publish(2.0, -0.5, 1.5, 0.25, 64);
        let snapshot = meter.snapshot();
        assert_eq!(snapshot.peak_left, 1.0);
        assert!((snapshot.peak_right - 0.5).abs() < f32::EPSILON);
        assert_eq!(snapshot.rms_left, 1.0);
        assert!((snapshot.rms_right - 0.25).abs() < f32::EPSILON);
        assert_eq!(snapshot.frames, 192);
    }

    #[test]
    fn native_meter_release_stays_full_then_decays_deterministically() {
        let hold_micros = duration_micros(NATIVE_METER_STALE_AFTER);
        assert_eq!(native_meter_release(hold_micros), 1.0);
        let one_second_release = native_meter_release(hold_micros + 1_000_000);
        assert!((one_second_release - 0.08).abs() < 0.000_001);
        assert!(native_meter_release(hold_micros + 2_000_000) < one_second_release);
    }

    #[test]
    fn invalid_upstream_samples_are_silenced_before_dsp() {
        let mut samples = [0.25, f32::NAN, f32::INFINITY, -0.5];

        assert_eq!(sanitize_non_finite_in_place(&mut samples), 2);
        assert_eq!(samples, [0.25, 0.0, 0.0, -0.5]);
    }

    #[test]
    fn invalid_dsp_output_falls_back_to_the_dry_block() {
        let dry = [0.1, -0.1, 0.2, -0.2];
        let mut processed = [f32::NAN, 0.5, f32::NEG_INFINITY, -0.5];

        assert_eq!(replace_non_finite_with_dry(&mut processed, &dry), 2);
        assert_eq!(processed, [0.1, 0.5, 0.2, -0.5]);
        assert!(processed.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn one_poison_event_queues_only_one_recovery_without_advancing_the_config_generation() {
        let config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_hardware_in",
            "wavelinux6-mic",
            Vec::new(),
        );
        let shared = NativeShared::new(&config, None, "");
        shared
            .recovery_requested
            .store(true, std::sync::atomic::Ordering::Release);

        recover_poisoned_chain(&shared);
        let first_generation = shared
            .chain_control
            .submitted_generation
            .load(std::sync::atomic::Ordering::Acquire);
        shared
            .recovery_requested
            .store(true, std::sync::atomic::Ordering::Release);
        recover_poisoned_chain(&shared);

        assert_eq!(first_generation, config.generation);
        assert_eq!(
            shared
                .chain_control
                .submitted_generation
                .load(std::sync::atomic::Ordering::Acquire),
            first_generation
        );
        assert!(shared
            .recovery_in_progress
            .load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn rate_correction_drives_buffer_fill_toward_target() {
        assert!(desired_rate_correction(1_600.0, 1_344.0) < 1.0);
        assert!(desired_rate_correction(1_000.0, 1_344.0) > 1.0);
        assert_eq!(desired_rate_correction(1_344.0, 1_344.0), 1.0);
    }

    #[test]
    fn rate_correction_is_bounded_to_avoid_audible_pitch_changes() {
        assert_eq!(desired_rate_correction(100_000.0, 1.0), 0.997);
        assert_eq!(desired_rate_correction(0.0, 1.0), 1.002);
    }

    #[test]
    fn realtime_callback_histogram_reports_p99_without_callback_allocation() {
        let timing = RealtimeTimingStats::default();
        for _ in 0..99 {
            timing.record(Duration::from_micros(10));
        }
        timing.record(Duration::from_micros(900));

        assert_eq!(timing.count(), 100);
        assert_eq!(timing.p99_micros(), 25);
        assert_eq!(timing.max_micros(), 900);

        timing.record(Duration::from_micros(900));
        assert_eq!(timing.p99_micros(), 900);
    }

    #[test]
    fn fixed_audio_history_publishes_frames_without_allocation() {
        let history = FixedAudioHistory::new(8);
        for index in 0..8_u64 {
            history.push([index as f32, -(index as f32)]);
        }

        assert_eq!(history.write_sequence(), 8);
        assert_eq!(history.get(0), Some([0.0, 0.0]));
        assert_eq!(history.get(7), Some([7.0, -7.0]));
        assert_eq!(history.get(8), None);
    }

    #[test]
    fn fixed_audio_history_rejects_overwritten_frames() {
        let history = FixedAudioHistory::new(4);
        for index in 0..6_u64 {
            history.push([index as f32, index as f32]);
        }

        assert_eq!(history.get(0), None);
        assert_eq!(history.get(1), None);
        assert_eq!(history.get(2), Some([2.0, 2.0]));
        assert_eq!(history.get(5), Some([5.0, 5.0]));
    }

    #[test]
    fn latency_alignment_selects_the_nearest_correlated_tap() {
        let history = FixedAudioHistory::new(8_192);
        for index in 0..4_000_u64 {
            let phase = std::f32::consts::TAU * index as f32 / 12.0;
            let sample = phase.sin();
            history.push([sample, -sample]);
        }

        assert_eq!(history.aligned_latency_sequence(3_000, 2_425, 0.0), 2_424);
    }

    #[test]
    fn latency_alignment_does_not_move_silence() {
        let history = FixedAudioHistory::new(8_192);
        for _ in 0..4_000 {
            history.push([0.0, 0.0]);
        }

        assert_eq!(history.aligned_latency_sequence(3_000, 2_425, 0.0), 2_425);
    }

    #[test]
    fn latency_change_prepares_a_twenty_millisecond_dual_tap_crossfade() {
        let config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux6",
            "wavelinux6_fx_input",
            "wavelinux6-mic",
            Vec::new(),
        );
        let shared = Arc::new(NativeShared::new(&config, None, ""));
        for index in 0..10_000_u64 {
            shared.history.push([index as f32, index as f32]);
        }
        let mut playback = NativePlaybackData {
            shared,
            last_frame: [0.0, 0.0],
            read_sequence: Some(8_000),
            observed_input_route_generation: 1,
            applied_target_frames: 2_000,
            frames_since_recenter: 0,
            transition: None,
            recovery: None,
            rate_match: std::ptr::null_mut(),
            rate_correction: 1.0,
        };

        begin_latency_transition(&mut playback, 8_000, 6_000);

        let transition = playback.transition.expect("transition");
        assert_eq!(transition.from_sequence, 8_000);
        assert_eq!(transition.to_sequence, 6_000);
        assert_eq!(
            transition.total_frames,
            48_000 * LATENCY_CROSSFADE_MSEC / 1000
        );
    }

    #[test]
    fn overwritten_history_recenters_with_a_bounded_crossfade() {
        let config = DspChannelConfig::new(
            "game",
            "Game",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux6",
            "wavelinux6_channel_game",
            "wavelinux6_fx_game_source",
            Vec::new(),
        );
        let shared = Arc::new(NativeShared::new(&config, None, ""));
        shared
            .capture_streaming
            .store(true, std::sync::atomic::Ordering::Release);
        let capacity = shared.history.capacity();
        for index in 0..capacity + 2_000 {
            shared.history.push([index as f32, -(index as f32)]);
        }
        let mut playback = NativePlaybackData {
            shared: Arc::clone(&shared),
            last_frame: [0.25, -0.25],
            read_sequence: Some(0),
            observed_input_route_generation: 1,
            applied_target_frames: 1_344,
            frames_since_recenter: 0,
            transition: None,
            recovery: None,
            rate_match: std::ptr::null_mut(),
            rate_correction: 1.0,
        };
        let mut underruns = 0;

        let first = render_native_frame(&mut playback, &mut underruns);
        assert!(first.iter().all(|sample| sample.is_finite()));
        let recovery_frames = playback.recovery.expect("recovery").total_frames;
        for _ in 1..recovery_frames {
            let frame = render_native_frame(&mut playback, &mut underruns);
            assert!(frame.iter().all(|sample| sample.is_finite()));
        }

        assert!(playback.recovery.is_none());
        assert_eq!(underruns, 0);
        assert!(
            shared
                .stats
                .dropped_frames
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
        );
        assert!(
            shared
                .current_buffer_frames
                .load(std::sync::atomic::Ordering::Relaxed)
                <= capacity
        );
        assert!(shared
            .history
            .get(playback.read_sequence.unwrap())
            .is_some());
    }

    #[test]
    fn paused_capture_fades_without_recording_false_underruns() {
        let config = DspChannelConfig::new(
            "browser",
            "Browser",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux6",
            "wavelinux6_channel_browser",
            "wavelinux6_fx_browser_source",
            Vec::new(),
        );
        let shared = Arc::new(NativeShared::new(&config, None, ""));
        let mut playback = NativePlaybackData {
            shared,
            last_frame: [0.5, -0.5],
            read_sequence: Some(100),
            observed_input_route_generation: 1,
            applied_target_frames: 1_344,
            frames_since_recenter: 100,
            transition: None,
            recovery: None,
            rate_match: std::ptr::null_mut(),
            rate_correction: 1.0,
        };
        let mut underruns = 0;

        let frame = render_native_frame(&mut playback, &mut underruns);

        assert_eq!(underruns, 0);
        assert_eq!(playback.read_sequence, None);
        assert_eq!(frame, [0.4975, -0.4975]);
    }

    #[test]
    fn mono_capture_is_expanded_into_a_stereo_scratch_buffer() {
        let input = [0.25_f32.to_le_bytes(), (-0.5_f32).to_le_bytes()].concat();
        let mut output = [0.0_f32; 4];

        let frames = decode_interleaved_stereo_into(&input, 1, &mut output);

        assert_eq!(frames, 2);
        assert_eq!(output, [0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn input_modes_are_applied_before_dsp_without_allocating() {
        let source = [0.25, -0.75, 0.5, 0.25];
        let cases = [
            (DspInputMode::Stereo, [0.25, -0.75, 0.5, 0.25]),
            (DspInputMode::MonoLeft, [0.25, 0.25, 0.5, 0.5]),
            (DspInputMode::MonoRight, [-0.75, -0.75, 0.25, 0.25]),
            (DspInputMode::SumMono, [-0.25, -0.25, 0.375, 0.375]),
            (DspInputMode::SwapLr, [-0.75, 0.25, 0.25, 0.5]),
        ];
        for (mode, expected) in cases {
            let mut samples = source;
            apply_dsp_input_mode(&mut samples, mode);
            assert_eq!(samples, expected, "mode={mode:?}");
        }
    }

    #[test]
    fn dsp_worker_processes_mono_input_without_touching_the_capture_callback() {
        let mut config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_hardware_in",
            "wavelinux6-mic",
            Vec::new(),
        );
        config.input_mode = DspInputMode::MonoLeft;
        let mut worker = test_worker_data(&config);
        worker.shared.raw_history.push([0.25, -0.75]);

        assert_eq!(super::process_available_dsp_frames(&mut worker), 1);
        assert_eq!(worker.shared.history.get(0), Some([0.25, 0.25]));
        assert_eq!(worker.read_sequence, 1);
        assert_eq!(
            worker
                .shared
                .stats
                .worker_overrun_frames
                .load(Ordering::Relaxed),
            0
        );
    }

    #[test]
    fn dsp_worker_crossfades_an_input_mode_change_and_acknowledges_it() {
        let config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_hardware_in",
            "wavelinux6-mic",
            Vec::new(),
        );
        let mut worker = test_worker_data(&config);
        let replacement = DspChain::new_with_channels(&[], 48_000, 2);
        worker
            .shared
            .chain_control
            .submit(replacement, 2, DspInputMode::MonoLeft);
        for _ in 0..worker.chain_crossfade_total {
            worker.shared.raw_history.push([0.5, -0.5]);
        }

        while worker.read_sequence < worker.chain_crossfade_total as u64 {
            assert!(super::process_available_dsp_frames(&mut worker) > 0);
        }

        let last = worker
            .shared
            .history
            .get(worker.chain_crossfade_total as u64 - 1)
            .expect("last crossfade frame");
        assert!((last[0] - 0.5).abs() < 0.000_001);
        assert!((last[1] - 0.5).abs() < 0.000_001);
        assert_eq!(
            worker
                .shared
                .chain_control
                .acknowledged_generation
                .load(Ordering::Acquire),
            2
        );
    }

    #[test]
    fn dsp_worker_overrun_preserves_raw_and_processed_sequence_alignment() {
        let config = DspChannelConfig::new(
            "game",
            "Game",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_game",
            "wavelinux6_fx_game_source",
            Vec::new(),
        );
        let mut worker = test_worker_data(&config);
        let capacity = worker.shared.raw_history.capacity();
        for sequence in 0..=capacity {
            worker
                .shared
                .raw_history
                .push([sequence as f32, -(sequence as f32)]);
        }

        assert_eq!(
            super::process_available_dsp_frames(&mut worker),
            DSP_WORKER_BLOCK_FRAMES
        );
        assert_eq!(worker.shared.history.write_sequence(), worker.read_sequence);
        assert_eq!(worker.shared.history.get(0), None);
        assert_eq!(worker.shared.history.get(1), Some([1.0, -1.0]));
        assert_eq!(
            worker
                .shared
                .stats
                .worker_overrun_frames
                .load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn native_stream_restore_ids_are_isolated_by_channel_and_direction() {
        let hardware = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux6",
            "wavelinux6_fx_input",
            "wavelinux6-mic",
            Vec::new(),
        );
        let browser = DspChannelConfig::new(
            "browser",
            "Browser",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux6",
            "wavelinux6_channel_browser",
            "wavelinux6_fx_browser_source",
            Vec::new(),
        );

        assert_ne!(
            native_stream_restore_id(&hardware, "playback"),
            native_stream_restore_id(&browser, "playback")
        );
        assert_ne!(
            native_stream_restore_id(&hardware, "capture"),
            native_stream_restore_id(&hardware, "playback")
        );
    }

    #[test]
    fn application_channel_sinks_keep_their_clock_warm_when_unlinked() {
        let config = DspChannelConfig::new(
            "music",
            "Music",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_music",
            "wavelinux6_fx_music_source",
            Vec::new(),
        );
        let mut props = pipewire::properties::PropertiesBox::new();

        insert_common_native_props(&mut props, &config, "effect_input");
        apply_capture_idle_policy(&mut props, false);

        assert_eq!(props.get("node.pause-on-idle"), Some("false"));
        assert_eq!(props.get("node.always-process"), Some("true"));
    }

    #[test]
    fn hardware_target_capture_retains_idle_suspension() {
        let config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_hardware_in",
            "wavelinux6-mic",
            Vec::new(),
        );
        let mut props = pipewire::properties::PropertiesBox::new();

        insert_common_native_props(&mut props, &config, "input_target");
        apply_capture_idle_policy(&mut props, true);

        assert_eq!(props.get("node.pause-on-idle"), Some("true"));
        assert_eq!(props.get("node.always-process"), None);
    }

    #[test]
    fn chain_generations_are_monotonic_and_reject_stale_requests() {
        let control = ChainSwapControl::default();
        control.submitted_generation.store(7, Ordering::Release);

        assert!(!control.reserve_generation(7));
        assert!(!control.reserve_generation(6));
        assert!(control.reserve_generation(8));
        assert_eq!(control.submitted_generation.load(Ordering::Acquire), 8);
    }

    #[test]
    fn input_target_updates_are_latest_wins_and_monotonic() {
        let control = InputTargetControl::new(Some("alsa_input.old".into()));

        assert_eq!(
            control.queue(Some("alsa_input.old".into()), None).unwrap(),
            1
        );
        assert_eq!(
            control.queue(Some("alsa_input.usb".into()), None).unwrap(),
            2
        );
        assert_eq!(
            control
                .queue(Some("alsa_input.bluetooth".into()), None)
                .unwrap(),
            3
        );

        let pending = control.take_pending().expect("latest target");
        assert_eq!(pending.generation, 3);
        assert_eq!(pending.target.as_deref(), Some("alsa_input.bluetooth"));
        control.acknowledge(&pending);
        assert_eq!(
            control.current_target().as_deref(),
            Some("alsa_input.bluetooth")
        );
        assert_eq!(control.applied_generation.load(Ordering::Acquire), 3);
        assert!(control
            .queue(Some("alsa_input.internal".into()), Some(2))
            .is_err());

        assert_eq!(control.queue(None, None).unwrap(), 4);
        let pending = control.take_pending().expect("clear target");
        assert!(pending.target.is_none());
        control.acknowledge(&pending);
        assert!(control.current_target().is_none());
        assert_eq!(control.applied_generation.load(Ordering::Acquire), 4);
        assert_eq!(control.queue(None, None).unwrap(), 4);
    }

    #[test]
    fn input_endpoint_status_distinguishes_ready_streaming_and_error_states() {
        let status = InputEndpointStatus::default();
        status.observe_state(&pw::stream::StreamState::Paused);
        assert!(status.connected.load(Ordering::Acquire));
        assert!(!status.streaming.load(Ordering::Acquire));
        assert!(!status.failed.load(Ordering::Acquire));

        status.observe_state(&pw::stream::StreamState::Streaming);
        assert!(status.connected.load(Ordering::Acquire));
        assert!(status.streaming.load(Ordering::Acquire));

        status.observe_state(&pw::stream::StreamState::Error("target lost".into()));
        assert!(!status.connected.load(Ordering::Acquire));
        assert!(!status.streaming.load(Ordering::Acquire));
        assert!(status.failed.load(Ordering::Acquire));
    }

    #[test]
    fn hardware_input_keeps_target_stream_capability_without_a_selected_device() {
        let mut config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_channel_hardware_in",
            "wavelinux6-mic",
            Vec::new(),
        );
        config.input_target_capable = true;
        assert!(config.input_target_node_name.is_none());
        assert!(channel_uses_input_target(&config));
    }

    #[test]
    fn startup_acknowledgement_uses_the_manifest_generation() {
        let mut config = DspChannelConfig::new(
            "hardware_in",
            "Input",
            "wavelinux6",
            "wavelinux6",
            "WaveLinux 6",
            "wavelinux6_fx_hardware_in_input",
            "wavelinux6-mic",
            Vec::new(),
        );
        config.generation = 42;
        let shared = NativeShared::new(&config, None, "");
        shared
            .chain_control
            .submitted_generation
            .store(config.generation, Ordering::Release);
        shared
            .chain_control
            .acknowledged_generation
            .store(config.generation, Ordering::Release);

        assert_eq!(
            shared
                .chain_control
                .acknowledged_generation
                .load(Ordering::Acquire),
            42
        );
    }
}
