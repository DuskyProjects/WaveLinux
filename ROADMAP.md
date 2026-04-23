# WaveLinux — Roadmap & Status

WaveLinux is a small PyQt6 app that drives PipeWire through `pactl` and
`pw-dump` to build per-app / per-bus routing without a custom daemon.

## Done

### UI
- [x] **Dark theme** (`wavelinux_theme.py`). Custom QSS tokens, applied app-wide.
- [x] **Tray integration**. System tray icon, Show / Quit menu. Falls back to
      plain quit-on-close when no tray is available (KDE without a systray,
      GNOME without an extension, etc).
- [x] **Per-row app routing**. One row per detected app with a sink picker
      and a direct volume slider.
- [x] **Hide / Show channels**. Clutter control for mics you never actually
      use. Hidden set survives restart.

### Core audio engine
- [x] **User virtual sinks** — `wavelinux_<name>`, created from the
      "+ Add Channel" button, persisted in config. `device.description`
      comes through correctly so apps show the channel by its real name.
- [x] **Monitor + Stream master buses**. Each bus is a null-sink plus a
      `module-virtual-source` pointed at its monitor, so OBS sees
      `WaveLinux Stream` as a dedicated recording device.
- [x] **Loopback routing from inputs to buses**. Idempotent — reuses an
      existing loopback when one already exists, and only touches PipeWire
      when the live module has actually gone away. No more duplicate
      loopbacks stacking up on every 2-second refresh.
- [x] **Hardware output routing per bus**. Remembered across runs.
- [x] **Cleanup on startup / shutdown / Emergency Reset**. Unloads every
      `wavelinux_*` module and any loopback referring to them.
- [x] **Master-bus volume sliders** driven via `pactl set-sink-volume <name>`
      (not `wpctl`, which needs numeric IDs).
- [x] **Per-channel Monitor/Stream volume sync** reads back from PipeWire
      once per refresh, so pavucontrol and media keys stay in sync with
      the UI.

### App identification
- [x] **Flatpak / Snap / wrapper-aware app names**. Reads
      `/proc/<pid>/environ` for `FLATPAK_ID` / `SNAP_INSTANCE_NAME`, falls
      back to `.flatpak-info` and the cgroup, and walks up past
      `bwrap` / `snap-confine` / `flatpak` parents. Generic PipeWire
      names like `audio-src` trigger the deeper lookup instead of being
      displayed.
- [x] **App-routing persistence**. Routing choices are remembered per app
      name; apps stay in the panel as "(Offline)" when closed so their
      routing is restored when they come back.

### Packaging / install
- [x] **`install.sh`** for Arch / CachyOS with pacman + paru/yay fallback
      for the AUR-only RNNoise plugin. Writes a `.desktop` file and a
      hicolor icon so the launcher shows up without needing a re-login.

## Not done yet

### Audio processing
- [ ] **FX panel UI**. Backend for Compressor, Limiter, Noise Gate, RNNoise
      is wired (`apply_effect` / `remove_effect`), but exposed only via
      the modal FX dialog. No per-effect parameter controls yet.
- [ ] **RNNoise threshold slider**. Backend constant at 50; no UI.
- [ ] **Meters / visualizers**. No peak / level display.

### Hardware & compatibility
- [ ] **ALSA profile switching** (Analog Stereo vs Pro Audio) from the
      mix-out dropdown.
- [ ] **Hot-plug animation / notification** when a USB device arrives or
      disappears. The refresh loop picks them up, but there's no visible
      cue.
- [ ] **Rename virtual channels** in-place. Today you Remove + Add with a
      new name.

### Workflow / UX
- [ ] **Routing profiles** — save the whole routing table under a name
      and switch between them.
- [ ] **Global hotkeys** (mute mic, master up/down).
- [ ] **Autostart** toggle in the UI (writes
      `~/.config/autostart/wavelinux.desktop`).
- [ ] **Clear offline apps** — today `app_routing` grows forever; there
      is no way to forget an app from the UI.

### Packaging
- [ ] **AUR `PKGBUILD`**.
- [ ] **Flatpak manifest**. Will need portal-based PipeWire access.

## Caveats that aren't on the backlog

- Mute state for the per-channel Monitor/Stream faders is tracked inside
  the app and only reconciled on refresh — a very fast external toggle
  could flip state between ticks.
- The filter-chain "Noise Gate" uses `gate_1410` from `swh-plugins`. If
  that package isn't installed the effect will fail; the log shows up at
  `~/.config/wavelinux/fx-logs/gate-<channel>.log`.
- The app doesn't (yet) watch PipeWire events; it polls `pw-dump` and
  `pactl` every 2 seconds. Fine for a desk tool, not great as a service.

## Architecture

- **Language**: Python 3.11+.
- **GUI**: PyQt6 (no PyQt5 fallback — the code uses Qt6 enum paths
  everywhere).
- **Audio**: PipeWire via `pactl` and `pw-dump`. No direct libpipewire
  binding.
- **Process model**: single desktop process. FX chains are spawned as
  separate `pipewire -c <conf>` clients with `core.daemon = false` so
  they can't take over the session, and their stderr is captured to
  `~/.config/wavelinux/fx-logs/`.
- **Config**: `~/.config/wavelinux/config.json`.
- **Log**: `~/.config/wavelinux/wavelinux.log`.
