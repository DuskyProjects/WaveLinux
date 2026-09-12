# Effects review — 12 September 2026

Release: **6.1.0**. Includes the visual EQ/compressor, selected-channel telemetry,
processed-output spectrum and native DeepFilterNet work validated through local
installation. The user authorized publication after local testing; the release
uses the same effect implementation with stable version metadata.

## Fixes

- RNNoise's Advanced dry mix now aligns with the neural output's analysis delay.
  Previously dry audio arrived 480 frames early, causing phase interference when
  blended. Both paths now include the same 960-frame streaming/analysis delay at
  48 kHz. Mono dry output also duplicates the selected mono input correctly.
- A closed RNNoise speech gate advances its input history and clears muted
  synthesis overlap. Reopening cannot replay the stale overlap from before the
  pause. Closed gates and fully dry output skip neural inference.
- RNNoise rejects unsupported sample rates, as DeepFilterNet already does.
- The voice-style tone filters and room feedback discard inaudible tiny state
  before it incurs subnormal arithmetic costs. Delay indexing and modulation
  wrapping use bounded comparisons in place of repeated divisions.
- Strength and Advanced sliders apply keyboard edits on key release. Escape and
  cancelled pointer gestures discard edits. Native thumbs retain pointer capture
  so dragging and releasing outside the control works in WebKit and Chromium.
- Simple readouts show actual Advanced settings, including values beyond the
  simple slider's range. A 500 Hz high-pass setting no longer displays 200 Hz.
- Speech Gate is an on/off switch. Dry and double mixes use percentages, with
  one-percent adjustments instead of ambiguous fractional readouts.
- Instrumented DSP checks now count inference failures in their fallback metrics,
  matching error reporting in the audio worker.

## Verification

- 550 Rust test executions, including all 54 catalog/default/preset/parameter-limit
  combinations. Every effect was checked for bypass transparency and consistent
  output across irregular block sizes. Existing compressor transfer/meter, gate,
  EQ response, stereo isolation and DeepFilterNet attenuation tests passed.
- New RNNoise regressions cover dry/wet timing, mono dry output, and gate reopening
  against fresh input. The explicit CPU neural stage still matches upstream
  processing within the existing numerical tolerance.
- 116 frontend tests, 33 Chromium checks across three desktop scales, and four
  WebKit interaction checks passed. One screenshot was updated after inspecting
  the intentional Speech Gate switch change.
- Formatting, Clippy, TypeScript, production web build, shell, documentation,
  dependency and isolated installer checks passed. Optional ONNX generation tools
  were unavailable; committed model and fixture hashes passed.
- The private virtual session passed in 11.81 seconds: microphone/Game/Music
  isolation, compressor telemetry, processed EQ cuts/boosts, native DeepFilterNet,
  microphone/output reconnects, audio-server/engine restart and saved settings.
  No physical audio was recorded and no test devices leaked into desktop audio.
- The packaged AppImage passed native, PipeWire and Wayland checks using the
  existing host-strip packaging workaround. Installed artifacts match the build;
  preferences were preserved through installation and startup. Five-second UI/core
  stability and 12 live meter frames passed, including microphone spectrum signal
  and compressor telemetry. Detailed evidence is in `target/effects-audit/`.

## Timing observation

The short offline fixture measured voice-style near-silence at 21.62 times the
signal processing time before the fix, and 1.02 times afterward. Absolute times
were 0.0318/0.6878 microseconds per frame before and 0.0459/0.0468 afterward
(signal/near-silence). Desktop load changed between runs, so these are evidence
that the silence-specific slowdown was removed, not a claim about total app CPU.
Raw measurements and the reusable example are retained for future comparisons.

Synthetic checks do not determine how a particular microphone sounds. Local
listening remains the final check for speech clarity, quiet words and preferred
noise strength. Existing user settings are preserved during installation.
No long-duration stress test is a requirement or was run for this review.
