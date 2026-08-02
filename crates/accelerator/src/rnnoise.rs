//! RNNoise neural-stage reference implementation with explicit recurrent state.
//!
//! This module intentionally covers only the neural stage. Feature extraction,
//! pitch filtering, and synthesis remain in the native DSP implementation.

use std::sync::OnceLock;

use crate::{
    monotonic_nanos, ProviderClient, QualifiedProviderPack, RNNOISE_DENOISE_STATE_COUNT,
    RNNOISE_FEATURE_COUNT, RNNOISE_GAIN_COUNT, RNNOISE_NOISE_STATE_COUNT, RNNOISE_STATE_COUNT,
    RNNOISE_VAD_STATE_COUNT,
};
use std::path::Path;
use std::time::Duration;

const WEIGHT_SCALE: f32 = 1.0 / 256.0;
const BUILTIN_WEIGHTS: &[u8] = include_bytes!("../../../providers/rnnoise/weights.rnn");
const MAX_NEURONS: usize = RNNOISE_DENOISE_STATE_COUNT;
const MAX_INPUTS: usize = 114;

static BUILTIN_MODEL: OnceLock<Result<NeuralModel, String>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuNeuralOutput {
    pub gains: [f32; RNNOISE_GAIN_COUNT],
    pub vad_probability: f32,
    pub state: [f32; RNNOISE_STATE_COUNT],
}

impl Default for CpuNeuralOutput {
    fn default() -> Self {
        Self {
            gains: [0.0; RNNOISE_GAIN_COUNT],
            vad_probability: 0.0,
            state: [0.0; RNNOISE_STATE_COUNT],
        }
    }
}

#[derive(Debug, Clone)]
pub struct CpuNeuralStage {
    model: &'static NeuralModel,
    state: [f32; RNNOISE_STATE_COUNT],
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderNeuralMetrics {
    pub provider_blocks: u64,
    pub fallback_blocks: u64,
    pub deadline_misses: u64,
    pub invalid_results: u64,
    pub stale_results: u64,
    pub consecutive_failures: u32,
    pub provider_disabled: bool,
    pub last_failure: Option<String>,
}

pub struct ProviderBackedNeuralStage {
    cpu: CpuNeuralStage,
    provider: ProviderClient,
    metrics: ProviderNeuralMetrics,
}

impl std::fmt::Debug for ProviderBackedNeuralStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderBackedNeuralStage")
            .field("provider", &self.provider.provider())
            .field("provider_pid", &self.provider.pid())
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

impl ProviderBackedNeuralStage {
    pub fn spawn(pack: &QualifiedProviderPack, runtime_directory: &Path) -> Result<Self, String> {
        Ok(Self {
            cpu: CpuNeuralStage::new()?,
            provider: ProviderClient::spawn(pack.resolved(), runtime_directory)
                .map_err(|error| error.to_string())?,
            metrics: ProviderNeuralMetrics::default(),
        })
    }

    /// Process one neural block on the provider and fall back from the exact
    /// uncommitted state when the result is late, stale, or invalid.
    ///
    /// This method waits and must run on an audio worker, never in a PipeWire
    /// real-time callback.
    pub fn process(
        &mut self,
        features: &[f32; RNNOISE_FEATURE_COUNT],
        timeout: Duration,
    ) -> CpuNeuralOutput {
        while self.provider.poll().is_some() {
            self.metrics.stale_results = self.metrics.stale_results.saturating_add(1);
        }
        if self.metrics.provider_disabled || timeout.is_zero() {
            return self.fallback(features, "provider disabled or deadline unavailable", false);
        }
        if !self.provider.is_running() {
            return self.fallback(features, "provider process exited", false);
        }

        let deadline = monotonic_nanos().saturating_add(timeout.as_nanos() as u64);
        let sequence = match self.provider.submit(*features, *self.cpu.state(), deadline) {
            Ok(sequence) => sequence,
            Err(_) => return self.fallback(features, "provider request queue is full", false),
        };
        let Some(response) = self.provider.wait(sequence, timeout) else {
            return self.fallback(features, "provider missed the block deadline", true);
        };
        if response.deadline_missed != 0 || response.completed_monotonic_ns > deadline {
            return self.fallback(
                features,
                "provider completed after the block deadline",
                true,
            );
        }
        if !response.vad_probability.is_finite()
            || !response.gains.iter().all(|value| value.is_finite())
            || !response.state.iter().all(|value| value.is_finite())
        {
            self.metrics.invalid_results = self.metrics.invalid_results.saturating_add(1);
            return self.fallback(features, "provider returned non-finite output", false);
        }

        self.cpu.replace_state(response.state);
        self.metrics.provider_blocks = self.metrics.provider_blocks.saturating_add(1);
        self.metrics.consecutive_failures = 0;
        self.metrics.last_failure = None;
        CpuNeuralOutput {
            gains: response.gains,
            vad_probability: response.vad_probability,
            state: response.state,
        }
    }

    pub fn metrics(&self) -> &ProviderNeuralMetrics {
        &self.metrics
    }

    pub fn provider_pid(&self) -> u32 {
        self.provider.pid()
    }

    pub fn provider(&self) -> crate::AcceleratorProvider {
        self.provider.provider()
    }

    pub fn committed_state(&self) -> &[f32; RNNOISE_STATE_COUNT] {
        self.cpu.state()
    }

    fn fallback(
        &mut self,
        features: &[f32; RNNOISE_FEATURE_COUNT],
        reason: &str,
        deadline_missed: bool,
    ) -> CpuNeuralOutput {
        self.metrics.fallback_blocks = self.metrics.fallback_blocks.saturating_add(1);
        self.metrics.deadline_misses = self
            .metrics
            .deadline_misses
            .saturating_add(u64::from(deadline_missed));
        self.metrics.consecutive_failures = self.metrics.consecutive_failures.saturating_add(1);
        self.metrics.last_failure = Some(reason.into());
        if self.metrics.consecutive_failures >= 3 {
            self.metrics.provider_disabled = true;
        }
        self.cpu.process(features)
    }
}

impl CpuNeuralStage {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            model: builtin_model()?,
            state: [0.0; RNNOISE_STATE_COUNT],
        })
    }

    pub fn with_state(state: [f32; RNNOISE_STATE_COUNT]) -> Result<Self, String> {
        Ok(Self {
            model: builtin_model()?,
            state,
        })
    }

    pub fn state(&self) -> &[f32; RNNOISE_STATE_COUNT] {
        &self.state
    }

    pub fn replace_state(&mut self, state: [f32; RNNOISE_STATE_COUNT]) {
        self.state = state;
    }

    pub fn process(&mut self, features: &[f32; RNNOISE_FEATURE_COUNT]) -> CpuNeuralOutput {
        let output = self.model.process(features, &self.state);
        self.state = output.state;
        output
    }
}

pub fn cpu_neural_step(
    features: &[f32; RNNOISE_FEATURE_COUNT],
    state: &[f32; RNNOISE_STATE_COUNT],
) -> Result<CpuNeuralOutput, String> {
    Ok(builtin_model()?.process(features, state))
}

fn builtin_model() -> Result<&'static NeuralModel, String> {
    BUILTIN_MODEL
        .get_or_init(|| NeuralModel::parse(BUILTIN_WEIGHTS))
        .as_ref()
        .map_err(Clone::clone)
}

#[derive(Debug, Clone, Copy)]
enum Activation {
    Tanh,
    Sigmoid,
    Relu,
}

impl Activation {
    fn parse(value: u8) -> Result<Self, String> {
        match value {
            0 => Ok(Self::Tanh),
            1 => Ok(Self::Sigmoid),
            2 => Ok(Self::Relu),
            _ => Err(format!("unsupported RNNoise activation {value}")),
        }
    }

    fn apply(self, value: f32) -> f32 {
        match self {
            Self::Tanh => value.tanh(),
            Self::Sigmoid => 0.5 + 0.5 * (0.5 * value).tanh(),
            Self::Relu => value.max(0.0),
        }
    }
}

#[derive(Debug)]
struct DenseLayer {
    inputs: usize,
    neurons: usize,
    activation: Activation,
    weights: &'static [u8],
    bias: &'static [u8],
}

impl DenseLayer {
    fn compute(&self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), self.inputs);
        debug_assert_eq!(output.len(), self.neurons);
        for (neuron, result) in output.iter_mut().enumerate() {
            let mut sum = signed(self.bias[neuron]);
            for (input_index, value) in input.iter().copied().enumerate() {
                sum += value * signed(self.weights[input_index * self.neurons + neuron]);
            }
            *result = self.activation.apply(sum * WEIGHT_SCALE);
        }
    }
}

#[derive(Debug)]
struct GruLayer {
    inputs: usize,
    neurons: usize,
    activation: Activation,
    weights: &'static [u8],
    recurrent: &'static [u8],
    bias: &'static [u8],
}

impl GruLayer {
    fn compute(&self, input: &[f32], state: &mut [f32]) {
        debug_assert_eq!(input.len(), self.inputs);
        debug_assert_eq!(state.len(), self.neurons);
        let mut update = [0.0_f32; MAX_NEURONS];
        let mut reset = [0.0_f32; MAX_NEURONS];
        let mut candidate = [0.0_f32; MAX_NEURONS];
        let gate_stride = self.neurons * 3;

        for (neuron, update_value) in update.iter_mut().enumerate().take(self.neurons) {
            let mut sum = signed(self.bias[neuron]);
            for (input_index, value) in input.iter().copied().enumerate() {
                sum += value * signed(self.weights[input_index * gate_stride + neuron]);
            }
            for (state_index, value) in state.iter().copied().enumerate() {
                sum += value * signed(self.recurrent[state_index * gate_stride + neuron]);
            }
            *update_value = Activation::Sigmoid.apply(sum * WEIGHT_SCALE);
        }

        for (neuron, reset_value) in reset.iter_mut().enumerate().take(self.neurons) {
            let offset = self.neurons + neuron;
            let mut sum = signed(self.bias[offset]);
            for (input_index, value) in input.iter().copied().enumerate() {
                sum += value * signed(self.weights[input_index * gate_stride + offset]);
            }
            for (state_index, value) in state.iter().copied().enumerate() {
                sum += value * signed(self.recurrent[state_index * gate_stride + offset]);
            }
            *reset_value = state[neuron] * Activation::Sigmoid.apply(sum * WEIGHT_SCALE);
        }

        for (neuron, candidate_value) in candidate.iter_mut().enumerate().take(self.neurons) {
            let offset = self.neurons * 2 + neuron;
            let mut sum = signed(self.bias[offset]);
            for (input_index, value) in input.iter().copied().enumerate() {
                sum += value * signed(self.weights[input_index * gate_stride + offset]);
            }
            for (state_index, value) in reset[..self.neurons].iter().copied().enumerate() {
                sum += value * signed(self.recurrent[state_index * gate_stride + offset]);
            }
            *candidate_value = self.activation.apply(sum * WEIGHT_SCALE);
        }

        for ((state, update), candidate) in state
            .iter_mut()
            .zip(&update[..self.neurons])
            .zip(&candidate[..self.neurons])
        {
            *state = *update * *state + (1.0 - *update) * *candidate;
        }
    }
}

#[derive(Debug)]
struct NeuralModel {
    input_dense: DenseLayer,
    vad_gru: GruLayer,
    noise_gru: GruLayer,
    denoise_gru: GruLayer,
    denoise_output: DenseLayer,
    vad_output: DenseLayer,
}

impl NeuralModel {
    fn parse(bytes: &'static [u8]) -> Result<Self, String> {
        let mut cursor = WeightCursor { bytes, offset: 0 };
        let model = Self {
            input_dense: cursor.dense()?,
            vad_gru: cursor.gru()?,
            noise_gru: cursor.gru()?,
            denoise_gru: cursor.gru()?,
            denoise_output: cursor.dense()?,
            vad_output: cursor.dense()?,
        };
        if cursor.offset != bytes.len() {
            return Err(format!(
                "RNNoise weights contain {} trailing bytes",
                bytes.len() - cursor.offset
            ));
        }
        if model.input_dense.inputs != RNNOISE_FEATURE_COUNT
            || model.input_dense.neurons != RNNOISE_VAD_STATE_COUNT
            || model.vad_gru.inputs != RNNOISE_VAD_STATE_COUNT
            || model.vad_gru.neurons != RNNOISE_VAD_STATE_COUNT
            || model.noise_gru.neurons != RNNOISE_NOISE_STATE_COUNT
            || model.denoise_gru.neurons != RNNOISE_DENOISE_STATE_COUNT
            || model.denoise_output.inputs != RNNOISE_DENOISE_STATE_COUNT
            || model.denoise_output.neurons != RNNOISE_GAIN_COUNT
            || model.vad_output.inputs != RNNOISE_VAD_STATE_COUNT
            || model.vad_output.neurons != 1
            || model.noise_gru.inputs
                != model.input_dense.neurons + RNNOISE_VAD_STATE_COUNT + RNNOISE_FEATURE_COUNT
            || model.denoise_gru.inputs
                != RNNOISE_VAD_STATE_COUNT + RNNOISE_NOISE_STATE_COUNT + RNNOISE_FEATURE_COUNT
        {
            return Err("RNNoise model dimensions do not match accelerator protocol v1".into());
        }
        Ok(model)
    }

    fn process(
        &self,
        features: &[f32; RNNOISE_FEATURE_COUNT],
        state: &[f32; RNNOISE_STATE_COUNT],
    ) -> CpuNeuralOutput {
        let mut next_state = *state;
        let (vad_state, remainder) = next_state.split_at_mut(RNNOISE_VAD_STATE_COUNT);
        let (noise_state, denoise_state) = remainder.split_at_mut(RNNOISE_NOISE_STATE_COUNT);

        let mut dense = [0.0_f32; RNNOISE_VAD_STATE_COUNT];
        self.input_dense.compute(features, &mut dense);
        self.vad_gru.compute(&dense, vad_state);
        let mut vad = [0.0_f32; 1];
        self.vad_output.compute(vad_state, &mut vad);

        let mut noise_input = [0.0_f32; MAX_INPUTS];
        let mut cursor = 0;
        copy_into(&mut noise_input, &mut cursor, &dense);
        copy_into(&mut noise_input, &mut cursor, vad_state);
        copy_into(&mut noise_input, &mut cursor, features);
        self.noise_gru
            .compute(&noise_input[..self.noise_gru.inputs], noise_state);

        let mut denoise_input = [0.0_f32; MAX_INPUTS];
        cursor = 0;
        copy_into(&mut denoise_input, &mut cursor, vad_state);
        copy_into(&mut denoise_input, &mut cursor, noise_state);
        copy_into(&mut denoise_input, &mut cursor, features);
        self.denoise_gru
            .compute(&denoise_input[..self.denoise_gru.inputs], denoise_state);
        let mut gains = [0.0_f32; RNNOISE_GAIN_COUNT];
        self.denoise_output.compute(denoise_state, &mut gains);

        CpuNeuralOutput {
            gains,
            vad_probability: vad[0],
            state: next_state,
        }
    }
}

struct WeightCursor {
    bytes: &'static [u8],
    offset: usize,
}

impl WeightCursor {
    fn header(&mut self) -> Result<(usize, usize, Activation), String> {
        let bytes = self.take(3)?;
        let inputs = bytes[0] as usize;
        let neurons = bytes[1] as usize;
        if inputs == 0 || neurons == 0 || neurons > MAX_NEURONS || inputs > MAX_INPUTS {
            return Err(format!(
                "invalid RNNoise layer dimensions inputs={inputs} neurons={neurons}"
            ));
        }
        Ok((inputs, neurons, Activation::parse(bytes[2])?))
    }

    fn dense(&mut self) -> Result<DenseLayer, String> {
        let (inputs, neurons, activation) = self.header()?;
        let weights = self.take(inputs * neurons)?;
        let bias = self.take(neurons)?;
        Ok(DenseLayer {
            inputs,
            neurons,
            activation,
            weights,
            bias,
        })
    }

    fn gru(&mut self) -> Result<GruLayer, String> {
        let (inputs, neurons, activation) = self.header()?;
        let weights = self.take(3 * inputs * neurons)?;
        let recurrent = self.take(3 * neurons * neurons)?;
        let bias = self.take(3 * neurons)?;
        Ok(GruLayer {
            inputs,
            neurons,
            activation,
            weights,
            recurrent,
            bias,
        })
    }

    fn take(&mut self, length: usize) -> Result<&'static [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| "truncated RNNoise weights".to_string())?;
        let value = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(value)
    }
}

fn signed(value: u8) -> f32 {
    (value as i8) as f32
}

fn copy_into<const N: usize>(destination: &mut [f32; N], cursor: &mut usize, source: &[f32]) {
    let end = *cursor + source.len();
    destination[*cursor..end].copy_from_slice(source);
    *cursor = end;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct GoldenFixture {
        max_abs_error: f32,
        cases: Vec<GoldenCase>,
    }

    #[derive(Deserialize)]
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

    #[test]
    fn cpu_neural_stage_matches_generated_golden_fixture() {
        let fixture: GoldenFixture = serde_json::from_str(include_str!(
            "../../../providers/rnnoise/rnnoise-neural-v1-golden.json"
        ))
        .unwrap();
        let mut maximum = 0.0_f32;
        for case in fixture.cases {
            let features: [f32; RNNOISE_FEATURE_COUNT] = case.features.try_into().unwrap();
            let mut state = [0.0_f32; RNNOISE_STATE_COUNT];
            state[..RNNOISE_VAD_STATE_COUNT].copy_from_slice(&case.vad_state);
            state[RNNOISE_VAD_STATE_COUNT..RNNOISE_VAD_STATE_COUNT + RNNOISE_NOISE_STATE_COUNT]
                .copy_from_slice(&case.noise_state);
            state[RNNOISE_VAD_STATE_COUNT + RNNOISE_NOISE_STATE_COUNT..]
                .copy_from_slice(&case.denoise_state);
            let output = cpu_neural_step(&features, &state).unwrap();
            maximum = maximum.max(max_error(&output.gains, &case.gains));
            maximum = maximum.max((output.vad_probability - case.vad_probability).abs());
            maximum = maximum.max(max_error(
                &output.state[..RNNOISE_VAD_STATE_COUNT],
                &case.vad_state_out,
            ));
            maximum = maximum.max(max_error(
                &output.state
                    [RNNOISE_VAD_STATE_COUNT..RNNOISE_VAD_STATE_COUNT + RNNOISE_NOISE_STATE_COUNT],
                &case.noise_state_out,
            ));
            maximum = maximum.max(max_error(
                &output.state[RNNOISE_VAD_STATE_COUNT + RNNOISE_NOISE_STATE_COUNT..],
                &case.denoise_state_out,
            ));
        }
        assert!(
            maximum <= fixture.max_abs_error,
            "maximum error {maximum} exceeded {}",
            fixture.max_abs_error
        );
    }

    #[test]
    fn stateful_stage_commits_only_its_latest_output_state() {
        let mut stage = CpuNeuralStage::new().unwrap();
        let mut features = [0.0_f32; RNNOISE_FEATURE_COUNT];
        features[0] = 1.0;
        let first = stage.process(&features);
        assert_eq!(stage.state(), &first.state);
        features[1] = -0.5;
        let second = stage.process(&features);
        assert_eq!(stage.state(), &second.state);
        assert_ne!(first.state, second.state);
    }

    fn max_error(actual: &[f32], expected: &[f32]) -> f32 {
        actual
            .iter()
            .zip(expected)
            .map(|(actual, expected)| (actual - expected).abs())
            .fold(0.0, f32::max)
    }
}
