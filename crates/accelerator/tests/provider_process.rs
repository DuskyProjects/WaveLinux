#![cfg(feature = "provider-runtime")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use wavelinux_accelerator::{
    blank_qualification, hardware_fingerprint, load_qualified_provider_pack_at, monotonic_nanos,
    rnnoise::{cpu_neural_step, ProviderBackedNeuralStage},
    sha256_file, AcceleratorProvider, ProviderClient, ProviderPackManifest,
    ACCELERATOR_PROTOCOL_VERSION, RNNOISE_FEATURE_COUNT, RNNOISE_STATE_COUNT,
};

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[test]
fn isolated_cpu_provider_round_trips_over_shared_memory() {
    let runtime = [
        "/usr/lib/libonnxruntime.so",
        "/usr/lib64/libonnxruntime.so",
        "/usr/lib/x86_64-linux-gnu/libonnxruntime.so",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.is_file());
    let Some(runtime) = runtime else {
        eprintln!("skipping provider process test: host ONNX Runtime is unavailable");
        return;
    };

    let temp = tempfile::tempdir().unwrap();
    let pack_root = temp.path().join("providers");
    let pack_dir = pack_root.join("cpu");
    let bin_dir = pack_dir.join("bin");
    let model_dir = pack_dir.join("models");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&model_dir).unwrap();
    fs::set_permissions(&pack_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let executable = bin_dir.join("wavelinux6-onnx-provider");
    fs::copy(env!("CARGO_BIN_EXE_wavelinux6-onnx-provider"), &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let model = model_dir.join("rnnoise.onnx");
    fs::copy(
        repository_path("providers/rnnoise/rnnoise-neural-v1.onnx"),
        &model,
    )
    .unwrap();
    fs::set_permissions(&model, fs::Permissions::from_mode(0o600)).unwrap();
    let fixture = model_dir.join("golden.json");
    fs::copy(
        repository_path("providers/rnnoise/rnnoise-neural-v1-golden.json"),
        &fixture,
    )
    .unwrap();
    fs::set_permissions(&fixture, fs::Permissions::from_mode(0o600)).unwrap();

    let manifest = ProviderPackManifest {
        protocol_version: ACCELERATOR_PROTOCOL_VERSION,
        pack_version: "test".into(),
        provider: AcceleratorProvider::Cpu,
        executable: "bin/wavelinux6-onnx-provider".into(),
        executable_sha256: sha256_file(&executable).unwrap(),
        model: "models/rnnoise.onnx".into(),
        model_sha256: sha256_file(&model).unwrap(),
        golden_fixture: "models/golden.json".into(),
        golden_fixture_sha256: sha256_file(&fixture).unwrap(),
        onnx_runtime_library: None,
    };
    fs::write(
        pack_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let mut qualification = blank_qualification(
        AcceleratorProvider::Cpu,
        &manifest.pack_version,
        &manifest.model_sha256,
        hardware_fingerprint(AcceleratorProvider::Cpu),
    );
    qualification.tested_unix = 1;
    qualification.blocks = 5_000;
    qualification.numerical_max_abs_error = 0.0;
    qualification.added_latency_msec = 0.0;
    qualification.cpu_reduction_percent = 31.0;
    qualification.fallback_validated = true;
    qualification.live_workload_validated = true;
    qualification = qualification.evaluate();
    fs::write(
        pack_dir.join("qualification.json"),
        serde_json::to_vec_pretty(&qualification).unwrap(),
    )
    .unwrap();
    let mut resolved = manifest.resolve(&pack_dir).unwrap();
    resolved.onnx_runtime_library = Some(runtime);
    let runtime_dir = temp.path().join("runtime");
    let mut provider = ProviderClient::spawn(&resolved, &runtime_dir).unwrap();
    assert!(provider.is_running());
    let sequence = provider
        .submit(
            [0.0; RNNOISE_FEATURE_COUNT],
            [0.0; RNNOISE_STATE_COUNT],
            monotonic_nanos() + 10_000_000,
        )
        .unwrap();
    let response = provider.wait(sequence, Duration::from_millis(100)).unwrap();
    assert_eq!(response.sequence, sequence);
    assert!(response.gains.iter().all(|value| value.is_finite()));
    assert!(response.state.iter().all(|value| value.is_finite()));
    provider.shutdown();

    let qualified = load_qualified_provider_pack_at(&pack_root, AcceleratorProvider::Cpu).unwrap();
    let qualified_runtime = temp.path().join("qualified-runtime");
    let mut stage = ProviderBackedNeuralStage::spawn(&qualified, &qualified_runtime).unwrap();
    let mut second_stage =
        ProviderBackedNeuralStage::spawn(&qualified, &qualified_runtime).unwrap();
    assert_ne!(stage.provider_pid(), second_stage.provider_pid());
    let mut features = [0.0; RNNOISE_FEATURE_COUNT];
    features[0] = 0.75;
    let expected = cpu_neural_step(&features, &[0.0; RNNOISE_STATE_COUNT]).unwrap();
    let actual = stage.process(&features, Duration::from_millis(100));
    let second_actual = second_stage.process(&features, Duration::from_millis(100));
    assert!(max_error(&actual.gains, &expected.gains) <= 1.0e-4);
    assert!(max_error(&second_actual.gains, &expected.gains) <= 1.0e-4);
    assert_eq!(stage.metrics().provider_blocks, 1);
    assert_eq!(second_stage.metrics().provider_blocks, 1);
    drop(second_stage);

    let committed = *stage.committed_state();
    unsafe {
        libc::kill(stage.provider_pid() as i32, libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(20));
    features[1] = -0.5;
    let expected_fallback = cpu_neural_step(&features, &committed).unwrap();
    let actual_fallback = stage.process(&features, Duration::from_millis(5));
    assert_eq!(actual_fallback, expected_fallback);
    assert_eq!(stage.metrics().fallback_blocks, 1);
    assert_eq!(stage.metrics().deadline_misses, 0);
    assert_eq!(
        stage.metrics().last_failure.as_deref(),
        Some("provider process exited")
    );
}

fn max_error(actual: &[f32], expected: &[f32]) -> f32 {
    actual
        .iter()
        .zip(expected)
        .map(|(actual, expected)| (actual - expected).abs())
        .fold(0.0, f32::max)
}
