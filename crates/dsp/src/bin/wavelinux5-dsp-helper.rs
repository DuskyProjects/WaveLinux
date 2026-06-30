use std::collections::VecDeque;
use std::env;
use std::mem;
use std::path::PathBuf;
use std::process;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use pipewire as pw;
use pw::{properties::properties, spa};
use serde::Serialize;
use spa::pod::Pod;
use wavelinux_dsp::{
    benchmark_fixture, human_duration, native_dsp_effect_supported, probe_backend_from_env,
    AudioRuntimeMode, ChainMetrics, DspBackendStatus, DspChain, DspChannelConfig,
    AUDIO_RUNTIME_ENV,
};

const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_FRAMES: usize = DEFAULT_SAMPLE_RATE_HZ as usize * 5;
const FILTER_CHAIN_PIPEWIRE_ENV: &str = "WAVELINUX_FILTER_CHAIN_PIPEWIRE";
static TERMINATE: AtomicBool = AtomicBool::new(false);

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
    let args = env::args().skip(1).collect::<Vec<_>>();
    let result = if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        Ok(())
    } else if args.iter().any(|arg| arg == "--run-native") {
        run_native_graph(&args)
    } else if args.iter().any(|arg| arg == "--run-filter-chain") {
        run_filter_chain_bridge(&args)
    } else if args.iter().any(|arg| arg == "--bench-fixture") {
        run_bench(&args)
    } else {
        run_probe()
    };

    if let Err(err) = result {
        eprintln!("wavelinux5-dsp-helper: {err}");
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

    let status = probe_backend_from_env();
    eprintln!(
        "wavelinux5-dsp-helper native_start channel_id={} runtime={} provider={} input={} output={} config={}",
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
        "wavelinux5-dsp-helper backend_status={}",
        serde_json::to_string(&status).map_err(|err| err.to_string())?
    );

    run_pipewire_native_graph(config, status)
}

fn run_probe() -> Result<(), String> {
    let report = ProbeReport {
        helper: "wavelinux5-dsp-helper",
        status: probe_backend_from_env(),
    };
    print_json(&report)
}

#[derive(Debug, Default)]
struct NativeStats {
    captured_frames: u64,
    rendered_frames: u64,
    dropped_frames: u64,
    underrun_frames: u64,
    process_calls: u64,
    last_process_micros: u128,
    max_process_micros: u128,
}

#[derive(Debug)]
struct NativeShared {
    ring: Mutex<VecDeque<f32>>,
    stats: Mutex<NativeStats>,
    capacity_samples: usize,
}

impl NativeShared {
    fn new(latency_frames: u32) -> Self {
        let capacity_frames = latency_frames.max(256).saturating_mul(8) as usize;
        Self {
            ring: Mutex::new(VecDeque::with_capacity(capacity_frames * 2)),
            stats: Mutex::new(NativeStats::default()),
            capacity_samples: capacity_frames * 2,
        }
    }
}

struct NativeCaptureData {
    format: spa::param::audio::AudioInfoRaw,
    chain: DspChain,
    shared: Arc<NativeShared>,
}

struct NativePlaybackData {
    shared: Arc<NativeShared>,
}

fn run_pipewire_native_graph(
    config: DspChannelConfig,
    _status: DspBackendStatus,
) -> Result<(), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|err| format!("PipeWire native DSP mainloop creation failed: {err}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|err| format!("PipeWire native DSP context creation failed: {err}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|err| format!("PipeWire native DSP core connection failed: {err}"))?;
    let shared = Arc::new(NativeShared::new(config.latency_frames));

    let mut capture_props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "DSP",
        *pw::keys::MEDIA_CLASS => "Audio/Sink",
        *pw::keys::NODE_NAME => config.input_node_name.clone(),
        *pw::keys::NODE_DESCRIPTION => format!("{} FX {} Input", config.app_name, config.channel_name),
        *pw::keys::NODE_NICK => format!("{} FX Input", config.app_name),
        *pw::keys::MEDIA_NAME => format!("{} FX {} Input", config.app_name, config.channel_name),
        *pw::keys::NODE_VIRTUAL => "true",
        *pw::keys::NODE_ALWAYS_PROCESS => "true",
    };
    insert_common_native_props(&mut capture_props, &config, "effect_input");

    let mut playback_props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Playback",
        *pw::keys::MEDIA_ROLE => "DSP",
        *pw::keys::MEDIA_CLASS => "Audio/Source",
        *pw::keys::NODE_NAME => config.output_node_name.clone(),
        *pw::keys::NODE_DESCRIPTION => format!("{} FX {} Output", config.app_name, config.channel_name),
        *pw::keys::NODE_NICK => format!("{} FX Output", config.app_name),
        *pw::keys::MEDIA_NAME => format!("{} FX {} Output", config.app_name, config.channel_name),
        *pw::keys::NODE_VIRTUAL => "true",
        *pw::keys::NODE_ALWAYS_PROCESS => "true",
    };
    insert_common_native_props(&mut playback_props, &config, "effect_output");

    let capture_stream = pw::stream::StreamBox::new(
        &core,
        &format!("{}-dsp-capture-{}", config.graph_prefix, config.channel_id),
        capture_props,
    )
    .map_err(|err| format!("PipeWire native DSP capture stream creation failed: {err}"))?;
    let playback_stream = pw::stream::StreamBox::new(
        &core,
        &format!("{}-dsp-playback-{}", config.graph_prefix, config.channel_id),
        playback_props,
    )
    .map_err(|err| format!("PipeWire native DSP playback stream creation failed: {err}"))?;

    let capture_data = NativeCaptureData {
        format: Default::default(),
        chain: DspChain::new(&config.active_effects(), config.sample_rate_hz),
        shared: Arc::clone(&shared),
    };
    let playback_data = NativePlaybackData {
        shared: Arc::clone(&shared),
    };

    let _capture_listener = capture_stream
        .add_local_listener_with_user_data(capture_data)
        .state_changed(|_, _, old, new| {
            eprintln!(
                "wavelinux5-dsp-helper native_capture_state {:?}->{:?}",
                old, new
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

    let _playback_listener = playback_stream
        .add_local_listener_with_user_data(playback_data)
        .state_changed(|_, _, old, new| {
            eprintln!(
                "wavelinux5-dsp-helper native_playback_state {:?}->{:?}",
                old, new
            );
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
    playback_stream
        .connect(
            spa::utils::Direction::Output,
            None,
            flags,
            &mut playback_params,
        )
        .map_err(|err| format!("PipeWire native DSP playback connect failed: {err}"))?;

    let mut last_log = Instant::now();
    while !TERMINATE.load(Ordering::SeqCst) {
        mainloop.loop_().iterate(Duration::from_millis(5));
        if last_log.elapsed() >= Duration::from_secs(5) {
            log_native_stats(&shared);
            last_log = Instant::now();
        }
    }
    log_native_stats(&shared);
    eprintln!(
        "wavelinux5-dsp-helper native_stop channel_id={}",
        config.channel_id
    );
    Ok(())
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
    let mut interleaved = decode_interleaved_stereo(&bytes[offset..end], channels);
    if interleaved.is_empty() {
        return;
    }

    let started = Instant::now();
    let metrics = user_data.chain.process_interleaved_stereo(&mut interleaved);
    let elapsed = started.elapsed().as_micros();
    let frames = metrics.frames as u64;

    if let Ok(mut ring) = user_data.shared.ring.lock() {
        for sample in interleaved {
            if ring.len() >= user_data.shared.capacity_samples {
                ring.pop_front();
                if let Ok(mut stats) = user_data.shared.stats.lock() {
                    stats.dropped_frames = stats.dropped_frames.saturating_add(1);
                }
            }
            ring.push_back(sample);
        }
    }
    if let Ok(mut stats) = user_data.shared.stats.lock() {
        stats.captured_frames = stats.captured_frames.saturating_add(frames);
        stats.process_calls = stats.process_calls.saturating_add(1);
        stats.last_process_micros = elapsed;
        stats.max_process_micros = stats.max_process_micros.max(elapsed);
    }
}

fn process_playback_buffer(stream: &pw::stream::Stream, user_data: &mut NativePlaybackData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
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
    let frames = bytes.len() / stride;
    let mut rendered = 0_u64;
    let mut underrun = 0_u64;
    if let Ok(mut ring) = user_data.shared.ring.lock() {
        for frame in 0..frames {
            for channel in 0..2 {
                let sample = ring.pop_front().unwrap_or_else(|| {
                    underrun = underrun.saturating_add(1);
                    0.0
                });
                let start = frame * stride + channel * mem::size_of::<f32>();
                bytes[start..start + mem::size_of::<f32>()].copy_from_slice(&sample.to_le_bytes());
            }
            rendered = rendered.saturating_add(1);
        }
    }
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = stride as _;
    *chunk.size_mut() = (frames * stride) as _;
    if let Ok(mut stats) = user_data.shared.stats.lock() {
        stats.rendered_frames = stats.rendered_frames.saturating_add(rendered);
        stats.underrun_frames = stats.underrun_frames.saturating_add(underrun / 2);
    }
}

fn decode_interleaved_stereo(bytes: &[u8], channels: usize) -> Vec<f32> {
    let sample_size = mem::size_of::<f32>();
    if channels == 0 || bytes.len() < sample_size {
        return Vec::new();
    }
    let frames = bytes.len() / (channels * sample_size);
    let mut out = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let base = frame * channels * sample_size;
        let left = read_f32le(bytes, base).unwrap_or(0.0);
        let right = if channels > 1 {
            read_f32le(bytes, base + sample_size).unwrap_or(left)
        } else {
            left
        };
        out.push(left);
        out.push(right);
    }
    out
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
    let ring_samples = shared
        .ring
        .lock()
        .map(|ring| ring.len())
        .unwrap_or_default();
    if let Ok(stats) = shared.stats.lock() {
        eprintln!(
            "wavelinux5-dsp-helper native_stats captured_frames={} rendered_frames={} dropped_frames={} underrun_frames={} process_calls={} last_process_us={} max_process_us={} buffered_frames={}",
            stats.captured_frames,
            stats.rendered_frames,
            stats.dropped_frames / 2,
            stats.underrun_frames,
            stats.process_calls,
            stats.last_process_micros,
            stats.max_process_micros,
            ring_samples / 2
        );
    }
}

fn run_filter_chain_bridge(args: &[String]) -> Result<(), String> {
    install_signal_handlers();
    let channel_id = value_after(args, "--channel-id")
        .ok_or_else(|| "--run-filter-chain requires --channel-id".to_string())?;
    let config = value_after(args, "--config")
        .map(PathBuf::from)
        .ok_or_else(|| "--run-filter-chain requires --config".to_string())?;
    if !config.is_file() {
        return Err(format!(
            "PipeWire filter-chain config is missing: {}",
            config.display()
        ));
    }

    let status = probe_backend_from_env();
    let pipewire_program = env::var(FILTER_CHAIN_PIPEWIRE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "pipewire".into());
    eprintln!(
        "wavelinux5-dsp-helper bridge_start channel_id={} runtime={} provider={} effective=pipewire_filter_chain pipewire={} config={}",
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
        "wavelinux5-dsp-helper backend_status={}",
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
    eprintln!("wavelinux5-dsp-helper bridge_child pid={child_pid}");

    loop {
        if TERMINATE.load(Ordering::SeqCst) {
            eprintln!("wavelinux5-dsp-helper bridge_stop child_pid={child_pid}");
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                eprintln!("wavelinux5-dsp-helper bridge_child_exit status={status}");
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
        helper: "wavelinux5-dsp-helper",
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
        "wavelinux5-dsp-helper\n\
         \n\
         Usage:\n\
           wavelinux5-dsp-helper --probe\n\
           wavelinux5-dsp-helper --run-native --config PATH\n\
           wavelinux5-dsp-helper --run-filter-chain --channel-id ID --config PATH\n\
           wavelinux5-dsp-helper --bench-fixture [--frames N] [--sample-rate HZ]\n\
         \n\
         Environment:\n\
           WAVELINUX_AUDIO_RUNTIME=pipewire_filter_chain|dsp_cpu|dsp_auto|dsp_accelerated\n\
           WAVELINUX_DSP_PROVIDER=auto|cuda|openvino|cpu\n\
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

#[cfg(not(unix))]
fn install_signal_handlers() {}
