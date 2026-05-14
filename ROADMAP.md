# WaveLinux Roadmap

## Shipped

- Mixer UI with channel strips, peak meters, MON/STR faders, link, and mic gain
- Responsive single-row mixer layout with wide-window fill and short-window compaction
- Single-mic runtime model with FX-safe mic switching
- Default-driven startup device policy with conservative runtime fallback and restore actions
- Stream virtual device for OBS (`WaveLinux-Stream`)
- App routing with persistence, icons, offline presets, and identity overrides
- Per-channel unified FX chains with runtime recovery and diagnostics
- Scene save/load and quick-start setup templates
- Verified AppImage updates, rollback backup, and Health recovery center
- Runtime-aware launcher repair for AppImage, source, bundle, and package modes

## Next

- Broader wrapper-app and sandboxed-app identity heuristics
- More targeted diagnostics and recovery UX polish
- Additional device-policy soak testing across broader hardware combinations

## Not Planned

- Native in-app VST hosting
- Flatpak packaging (sandbox conflicts with current routing model)
- Stream Deck integration
- Global cross-compositor hotkey system
- Multi-user daemon/service architecture
