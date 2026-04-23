# WaveLinux

A PipeWire mixer for Linux that lets you split apps into separate buses,
keep a dedicated **Stream** bus for OBS that doesn't include your own voice
monitor, and route each app to the mix you want.

It's a small PyQt6 app that talks to PipeWire through `pactl` and `pw-dump`.
No daemon, no system service.

## What it does

- Creates virtual sinks (Game / Music / Browser / SFX by default) that
  apps can play to.
- Creates two master buses — **Monitor** (what you hear) and **Stream**
  (what OBS records) — each with its own volume and its own hardware
  output.
- Exposes the Stream bus as a named recording source (`WaveLinux Stream`)
  so OBS picks it up from the input device list directly, instead of
  "Monitor of Null Sink N".
- Per-channel Monitor/Stream faders, so your mic can be in Monitor but
  not Stream, or your game audio can be louder for your audience than
  for you.
- Per-app routing — each running app shows up with its own sink picker,
  and the choice persists even when the app is closed.
- Tries to identify Flatpak/Snap apps by reading `FLATPAK_ID`,
  `.flatpak-info`, `SNAP_INSTANCE_NAME`, cgroup info, and walking up the
  `bwrap`/`snap-confine` wrapper chain, so things stop showing up as
  "audio-src".
- Optional RNNoise noise suppression on mic channels via PipeWire's
  filter-chain (spawned as a client — it will not try to take over your
  audio system).
- An **Emergency Reset** button that unloads every WaveLinux-owned
  PipeWire module, for when something has wedged.

## Install

Tested on CachyOS / Arch with KDE. Other distros will probably work —
you need `pipewire`, `pipewire-pulse`, `wireplumber`, `python` and
`python-pyqt6`, plus `swh-plugins` for the compressor/gate/limiter and
the AUR's `noise-suppression-for-voice` for RNNoise.

```bash
git clone https://github.com/excalprimeacct-gif/WaveLinux.git
cd WaveLinux
./install.sh
```

Or just `python3 main.py` from the repo.

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
- The filter-chain gate uses `gate_1410` from `swh-plugins`; if you
  don't have that package installed, the gate effect won't start.

## License / credits

Do what you want with it. See ROADMAP.md for what's done and what isn't.
