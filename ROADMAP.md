# WaveLinux — Roadmap & Status

WaveLinux is a small PyQt6 app that drives PipeWire through `pactl` and
`pw-dump` to build per-app / per-bus routing without a custom daemon.

The target is Elgato Wave Link-style behaviour on Linux: three sliders
per channel (gain / monitor send / stream send), two master buses, a
dedicated stream recording device, and scenes.

## Done

### UI
- [x] **Dark theme** (`wavelinux_theme.py`). Custom QSS tokens, applied
      app-wide.
- [x] **Tray integration** with graceful fallback to quit-on-close when
      no system tray is available.
- [x] **Per-row app routing**. One row per detected app with a sink
      picker and a direct volume slider.
- [x] **Hide / Show channels**. Hidden set survives restart.
- [x] **Scene picker** in the header — save / load / delete named
      setups. Applying a scene reconciles virtual channels, hardware
      routing, per-channel mutes/volumes, hidden set, and Clipguard
      state in one action.

### Core audio engine
- [x] **User virtual sinks** — `wavelinux_<name>` with a sanitised safe
      name and a real `device.description`. Apps see the channel's
      actual display name.
- [x] **Monitor + Stream master buses**. Each bus is a null-sink plus a
      `module-virtual-source` pointed at its monitor, so OBS sees
      `WaveLinux Stream` as a dedicated recording device.
- [x] **Loopback routing from inputs to buses**. Idempotent — reuses an
      existing loopback when one already exists, and only touches
      PipeWire when the live module has actually gone away.
- [x] **Hardware output routing per bus**, persisted across runs. The
      "None (Disconnected)" choice actually unloads the loopback now,
      rather than silently no-opping.
- [x] **Cleanup on startup / shutdown / Emergency Reset**.
- [x] **Master-bus volume sliders** via `pactl set-sink-volume <name>`.
- [x] **Per-channel volume/mute sync** reads engine state once per
      refresh, so pavucontrol and media keys stay in sync with the UI.
- [x] **Per-channel Input Gain slider** — Wave Link's "channel gain",
      controls the underlying sink/mic node volume before the bus send
      faders.
- [x] **Solo** — mutes every other channel in Monitor while held, then
      restores the previous mute layout on exit. Monitor-only (your
      stream doesn't suddenly go silent when you solo).
- [x] **Clipguard** — a Wave Link-style limiter on the Stream bus, on
      by default in scenes that save it.
- [x] **Snapshot-cached refresh**. One refresh tick calls each heavy
      `pactl` / `pw-dump` invocation exactly once (down from ~15) and
      threads the cached text through every helper that needs it.
      Refresh is also short-circuited when the window is hidden to the
      tray.
- [x] **Event-driven refresh**. `pactl subscribe` runs under a QProcess
      and kicks a 150 ms debounce whenever the audio graph changes, so
      an external mute / volume tweak (pavucontrol, media keys, another
      app) lands in the UI within ~150 ms instead of waiting up to 2 s
      for the poll timer. The 2 s poll stays as a backstop.
- [x] **Atomic config writes** (temp file + `os.replace`) so a crash
      mid-save can't corrupt `config.json`.
- [x] **Stable per-channel state**. Submix volume / mute / gain and
      the hidden set are keyed by PipeWire `node.name` (which survives
      a PipeWire restart), not the ephemeral numeric id. A one-shot
      migration drops any legacy pw_id-keyed entries on load.
- [x] **LADSPA plugin probe**. On startup the engine scans
      `$LADSPA_PATH` + common distro paths for each effect's backing
      `.so`; the FX dialog shows unavailable effects as "N/A" with a
      tooltip explaining which package is missing, and Clipguard
      refuses to enable when `fast_lookahead_limiter_1913` isn't
      installed. No more silent fail-to-start.

### App identification
- [x] **Flatpak / Snap / wrapper-aware app names**. Reads
      `/proc/<pid>/environ`, falls back to `.flatpak-info` and cgroup,
      walks up past `bwrap` / `snap-confine`. Generic PipeWire names
      like `audio-src` trigger the deeper lookup.
- [x] **App-routing persistence**. Routing choices are remembered per
      app name; apps stay in the panel as "(Offline)" when closed.
- [x] **Forget offline apps**. Each offline routing row has a ✕
      button that drops the saved entry from `app_routing` and removes
      the row, so the panel doesn't grow forever as you try new apps.

### Packaging / install
- [x] **`install.sh`** for Arch / CachyOS with pacman + paru/yay fallback
      for the AUR-only RNNoise plugin. Writes a `.desktop` file and a
      hicolor icon.

## Wave Link parity — what's still missing

### High priority
- [ ] **VU / peak meters per channel**. Wave Link shows per-channel
      levels next to the fader. Needs either a peak-detector LADSPA
      node per loopback or a `pw-record`-based sampler; the polling
      model won't hack it at animation frame rate.
- [ ] **Parameterised mic FX**. The FX dialog toggles rnnoise /
      compressor / gate / limiter but doesn't expose their parameters
      (threshold, ratio, attack, release, VAD %). Needs a richer FX
      panel — per-effect sliders plus a reorderable chain.
- [ ] **Filter / EQ on mic**. Wave Link has a tiltable 3-band EQ and a
      high-pass. Backend would be another filter-chain preset; UI is
      the real work.

### Medium priority
- [ ] **Drag-to-reorder channels**. Today channels appear in PipeWire
      discovery order. Wave Link lets you drag strips.
- [ ] **Rename virtual channels in-place** (currently it's
      remove-and-add with the new name).
- [ ] **ALSA profile switching** (Analog Stereo vs Pro Audio) from the
      mix-out dropdown.
- [ ] **Hot-plug notification / animation** when a USB device arrives
      or disappears — the refresh loop picks them up but there's no
      visible cue.
- [ ] **Clear offline apps** — `app_routing` grows forever, with no UI
      to forget an app.

### Lower priority
- [ ] **Global hotkeys** (mute mic, master up/down).
- [ ] **Autostart** toggle (writes
      `~/.config/autostart/wavelinux.desktop`).
- [ ] **Ducking** — Wave Link ducks music under voice chat. PipeWire's
      primitives make this fiddlier than on PulseAudio.
- [ ] **Scene hotkeys** — bind a key to a scene.
- [ ] **AUR `PKGBUILD`** and **Flatpak manifest**. Flatpak will need
      portal-based PipeWire access.

### Explicitly not planned
- **Stream Deck integration** — out of scope for a plain desktop app.
- **VST3 hosting** — Wave Link runs proprietary Elgato VST3 plugins;
  WaveLinux stays on LADSPA via filter-chain.

## Architecture

- **Language**: Python 3.11+.
- **GUI**: PyQt6.
- **Audio**: PipeWire via `pactl`, `pw-dump`, and `wpctl` — no direct
  libpipewire binding.
- **Process model**: single desktop process. FX chains (rnnoise,
  compressor, gate, limiter, Clipguard) are spawned as separate
  `pipewire -c <conf>` clients with `core.daemon = false` so they
  can't take over the session, and their stderr is captured to
  `~/.config/wavelinux/fx-logs/`.
- **Per-refresh snapshot** (`EngineSnapshot`): the UI fetches
  `pactl list modules / short modules / sink-inputs / sinks / short
  sinks` and `pw-dump` once per tick and threads the cached text
  through engine helpers.
- **Event subscriber**: `pactl subscribe` runs under a `QProcess`;
  each event nudges a 150 ms debounce. The 2 s poll is the backstop
  when the subscriber is unavailable or misses an event.
- **LADSPA probe**: engine scans `$LADSPA_PATH` plus the common
  distro paths at startup and exposes an `effect_available()` helper
  so the UI can grey out effects whose backing plugin isn't
  installed.
- **State keys**: all persisted per-channel state uses PipeWire
  `node.name` (e.g. `alsa_input.pci-...`, `wavelinux_game`), which
  survives PipeWire restarts. The ephemeral numeric id is only
  passed to the engine for pactl calls.
- **Config**: `~/.config/wavelinux/config.json` (written atomically).
- **Log**: `~/.config/wavelinux/wavelinux.log`.
