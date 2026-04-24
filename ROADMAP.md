# WaveLinux — Roadmap & Status

WaveLinux is a small PyQt6 app that drives PipeWire through `pactl`,
`pw-dump`, `wpctl`, and `parec` to build per-app / per-bus routing
without a custom daemon.

The target is Elgato Wave Link-style behaviour on Linux: three sliders
per channel (gain / monitor send / stream send), two master buses, a
dedicated stream recording device, scenes, per-channel FX, and peak
meters.

## Done

### UI
- [x] **Dark theme** (`wavelinux_theme.py`).
- [x] **Tray integration** with graceful fallback to quit-on-close when
      no system tray is available.
- [x] **Per-row app routing**. One row per detected app with a sink
      picker and a direct volume slider.
- [x] **Hide / Show channels**. Hidden set survives restart.
- [x] **Scene picker** in the header — save / load / delete named
      setups. Applying a scene reconciles virtual channels, hardware
      routing, per-channel mutes/volumes, hidden set, channel order,
      effect parameters, and Clipguard state in one action.
- [x] **Scene hotkeys**. `Ctrl+1`..`Ctrl+9` jumps to the first nine
      scenes (in sorted order) while the window has focus.
- [x] **Reorder channels** with `◀` / `▶` arrow buttons on each strip.
      Order persists in config.
- [x] **Rename virtual channels in place**. State (volumes, mutes,
      hidden flag, solo selection, effect parameters, order position)
      is migrated to the new `node.name` in one atomic step.
- [x] **Hot-plug tray bubbles** when a user-visible device is added
      or removed.
- [x] **Sound Card Profiles** dialog — switch ALSA card profiles
      (Analog Stereo / Pro Audio / …) directly from WaveLinux via
      `pactl set-card-profile`.
- [x] **Per-channel peak meter** driven by a dedicated `parec`
      subprocess per visible strip at ~20 Hz, with a simple release
      envelope so the meter doesn't flicker.

### Core audio engine
- [x] **User virtual sinks** (`wavelinux_<name>`) with sanitised safe
      names and real `device.description`.
- [x] **Monitor + Stream master buses**. Null-sink + `module-virtual-source`
      so OBS sees `WaveLinux Stream` as a dedicated recording device.
- [x] **Loopback routing from inputs to buses**. Idempotent — reuses
      existing loopbacks and never duplicates under the 2-second poll.
- [x] **Hardware output routing per bus**, persisted across runs. The
      "None (Disconnected)" choice actually unloads the loopback.
- [x] **Cleanup on startup / shutdown / Emergency Reset**.
- [x] **Master-bus volume sliders** via `pactl set-sink-volume <name>`.
- [x] **Per-channel volume/mute sync** reads engine state once per
      refresh so pavucontrol / media-key changes land in the UI.
- [x] **Per-channel Input Gain slider** — Wave Link's "channel gain".
- [x] **Solo** — mutes every other channel in Monitor while held.
- [x] **Clipguard** — Wave Link-style limiter on the Stream bus,
      persisted per-scene.
- [x] **High-pass filter** effect using PipeWire's built-in
      `bq_highpass` — no LADSPA plugin required.
- [x] **Parameterised FX**. Threshold / ratio / attack / release /
      VAD / cutoff sliders for rnnoise, highpass, compressor, gate,
      and limiter. Per-(node.name, effect_id) parameter sets persist
      in config and scenes; changing a live slider re-applies the
      effect immediately.
- [x] **Snapshot-cached refresh**. One refresh tick calls each heavy
      `pactl` / `pw-dump` invocation exactly once (down from ~15) and
      threads the cached text through every helper that needs it.
- [x] **Event-driven refresh** via `pactl subscribe` under a `QProcess`,
      with a 150 ms debounce and the 2 s poll as backstop.
- [x] **Atomic config writes** (temp file + `os.replace`).
- [x] **Stable `node.name`-keyed state** for submixes, hidden set,
      solo selection, effect params, and channel order.
- [x] **LADSPA plugin probe** at startup; effects whose `.so` isn't
      installed are shown as "N/A" with a tooltip naming the package.

### App identification
- [x] **Flatpak / Snap / wrapper-aware app names** via
      `/proc/<pid>/environ`, `.flatpak-info`, cgroup scopes, and
      `ppid` walking past `bwrap` / `snap-confine` / `flatpak`.
- [x] **App-routing persistence** with per-row "(Offline)" markers.
- [x] **Forget offline apps** via a per-row ✕ button.

### Packaging / install
- [x] **`install.sh`** for Arch / CachyOS with pacman + paru/yay
      fallback for the AUR-only RNNoise plugin. Writes a `.desktop`
      file and a hicolor icon.
- [x] **AUR `PKGBUILD`** — installs sources under `/usr/share/wavelinux`,
      adds a `/usr/bin/wavelinux` wrapper, and registers the desktop
      + icon entries. Depends on `swh-plugins`; optionally pulls in
      `noise-suppression-for-voice` from the AUR.
- [x] **Autostart toggle** in the tray menu — writes / removes
      `~/.config/autostart/wavelinux.desktop`.

## Explicitly not planned

- **Stream Deck integration** — out of scope for a plain desktop app.
- **VST3 hosting** — Wave Link runs proprietary Elgato VST3 plugins;
  WaveLinux stays on LADSPA via filter-chain, which works without a
  VST host.
- **Drag-and-drop reorder** — superseded by the `◀` / `▶` buttons.
  Real drag-reorder on a `QHBoxLayout` is surprisingly fiddly and the
  buttons are keyboard-and-touchpad-friendly in a way drag isn't.
- **True global hotkeys** — Wayland doesn't expose a cross-compositor
  registration API, and on X11 Qt's `QShortcut` is window-scoped. Use
  your compositor / DE's shortcut settings to bind a keystroke to
  `wavelinux` if you need out-of-focus control.
- **Ducking** — a sidechain compressor on the "music" bus driven by
  the voice bus needs a sidechain-aware LADSPA (or a native PipeWire
  filter-chain with sidechain) that we don't want to pull in as a
  hard dependency. Revisit if PipeWire grows a first-class solution.
- **Flatpak manifest** — WaveLinux manages its own PipeWire sinks
  and spawns `pipewire -c` clients; Flatpak's sandbox makes this
  awkward without a privileged portal we don't have. AUR is the
  supported distribution path.
- **3-band parametric EQ on mic** — we ship the high-pass, and the FX
  chain now has fully parameterised effects. A band-shelf EQ would
  be another filter-chain preset; not planned for the current scope.

## Architecture

- **Language**: Python 3.11+.
- **GUI**: PyQt6.
- **Audio**: PipeWire via `pactl`, `pw-dump`, `wpctl`, `parec`. No
  direct libpipewire binding.
- **Process model**: single desktop process. FX chains (rnnoise,
  highpass, compressor, gate, limiter, Clipguard) are spawned as
  separate `pipewire -c <conf>` clients with `core.daemon = false`
  so they can't take over the session, and their stderr is captured
  to `~/.config/wavelinux/fx-logs/`.
- **Per-refresh snapshot** (`EngineSnapshot`): the UI fetches
  `pactl list modules / short modules / sink-inputs / sinks / short
  sinks` and `pw-dump` once per tick and threads the cached text
  through engine helpers.
- **Event subscriber**: `pactl subscribe` runs under a `QProcess`;
  each event nudges a 150 ms debounce. The 2 s poll is the backstop
  when the subscriber is unavailable or misses an event.
- **Peak meters**: one `parec --raw --format=s16le --channels=1`
  subprocess per visible channel, feeding a `MeterWorker` that
  emits a peak signal at ~20 Hz. Meters are reaped when the channel
  disappears or is renamed.
- **LADSPA probe**: engine scans `$LADSPA_PATH` plus common distro
  paths at startup and exposes `effect_available()` for the UI.
- **State keys**: all persisted per-channel state uses PipeWire
  `node.name` (`alsa_input.pci-...`, `wavelinux_game`), which
  survives PipeWire restarts.
- **Config**: `~/.config/wavelinux/config.json` (atomic writes).
- **Log**: `~/.config/wavelinux/wavelinux.log`.
- **Per-effect logs**: `~/.config/wavelinux/fx-logs/`.
