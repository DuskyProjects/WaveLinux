# WaveLinux

A PipeWire mixer for Linux that lets you split apps into separate buses,
keep a dedicated **Stream** bus for OBS that doesn't include your own voice
monitor, and route each app to the mix you want.

It's a small PyQt6 app that talks to PipeWire through `pactl` and `pw-dump`.
No daemon, no system service.

## What it does

- Creates virtual sinks (Game / Music / Browser / SFX by default) that
  apps can play to.
- Two master buses — **Monitor** (what you hear) and **Stream** (what
  OBS records) — each with its own volume, hardware output, and
  Clipguard-style limiter.
- The Stream bus is exposed as a dedicated recording source
  (`WaveLinux Stream`) so OBS picks it up directly from the input list.
- **Three sliders per channel**: Input Gain (pre-fader), Monitor send,
  Stream send — Wave Link's core mental model.
- Per-channel **Solo** (mutes every other channel in Monitor),
  **Rename** (in-place, state migrates), and **Reorder** (`◀` / `▶`
  buttons, persistent).
- **Per-channel peak meters** driven by a `parec` subprocess each,
  updating at ~20 Hz with a release envelope so they don't flicker.
- **FX with parameters** — rnnoise (VAD), high-pass (cutoff),
  compressor (threshold/ratio/attack/release/makeup), gate, limiter.
  High-pass uses PipeWire's built-in biquad; the rest are LADSPA via
  filter-chain. Parameter sliders re-apply the effect live.
- **Scenes** — save / load / delete named snapshots of the whole
  setup. `Ctrl+1`..`Ctrl+9` jumps to the first nine in sorted order.
- **Sound Card Profiles** dialog — switch ALSA profiles (Analog Stereo
  vs Pro Audio, etc.) from the header without dropping into
  pavucontrol.
- Per-app routing with Flatpak / Snap / wrapper-aware app
  identification. Apps stay in the panel as "(Offline)" when closed;
  click ✕ to forget a routing.
- **Autostart** toggle in the tray menu (writes
  `~/.config/autostart/wavelinux.desktop`).
- **Hot-plug tray notifications** when a device appears or goes away.
- **Emergency Reset** unloads every WaveLinux-owned PipeWire module
  when something wedges.

## Install

Tested on CachyOS / Arch with KDE. Other distros will probably work —
you need `pipewire`, `pipewire-pulse`, `wireplumber`, `python`,
`python-pyqt6`, `libpulse` (for `pactl` and `parec`), plus
`swh-plugins` for the compressor / gate / limiter and the AUR's
`noise-suppression-for-voice` for RNNoise.

From source:

```bash
git clone https://github.com/excalprimeacct-gif/WaveLinux.git
cd WaveLinux
./install.sh
```

Or via the bundled AUR PKGBUILD:

```bash
git clone https://github.com/excalprimeacct-gif/WaveLinux.git
cd WaveLinux
makepkg -si
```

You can also run straight from the repo with `python3 main.py`.

## Config and logs

- Settings: `~/.config/wavelinux/config.json`
- App log: `~/.config/wavelinux/wavelinux.log`
- Per-effect filter-chain logs: `~/.config/wavelinux/fx-logs/`

If an effect shows OFF and refuses to turn on, the fx-log is usually
the first place to look.

## Known limitations

- Not a replacement for pavucontrol — it won't manage every sink in the
  system, just WaveLinux-owned buses and app routing.
- Per-channel Monitor/Stream sliders control the loopback sink-input
  volume, not the app's own volume. Use the app-routing row for the
  per-app fader.
- Each filter-chain effect needs its LADSPA plugin installed
  (`swh-plugins` for compressor / gate / limiter,
  `noise-suppression-for-voice` from the AUR for RNNoise). If a
  plugin isn't on disk the effect shows as "N/A" in the FX dialog
  with a tooltip pointing at the package you need.
- No VU meters yet. Wave Link shows real-time levels per channel;
  WaveLinux doesn't. It would need a peak-detector filter-chain node
  per channel or a small `pw-record`-based probe — see ROADMAP.
- Wave Link ships proprietary VST3 effects (e.g. AI voice filter). We
  use the LADSPA equivalents from `swh-plugins`, which are solid but
  not the same set.

## License / credits

Do what you want with it. See ROADMAP.md for what's done and what isn't.
