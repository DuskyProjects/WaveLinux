//! Embedded DeepFilterNet3. Each model stays on its own inference thread because
//! tract's recurrent state contains Rc values and cannot safely move across threads.
//! Only reusable audio buffers cross the boundary; the capture callback never waits.

use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;
use wavelinux_model::EffectInstance;

static MODEL_PARAMS: OnceLock<DfParams> = OnceLock::new();
const FRAME_DEADLINE: Duration = Duration::from_millis(250);

struct AudioBlock {
    input: Array2<f32>,
    output: Array2<f32>,
}

pub(super) struct DeepFilterNode {
    request: Option<mpsc::SyncSender<Box<AudioBlock>>>,
    response: mpsc::Receiver<Box<AudioBlock>>,
    block: Option<Box<AudioBlock>>,
    channels: usize,
    hop: usize,
    position: usize,
    bypass: bool,
    failed: bool,
}

impl std::fmt::Debug for DeepFilterNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepFilterNode")
            .field("channels", &self.channels)
            .field("hop_size", &self.hop)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl DeepFilterNode {
    pub(super) fn new(
        effect: &EffectInstance,
        sample_rate: u32,
        channels: u8,
    ) -> Result<Self, String> {
        if sample_rate != 48_000 {
            return Err("DeepFilterNet 3 requires 48 kHz audio".into());
        }
        let channels = if channels <= 1 { 1 } else { 2 };
        let reduction = super::param(effect, "reduction_db", 12.0).clamp(0.0, 60.0);
        let (request, requests) = mpsc::sync_channel::<Box<AudioBlock>>(1);
        let (responses, response) = mpsc::sync_channel(1);
        if reduction == 0.0 {
            return Ok(Self {
                request: None,
                response,
                block: None,
                channels,
                hop: 0,
                position: 0,
                bypass: true,
                failed: false,
            });
        }
        let (ready, readiness) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("wl6-deepfilter".into())
            .spawn(move || {
                let prepare = || -> Result<(DfTract, Box<AudioBlock>), String> {
                    let mut model = DfTract::new(
                        MODEL_PARAMS.get_or_init(DfParams::default).clone(),
                        &RuntimeParams::default_with_ch(channels).with_atten_lim(reduction),
                    )
                    .map_err(|error| format!("could not load DeepFilterNet 3: {error:#}"))?;
                    if model.sr != sample_rate as usize
                        || model.hop_size == 0
                        || model.ch != channels
                    {
                        return Err("DeepFilterNet model has incompatible audio dimensions".into());
                    }
                    // No additional post-filter or speech gate: preserve quiet words.
                    model.set_pf_beta(0.0);
                    let input = Array2::zeros((channels, model.hop_size));
                    let mut output = input.clone();
                    model
                        .process(input.view(), output.view_mut())
                        .map_err(|error| format!("could not prepare DeepFilterNet 3: {error:#}"))?;
                    Ok((model, Box::new(AudioBlock { input, output })))
                };
                let (mut model, block) = match prepare() {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok((model.hop_size, block))).is_err() {
                    return;
                }
                while let Ok(mut block) = requests.recv() {
                    if model
                        .process(block.input.view(), block.output.view_mut())
                        .is_err()
                        || block.output.iter().any(|sample| !sample.is_finite())
                    {
                        // Closing the response channel reports a fault to the audio core.
                        break;
                    }
                    if responses.send(block).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("could not start DeepFilterNet: {error}"))?;
        let (hop, block) = readiness
            .recv_timeout(Duration::from_secs(30))
            .map_err(|error| format!("DeepFilterNet initialization failed: {error}"))??;
        Ok(Self {
            request: Some(request),
            response,
            block: Some(block),
            channels,
            hop,
            position: 0,
            bypass: false,
            failed: false,
        })
    }

    /// Called only by the DSP worker in the native core. A bounded deadline or
    /// disconnected inference thread requests the existing chain recovery path.
    pub(super) fn process(&mut self, samples: &mut [f32]) -> bool {
        if self.bypass {
            return true;
        }
        if self.failed {
            samples.fill(0.0);
            return false;
        }
        for frame in samples.as_chunks_mut::<2>().0 {
            let block = self
                .block
                .as_mut()
                .expect("healthy DeepFilterNet has an audio buffer");
            let output = [
                block.output[[0, self.position]],
                block.output[[self.channels - 1, self.position]],
            ];
            block.input[[0, self.position]] = frame[0];
            if self.channels == 2 {
                block.input[[1, self.position]] = frame[1];
            }
            frame.copy_from_slice(&output);
            self.position += 1;
            if self.position == self.hop {
                self.position = 0;
                let block = self.block.take().expect("audio buffer is present");
                let sent = self
                    .request
                    .as_ref()
                    .is_some_and(|request| request.try_send(block).is_ok());
                self.block = if sent {
                    self.response.recv_timeout(FRAME_DEADLINE).ok()
                } else {
                    None
                };
                if self.block.is_none() {
                    self.failed = true;
                    self.request.take();
                    break;
                }
            }
        }
        if self.failed {
            samples.fill(0.0);
        }
        !self.failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DspChain, RealtimeProcessStatus};

    fn effect(id: &str, reduction: f32) -> EffectInstance {
        let mut effect = EffectInstance::new(id);
        effect.params.extend([
            ("reduction_db".into(), reduction),
            ("voice_gate".into(), 0.0),
            ("dry_mix".into(), 0.0),
        ]);
        effect
    }

    fn noise(frames: usize) -> Vec<f32> {
        let mut seed = 0x194a73_u32;
        (0..frames)
            .flat_map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let value = (seed as f32 / u32::MAX as f32 - 0.5) * 0.08;
                [value, 0.0]
            })
            .collect()
    }

    fn process(id: &str, reduction: f32, input: &[f32], block_samples: usize) -> Vec<f32> {
        let mut chain = DspChain::new(&[effect(id, reduction)], 48_000);
        assert!(
            chain.is_fully_initialized(),
            "{:?}",
            chain.initialization_failures()
        );
        let mut output = input.to_vec();
        for block in output.chunks_mut(block_samples) {
            assert_eq!(
                chain.process_worker_interleaved_stereo(block),
                RealtimeProcessStatus::default()
            );
        }
        output
    }

    #[test]
    fn both_noise_filters_have_transparent_zero_strength() {
        let input = noise(1200);
        for id in ["rnnoise", "deepfilternet3"] {
            assert_eq!(process(id, 0.0, &input, 514), input);
        }
    }

    #[test]
    fn noise_strength_changes_suppression_without_cross_channel_leakage() {
        let input = noise(28_800);
        for id in ["rnnoise", "deepfilternet3"] {
            let gentle = process(id, 6.0, &input, 514);
            let strong = process(id, 36.0, &input, 480);
            let level = |samples: &[f32]| crate::rms(&samples[19_200..]);
            let input_level = level(&input);
            let gentle_level = level(&gentle);
            let strong_level = level(&strong);
            eprintln!(
                "{id}: input={input_level:.7} gentle={gentle_level:.7} strong={strong_level:.7}"
            );
            assert!(gentle_level < input_level * 0.9, "{id} did not reduce hiss");
            assert!(gentle_level > input_level * 0.3, "{id} Gentle cut too much");
            assert!(
                strong_level < gentle_level * 0.8,
                "{id} Strength did not change suppression"
            );
            for output in [&gentle, &strong] {
                assert!(output.iter().all(|sample| sample.is_finite()));
                assert!(
                    output
                        .iter()
                        .skip(1)
                        .step_by(2)
                        .all(|sample| sample.abs() < 1e-7),
                    "{id} leaked left audio into right"
                );
            }
        }
    }

    #[test]
    fn deepfilter_gentle_reduces_faint_hiss_without_silencing_it() {
        let input: Vec<_> = noise(28_800)
            .into_iter()
            .map(|sample| sample * 0.1)
            .collect();
        let output = process("deepfilternet3", 6.0, &input, 480);
        let ratio = crate::rms(&output[19_200..]) / crate::rms(&input[19_200..]);
        eprintln!("DeepFilterNet Gentle faint-hiss output/input={ratio:.4}");
        assert!(
            (0.35..0.85).contains(&ratio),
            "unexpected faint-hiss reduction: {ratio}"
        );
    }

    #[test]
    fn deepfilter_is_independent_of_worker_block_boundaries_and_previous_instances() {
        let input = noise(4_800);
        let whole = process("deepfilternet3", 12.0, &input, input.len());
        let split = process("deepfilternet3", 12.0, &input, 254);
        assert_eq!(whole, split);
        let silence = process("deepfilternet3", 12.0, &vec![0.0; input.len()], 480);
        assert!(silence.iter().all(|sample| sample.abs() < 1e-7));
    }

    #[test]
    fn deepfilter_rejects_unsupported_rate_and_reports_inference_disconnect() {
        assert!(DeepFilterNode::new(&effect("deepfilternet3", 12.0), 44_100, 1).is_err());
        let mut node = DeepFilterNode::new(&effect("deepfilternet3", 12.0), 48_000, 1).unwrap();
        node.request.take();
        let mut chain = DspChain::new(&[], 48_000);
        chain.nodes.push(crate::DspNode::DeepFilter(Box::new(node)));
        // Inference faults must remain visible after startup sample validation ends.
        chain.validation_blocks_remaining = 0;
        let mut samples = noise(960);
        let status = chain.process_worker_interleaved_stereo(&mut samples);
        assert_eq!(status.processing_errors, 1);
        assert_eq!(status.effect_mask, 1 << 7);
        assert!(samples.iter().all(|sample| *sample == 0.0));
        let metrics = chain.process_interleaved_stereo(&mut samples);
        assert_eq!(metrics.fallback_count, 1);
    }
}
