# WaveLinux compatibility patch

This directory is the BSD-3-Clause `nnnoiseless 0.5.2` source. WaveLinux keeps
it pinned locally so the RNNoise feature/synthesis boundary is reproducible.

The only behavioral extension is
`DenoiseFeatures::apply_denoise_gains_and_synthesize` plus
`DenoiseFeatures::synthesize_unmodified`. These methods expose the existing
gain smoothing and overlap-add synthesis after an isolated neural stage. The
upstream `DenoiseState::process_frame` API and model weights are unchanged.

Upstream copyright and license terms are in `COPYING` and `AUTHORS`.
