use serde::Deserialize;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wavelinux_accelerator::{
    blank_qualification, hardware_fingerprint, monotonic_nanos, AcceleratorProvider,
    NeuralResponse, ProviderClient, ProviderPackManifest, QualificationRecord,
    ResolvedProviderPack, RNNOISE_DENOISE_STATE_COUNT, RNNOISE_FEATURE_COUNT, RNNOISE_GAIN_COUNT,
    RNNOISE_NOISE_STATE_COUNT, RNNOISE_STATE_COUNT, RNNOISE_VAD_STATE_COUNT,
};

const BLOCK_DEADLINE: Duration = Duration::from_millis(10);
const BLOCK_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Debug)]
struct Arguments {
    pack: PathBuf,
    runtime: Option<PathBuf>,
    blocks: u64,
    write_record: bool,
}

#[derive(Debug, Deserialize)]
struct GoldenFixture {
    max_abs_error: f64,
    cases: Vec<GoldenCase>,
}

#[derive(Debug, Deserialize)]
struct GoldenCase {
    features: Vec<f32>,
    vad_state: Vec<f32>,
    noise_state: Vec<f32>,
    denoise_state: Vec<f32>,
    gains: Vec<f32>,
    vad_probability: f32,
    vad_state_out: Vec<f32>,
    noise_state_out: Vec<f32>,
    denoise_state_out: Vec<f32>,
}

#[derive(Debug)]
struct RunMetrics {
    maximum_error: f64,
    deadline_misses: u64,
    p95_latency_us: f64,
    process_cpu_seconds: f64,
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut values = env::args().skip(1);
    let mut pack = None;
    let mut runtime = None;
    let mut blocks = 5_000_u64;
    let mut write_record = false;
    while let Some(argument) = values.next() {
        match argument.as_str() {
            "--pack" => pack = values.next().map(PathBuf::from),
            "--runtime" => runtime = values.next().map(PathBuf::from),
            "--blocks" => {
                let value = values
                    .next()
                    .ok_or_else(|| "--blocks requires a value".to_string())?;
                blocks = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --blocks value: {value}"))?;
                if blocks == 0 || blocks > 1_000_000 {
                    return Err("--blocks must be between 1 and 1000000".into());
                }
            }
            "--write" => write_record = true,
            "--version" | "-V" => {
                println!("wavelinux6-accelerator-qualify protocol 1");
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: wavelinux6-accelerator-qualify --pack DIRECTORY \
                     [--runtime libonnxruntime.so] [--blocks N] [--write]"
                );
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}")),
        }
    }
    Ok(Arguments {
        pack: pack.ok_or_else(|| "--pack is required".to_string())?,
        runtime,
        blocks,
        write_record,
    })
}

fn load_pack(arguments: &Arguments) -> Result<ResolvedProviderPack, String> {
    let payload = fs::read(arguments.pack.join("manifest.json"))
        .map_err(|error| format!("failed to read provider manifest: {error}"))?;
    let manifest: ProviderPackManifest = serde_json::from_slice(&payload)
        .map_err(|error| format!("invalid provider manifest: {error}"))?;
    let mut resolved = manifest
        .resolve(&arguments.pack)
        .map_err(|error| error.to_string())?;
    if let Some(runtime) = &arguments.runtime {
        if !runtime.is_file() {
            return Err(format!(
                "ONNX Runtime library is missing: {}",
                runtime.display()
            ));
        }
        resolved.onnx_runtime_library = Some(runtime.clone());
    }
    Ok(resolved)
}

fn load_fixture(path: &Path) -> Result<GoldenFixture, String> {
    let fixture: GoldenFixture = serde_json::from_slice(
        &fs::read(path).map_err(|error| format!("failed to read golden fixture: {error}"))?,
    )
    .map_err(|error| format!("invalid golden fixture: {error}"))?;
    if fixture.cases.is_empty() {
        return Err("golden fixture contains no cases".into());
    }
    for case in &fixture.cases {
        if case.features.len() != RNNOISE_FEATURE_COUNT
            || case.vad_state.len() != RNNOISE_VAD_STATE_COUNT
            || case.noise_state.len() != RNNOISE_NOISE_STATE_COUNT
            || case.denoise_state.len() != RNNOISE_DENOISE_STATE_COUNT
            || case.gains.len() != RNNOISE_GAIN_COUNT
            || case.vad_state_out.len() != RNNOISE_VAD_STATE_COUNT
            || case.noise_state_out.len() != RNNOISE_NOISE_STATE_COUNT
            || case.denoise_state_out.len() != RNNOISE_DENOISE_STATE_COUNT
        {
            return Err("golden fixture contains an invalid tensor shape".into());
        }
    }
    Ok(fixture)
}

fn run_provider(
    pack: &ResolvedProviderPack,
    fixture: &GoldenFixture,
    blocks: u64,
    runtime_directory: &Path,
) -> Result<RunMetrics, String> {
    let mut client = ProviderClient::spawn(pack, runtime_directory)
        .map_err(|error| format!("provider startup failed: {error}"))?;
    let cpu_before = process_cpu_ticks(client.pid()).unwrap_or(0);
    let mut latencies = Vec::with_capacity(blocks as usize);
    let mut maximum_error = 0.0_f64;
    let mut deadline_misses = 0_u64;
    for index in 0..blocks {
        let case = &fixture.cases[index as usize % fixture.cases.len()];
        let features = copy_array::<RNNOISE_FEATURE_COUNT>(&case.features);
        let mut state = [0.0_f32; RNNOISE_STATE_COUNT];
        state[..RNNOISE_VAD_STATE_COUNT].copy_from_slice(&case.vad_state);
        let noise_start = RNNOISE_VAD_STATE_COUNT;
        let denoise_start = noise_start + RNNOISE_NOISE_STATE_COUNT;
        state[noise_start..denoise_start].copy_from_slice(&case.noise_state);
        state[denoise_start..].copy_from_slice(&case.denoise_state);
        let started = monotonic_nanos();
        let sequence = client
            .submit(
                features,
                state,
                started.saturating_add(BLOCK_DEADLINE.as_nanos() as u64),
            )
            .map_err(|error| error.to_string())?;
        let response = client
            .wait(sequence, BLOCK_TIMEOUT)
            .ok_or_else(|| format!("provider missed response for sequence {sequence}"))?;
        latencies.push(monotonic_nanos().saturating_sub(started));
        deadline_misses = deadline_misses.saturating_add(u64::from(response.deadline_missed));
        maximum_error = maximum_error.max(response_error(&response, case));
    }
    let cpu_after = process_cpu_ticks(client.pid()).unwrap_or(cpu_before);
    client.shutdown();
    latencies.sort_unstable();
    let p95_index = (latencies.len().saturating_sub(1) * 95) / 100;
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    Ok(RunMetrics {
        maximum_error,
        deadline_misses,
        p95_latency_us: latencies.get(p95_index).copied().unwrap_or(0) as f64 / 1_000.0,
        process_cpu_seconds: cpu_after.saturating_sub(cpu_before) as f64 / ticks_per_second,
    })
}

fn response_error(response: &NeuralResponse, case: &GoldenCase) -> f64 {
    let mut maximum = max_error(&response.gains, &case.gains);
    maximum = maximum.max((response.vad_probability - case.vad_probability).abs() as f64);
    maximum = maximum.max(max_error(
        &response.state[..RNNOISE_VAD_STATE_COUNT],
        &case.vad_state_out,
    ));
    let noise_start = RNNOISE_VAD_STATE_COUNT;
    let denoise_start = noise_start + RNNOISE_NOISE_STATE_COUNT;
    maximum = maximum.max(max_error(
        &response.state[noise_start..denoise_start],
        &case.noise_state_out,
    ));
    maximum.max(max_error(
        &response.state[denoise_start..],
        &case.denoise_state_out,
    ))
}

fn max_error(actual: &[f32], expected: &[f32]) -> f64 {
    actual
        .iter()
        .zip(expected)
        .map(|(actual, expected)| (*actual - *expected).abs() as f64)
        .fold(0.0, f64::max)
}

fn copy_array<const N: usize>(values: &[f32]) -> [f32; N] {
    let mut output = [0.0; N];
    output.copy_from_slice(values);
    output
}

fn process_cpu_ticks(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let user = fields.get(11)?.parse::<u64>().ok()?;
    let system = fields.get(12)?.parse::<u64>().ok()?;
    Some(user.saturating_add(system))
}

fn runtime_directory(provider: AcceleratorProvider, suffix: &str) -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("wavelinux6/accelerator-qualification")
        .join(format!(
            "{}-{suffix}-{}",
            provider.as_str(),
            std::process::id()
        ))
}

fn atomic_write(path: &Path, record: &QualificationRecord) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "qualification path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(".qualification-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    serde_json::to_writer_pretty(&mut file, record).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    Ok(())
}

fn qualify(arguments: &Arguments) -> Result<QualificationRecord, String> {
    let candidate = load_pack(arguments)?;
    if candidate.manifest.provider == AcceleratorProvider::Cpu {
        return Err("CPU is the qualification baseline, not an accelerator provider".into());
    }
    let fixture = load_fixture(&candidate.golden_fixture)?;
    let candidate_metrics = run_provider(
        &candidate,
        &fixture,
        arguments.blocks,
        &runtime_directory(candidate.manifest.provider, "candidate"),
    )?;
    let mut baseline = candidate.clone();
    baseline.manifest.provider = AcceleratorProvider::Cpu;
    let baseline_metrics = run_provider(
        &baseline,
        &fixture,
        arguments.blocks,
        &runtime_directory(candidate.manifest.provider, "baseline"),
    )?;
    let cpu_reduction_percent = if baseline_metrics.process_cpu_seconds <= 0.0 {
        0.0
    } else {
        100.0 * (baseline_metrics.process_cpu_seconds - candidate_metrics.process_cpu_seconds)
            / baseline_metrics.process_cpu_seconds
    };
    let mut record = blank_qualification(
        candidate.manifest.provider,
        &candidate.manifest.pack_version,
        &candidate.manifest.model_sha256,
        hardware_fingerprint(candidate.manifest.provider),
    );
    record.tested_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    record.blocks = arguments.blocks;
    record.numerical_max_abs_error = candidate_metrics.maximum_error;
    record.deadline_misses = candidate_metrics.deadline_misses;
    record.discontinuities = 0;
    record.added_latency_msec =
        (candidate_metrics.p95_latency_us - baseline_metrics.p95_latency_us) / 1_000.0;
    record.cpu_reduction_percent = cpu_reduction_percent;
    // The isolated fixture validates transport and inference. These remain
    // false until the live audio-core stress gate validates state fallback and
    // total active-core CPU without a discontinuity.
    record.fallback_validated = false;
    record.live_workload_validated = false;
    if record.numerical_max_abs_error > fixture.max_abs_error {
        record.reason = format!("provider exceeded fixture limit {}", fixture.max_abs_error);
    }
    Ok(record.evaluate())
}

fn main() {
    let result = parse_arguments().and_then(|arguments| {
        let record = qualify(&arguments)?;
        if arguments.write_record {
            atomic_write(&arguments.pack.join("qualification.json"), &record)?;
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&record).map_err(|error| error.to_string())?
        );
        Ok(())
    });
    if let Err(error) = result {
        eprintln!("wavelinux6-accelerator-qualify: {error}");
        std::process::exit(1);
    }
}
