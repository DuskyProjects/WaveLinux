//! Display analysis runs on the meter socket worker, never an audio callback.
use crate::SPECTRUM_BINS;
use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;

pub const SPECTRUM_FRAMES: usize = 4096;
pub struct SpectrumAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    buffer: Vec<Complex32>,
    scratch: Vec<Complex32>,
    window: Vec<f32>,
    levels: [f32; SPECTRUM_BINS],
}
impl Default for SpectrumAnalyzer {
    fn default() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(SPECTRUM_FRAMES);
        Self {
            scratch: vec![Complex32::default(); fft.get_inplace_scratch_len()],
            fft,
            buffer: vec![Complex32::default(); SPECTRUM_FRAMES],
            window: (0..SPECTRUM_FRAMES)
                .map(|i| {
                    0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / SPECTRUM_FRAMES as f32).cos()
                })
                .collect(),
            levels: [0.0; SPECTRUM_BINS],
        }
    }
}
impl SpectrumAnalyzer {
    pub fn reset(&mut self) {
        self.levels.fill(0.0);
    }
    /// Both stereo components are analysed independently, preserving antiphase audio.
    pub fn analyse(
        &mut self,
        rate: u32,
        mut frame: impl FnMut(usize) -> Option<[f32; 2]>,
    ) -> Option<[f32; SPECTRUM_BINS]> {
        let mut amplitudes = [0.0_f32; SPECTRUM_BINS];
        for channel in 0..2 {
            for i in 0..SPECTRUM_FRAMES {
                let sample = frame(i)?[channel];
                self.buffer[i] = Complex32::new(
                    if sample.is_finite() {
                        sample * self.window[i]
                    } else {
                        0.0
                    },
                    0.0,
                );
            }
            self.fft
                .process_with_scratch(&mut self.buffer, &mut self.scratch);
            for (index, amplitude) in amplitudes.iter_mut().enumerate() {
                let frequency = 20.0 * 1000.0_f32.powf(index as f32 / (SPECTRUM_BINS - 1) as f32);
                let half_band = 1000.0_f32.powf(0.5 / (SPECTRUM_BINS - 1) as f32);
                let bin_scale = SPECTRUM_FRAMES as f32 / rate.max(1) as f32;
                let start = (frequency / half_band * bin_scale).round().max(1.0) as usize;
                let end = (frequency * half_band * bin_scale)
                    .round()
                    .max(start as f32) as usize;
                for bin in start..=end.min(SPECTRUM_FRAMES / 2) {
                    *amplitude =
                        amplitude.max(self.buffer[bin].norm() * (4.0 / SPECTRUM_FRAMES as f32));
                }
            }
        }
        for (level, amplitude) in self.levels.iter_mut().zip(amplitudes) {
            let next = ((20.0 * amplitude.max(1e-9).log10() + 90.0) / 90.0).clamp(0.0, 1.0);
            *level += (next - *level) * if next > *level { 0.65 } else { 0.25 };
        }
        Some(self.levels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spectrum_locates_stereo_tones_and_distinguishes_silence_from_missing_input() {
        let mut analyser = SpectrumAnalyzer::default();
        let mut result = [0.0; SPECTRUM_BINS];
        for _ in 0..10 {
            result = analyser
                .analyse(48000, |i| {
                    let tone = (std::f32::consts::TAU * 1000.0 * i as f32 / 48000.0).sin() * 0.5;
                    Some([tone, -tone])
                })
                .unwrap();
        }
        let peak = result
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        let frequency = 20.0 * 1000.0_f32.powf(peak as f32 / 63.0);
        assert!((frequency - 1000.0).abs() < 80.0);
        assert!(result[peak] > 0.9);
        assert!(analyser.analyse(48000, |_| None).is_none());
        analyser.reset();
        assert_eq!(
            analyser.analyse(48000, |_| Some([0.0; 2])).unwrap(),
            [0.0; SPECTRUM_BINS]
        );
    }
}
