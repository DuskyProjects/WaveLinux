# WaveLinux Roadmap

## Shipped

- Mixer UI with channel strips, peak meters, MON/STR faders, link, and mic gain
- Single-mic runtime model with FX-safe mic switching
- Stream virtual device for OBS (`WaveLinux-Stream`)
- App routing with persistence, icons, offline presets, and identity overrides
- Per-channel unified FX chains with runtime recovery and diagnostics
- Scene save/load and quick-start setup templates
- Verified AppImage updates, rollback backup, and Health recovery center
- Runtime-aware launcher repair for AppImage, source, bundle, and package modes

## Next

- Hotplug and device-transition hardening around Bluetooth, profile churn, and mic swaps
- Mixer layout polish at extreme window sizes without clipping or dead space
- Broader wrapper-app and sandboxed-app identity heuristics
- More targeted diagnostics and recovery UX polish

## Not Planned

- Native in-app VST hosting
- Flatpak packaging (sandbox conflicts with current routing model)
- Stream Deck integration
- Global cross-compositor hotkey system
- Multi-user daemon/service architecture
