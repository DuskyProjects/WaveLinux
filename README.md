# WaveLinux

A PipeWire mixer for Linux that behaves like Elgato Wave Link: split
apps into separate channels, keep a dedicated **Stream** bus for OBS
that doesn't include your voice monitor, and set the mix your audience
hears independently of what you hear.

PyQt6 app. Talks to PipeWire through `pactl`, `pw-dump`, `wpctl`, and
`parec`. No daemon, no system service.

## What it does

- **🎧 Monitor Output / 📡 Stream** — two master buses, two knobs. The
  Stream bus is a named virtual recording device (`WaveLinux-Stream`)
  that shows up directly in OBS's audio input picker.
- **Per-channel dual faders** — every channel has one fader for what
  *you* hear (MON) and one for what your *audience* hears (STR). A
  🔗 Link button ties them together when you want them to move as one.
- **Per-channel peak meter** so you can see who's loud, updated at
  ~20 Hz with a release envelope.
- **Clipguard / Limiter** per microphone — a brickwall limiter that
  protects the active mic from clipping. Lives as the `Limiter` row in
  the channel's right-click → Effects dialog. Earlier builds put a
  Clipguard button on the Stream master bus; that affected the whole
  broadcast (music, game, voice mixed together) which made gain-staging
  awkward, so it now sits per-channel where it can actually catch the
  source that's clipping.
- **Per-app routing** — every running app shows up as a row with a
  volume slider and a destination picker (system default, a hardware
  output, or any WaveLinux channel). Routing persists per app and
  survives the app being closed.
- **Flatpak / Snap / wrapper-aware names** — reads `FLATPAK_ID`,
  `.flatpak-info`, cgroup scopes, and walks up past `bwrap` /
  `snap-confine` so things stop showing up as "audio-src".
- **Parameterised effects** per channel — RNNoise, High-Pass,
  3-Band EQ, Compressor, Noise Gate, Limiter. Each effect has
  an in-dialog description, preset buttons ("Gentle / Broadcast /
  Aggressive", "Flat / Broadcast Voice / Warm Music", etc.) and
  parameter sliders that apply live. Effect on/off state and
  parameters persist per channel across restarts and are
  auto-reapplied when the channel reappears.
- **Optional VST/LV2 hosting via Carla** — if `carla` is on
  `$PATH`, a "🎹 Open VST plugin (Carla)…" entry appears in the
  channel context menu. WaveLinux doesn't host VST3 natively; it
  bridges to Carla, which does.
- **Sound card profile picker** (tray menu) — switch ALSA profiles
  (Analog Stereo vs Pro Audio, etc.) without dropping into
  pavucontrol.
- **Autostart** toggle (tray menu + Settings → Advanced).
- **Settings → Apps / Hidden / Advanced**. Advanced holds app-prune
  cutoff, Emergency Reset, and a LADSPA plugin diagnostic count so
  you know immediately why an effect shows "N/A".
- **Stale routing prune** — app_routing entries that haven't been
  seen in `N` days (default 14) get dropped on startup so the list
  doesn't grow forever. Adjustable in Settings → Advanced.
- **Volumes cap at 100%** everywhere. PipeWire allows up to 150% but
  it sounds clipped; WaveLinux enforces unity as the ceiling.
- **Minimal chrome** — icon, name, meter, fader. Everything else
  (Effects, Rename, Move, Remove, Hide, Open in Carla) is one
  right-click away.

## Install

Tested on CachyOS / Arch with KDE. Any PipeWire distro should work —
dependencies: `pipewire`, `pipewire-pulse`, `wireplumber`, `python`,
`python-pyqt6`, `libpulse` (for `pactl` / `parec`), and `swh-plugins`
for the compressor / gate / limiter. RNNoise needs
`noise-suppression-for-voice` from the AUR.

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

Run it from source with `python3 main.py` (or `./start.sh`, which
just `cd`s to the source tree and runs `python3 main.py` for you).
After `install.sh`, you can also launch the wrapper from anywhere
with `wavelinux` (assuming `~/.local/bin` is on your `$PATH`).

## Where things live

- Settings: `~/.config/wavelinux/config.json`
- App log: `~/.config/wavelinux/wavelinux.log`
- Per-effect filter-chain logs: `~/.config/wavelinux/fx-logs/`

If an effect shows "N/A" or "OFF" and won't turn on, the fx-log is the
first place to look — the LADSPA plugin it needs may not be installed.

## OBS setup

1. Start WaveLinux. A virtual audio input called **WaveLinux-Stream**
   appears in PipeWire / pavucontrol / KDE's Audio Volume panel.
2. In OBS, add an *Audio Input Capture* source and pick
   `WaveLinux-Stream` (or `Monitor of WaveLinux-Stream` on some
   setups). That's the whole Stream mix in one channel.
3. Use each channel's STR fader (and 📡 mute) to decide what gets
   sent to OBS. Your own audio monitoring stays on the MON side.

## Known limitations

- Not a replacement for pavucontrol. WaveLinux only manages its own
  buses and app routing.
- Each filter-chain effect needs its LADSPA plugin installed. If a
  plugin isn't on disk the effect shows "N/A" in the FX dialog with a
  tooltip naming the package.
- Wave Link ships proprietary VST3 effects; WaveLinux uses the LADSPA
  equivalents from `swh-plugins` and PipeWire's built-in biquad HPF.
  Not exactly the same set of processors, but enough for a clean
  broadcast chain.

## License / credits

Do what you want with it. See ROADMAP.md for what's done and what's
not planned (Stream Deck integration, VST3 hosting, global hotkeys,
Flatpak — with reasons for each).
