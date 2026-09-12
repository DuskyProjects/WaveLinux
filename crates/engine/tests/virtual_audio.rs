//! Run only through scripts/test-virtual-audio.sh; never uses desktop audio.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use wavelinux_engine::{EngineOptions, EnginePaths, WaveLinuxEngine};
use wavelinux_model::{AppMatcher, AppStateSnapshot, EffectInstance, LevelMeter};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn report_dir() -> PathBuf {
    std::env::var_os("WAVELINUX_TEST_REPORT_DIR")
        .unwrap()
        .into()
}

fn spawn(program: &str, args: &[&str], log: &str) -> Process {
    let file = fs::File::create(report_dir().join(log)).unwrap();
    Process(
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .spawn()
            .unwrap(),
    )
}

fn command(program: &str, args: &[&str]) -> String {
    let output = Command::new("timeout")
        .args(["5s", program])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn wait_for(label: &str, seconds: u64, mut predicate: impl FnMut() -> bool) {
    let start = Instant::now();
    while !predicate() {
        assert!(
            start.elapsed() < Duration::from_secs(seconds),
            "timed out: {label}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

struct AudioSession {
    // Reverse dependency order for shutdown.
    pulse: Process,
    policy: Process,
    server: Process,
}
impl AudioSession {
    fn start() -> Self {
        let server = spawn("pipewire", &[], "pipewire.log");
        wait_for("private PipeWire", 5, || {
            Path::new(&std::env::var("XDG_RUNTIME_DIR").unwrap())
                .join("pipewire-0")
                .exists()
        });
        let policy = spawn("wireplumber", &["-p", "policy"], "wireplumber.log");
        let pulse = spawn("pipewire-pulse", &[], "pulse.log");
        wait_for("private Pulse", 5, || {
            Command::new("pactl")
                .arg("info")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        });
        let graph: serde_json::Value = serde_json::from_str(&command("pw-dump", &[])).unwrap();
        assert!(
            !graph
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["info"]["props"]["device.api"] == "alsa"
                    || node["info"]["props"]["device.api"] == "bluez5"),
            "hardware monitor leaked into private session"
        );
        Self {
            pulse,
            policy,
            server,
        }
    }
    fn stop(self) {
        drop(self.pulse);
        drop(self.policy);
        drop(self.server);
    }
}

fn load_sink(name: &str) -> String {
    command(
        "pactl",
        &[
            "load-module",
            "module-null-sink",
            &format!("sink_name={name}"),
            "rate=48000",
            "channels=2",
        ],
    )
}
fn load_mic() -> String {
    command(
        "pactl",
        &[
            "load-module",
            "module-remap-source",
            "master=wl6_test_feed.monitor",
            "source_name=wl6_test_mic",
            "channels=2",
        ],
    )
}
fn devices() -> (String, String) {
    let output = load_sink("wl6_test_output");
    load_sink("wl6_test_feed");
    let mic = load_mic();
    command("pactl", &["set-default-sink", "wl6_test_output"]);
    command("pactl", &["set-default-source", "wl6_test_mic"]);
    (output, mic)
}

struct RunningEngine {
    engine: Arc<WaveLinuxEngine>,
    worker: Option<thread::JoinHandle<()>>,
}
impl RunningEngine {
    fn new(root: &Path) -> Self {
        let engine = WaveLinuxEngine::new(
            EnginePaths::for_tests(root),
            EngineOptions {
                dry_run: false,
                auto_repair_on_start: false,
                poll_interval: Duration::from_millis(100),
            },
        )
        .unwrap();
        let worker = Some(engine.spawn_background());
        Self { engine, worker }
    }
}
impl Drop for RunningEngine {
    fn drop(&mut self) {
        self.engine.stop_background();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = self.engine.cleanup_audio_graph();
    }
}

fn state_until(
    engine: &WaveLinuxEngine,
    label: &str,
    predicate: impl Fn(&AppStateSnapshot) -> bool,
) -> AppStateSnapshot {
    let mut state = engine.get_state().unwrap();
    wait_for(label, 12, || {
        let _ = engine.refresh_runtime();
        state = engine.get_state().unwrap();
        fs::write(
            report_dir().join("last-state.json"),
            serde_json::to_vec_pretty(&state).unwrap(),
        )
        .unwrap();
        predicate(&state)
    });
    state
}

fn tone(root: &Path, label: &str, hz: f32, sink: Option<&str>) -> Process {
    let path = root.join(format!("{label}.raw"));
    let mut samples = Vec::with_capacity(48000 * 20 * 4);
    for n in 0..48000 * 20 {
        let sample = ((n as f32 * hz * std::f32::consts::TAU / 48000.0).sin() * 8192.0) as i16;
        samples.extend_from_slice(&sample.to_le_bytes());
        samples.extend_from_slice(&sample.to_le_bytes());
    }
    fs::write(&path, samples).unwrap();
    let id = format!("--property=application.id=wl6.test.{label}");
    let name = format!("--property=application.name=Virtual {label} check");
    let device = format!("--device={}", sink.unwrap_or("wl6_test_output"));
    let mut args = vec![
        "--raw",
        "--rate=48000",
        "--format=s16le",
        "--channels=2",
        &id,
        &name,
    ];
    if sink.is_some() {
        args.push(&device);
    }
    args.push(path.to_str().unwrap());
    spawn("paplay", &args, &format!("tone-{label}.log"))
}

fn meters_until(
    engine: &WaveLinuxEngine,
    label: &str,
    predicate: impl Fn(&[LevelMeter]) -> bool,
) -> Vec<LevelMeter> {
    let mut meters = Vec::new();
    let mut stream = None;
    wait_for(label, 10, || {
        if stream.is_none() {
            stream = engine.open_meter_stream().ok();
        }
        if let Some(client) = &mut stream {
            match engine.read_meter_stream(client) {
                Ok(frame) => meters = frame,
                Err(_) => stream = None,
            }
        }
        fs::write(
            report_dir().join("last-meters.json"),
            serde_json::to_vec_pretty(&meters).unwrap(),
        )
        .unwrap();
        predicate(&meters)
    });
    fs::write(
        report_dir().join(format!("{label}.json")),
        serde_json::to_vec_pretty(&meters).unwrap(),
    )
    .unwrap();
    meters
}

fn has_tone(meters: &[LevelMeter], channel: &str, hz: f32) -> bool {
    let Some(meter) = meters.iter().find(|m| m.node_id == channel) else {
        return false;
    };
    let Some(comp) = meter.compressor else {
        return false;
    };
    let Some(spectrum) = &meter.spectrum else {
        return false;
    };
    let peak = spectrum
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    let bin = (hz / 20.0).log10() / 1000.0_f32.log10() * 63.0;
    comp.input_peak > 0.05
        && comp.output_peak < comp.input_peak
        && comp.gain_reduction_db > 1.0
        && *peak.1 > 0.4
        && (peak.0 as f32 - bin).abs() <= 2.0
}

fn check_processed_spectrum_and_deepfilter(engine: &Arc<WaveLinuxEngine>) {
    let saved = engine
        .get_state()
        .unwrap()
        .config
        .channels
        .into_iter()
        .find(|c| c.id == "music")
        .unwrap()
        .effects;
    let peak = |meters: &[LevelMeter]| {
        meters
            .iter()
            .find(|m| m.node_id == "music")
            .and_then(|m| m.spectrum.as_ref())
            .map(|s| s.iter().copied().fold(0.0, f32::max))
            .unwrap_or(0.0)
    };
    let generation = || {
        engine.refresh_runtime().unwrap();
        engine
            .get_state()
            .unwrap()
            .engine
            .audio_core
            .into_iter()
            .find(|c| c.channel_id == "music")
            .unwrap()
            .submitted_generation
    };
    let mut levels = Vec::new();
    for (label, gain) in [
        ("eq-flat-output", 0.0),
        ("eq-cut-output", -12.0),
        ("eq-boost-output", 6.0),
    ] {
        let previous_generation = generation();
        let mut eq = EffectInstance::new("eq");
        eq.params.extend([
            ("band_1k_frequency_hz".into(), 880.0),
            ("band_1k_gain_db".into(), gain),
            ("band_1k_q".into(), 2.0),
        ]);
        engine.set_effect_chain("music".into(), vec![eq]).unwrap();
        state_until(engine, label, |s| {
            s.engine.audio_core.iter().any(|c| {
                c.channel_id == "music"
                    && c.acknowledged_generation > previous_generation
                    && c.acknowledged_generation == c.submitted_generation
            })
        });
        // Let the FFT window and display smoothing settle after the chain fade.
        thread::sleep(Duration::from_millis(350));
        // Pulse volume percentages use cubic amplitude scaling.
        let expected =
            ((20.0 * (0.25_f32 * 0.7_f32.powi(3)).log10() + gain + 90.0) / 90.0).clamp(0.0, 1.0);
        let meters = meters_until(engine, label, |m| (peak(m) - expected).abs() < 0.015);
        levels.push(peak(&meters));
    }
    assert!(
        levels[1] < levels[0] - 0.08 && levels[2] > levels[0] + 0.03,
        "EQ spectrum did not follow processed output: {levels:?}"
    );
    println!("PASS: live EQ spectrum follows actual cuts and boosts after effects {levels:?}");

    let previous_generation = generation();
    let mut df = EffectInstance::new("deepfilternet3");
    df.params.insert("reduction_db".into(), 6.0);
    engine
        .set_effect_chain("music".into(), vec![EffectInstance::new("rnnoise"), df])
        .unwrap();
    let state = state_until(engine, "DeepFilterNet active on Music", |s| {
        s.engine.audio_core.iter().any(|c| {
            c.channel_id == "music"
                && c.online
                && c.worker_running
                && c.acknowledged_generation > previous_generation
                && c.acknowledged_generation == c.submitted_generation
        })
    });
    let effects = &state
        .config
        .channels
        .iter()
        .find(|c| c.id == "music")
        .unwrap()
        .effects;
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0].effect_id, "deepfilternet3");
    meters_until(engine, "deepfilter-processed-music", |m| {
        m.iter().any(|m| {
            m.node_id == "music" && m.peak_left.max(m.peak_right) > 0.01 && m.spectrum.is_some()
        })
    });
    let state = engine.get_state().unwrap();
    let status = state
        .engine
        .audio_core
        .iter()
        .find(|c| c.channel_id == "music")
        .unwrap();
    assert_eq!(status.processing_errors, 0);
    assert_eq!(status.non_finite_samples, 0);
    println!(
        "PASS: DeepFilterNet replaces RNNoise and processes only Music without inference faults"
    );
    engine.set_effect_chain("music".into(), saved).unwrap();
    meters_until(engine, "music-chain-restored", |m| {
        has_tone(m, "music", 880.0)
    });
}

fn check_virtual_speaker() {
    let sinks: serde_json::Value =
        serde_json::from_str(&command("pactl", &["-f", "json", "list", "sinks"])).unwrap();
    let output = sinks
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "wl6_test_output")
        .unwrap();
    let index = output["index"].clone();
    wait_for("monitor routed to reconnected speaker", 8, || {
        let streams: serde_json::Value =
            serde_json::from_str(&command("pactl", &["-f", "json", "list", "sink-inputs"]))
                .unwrap();
        streams.as_array().unwrap().iter().any(|s| {
            s["properties"]["wavelinux6.role"] == "mix_output_target"
                && s["properties"]["wavelinux6.mix_id"] == "monitor"
                && s["sink"] == index
        })
    });
    // Only synthetic audio from our private null sink; never a real microphone.
    let output = Command::new("timeout")
        .args([
            "1s",
            "parec",
            "--raw",
            "--latency-msec=20",
            "--device=wl6_test_output.monitor",
            "--rate=48000",
            "--format=s16le",
            "--channels=2",
        ])
        .output()
        .unwrap();
    assert!(matches!(output.status.code(), Some(0 | 124)));
    let samples = output
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|v| f64::from(i16::from_le_bytes([v[0], v[1]])) / 32768.0)
        .collect::<Vec<_>>();
    assert!(
        samples.len() > 4800,
        "virtual speaker capture returned too few samples"
    );
    let rms = (samples.iter().map(|v| v * v).sum::<f64>() / samples.len() as f64).sqrt();
    assert!(
        rms > 0.003,
        "reconnected virtual speaker stayed silent: {rms}"
    );
    println!("PASS: reconnected speaker receives processed audio (RMS {rms:.4})");
}

#[test]
#[ignore = "requires the private virtual session created by scripts/test-virtual-audio.sh"]
fn virtual_routing_hotplug_and_saved_settings() {
    assert_eq!(
        std::env::var("WAVELINUX_VIRTUAL_AUDIO_TEST").as_deref(),
        Ok("1")
    );
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap();
    assert!(runtime.starts_with("/tmp/wl6-virtual."));
    assert_eq!(
        std::env::var("PULSE_SERVER").unwrap(),
        format!("unix:{runtime}/pulse/native")
    );
    assert_eq!(std::env::var("PIPEWIRE_RUNTIME_DIR").unwrap(), runtime);
    let mut audio = AudioSession::start();
    let (output, mic) = devices();
    let root = Path::new(&runtime).join("engine");
    let running = RunningEngine::new(&root);
    let engine = &running.engine;
    engine
        .set_channel_input("hardware_in".into(), Some("wl6_test_mic".into()))
        .unwrap();
    engine
        .set_mix_monitor_output("monitor".into(), Some("wl6_test_output".into()))
        .unwrap();
    engine
        .set_device_hardware_profile("wl6_test_mic".into(), Some("default.generic-audio".into()))
        .unwrap();
    for channel in ["hardware_in", "music", "game"] {
        let mut compressor = EffectInstance::new("compressor");
        compressor.params.insert("threshold_db".into(), -32.0);
        compressor.params.insert("ratio".into(), 4.0);
        let mut eq = EffectInstance::new("eq");
        eq.params.insert("band_500_frequency_hz".into(), 570.0);
        eq.params.insert("band_500_gain_db".into(), -2.2);
        eq.params.insert("band_500_q".into(), 0.5);
        engine
            .set_effect_chain(channel.into(), vec![eq, compressor])
            .unwrap();
        engine
            .set_channel_effects_enabled(channel.into(), true)
            .unwrap();
        for bus in ["monitor", "stream"] {
            engine
                .set_channel_mute(channel.into(), bus.into(), false)
                .unwrap();
        }
    }
    for channel in ["music", "game"] {
        engine
            .assign_app_to_channel(
                channel.into(),
                AppMatcher::from_app_id(format!("wl6.test.{channel}")),
            )
            .unwrap();
    }
    engine
        .set_app_volume_preset(AppMatcher::from_app_id("wl6.test.music"), 0.7)
        .unwrap();
    engine.repair_audio_graph().unwrap();
    state_until(engine, "native graph ready", |s| {
        s.engine.audio_graph_running
    });
    println!("PASS: private virtual input/output and native graph ready");

    for (channel, hz) in [("music", 880.0), ("game", 2200.0), ("hardware_in", 440.0)] {
        let player = tone(
            &root,
            channel,
            hz,
            (channel == "hardware_in").then_some("wl6_test_feed"),
        );
        if channel != "hardware_in" {
            let state = state_until(engine, "app route", |s| {
                s.graph.app_streams.iter().any(|a| {
                    a.app_id.as_deref() == Some(&format!("wl6.test.{channel}"))
                        && a.routed_channel_id.as_deref() == Some(channel)
                        && (channel != "music" || (a.volume - 0.7).abs() < 0.04)
                })
            });
            if channel == "music" {
                assert!(state
                    .graph
                    .app_streams
                    .iter()
                    .any(|a| a.app_id.as_deref() == Some("wl6.test.music")
                        && (a.volume - 0.7).abs() < 0.04));
            }
        }
        let frame = meters_until(engine, &format!("routing-{channel}"), |m| {
            has_tone(m, channel, hz)
        });
        for other in ["hardware_in", "music", "game"] {
            if other != channel {
                let meter = frame.iter().find(|m| m.node_id == other).unwrap();
                assert!(
                    meter.compressor.unwrap().input_peak < 0.01,
                    "audio leaked from {channel} into {other}"
                );
            }
        }
        println!(
            "PASS: {channel} route, compressor gain reduction, EQ frequency, other channels silent"
        );
        if channel == "music" {
            check_processed_spectrum_and_deepfilter(engine);
        }
        drop(player);
        meters_until(engine, &format!("silence-{channel}"), |m| {
            m.iter()
                .find(|m| m.node_id == channel)
                .and_then(|m| m.compressor)
                .is_some_and(|c| c.input_peak < 0.001)
        });
    }

    let player = tone(&root, "mic-reconnect", 440.0, Some("wl6_test_feed"));
    meters_until(engine, "mic-before-removal", |m| {
        has_tone(m, "hardware_in", 440.0)
    });
    command("pactl", &["unload-module", &mic]);
    state_until(engine, "input disappeared", |s| {
        !s.graph.inputs.iter().any(|d| d.name == "wl6_test_mic")
    });
    meters_until(engine, "mic-removed-silence", |m| {
        m.iter()
            .find(|m| m.node_id == "hardware_in")
            .and_then(|m| m.compressor)
            .is_some_and(|c| c.input_peak < 0.001)
    });
    load_mic();
    state_until(engine, "input reappeared", |s| {
        s.graph.inputs.iter().any(|d| d.name == "wl6_test_mic")
    });
    meters_until(engine, "mic-after-reconnect", |m| {
        has_tone(m, "hardware_in", 440.0)
    });
    println!("PASS: virtual microphone disconnect/reconnect resumes compressor and EQ telemetry");
    drop(player);

    command("pactl", &["unload-module", &output]);
    state_until(engine, "output disappeared", |s| {
        !s.graph.outputs.iter().any(|d| d.name == "wl6_test_output")
    });
    load_sink("wl6_test_output");
    state_until(engine, "output reappeared", |s| {
        s.graph.outputs.iter().any(|d| d.name == "wl6_test_output")
    });
    let player = tone(&root, "music", 880.0, None);
    meters_until(engine, "output-reconnect-music", |m| {
        has_tone(m, "music", 880.0)
    });
    check_virtual_speaker();
    println!("PASS: virtual speaker disconnect/reconnect preserves Music processing");
    drop(player);

    let saved = engine.get_state().unwrap().config;
    fs::write(
        report_dir().join("saved-settings.json"),
        serde_json::to_vec_pretty(&saved).unwrap(),
    )
    .unwrap();
    audio.stop();
    thread::sleep(Duration::from_millis(500));
    audio = AudioSession::start();
    devices();
    state_until(engine, "audio server recovery", |s| {
        s.engine.audio_graph_running
            && s.graph
                .inputs
                .iter()
                .any(|d| d.name == "wavelinux6_mix_monitor_source")
    });
    let player = tone(&root, "music", 880.0, None);
    meters_until(engine, "server-restart-music", |m| {
        has_tone(m, "music", 880.0)
    });
    check_virtual_speaker();
    println!("PASS: private audio server restart recovers routing and live effect telemetry");
    drop(player);
    drop(running);

    let reopened = RunningEngine::new(&root);
    let restored = reopened.engine.get_state().unwrap().config;
    assert_eq!(
        saved.channels, restored.channels,
        "channel effects/settings changed after restart"
    );
    assert_eq!(
        saved.app_routes, restored.app_routes,
        "app routing changed after restart"
    );
    assert_eq!(saved.app_volume_presets, restored.app_volume_presets);
    assert_eq!(
        saved.device_policy.hardware_profile_assignments,
        restored.device_policy.hardware_profile_assignments
    );
    assert_eq!(saved.mixes, restored.mixes);
    reopened.engine.repair_audio_graph().unwrap();
    let player = tone(&root, "music", 880.0, None);
    state_until(&reopened.engine, "reopened graph", |s| {
        s.engine.audio_graph_running
    });
    meters_until(&reopened.engine, "reopened-music", |m| {
        has_tone(m, "music", 880.0)
    });
    println!("PASS: engine restart preserves EQ, compressor, app routes/volume, mixes and device profile");
    drop(player);
    reopened.engine.cleanup_audio_graph().unwrap();
    audio.stop();
    audio = AudioSession::start();
    devices();
    state_until(&reopened.engine, "stopped engine reconnect", |s| {
        s.engine.pipewire_registry.reconnects > 0
    });
    // Allow the queued reconnect handler to run before checking cleanup intent.
    thread::sleep(Duration::from_millis(500));
    let stopped = reopened.engine.get_state().unwrap();
    assert!(!stopped.engine.audio_graph_running);
    assert!(!stopped
        .graph
        .outputs
        .iter()
        .any(|d| d.name.starts_with("wavelinux6_")));
    println!("PASS: deliberately stopped graph stays stopped after audio server restart");
    drop(reopened);
    audio.stop();
}
