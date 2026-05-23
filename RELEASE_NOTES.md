# WaveLinux 4.1.0

WaveLinux 4.1 is the background optimization and hardware profile release. It
adds safe audio-only hardware profiles, Bluetooth headset protection, searchable
profile assignment, lower-lag mixer controls, better metering, and release
packaging updates without changing the mixer-first workflow.

## Highlights

- Added audio-only hardware profiles as individual JSON device files under
  `profiles/v1`, with schema docs, examples, local overrides, signed remote
  bundle support, install-time hardware prewarm, and an editable safe generic
  fallback profile.
- Added shipped profile seeds for common creator/audio hardware, including Sony
  WH-1000XM4/XM5, DJI Wireless Mic Rx, Realtek ALC3254, NVIDIA HDA HDMI,
  Logitech webcam audio, JDS Labs Element II, Massdrop/Fostex TH-X00, USB audio
  interfaces, USB microphones, capture cards, USB headsets, and Bluetooth
  headsets.
- Protected Bluetooth A2DP playback by refusing HFP/HSP headset microphones as
  an optimization when they would degrade playback quality. WaveLinux now routes
  capture to DJI, USB, internal, or other non-Bluetooth microphones when
  available.
- Added searchable profile and route selectors, manual per-device profile
  assignment, and Profiles/Health tabs under Settings instead of adding main
  navigation clutter.
- Reworked UI command handling with optimistic updates and coalesced refreshes
  so faders, toggles, low-latency monitoring, device selection, and app volume
  changes do not freeze the interface while audio commands run.
- Fixed hardware input metering so it shows the selected microphone pre-FX when
  no effects are loaded, and the microphone-only post-FX signal when effects are
  active.
- Fixed channel Stream/Monitor VUs so they follow the effective channel-send and
  destination mix/master level.
- Renamed the effects microphone export to `wavelinux-mic` / `WaveLinux-mic`
  for clearer selection in Discord, OBS, browser capture, and similar apps.
- Added startup microphone safety repair for real non-Bluetooth sources,
  restoring them to 100% and unmuted while ignoring WaveLinux virtual and
  monitor sources.
- Added external command timeouts, release/native build paths, AUR check
  coverage, local profile seeding, and hardware profile asset generation.

## Notes

- WaveLinux still does not pretend impossible Bluetooth behavior is possible:
  normal Sony WH-1000XM4-style Bluetooth cannot provide full-quality A2DP
  playback and the headset microphone at the same time.
- HFP/HSP remains a compatibility fallback, not a performance optimization.
- Unknown devices use the safe generic profile unless a local or downloaded
  audio profile matches.
