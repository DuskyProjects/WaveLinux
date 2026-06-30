# WaveLinux Architecture Notes

This document is the maintenance map for WaveLinux. It explains how the
desktop shell, Rust engine, PipeWire graph planning, effect chains, logs, and
packaging scripts fit together so future changes can preserve the behavior that
keeps the app responsive.

For setup commands, see [setup.md](setup.md). For test commands, see
[testing.md](testing.md).

## Repository Map

- `src`: React and TypeScript UI.
- `crates/app`: Tauri shell and IPC boundary.
- `crates/dsp`: WaveLinux5 experimental DSP helper, provider probing, CPU DSP
  nodes, and fixture benchmarks.
- `crates/engine`: runtime state, diagnostics, hardware profiles, graph repair,
  app routing, effect process supervision, and log maintenance.
- `crates/pw`: PipeWire/PulseAudio snapshot parsing, command planning, and
  filter-chain config rendering.
- `crates/model`: shared model types, defaults, and migrations.
- `profiles/v1`: hardware profile schema, device seeds, and authoring docs.
- `scripts`: local install, build, release, dependency, and packaging helpers.
- `docs`: developer-facing setup, testing, theme, and architecture notes.

## Runtime Ownership

`WaveLinuxEngine` is the main owner of mutable runtime state. It keeps the saved
`MixerConfig` and live `RuntimeCache` behind `RwLock`s and coordinates PipeWire
mutations with a small set of mutexes:

- `runtime_refresh`: serializes full snapshots and runtime-cache refreshes.
- `audio_commands`: serializes live graph mutations such as repair, stream
  moves, route creation, cleanup, volume/mute writes, and effect-route sync.
- `deferred_graph_repair`: debounces config-triggered repair requests.
- `deferred_effect_sync`: debounces per-channel effect changes and merges channel
  ids that change close together.
- `capture_move_failures`: remembers failed capture-stream moves so repeated
  default-input repairs do not hammer PipeWire with the same failing command.

The important rule is that state reads must stay responsive while graph
mutations are running. UI state refreshes use cached state when another refresh
is already in progress. Deferred background jobs that need to mutate the graph
try to take `audio_commands`; if it is busy, they log the condition and requeue
instead of blocking.

## Graph Lifecycle

The engine builds a graph view by asking `PwClient` for PipeWire, PulseAudio,
and effect availability snapshots. The raw saved config is then adjusted in
layers before graph commands are planned:

1. Hardware profiles apply safer defaults for known devices.
2. Unhealthy effect chains are temporarily bypassed for routing decisions.
3. Effects that are unavailable in the live graph are bypassed in the effective
   audio config.
4. PipeWire commands are planned from the effective config and the live graph.

Startup and repair use the same basic shape:

1. Snapshot the live graph.
2. Restore Bluetooth A2DP profile when needed.
3. Prune stale WaveLinux-owned nodes.
4. Start or restart effect filter-chain helpers.
5. Create virtual channel, mix, monitor, and route modules.
6. Apply saved levels, mutes, default input, and app routes.
7. Refresh runtime state and meter targets from the live graph.

`audio_commands` must wrap any live PipeWire mutation that can overlap another
mutation. Direct user actions that already happen on command paths may wait for
the lock with a timeout. Deferred repair and effect sync use the non-blocking
path and requeue when the graph is busy.

## Effect Chains

Effect chains are rendered as PipeWire filter-chain configs under:

```text
~/.local/share/wavelinux/effects/wavelinux-chain-<channel>.conf
```

Each active chain is started with:

```text
pipewire -c <generated-chain-config>
```

The effect process writes stdout and stderr to:

```text
~/.config/wavelinux/wavelinux-chain-<channel>.log
```

The engine tracks helper child processes when it starts them. It also scans for
stale helper commands during repair so it can clean up helpers from previous
runs or older app versions.

Effect changes follow this flow:

1. The config update schedules `schedule_effect_graph_sync_many`.
2. Effect config files are rebuilt after a short debounce.
3. If the WaveLinux graph is running, the sync tries to take `audio_commands`.
4. If another graph mutation is running, the sync logs a requeue and schedules
   itself again.
5. Once the lock is available, stale helpers are killed, old endpoints are given
   a bounded wait to disappear, fresh helpers are started, and fresh endpoints
   are given a bounded wait to appear.
6. Routes for the affected channels are rebuilt. If the effect helper fails to
   start or endpoints do not appear, the channel is routed through its raw
   monitor for that sync.

This non-blocking requeue behavior is important. An effect edit can happen while
automatic graph repair is already fixing a hotplug/default-device event. The
effect sync must not wait indefinitely behind that repair thread, and repair must
not wait indefinitely behind effect sync.

The logs that show a healthy busy-graph effect change look like this:

```text
[effects.sync] effect chain changed; syncing affected channels: music
[effects.sync] graph mutation already in progress; deferring effect route sync
[effects.sync] effect route sync requeued; graph mutation is still running
```

After the current mutation finishes, the requeued sync should run and either log
the planned commands or log an explicit effect startup/route error. If those
requeue lines are missing on a build that freezes after applying EQ or another
effect, check the effect-sync locking path first.

## Stream Routing And Backoff

App playback streams and capture streams can appear, disappear, and reuse ids
while PipeWire is changing. The engine avoids repeatedly moving the same failing
capture stream by remembering a failure signature made from the source-output id
and route details. A later command with the same id but a different signature is
treated as new work and is allowed through.

Capture move backoff grows exponentially and is capped at 30 minutes. A
successful move clears the remembered failure for that source-output id.

## Logs And Diagnostics

The primary engine log is:

```text
~/.config/wavelinux/wavelinux-engine.log
```

Effect helper logs are:

```text
~/.config/wavelinux/wavelinux-chain-<channel>.log
```

Logs rotate when they exceed 2 MiB. The engine keeps four rotated files per log
and rotates current logs when the app version changes. Settings > Health >
Testing Health Report includes the engine log path, a compact runtime summary,
diagnostics, and recent debug-log lines.

Useful log areas:

| Area | Meaning |
| --- | --- |
| `runtime.refresh` | Full live graph snapshot and runtime-cache refresh. |
| `hotplug.device` | Device/default-source changes noticed by polling. |
| `repair.auto` | Debounced automatic repair after config or device changes. |
| `repair.*` | Startup or explicit graph repair phases. |
| `effects.sync` | Effect-chain config writes, helper restart, and route rebuild. |
| `effects.process` | Effect helper process lifecycle and health warnings. |
| `default.input` | Controlled microphone/default-input capture moves. |
| `route.streams` | App playback route moves. |
| `meters.supervisor` | Meter target refresh and helper stream supervision. |
| `audio.lock` | Timed waits for the graph mutation lock. |

For a freeze report, collect the last 100 engine lines and any matching effect
helper log:

```sh
tail -n 100 ~/.config/wavelinux/wavelinux-engine.log
ls ~/.config/wavelinux/wavelinux-chain-*.log
tail -n 100 ~/.config/wavelinux/wavelinux-chain-music.log
```

WaveLinux5 uses separate paths and node ownership:

```text
~/.config/wavelinux5/wavelinux-engine.log
~/.config/wavelinux5/wavelinux5-chain-<channel>.log
~/.local/share/wavelinux5/effects/wavelinux5-chain-<channel>.conf
```

Its graph nodes use the `wavelinux5_*` namespace and dynamic `wavelinux5.*`
PipeWire properties. Stable WaveLinux keeps using `wavelinux_*` and
`wavelinux.*`. See [wavelinux5-hardware-acceleration.md](wavelinux5-hardware-acceleration.md)
for the test-line runtime modes and benchmark gate.

## Startup Audio Preflight

Before the Tauri UI opens, AppImage launches verify that `pactl info` can reach
the host PipeWire/PulseAudio server. Installed packages alone are not enough for
WaveLinux to build virtual sinks; `pipewire-pulse` must be running in the user
session.

If `pactl info` fails, the app tries `systemctl --user start` for
`pipewire.socket`, `pipewire-pulse.socket`, `pipewire.service`,
`pipewire-pulse.service`, and `wireplumber.service`. On non-systemd sessions it
falls back to starting `pipewire`, `pipewire-pulse`, and `wireplumber`
directly. If the probe still fails, startup stops with an explicit setup error.

Disable this recovery only for packaging tests or unusual host supervision with
`WAVELINUX_SKIP_AUDIO_SERVICE_START=1`.

## AppImage Packaging

WaveLinux AppImages bundle the desktop stack needed by Tauri/WebKitGTK and
GStreamer, but they intentionally do not bundle PipeWire client libraries,
PipeWire GStreamer plugins, or partial `pipewire-0.3`/`spa-0.2` module trees.
Meters and routing use the host PipeWire stack; mixing bundled client libraries
with host modules can prevent live streams from appearing.

The local build path is:

```text
yarn desktop:build
  -> scripts/build-local.sh
     -> scripts/stage-appimage-runtime.sh
     -> tauri build
     -> scripts/rebuild-appimage-with-host-strip.sh, only if Tauri bundling fails
     -> scripts/finalize-appimage.sh
        -> scripts/sanitize-appimage-pipewire.sh --sanitize
        -> linuxdeploy AppImage plugin rebuild
        -> scripts/sanitize-appimage-pipewire.sh --check
```

`scripts/rebuild-appimage-with-host-strip.sh` exists because Tauri's cached
`linuxdeploy-x86_64.AppImage` can contain an older `strip` binary that fails on
newer ELF sections such as `.relr.dyn`. The fallback extracts linuxdeploy,
replaces its embedded `strip` with the host `strip`, symlinks the cached GTK and
GStreamer plugins into a temporary plugin directory, and reruns linuxdeploy
against the existing AppDir.

`scripts/finalize-appimage.sh --updater` also recreates the `.AppImage.tar.gz`
updater archive and signs refreshed artifacts when the Tauri signing environment
is present.

In environments without FUSE, run AppImage tooling with:

```sh
APPIMAGE_EXTRACT_AND_RUN=1 yarn desktop:build
```

## Change Checklist

Before changing graph mutation, routing, effects, or packaging behavior:

- Keep live PipeWire mutations under `audio_commands`.
- Use the non-blocking try-lock and requeue pattern for deferred/background
  graph work.
- Do not make UI state reads wait behind long graph mutations or refreshes.
- Preserve the effect fallback that routes a channel raw when FX endpoints do
  not appear.
- Preserve the AppImage rule that PipeWire client artifacts stay host-bound.
- Add focused engine tests for new lock, debounce, retry, or backoff behavior.
- Run at least `cargo test -p wavelinux-engine` for engine changes.
- Run `bash -n` over changed shell scripts for packaging changes.
- Run `yarn desktop:build` or `bash scripts/build-local.sh` before shipping
  AppImage packaging changes.
