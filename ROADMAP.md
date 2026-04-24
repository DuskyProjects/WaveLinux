# WaveLinux — Roadmap & Status

A PyQt6 mixer for PipeWire that mirrors Elgato Wave Link's
day-to-day UX on Linux: per-channel Headphones (MON) and Stream
(STR) sends, a master Headphones output, a dedicated Stream virtual
device for OBS, and Clipguard.

## Done

### UI (simplified for Wave Link parity)
- [x] **Dark theme** (`wavelinux_theme.py`) — calmer palette, tighter
      strip dimensions (~140–160 px wide, ~340 px tall).
- [x] **Channel strip** is icon + name + peak meter + Link button +
      MON fader / 🎧 mute + STR fader / 📡 mute. **Everything else
      lives in the right-click menu** — Effects, Move Left / Right,
      Rename (virtual channels only), Remove, Hide. No visible
      reorder arrows, no "type" label, no always-on RNNoise badge,
      no "Add FX" or "❌ Remove" text buttons.
- [x] **Link button** toggles per-channel fader linkage; moving one
      fader while linked drives the other in lockstep.
- [x] **Peak meter** driven by a dedicated `parec` subprocess per
      visible strip at ~20 Hz, with a release envelope so it doesn't
      flicker.
- [x] **Tiny ✨ indicator** next to the icon lights up when an effect
      is active on the channel.
- [x] **Master panel** is minimal:
      - **🎧 Headphones** — physical output picker (ALSA / Bluetooth /
        JACK, anything WaveLinux didn't make) + master fader.
      - **📡 Stream** — permanently routed to the virtual
        `WaveLinux-Stream` recording device (matches Wave Link's
        fixed broadcast device), hint text reads *"OBS input:
        WaveLinux-Stream"*, master fader + 🛡 Clipguard button.
- [x] **Settings dialog** (header ⚙) hosts App Routing and the list
      of hidden channels with a one-click unhide.
- [x] **Tray integration** with graceful fallback to quit-on-close
      when no system tray is available. Tray menu also exposes Sound
      Card Profiles and Start-at-login.
- [x] **Scenes / Solo / Emergency Reset buttons are gone** — they
      weren't in Wave Link and added noise. Solo was replaced by
      Link; Reset lives as an engine call if needed.

### Core audio engine
- [x] **User virtual channels** (`wavelinux_<name>`) with a
      **whitespace-free** `device.description` so the KDE Audio
      Volume panel / pavucontrol / OBS show `WaveLinux-Game` instead
      of four identical "WaveLinux" entries. `pactl`'s
      `sink_properties=` parser splits on whitespace and some
      versions mis-quote strings with spaces — the engine now
      collapses spaces to hyphens in every `device.description`,
      `node.description`, `node.nick`, `media.name`, and
      `application.name` value it writes.
- [x] **🎧 Headphones and 📡 Stream master buses**. Each bus is a
      null-sink plus a `module-virtual-source` pointed at its
      monitor, so OBS sees `WaveLinux-Stream` as a dedicated
      recording device.
- [x] **Loopback routing** from inputs to buses is idempotent —
      reuses an existing loopback and only touches PipeWire when the
      live module has actually gone away.
- [x] **Stream bus locked** to `wavelinux_mix_stream`. You can't
      re-route it to hardware, matching Wave Link.
- [x] **Master-bus volume sliders** via `pactl set-sink-volume
      <name>` (by-name, not wpctl numeric id).
- [x] **Per-channel volume / mute state** is keyed by PipeWire
      `node.name`, which survives a PipeWire restart. An initial
      sync push runs on the first tick for each node so saved mutes
      aren't clobbered by live PipeWire state.
- [x] **Clipguard** — Wave Link-style limiter on the Stream bus.
      Button is disabled with an informative tooltip if the
      `fast_lookahead_limiter_1913` LADSPA plugin isn't installed.
- [x] **Snapshot-cached refresh**. Each refresh tick runs each heavy
      `pactl` / `pw-dump` invocation exactly once; helpers read from
      the cached text.
- [x] **Event-driven refresh** via `pactl subscribe` under a
      `QProcess`, 150 ms debounced. The 2 s poll is the backstop.
- [x] **Atomic config writes** (temp file + `os.replace`).
- [x] **LADSPA plugin probe** at startup; unavailable effects show
      "N/A" in the FX dialog with a tooltip naming the package.
- [x] **Built-in High-Pass Filter** (PipeWire `bq_highpass` — no
      LADSPA plugin needed).
- [x] **Parameterised FX**. Threshold / ratio / attack / release /
      VAD / cutoff sliders for rnnoise, highpass, compressor, gate,
      limiter. Params are per-(`node.name`, `effect_id`), persisted,
      and applied live.
- [x] **ALSA card profile picker** (`pactl set-card-profile`) —
      reach it from the tray menu.

### App identification
- [x] **Flatpak / Snap / wrapper-aware app names** via
      `/proc/<pid>/environ`, `.flatpak-info`, cgroup scopes, and
      `ppid` walking past `bwrap` / `snap-confine` / `flatpak`.
- [x] **App-routing persistence** with per-row "(Offline)" markers,
      and a ✕ button that *forgets* an offline app so `app_routing`
      doesn't grow forever.

### Packaging
- [x] **`install.sh`** for Arch / CachyOS with pacman + paru/yay
      fallback for the AUR-only RNNoise plugin.
- [x] **AUR `PKGBUILD`** — installs to `/usr/share/wavelinux`, adds
      `/usr/bin/wavelinux`, registers desktop + icon entries.
- [x] **Autostart toggle** in the tray menu.

## Explicitly not planned

- **Stream Deck integration** — out of scope for a plain desktop app.
- **VST3 hosting** — Wave Link runs proprietary Elgato VST3 plugins;
  WaveLinux stays on LADSPA via filter-chain.
- **Drag-and-drop reorder** — Move Left / Move Right in the
  right-click menu does the same job without layout fragility.
- **True global hotkeys** — Wayland doesn't expose a cross-compositor
  registration API. Use your DE's shortcut settings to bind
  `wavelinux` if you need out-of-focus control.
- **Ducking** — PipeWire's filter-chain doesn't give us a
  cross-stream sidechain primitive we trust. Revisit when it does.
- **Flatpak manifest** — WaveLinux manages its own PipeWire sinks
  and spawns `pipewire -c` clients; the Flatpak sandbox makes this
  awkward without a privileged portal we don't have. AUR is the
  path.
- **3-band parametric EQ on mic** — we ship the built-in high-pass
  and the FX chain is fully parameterised. A full EQ is nice but
  not planned for now.

## Architecture

- **Language**: Python 3.11+.
- **GUI**: PyQt6.
- **Audio**: PipeWire via `pactl`, `pw-dump`, `wpctl`, `parec`. No
  direct libpipewire binding.
- **Process model**: single desktop process. FX chains (rnnoise,
  highpass, compressor, gate, limiter, Clipguard) are spawned as
  separate `pipewire -c <conf>` clients with `core.daemon = false`
  so they can't take over the session. stderr goes to
  `~/.config/wavelinux/fx-logs/`.
- **Per-refresh snapshot** (`EngineSnapshot`): the UI fetches
  `pactl list modules / short modules / sink-inputs / sinks / short
  sinks` and `pw-dump` once per tick, helpers reuse the cached text.
- **Event subscriber**: `pactl subscribe` under a `QProcess` kicks a
  150 ms debounce on every event.
- **Peak meters**: one `parec --raw --format=s16le --channels=1`
  subprocess per visible strip.
- **LADSPA probe**: engine scans `$LADSPA_PATH` + common distro
  paths at startup; the UI greys out effects whose backing plugin
  isn't installed.
- **State keys**: all persisted per-channel state uses PipeWire
  `node.name` (e.g. `alsa_input.pci-...`, `wavelinux_game`), which
  survives PipeWire restarts.
- **Branding**: every `device.description` / `node.description` /
  `node.nick` / `media.name` / `application.name` WaveLinux writes
  is **whitespace-free** (`WaveLinux-Voice-Chat`, not `WaveLinux
  Voice Chat`) so every front-end that reads these properties shows
  the full label. Internal channel names (pactl `sink_name=`)
  remain underscore-style (`wavelinux_voice_chat`).
- **Config**: `~/.config/wavelinux/config.json` (atomic writes).
- **Log**: `~/.config/wavelinux/wavelinux.log`.
- **Per-effect logs**: `~/.config/wavelinux/fx-logs/`.
