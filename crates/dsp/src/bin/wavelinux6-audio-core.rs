use std::collections::VecDeque;
use std::env;
use std::io::{Read, Write};
use std::mem;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    AudioRuntimeMode, ChainMetrics, DspBackendStatus, DspChain, DspChannelConfig,
    AUDIO_RUNTIME_ENV,
};

const DEFAULT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_FRAMES: usize = DEFAULT_SAMPLE_RATE_HZ as usize * 5;
const FILTER_CHAIN_PIPEWIRE_ENV: &str = "WAVELINUX_FILTER_CHAIN_PIPEWIRE";
const BUFFER_TARGET_HYSTERESIS_FRAMES: usize = 256;
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
    sample_rate_hz: u32,
    render_quantum_frames: usize,
    target_latency_msec: AtomicUsize,
    target_samples: AtomicUsize,
    last_latency_reason: Mutex<String>,
}

impl NativeShared {
    fn new(config: &DspChannelConfig) -> Self {
        let adaptive = &config.adaptive_latency;
        let max_msec = adaptive.max_msec.max(adaptive.min_msec).max(28);
        let min_msec = adaptive.min_msec.min(max_msec).max(5);
        let target_frames = msec_to_frames(min_msec, config.sample_rate_hz);
        let capacity_frames = msec_to_frames(max_msec.saturating_mul(2), config.sample_rate_hz)
            .max(config.latency_frames.max(256) as usize * 8);
        Self {
            ring: Mutex::new(VecDeque::with_capacity(capacity_frames * 2)),
            stats: Mutex::new(NativeStats::default()),
            capacity_samples: capacity_frames * 2,
            sample_rate_hz: config.sample_rate_hz,
            render_quantum_frames: config.latency_frames.max(1) as usize,
            target_latency_msec: AtomicUsize::new(min_msec as usize),
            target_samples: AtomicUsize::new(target_frames * 2),
            last_latency_reason: Mutex::new("initial".into()),
        }
    }

    fn set_target_latency(&self, target_msec: u16, reason: &str) {
        let target_msec = target_msec.clamp(5, 500);
        self.target_latency_msec
            .store(target_msec as usize, Ordering::Relaxed);
        self.target_samples.store(
            msec_to_frames(target_msec, self.sample_rate_hz) * 2,
            Ordering::Relaxed,
        );
        if let Ok(mut last_reason) = self.last_latency_reason.lock() {
            *last_reason = reason.to_string();
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
    last_frame: [f32; 2],
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
    let shared = Arc::new(NativeShared::new(&config));
    start_latency_control_socket(&config, Arc::clone(&shared));

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
    let input_role = config.input_role.as_deref().unwrap_or("effect_input");
    insert_common_native_props(&mut capture_props, &config, input_role);

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
    let output_role = config.output_role.as_deref().unwrap_or("effect_output");
    insert_common_native_props(&mut playback_props, &config, output_role);

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
        last_frame: [0.0, 0.0],
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
    let requested_frames = buffer.requested() as usize;
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
    let mut rendered = 0_u64;
    let mut underrun = 0_u64;
    if let Ok(mut ring) = user_data.shared.ring.lock() {
        let target_samples = user_data.shared.target_samples.load(Ordering::Relaxed);
        let hysteresis_samples = BUFFER_TARGET_HYSTERESIS_FRAMES * 2;
        for frame in 0..frames {
            let duplicate_frame =
                under_target_duplicate_interval(ring.len(), target_samples, hysteresis_samples)
                    .is_some_and(|interval| frame % interval == interval - 1)
                    && !ring.is_empty();
            let mut output_frame = user_data.last_frame;
            for (channel, output_sample) in output_frame.iter_mut().enumerate() {
                let sample = if duplicate_frame {
                    user_data.last_frame[channel]
                } else {
                    ring.pop_front().unwrap_or_else(|| {
                        underrun = underrun.saturating_add(1);
                        user_data.last_frame[channel]
                    })
                };
                *output_sample = sample;
                let start = frame * stride + channel * mem::size_of::<f32>();
                bytes[start..start + mem::size_of::<f32>()].copy_from_slice(&sample.to_le_bytes());
            }
            user_data.last_frame = output_frame;
            let drop_frame =
                over_target_drop_interval(ring.len(), target_samples, hysteresis_samples)
                    .is_some_and(|interval| frame % interval == interval - 1);
            if drop_frame && ring.len() >= 2 {
                ring.pop_front();
                ring.pop_front();
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

fn under_target_duplicate_interval(
    current_samples: usize,
    target_samples: usize,
    hysteresis_samples: usize,
) -> Option<usize> {
    if target_samples == 0 || current_samples.saturating_add(hysteresis_samples) >= target_samples {
        return None;
    }
    Some(drift_correction_interval(
        target_samples.saturating_sub(current_samples),
        target_samples,
    ))
}

fn over_target_drop_interval(
    current_samples: usize,
    target_samples: usize,
    hysteresis_samples: usize,
) -> Option<usize> {
    if target_samples == 0 || current_samples <= target_samples.saturating_add(hysteresis_samples) {
        return None;
    }
    Some(drift_correction_interval(
        current_samples.saturating_sub(target_samples),
        target_samples,
    ))
}

fn drift_correction_interval(delta_samples: usize, target_samples: usize) -> usize {
    let quarter = (target_samples / 4).max(1);
    if delta_samples >= quarter.saturating_mul(3) {
        2
    } else if delta_samples >= quarter.saturating_mul(2) {
        4
    } else if delta_samples >= quarter {
        8
    } else {
        12
    }
}

#[derive(Debug, Deserialize)]
struct LatencyControlCommand {
    command: String,
    #[serde(default)]
    route_id: Option<String>,
    target_msec: u16,
    #[serde(default)]
    reason: Option<String>,
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
                "wavelinux5-dsp-helper adaptive_latency_socket_failed path={}",
                path.display()
            );
            return;
        };
        let _ = listener.set_nonblocking(true);
        eprintln!(
            "wavelinux5-dsp-helper adaptive_latency_socket path={} channel_id={}",
            path.display(),
            channel_id
        );
        while !TERMINATE.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    let mut payload = String::new();
                    let _ = stream.read_to_string(&mut payload);
                    let response = match serde_json::from_str::<LatencyControlCommand>(&payload) {
                        Ok(command)
                            if command.command == "set_target_latency"
                                && command
                                    .route_id
                                    .as_deref()
                                    .is_none_or(|route| route == channel_id) =>
                        {
                            let reason = command
                                .reason
                                .as_deref()
                                .unwrap_or("adaptive_latency_control");
                            shared.set_target_latency(command.target_msec, reason);
                            format!("{{\"ok\":true,\"target_msec\":{}}}\n", command.target_msec)
                        }
                        Ok(_) => "{\"ok\":false,\"error\":\"unsupported_command\"}\n".into(),
                        Err(err) => format!("{{\"ok\":false,\"error\":\"{err}\"}}\n"),
                    };
                    let _ = stream.write_all(response.as_bytes());
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(err) => {
                    eprintln!("wavelinux5-dsp-helper adaptive_latency_accept_error {err}");
                    thread::sleep(Duration::from_millis(250));
                }
            }
        }
        let _ = std::fs::remove_file(&path);
    });
}

fn msec_to_frames(msec: u16, sample_rate_hz: u32) -> usize {
    ((u64::from(msec) * u64::from(sample_rate_hz)) / 1000)
        .max(1)
        .min(usize::MAX as u64) as usize
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
        let target_msec = shared.target_latency_msec.load(Ordering::Relaxed);
        let reason = shared
            .last_latency_reason
            .lock()
            .map(|reason| reason.clone())
            .unwrap_or_else(|_| "unknown".into());
        eprintln!(
            "wavelinux5-dsp-helper native_stats captured_frames={} rendered_frames={} dropped_frames={} underrun_frames={} process_calls={} last_process_us={} max_process_us={} buffered_frames={} target_latency_msec={} reason={}",
            stats.captured_frames,
            stats.rendered_frames,
            stats.dropped_frames / 2,
            stats.underrun_frames,
            stats.process_calls,
            stats.last_process_micros,
            stats.max_process_micros,
            ring_samples / 2,
            target_msec,
            reason
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

    if let Some(bridge_config_path) = adaptive_bridge_config {
        let bridge_config: DspChannelConfig = serde_json::from_str(
            &std::fs::read_to_string(&bridge_config_path)
                .map_err(|err| format!("failed to read adaptive bridge config: {err}"))?,
        )
        .map_err(|err| format!("failed to parse adaptive bridge config: {err}"))?;
        eprintln!(
            "wavelinux5-dsp-helper adaptive_bridge_start channel_id={} input={} output={} config={}",
            bridge_config.channel_id,
            bridge_config.input_node_name,
            bridge_config.output_node_name,
            bridge_config_path.display()
        );
        thread::spawn(move || loop {
            if TERMINATE.load(Ordering::SeqCst) {
                eprintln!("wavelinux5-dsp-helper bridge_stop child_pid={child_pid}");
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!("wavelinux5-dsp-helper bridge_child_exit status={status}");
                    TERMINATE.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(err) => {
                    eprintln!("wavelinux5-dsp-helper bridge_child_wait_error {err}");
                    TERMINATE.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
        let result = run_pipewire_native_graph(bridge_config, status);
        TERMINATE.store(true, Ordering::SeqCst);
        return result;
    }

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

#[cfg(test)]
mod tests {
    use super::{
        drift_correction_interval, over_target_drop_interval, playback_render_frames,
        under_target_duplicate_interval,
    };

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
    fn drift_correction_gets_more_aggressive_when_far_from_target() {
        let target = 4800 * 2;

        assert_eq!(drift_correction_interval(target, target), 2);
        assert_eq!(drift_correction_interval(target / 2, target), 4);
        assert_eq!(drift_correction_interval(target / 4, target), 8);
        assert_eq!(drift_correction_interval(target / 8, target), 12);
    }

    #[test]
    fn under_target_duplicate_interval_respects_hysteresis() {
        let target = 4800 * 2;
        let hysteresis = 256 * 2;

        assert_eq!(
            under_target_duplicate_interval(0, target, hysteresis),
            Some(2)
        );
        assert_eq!(
            under_target_duplicate_interval(target - hysteresis, target, hysteresis),
            None
        );
    }

    #[test]
    fn over_target_drop_interval_respects_hysteresis() {
        let target = 4800 * 2;
        let hysteresis = 256 * 2;

        assert_eq!(
            over_target_drop_interval(target + hysteresis + target, target, hysteresis),
            Some(2)
        );
        assert_eq!(
            over_target_drop_interval(target + hysteresis, target, hysteresis),
            None
        );
    }
}
