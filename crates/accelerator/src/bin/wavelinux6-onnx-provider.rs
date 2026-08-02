use ort::ep;
use ort::session::Session;
use ort::value::Tensor;
use serde::Serialize;
use std::env;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use wavelinux_accelerator::{
    monotonic_nanos, sha256_file, AcceleratorProvider, HostControlMessage, NeuralRequest,
    NeuralResponse, ProviderControlMessage, SharedNeuralQueue, ACCELERATOR_PROTOCOL_VERSION,
    RNNOISE_DENOISE_STATE_COUNT, RNNOISE_FEATURE_COUNT, RNNOISE_GAIN_COUNT,
    RNNOISE_NOISE_STATE_COUNT, RNNOISE_VAD_STATE_COUNT,
};

const MAX_CONTROL_MESSAGE_BYTES: usize = 16 * 1024;
const IDLE_SLEEP: Duration = Duration::from_micros(50);

#[derive(Debug)]
struct Arguments {
    mode: Mode,
    provider: AcceleratorProvider,
    model: PathBuf,
    runtime: PathBuf,
    socket: Option<PathBuf>,
    shared_memory: Option<PathBuf>,
    nonce: Option<String>,
    blocks: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Probe,
    Benchmark,
    Serve,
}

#[derive(Serialize)]
struct ProbeReport {
    protocol_version: u16,
    provider: AcceleratorProvider,
    execution_provider: &'static str,
    model_sha256: String,
    runtime: String,
    ready: bool,
}

#[derive(Serialize)]
struct BenchmarkReport {
    protocol_version: u16,
    provider: AcceleratorProvider,
    execution_provider: &'static str,
    model_sha256: String,
    blocks: u64,
    elapsed_msec: f64,
    blocks_per_second: f64,
    processing_p50_us: f64,
    processing_p95_us: f64,
    processing_max_us: f64,
    deadline_misses: u64,
}

struct OnnxNeuralStage {
    session: Session,
}

impl OnnxNeuralStage {
    fn load(arguments: &Arguments) -> Result<Self, String> {
        let initialized = ort::init_from(&arguments.runtime)
            .map_err(|error| format!("failed to load ONNX Runtime: {error}"))?
            .with_name("wavelinux6-accelerator-provider")
            .commit();
        if !initialized {
            return Err(
                "ONNX Runtime was initialized before the provider selected its library".into(),
            );
        }

        let provider = match arguments.provider {
            AcceleratorProvider::Cuda => ep::CUDA::default().build().error_on_failure(),
            AcceleratorProvider::OpenVino => ep::OpenVINO::default().build().error_on_failure(),
            AcceleratorProvider::MiGraphX => ep::MIGraphX::default().build().error_on_failure(),
            AcceleratorProvider::Cpu => ep::CPU::default().build().error_on_failure(),
        };
        let session = Session::builder()
            .map_err(|error| format!("failed to create ONNX session builder: {error}"))?
            .with_intra_threads(1)
            .map_err(|error| format!("failed to set ONNX intra-op threads: {error}"))?
            .with_inter_threads(1)
            .map_err(|error| format!("failed to set ONNX inter-op threads: {error}"))?
            .with_execution_providers([provider])
            .map_err(|error| {
                format!(
                    "failed to register {}: {error}",
                    arguments.provider.execution_provider_name()
                )
            })?
            .commit_from_file(&arguments.model)
            .map_err(|error| format!("failed to load RNNoise ONNX model: {error}"))?;
        let mut stage = Self { session };

        // Execution providers initialize kernels and device memory lazily. Do
        // that work before the control handshake so "ready" means the first
        // real audio block can meet its deadline.
        stage
            .process(&NeuralRequest::default())
            .map_err(|error| format!("failed to prime ONNX provider: {error}"))?;
        Ok(stage)
    }

    fn process(&mut self, request: &NeuralRequest) -> Result<NeuralResponse, String> {
        let started = monotonic_nanos();
        let vad_start = 0;
        let noise_start = vad_start + RNNOISE_VAD_STATE_COUNT;
        let denoise_start = noise_start + RNNOISE_NOISE_STATE_COUNT;
        let features = Tensor::from_array((
            [1usize, RNNOISE_FEATURE_COUNT],
            request.features.to_vec().into_boxed_slice(),
        ))
        .map_err(|error| format!("invalid feature tensor: {error}"))?;
        let vad_state = Tensor::from_array((
            [1usize, RNNOISE_VAD_STATE_COUNT],
            request.state[vad_start..noise_start]
                .to_vec()
                .into_boxed_slice(),
        ))
        .map_err(|error| format!("invalid VAD state tensor: {error}"))?;
        let noise_state = Tensor::from_array((
            [1usize, RNNOISE_NOISE_STATE_COUNT],
            request.state[noise_start..denoise_start]
                .to_vec()
                .into_boxed_slice(),
        ))
        .map_err(|error| format!("invalid noise state tensor: {error}"))?;
        let denoise_state = Tensor::from_array((
            [1usize, RNNOISE_DENOISE_STATE_COUNT],
            request.state[denoise_start..].to_vec().into_boxed_slice(),
        ))
        .map_err(|error| format!("invalid denoise state tensor: {error}"))?;

        let outputs = self
            .session
            .run(ort::inputs![
                "features" => features,
                "vad_state" => vad_state,
                "noise_state" => noise_state,
                "denoise_state" => denoise_state,
            ])
            .map_err(|error| format!("ONNX inference failed: {error}"))?;
        let completed = monotonic_nanos();
        let mut response = NeuralResponse {
            sequence: request.sequence,
            completed_monotonic_ns: completed,
            processing_ns: completed.saturating_sub(started),
            deadline_missed: u32::from(
                request.deadline_monotonic_ns != 0 && completed > request.deadline_monotonic_ns,
            ),
            ..NeuralResponse::default()
        };
        copy_output(&outputs, "gains", &mut response.gains)?;
        let mut vad = [0.0_f32; 1];
        copy_output(&outputs, "vad_probability", &mut vad)?;
        response.vad_probability = vad[0];
        copy_output(
            &outputs,
            "vad_state_out",
            &mut response.state[vad_start..noise_start],
        )?;
        copy_output(
            &outputs,
            "noise_state_out",
            &mut response.state[noise_start..denoise_start],
        )?;
        copy_output(
            &outputs,
            "denoise_state_out",
            &mut response.state[denoise_start..],
        )?;
        if response
            .gains
            .iter()
            .chain(std::iter::once(&response.vad_probability))
            .chain(response.state.iter())
            .any(|value| !value.is_finite())
        {
            return Err("ONNX provider returned non-finite RNNoise state".into());
        }
        Ok(response)
    }
}

fn copy_output(
    outputs: &ort::session::SessionOutputs<'_>,
    name: &str,
    destination: &mut [f32],
) -> Result<(), String> {
    let output = outputs
        .get(name)
        .ok_or_else(|| format!("ONNX model omitted output {name}"))?;
    let (_, values) = output
        .try_extract_tensor::<f32>()
        .map_err(|error| format!("ONNX output {name} is not f32: {error}"))?;
    if values.len() != destination.len() {
        return Err(format!(
            "ONNX output {name} has {} values, expected {}",
            values.len(),
            destination.len()
        ));
    }
    destination.copy_from_slice(values);
    Ok(())
}

fn run_probe(arguments: &Arguments, mut provider: OnnxNeuralStage) -> Result<(), String> {
    let response = provider.process(&NeuralRequest {
        sequence: 1,
        deadline_monotonic_ns: monotonic_nanos().saturating_add(1_000_000_000),
        ..NeuralRequest::default()
    })?;
    if response.gains.len() != RNNOISE_GAIN_COUNT {
        return Err("provider returned an invalid gain count".into());
    }
    let report = ProbeReport {
        protocol_version: ACCELERATOR_PROTOCOL_VERSION,
        provider: arguments.provider,
        execution_provider: arguments.provider.execution_provider_name(),
        model_sha256: sha256_file(&arguments.model).map_err(|error| error.to_string())?,
        runtime: arguments.runtime.display().to_string(),
        ready: true,
    };
    println!(
        "{}",
        serde_json::to_string(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn run_benchmark(arguments: &Arguments, mut provider: OnnxNeuralStage) -> Result<(), String> {
    let started = monotonic_nanos();
    let mut timings = Vec::with_capacity(arguments.blocks as usize);
    let mut state = [0.0_f32; wavelinux_accelerator::RNNOISE_STATE_COUNT];
    let mut deadline_misses = 0_u64;
    for sequence in 1..=arguments.blocks {
        let mut features = [0.0_f32; RNNOISE_FEATURE_COUNT];
        for (index, value) in features.iter_mut().enumerate() {
            *value = ((sequence as f32 * 0.017) + index as f32 * 0.11).sin();
        }
        let response = provider.process(&NeuralRequest {
            sequence,
            deadline_monotonic_ns: monotonic_nanos().saturating_add(10_000_000),
            features,
            state,
        })?;
        state = response.state;
        deadline_misses = deadline_misses.saturating_add(u64::from(response.deadline_missed));
        timings.push(response.processing_ns);
    }
    timings.sort_unstable();
    let elapsed_ns = monotonic_nanos().saturating_sub(started);
    let percentile = |percent: usize| -> f64 {
        let index = (timings.len().saturating_sub(1) * percent) / 100;
        timings.get(index).copied().unwrap_or(0) as f64 / 1_000.0
    };
    let elapsed_msec = elapsed_ns as f64 / 1_000_000.0;
    let report = BenchmarkReport {
        protocol_version: ACCELERATOR_PROTOCOL_VERSION,
        provider: arguments.provider,
        execution_provider: arguments.provider.execution_provider_name(),
        model_sha256: sha256_file(&arguments.model).map_err(|error| error.to_string())?,
        blocks: arguments.blocks,
        elapsed_msec,
        blocks_per_second: if elapsed_ns == 0 {
            0.0
        } else {
            arguments.blocks as f64 * 1_000_000_000.0 / elapsed_ns as f64
        },
        processing_p50_us: percentile(50),
        processing_p95_us: percentile(95),
        processing_max_us: timings.last().copied().unwrap_or(0) as f64 / 1_000.0,
        deadline_misses,
    };
    println!(
        "{}",
        serde_json::to_string(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn run_server(arguments: &Arguments, mut provider: OnnxNeuralStage) -> Result<(), String> {
    let socket_path = arguments
        .socket
        .as_deref()
        .ok_or_else(|| "--socket is required with --serve".to_string())?;
    let shared_memory_path = arguments
        .shared_memory
        .as_deref()
        .ok_or_else(|| "--shared-memory is required with --serve".to_string())?;
    let nonce = arguments
        .nonce
        .as_deref()
        .ok_or_else(|| "--nonce is required with --serve".to_string())?;
    let model_sha256 = sha256_file(&arguments.model).map_err(|error| error.to_string())?;
    let mut queue =
        SharedNeuralQueue::open(shared_memory_path).map_err(|error| error.to_string())?;
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|error| format!("failed to connect control socket: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| error.to_string())?;
    let hello = read_control_message(&stream)?;
    match hello {
        HostControlMessage::Hello {
            protocol_version,
            nonce: host_nonce,
            provider: host_provider,
            shared_memory_path: host_shared_memory,
            model_path,
            model_sha256: host_model_sha256,
        } if protocol_version == ACCELERATOR_PROTOCOL_VERSION
            && host_nonce == nonce
            && host_provider == arguments.provider
            && Path::new(&host_shared_memory) == shared_memory_path
            && Path::new(&model_path) == arguments.model
            && host_model_sha256.eq_ignore_ascii_case(&model_sha256) => {}
        other => return Err(format!("invalid host handshake: {other:?}")),
    }
    write_control_message(
        &mut stream,
        &ProviderControlMessage::Hello {
            protocol_version: ACCELERATOR_PROTOCOL_VERSION,
            nonce: nonce.to_string(),
            provider: arguments.provider,
            execution_provider: arguments.provider.execution_provider_name().into(),
            pid: std::process::id(),
            model_sha256,
        },
    )?;
    stream
        .set_read_timeout(None)
        .map_err(|error| error.to_string())?;

    let mut processed_blocks = 0_u64;
    let mut missed_deadlines = 0_u64;
    while !queue.shutdown_requested() {
        let Some(request) = queue.receive_request() else {
            thread::sleep(IDLE_SLEEP);
            continue;
        };
        let response = provider.process(&request)?;
        missed_deadlines = missed_deadlines.saturating_add(u64::from(response.deadline_missed));
        processed_blocks = processed_blocks.saturating_add(1);
        if queue.publish(response).is_err() {
            return Err("host stopped draining accelerator responses".into());
        }
    }
    let _ = write_control_message(
        &mut stream,
        &ProviderControlMessage::Status {
            protocol_version: ACCELERATOR_PROTOCOL_VERSION,
            processed_blocks,
            missed_deadlines,
            last_error: None,
        },
    );
    Ok(())
}

fn read_control_message(stream: &UnixStream) -> Result<HostControlMessage, String> {
    let mut line = Vec::new();
    BufReader::new(stream)
        .take((MAX_CONTROL_MESSAGE_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)
        .map_err(|error| error.to_string())?;
    if line.is_empty() || line.len() > MAX_CONTROL_MESSAGE_BYTES || !line.ends_with(b"\n") {
        return Err("invalid or oversized accelerator control message".into());
    }
    serde_json::from_slice(&line).map_err(|error| format!("invalid control JSON: {error}"))
}

fn write_control_message(
    stream: &mut UnixStream,
    message: &ProviderControlMessage,
) -> Result<(), String> {
    let payload = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    if payload.len() + 1 > MAX_CONTROL_MESSAGE_BYTES {
        return Err("accelerator control message exceeds protocol bound".into());
    }
    stream
        .write_all(&payload)
        .map_err(|error| error.to_string())?;
    stream.write_all(b"\n").map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut values = env::args().skip(1);
    let mut mode = None;
    let mut provider = None;
    let mut model = None;
    let mut runtime = None;
    let mut socket = None;
    let mut shared_memory = None;
    let mut nonce = None;
    let mut blocks = 5_000_u64;
    while let Some(argument) = values.next() {
        match argument.as_str() {
            "--probe" => mode = Some(Mode::Probe),
            "--benchmark" => mode = Some(Mode::Benchmark),
            "--serve" => mode = Some(Mode::Serve),
            "--provider" => {
                let value = values
                    .next()
                    .ok_or_else(|| "--provider requires a value".to_string())?;
                provider = AcceleratorProvider::parse(&value);
                if provider.is_none() {
                    return Err(format!("unsupported provider: {value}"));
                }
            }
            "--model" => model = values.next().map(PathBuf::from),
            "--runtime" => runtime = values.next().map(PathBuf::from),
            "--socket" => socket = values.next().map(PathBuf::from),
            "--shared-memory" => shared_memory = values.next().map(PathBuf::from),
            "--nonce" => nonce = values.next(),
            "--blocks" => {
                let value = values
                    .next()
                    .ok_or_else(|| "--blocks requires a value".to_string())?;
                blocks = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --blocks value: {value}"))?;
                if blocks == 0 || blocks > 10_000_000 {
                    return Err("--blocks must be between 1 and 10000000".into());
                }
            }
            "--version" | "-V" => {
                println!(
                    "wavelinux6-onnx-provider {} protocol {}",
                    env!("CARGO_PKG_VERSION"),
                    ACCELERATOR_PROTOCOL_VERSION
                );
                std::process::exit(0);
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}")),
        }
    }
    let mode = mode.ok_or_else(|| "--probe, --benchmark, or --serve is required".to_string())?;
    let provider = provider.ok_or_else(|| "--provider is required".to_string())?;
    let model = model.ok_or_else(|| "--model is required".to_string())?;
    let runtime = runtime
        .or_else(|| env::var_os("WAVELINUX_ONNXRUNTIME_LIBRARY").map(PathBuf::from))
        .or_else(find_onnx_runtime)
        .ok_or_else(|| {
            "--runtime or WAVELINUX_ONNXRUNTIME_LIBRARY must identify libonnxruntime.so".to_string()
        })?;
    if !model.is_file() {
        return Err(format!("ONNX model is missing: {}", model.display()));
    }
    if !runtime.is_file() {
        return Err(format!(
            "ONNX Runtime library is missing: {}",
            runtime.display()
        ));
    }
    Ok(Arguments {
        mode,
        provider,
        model,
        runtime,
        socket,
        shared_memory,
        nonce,
        blocks,
    })
}

fn find_onnx_runtime() -> Option<PathBuf> {
    [
        "/usr/lib/libonnxruntime.so",
        "/usr/lib64/libonnxruntime.so",
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
        "/usr/local/lib/libonnxruntime.so",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file())
}

fn print_help() {
    println!(
        "WaveLinux 6 isolated ONNX provider\n\n\
         Usage:\n\
           wavelinux6-onnx-provider --probe --provider PROVIDER --model FILE [--runtime FILE]\n\
           wavelinux6-onnx-provider --benchmark --provider PROVIDER --model FILE [--runtime FILE] [--blocks N]\n\
           wavelinux6-onnx-provider --serve --provider PROVIDER --model FILE --socket FILE \\\n              --shared-memory FILE --nonce VALUE [--runtime FILE]\n\n\
         Providers: cuda, openvino, migraphx, cpu"
    );
}

fn main() {
    let result = parse_arguments().and_then(|arguments| {
        let provider = OnnxNeuralStage::load(&arguments)?;
        match arguments.mode {
            Mode::Probe => run_probe(&arguments, provider),
            Mode::Benchmark => run_benchmark(&arguments, provider),
            Mode::Serve => run_server(&arguments, provider),
        }
    });
    if let Err(error) = result {
        eprintln!("wavelinux6-onnx-provider: {error}");
        std::process::exit(1);
    }
}
