# WaveLinux — Roadmap & Status

A PyQt6 mixer for PipeWire that mirrors Elgato Wave Link's
day-to-day UX on Linux: per-channel Headphones (MON) and Stream
(STR) sends, a master Headphones output, a dedicated Stream virtual
device for OBS, and a per-mic Limiter (Wave Link's "Clipguard").

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
        WaveLinux-Stream"*, master fader. (The earlier 🛡 master-bus
        Clipguard button has been moved to the per-mic Limiter
        effect.)
- [x] **Settings dialog** (header ⚙) hosts App Routing and the list
      of hidden channels with a one-click unhide.
- [x] **Tray integration** with graceful fallback to quit-on-close
      when no system tray is available. Tray menu also exposes Sound
      Card Profiles and Start-at-login.
- [x] **Scenes / Solo / header Reset are gone** — they weren't in
      Wave Link and added noise. Solo was replaced by Link; Reset
      now lives in Settings → Advanced where it's out of the way.
- [x] **Settings dialog is tabbed** — Apps / Hidden / Advanced.
      Advanced holds app-prune cutoff, autostart toggle, card
      profiles, a LADSPA plugin diagnostic count, a "Forget all
      offline apps now" button, and Emergency Reset.
- [x] **Inputs row horizontally scrolls** when the window is
      narrower than the strips need. The strips stop being squashed
      below fullscreen; a horizontal scrollbar appears as needed.
- [x] **Peak meter at ~40 Hz** (was 20 Hz) with 25 ms parec latency
      for more responsive live-audio feedback.

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
- [x] **Clipguard / Limiter** per microphone. Wave Link's master-bus
      Clipguard button is gone; the equivalent now lives as the
      `Limiter` effect inside the channel's unified FX chain so it only
      affects the active mic, not every source mixed into Stream.
      Uses the `fast_lookahead_limiter_1913` LADSPA plugin when
      available, otherwise falls back to PipeWire's builtin
      `linear` + `clamp` chain so the limiter still works on a stock
      PipeWire install.
- [x] **FX bus per channel** — one unified `pipewire -c` filter-chain
      process per channel. Every enabled effect lives as a node in a
      single `filter.graph` block with explicit inter-stage `links`,
      so RNNoise → High-Pass → EQ → Compressor → Gate → Limiter is
      one process and one virtual sink/source pair, no matter how
      many effects are stacked. The capture side binds to the
      mic via a `module-loopback`; the submix loopbacks pull from
      the chain's virtual `Audio/Source`. Architecture credit:
      EasyEffects (https://github.com/wwmm/easyeffects, GPL-3.0) for
      the unified-graph design; PipeWire's `module-filter-chain` for
      the implementation primitives.
- [x] **Snapshot-cached refresh**. Each refresh tick runs each heavy
      `pactl` / `pw-dump` invocation exactly once; helpers read from
      the cached text.
- [x] **Event-driven refresh** via `pactl subscribe` under a
      `QProcess`, 150 ms debounced. The 2 s poll is the backstop.
- [x] **Atomic config writes** (temp file + `os.replace`).
- [x] **LADSPA plugin probe** at startup; unavailable effects show
      "N/A" in the FX dialog with a tooltip naming the package.
- [x] **Built-in High-Pass Filter and 3-Band parametric EQ** —
      both implemented on PipeWire's `bq_highpass` / `bq_lowshelf` /
      `bq_peaking` / `bq_highshelf` so they work without any LADSPA
      plugin installed.
- [x] **Parameterised FX**. Threshold / ratio / attack / release /
      VAD / cutoff / EQ gain sliders for rnnoise, highpass, eq,
      compressor, gate, limiter. Params are per-(`node.name`,
      `effect_id`), persisted, and applied live.
- [x] **Effect persistence** — on/off state plus parameters are
      keyed by node.name, saved to config, and auto-reapplied when
      the channel reappears after a restart.
- [x] **FX dialog help + presets** — every effect has a plain-English
      description in the dialog and 2–3 preset buttons that snap
      every parameter to a sane starting point.
- [x] **Optional VST / VST3 / LV2 hosting via Carla**. The channel
      right-click menu grows a "🎹 Open VST plugin (Carla)…" entry
      when `carla` is on `$PATH`. WaveLinux itself doesn't host VST3
      — that needs a real plugin host and Carla is the stable Linux
      answer — so we bridge to it rather than reimplement it.
- [x] **ALSA card profile picker** (`pactl set-card-profile`) —
      reach it from the tray menu or Settings → Advanced.
- [x] **Broadened LADSPA probe** — `$LADSPA_PATH`, user paths
      (`~/.ladspa`, `~/.local/lib/ladspa`), and prefix-matching so a
      plugin named `fast_lookahead_limiter_1913.so` still answers to
      `fast_lookahead_limiter` in the requirements list. Clipguard
      no longer fails to enable on systems where the plugin is
      actually installed.
- [x] **Volume ceiling at 100%** everywhere. PipeWire allows up to
      150% but it sounds clipped; `PipeWireEngine.MAX_VOLUME` is
      clamped in every write path.

### App identification
- [x] **Flatpak / Snap / wrapper-aware app names** via
      `/proc/<pid>/environ`, `.flatpak-info`, cgroup scopes, and
      `ppid` walking past `bwrap` / `snap-confine` / `flatpak`.
- [x] **`.desktop` file discovery** — we index every
      `/usr/share/applications/*.desktop` (and the user / Flatpak
      mirrors) and resolve a sink-input's PID through
      `/proc/<pid>/exe` and `comm` to the matching `Name=` field.
      That's how native AUR Spotify (`/usr/bin/spotify`) now
      shows as "Spotify" instead of "audio-src".
- [x] **Install-path inference** — when a process's binary lives
      under `…/steamapps/common/<Title>/`, `…/drive_c/Program
      Files/<Title>/`, `/Games/<Title>/`, etc., the directory
      title is used as the friendly app name. Lets games like
      War Thunder (binary: `aces`) show their real title.
- [x] **Local-host filter** — sink-inputs whose visible name
      matches the system hostname (e.g. `DuskyPC`) are dropped
      from App Routing. PipeWire surfaces system-level streams
      under the host name when nothing better is available; we
      know we're not an app on our own mixer.
- [x] **Configured apps stay visible after closing** — the App
      Routing panel keeps a row for every app we've seen within
      the prune window, not just apps with a saved sink target.
      Closing Spotify no longer makes its row vanish.
- [x] **Mics default Monitor=muted on first launch** — a brand
      new mic is auto-muted in the Monitor (headphones) mix so
      a fresh install doesn't immediately scream the user's voice
      back at them through their speakers. External mute changes
      (pavucontrol, media keys) are also written back to config
      so they survive a restart.
- [x] **App-routing persistence** with per-row "(Offline)" markers,
      and a ✕ button that *forgets* an offline app so `app_routing`
      doesn't grow forever.
- [x] **Automatic stale-routing prune**. Each app has a last-seen
      epoch stamp that's refreshed every tick it's active; apps
      that haven't been seen in `app_prune_days` (default 14, set
      in Settings → Advanced) are dropped on startup. Quiet apps
      (Discord / Slack / Telegram) keep their slot as long as they
      still hold their PulseAudio client; long-abandoned entries
      go away.
- [x] **Curated app-id table** — Flatpak / .desktop app IDs like
      `com.spotify.client` now render as "Spotify" instead of
      the generic "audio-src" the Flatpak'd Spotify sets.

### Packaging
- [x] **`install.sh`** for Arch / CachyOS with pacman + paru/yay
      fallback for the AUR-only RNNoise plugin.
- [x] **AUR `PKGBUILD`** — installs to `/usr/share/wavelinux`, adds
      `/usr/bin/wavelinux`, registers desktop + icon entries.
- [x] **Autostart toggle** in the tray menu.

## Planned (not yet shipped)

- [ ] **Native VST host as an FX list entry** — currently the
      "Open VST plugin (Carla)…" right-click action launches Carla
      as a separate app, leaving the user to wire its inputs and
      outputs by hand. The intent is for VST to be a row in the
      effects dialog itself, just like High-Pass / EQ / Compressor,
      so it slots into the channel chain transparently. That
      requires hosting an actual VST runtime (LV2 may come along
      for the ride) and an in-app graph editor, and it's a real
      lift — deferred to a future session.
- [ ] **Peak meter on a worker thread** — `parec` output is parsed
      on the Qt event loop today, which means heavy UI work can
      delay meter updates. Moving each `MeterWorker` onto a
      `QThread` should remove the lag. Deferred (touches every
      strip's lifecycle and the shutdown path).

## Explicitly not planned

- **Stream Deck integration** — out of scope for a plain desktop app.
  <!-- Native VST hosting was previously listed here as out of scope.
       It's been moved to the "Planned (not yet shipped)" section: a
       VST row inside the FX dialog is a long-running goal, just one
       big enough to need its own session. -->

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
- **Process model**: single desktop process. Each channel with at
  least one effect spawns ONE `pipewire -c <conf>` client running a
  unified `module-filter-chain` whose `filter.graph` lists every
  enabled effect (rnnoise / highpass / EQ / compressor / gate /
  limiter) as a node with explicit `links` between them.
  `core.daemon = false` so the spawn can't take over the session;
  stderr goes to `~/.config/wavelinux/fx-logs/`.
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
