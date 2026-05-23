# WaveLinux 4.1.1

WaveLinux 4.1.1 is a focused Bluetooth/profile fix release.

## Fixes

- Updated the Sony WH-1000XM4 hardware profile to prioritize stable A2DP
  playback over maximum LDAC bitrate.
- Raised the XM4 Bluetooth latency floor from 80 ms to 120 ms to reduce
  crackle/dropouts on marginal links.
- Changed the XM4 codec policy to prefer AAC, then SBC-XQ, then SBC before
  LDAC, and to avoid high-bitrate LDAC modes for stability.
- Set the XM4 LDAC profile guidance to stable/standard quality instead of auto
  so profile authors and local overrides do not treat high-bitrate LDAC as the
  default safe path.
- Fixed release packaging so signed hardware profile JSON assets are published
  with GitHub releases and can be downloaded by WaveLinux without an app rebuild.

## Notes

- The XM4 microphone guardrail is unchanged: HFP/HSP headset microphone mode is
  still treated as a compatibility fallback, not an optimization.
- If an XM4 is already connected on LDAC, switch the card profile to AAC locally
  to test the stability fix immediately.
