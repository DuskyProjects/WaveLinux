//! Short offline comparison; no PipeWire connection or real audio capture.
use std::time::Instant;
use wavelinux_dsp::{generated_stereo_fixture, DspChain, RealtimeProcessStatus};
use wavelinux_model::{EffectCatalog, EffectInstance};

fn main() {
    let frames = 24_000;
    let fixture = generated_stereo_fixture(frames, 48_000);
    for definition in EffectCatalog::default().effects {
        for scenario in ["signal", "quiet"] {
            let mut effect = EffectInstance::new(&definition.id);
            effect.params = definition
                .params
                .iter()
                .map(|p| (p.id.clone(), p.default))
                .collect();
            if effect.effect_id == "rnnoise" {
                effect.params.insert("voice_gate".into(), 0.0);
                effect.params.insert("reduction_db".into(), 12.0);
            }
            let init = Instant::now();
            let mut chain = DspChain::new(&[effect], 48_000);
            assert!(
                chain.is_fully_initialized(),
                "{:?}",
                chain.initialization_failures()
            );
            let initialization_ms = init.elapsed().as_secs_f64() * 1000.0;
            let mut warm = fixture[..4800].to_vec();
            assert_eq!(
                chain.process_worker_interleaved_stereo(&mut warm),
                RealtimeProcessStatus::default()
            );
            let mut timings = Vec::new();
            for _ in 0..3 {
                let mut audio = if scenario == "signal" {
                    fixture.clone()
                } else {
                    vec![1.0e-35; frames * 2]
                };
                let start = Instant::now();
                for block in audio.chunks_mut(480) {
                    assert_eq!(
                        chain.process_worker_interleaved_stereo(block),
                        RealtimeProcessStatus::default()
                    );
                }
                timings.push(start.elapsed().as_secs_f64() * 1_000_000.0 / frames as f64);
                assert!(audio.iter().all(|s| s.is_finite()));
                std::hint::black_box(audio);
            }
            timings.sort_by(f64::total_cmp);
            println!(
                "{}",
                serde_json::json!({"effect":definition.id,"scenario":scenario,"initialization_ms":initialization_ms,"median_us_per_frame":timings[1],"audio_seconds_per_iteration":0.5})
            );
        }
    }
}
