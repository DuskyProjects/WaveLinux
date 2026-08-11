# WaveLinux 6 Architecture

This document describes the current WaveLinux 6 implementation. It is a
maintenance map, not a statement that every item in the long-term WaveLinux 6
design is complete. The final section records the remaining architectural work.

See [setup.md](setup.md), [testing.md](testing.md), and
[audio-core.md](audio-core.md) for operational detail. Profile trust,
acceleration, integrations, migration, and release boundaries have dedicated
runbooks linked from the project README.

## Repository Map

| Path | Responsibility |
| --- | --- |
| `src` | React mixer UI, state store, themes, and Tauri IPC client. |
| `crates/app` | Tauri process, command boundary, tray, and backend event pump. |
| `crates/model` | Schema 14 config, runtime models, defaults, and migrations. |
| `crates/pw` | PipeWire/Pulse snapshots, graph planning, and command rendering. |
| `crates/engine` | Runtime reconciliation, routing, devices, effects, Health, and logs. |
| `crates/dsp` | Native DSP library and `wavelinux6-audio-core`. |
| `profiles/v1` | Signed hardware-profile schema, index, and local seeds. |
| `scripts` | Builds, local install, dependency checks, packaging, and smoke tests. |

## Identity And State

WaveLinux 6 uses these identities exclusively:

```text
product:       WaveLinux6
display name:  WaveLinux 6
binary:        wavelinux6
audio core:    wavelinux6-audio-core
identifier:    io.github.duskyprojects.WaveLinux6
config:        ~/.config/wavelinux6/config.json
data:          ~/.local/share/wavelinux6
runtime:       $XDG_RUNTIME_DIR/wavelinux6
```

Config schema 14 is normalized on load. The first WaveLinux 6 launch can import
a WaveLinux5 config transactionally, rewrite owned node names, remove transient
route ids, validate the WaveLinux 6 graph, and then remove WaveLinux5 artifacts.
Legacy DeepFilterNet entries migrate to RNNoise; DeepFilterNet is not a runtime
effect.

## Runtime Owners

`WaveLinuxEngine` owns saved `MixerConfig` and live `RuntimeCache`. Its main
coordination boundaries are:

- `audio_commands`: serializes PipeWire/Pulse graph mutations.
- `runtime_refresh`: prevents overlapping expensive host snapshots.
- `deferred_graph_repair`: coalesces repeated graph repair requests.
- `deferred_effect_sync`: implements per-channel latest-wins effect updates.
- route failure maps: apply bounded backoff to stale stream ids.

Background work must not make UI reads wait on a graph mutation. State callers
receive the last coherent snapshot when a refresh is already active. Deferred
mutations requeue when `audio_commands` is busy.

Normal stream and device routing is event-driven through one persistent
PipeWire registry monitor. The immutable registry cache tracks nodes, devices,
ports, links, clients, defaults, and metadata generations. A 120-second
watchdog performs a cache-backed recovery audit if an event was lost; it is not
the old two-second polling loop.

Read-only refreshes project one registry generation into a coherent
`AudioStateSnapshot`. Graph, device, route, level, and active-output decisions
reuse that result. Native streams move through WirePlumber `target.object`
metadata; `pactl` is retained for clients whose `client.api` is
`pipewire-pulse`. A healthy refresh does not issue a second host snapshot.

The binary meter socket is independent of control traffic. Health also reads
direct error deltas from one persistent PipeWire profiler subscription;
journal monitoring remains supplemental context rather than the source of
truth for adaptive latency. The predictive pressure signal combines aggregate
CPU busy time, Linux CPU PSI scheduler stalls, and normalized one-minute load.
This lets the controller increase its buffer before an xrun when runnable or
I/O-blocked work is backing up even though average CPU utilization is below the
busy-time threshold.

## Current Audio Graph

Startup uses one persistent native PipeWire graph. Pulse compatibility commands
remain only for moving third-party Pulse streams and managing app-facing
defaults:

1. `wavelinux6-audio-core` creates every channel sink and processed source on
   one PipeWire connection and main loop.
2. Each channel capture callback publishes into a fixed-capacity raw queue. A
   non-real-time channel worker runs DSP into the processed history while the
   stable source renders from the current latency tap.
3. Native Monitor and Stream mix sources read those histories directly and
   apply smoothed atomic bus and master gains. No channel-to-mix loopback is
   created.
4. The hardware-input capture stream targets the selected physical microphone
   directly without changing the public `wavelinux6-mic` source.
5. The native Monitor playback stream targets the selected physical output
   directly and can retarget without replacing the public Monitor source.
6. Third-party Pulse streams are moved to the native channel sinks as they
   appear. The native node set is present before those streams become active.

The WaveLinux 6 graph does not use Pulse null sinks, remap sources, per-bus
loopbacks, or physical-endpoint `module-loopback` bridges.

Important public nodes include:

```text
wavelinux6-mic
wavelinux6_mix_monitor_source
wavelinux6_mix_stream_source
wavelinux6_channel_hardware_in
wavelinux6_channel_music
wavelinux6_channel_game
wavelinux6_channel_chat
wavelinux6_channel_browser
wavelinux6_channel_system
```

Owned modules and nodes carry `wavelinux6.*` properties. Cleanup and repair must
never match an unowned node merely because its display label is similar.

## Audio Core

The core has one process, one PipeWire client/main loop, and independent stream
pairs per logical channel. Each channel retains its own capture stream, stable
playback source, fixed-capacity raw queue and stereo history, non-real-time DSP
worker, preallocated scratch, chain state, and control socket. Sharing the
PipeWire connection removes redundant client and event-loop threads without
coupling channel DSP state. Empty channels use a bulk-zero playback path and do
not run DSP.

The real-time callback must not allocate, lock, run effects, start subprocesses,
access the filesystem, or log. It only converts and publishes input frames.
Prepared chains arrive through atomic pointer exchange to the DSP worker.
Topology and input-mode changes process old and new chains together for a 20 ms
equal-power crossfade; the public source remains present.

Filter state is flushed below `1e-20` at block boundaries. This is required:
subnormal floating-point state previously raised an idle microphone chain from
roughly 2-3% to nearly 50% of one CPU core.

Per-channel Unix control sockets support diagnostics, target-latency changes,
and chain swaps. They are blocking event-driven listeners with bounded client
I/O timeouts, so idle control threads do not poll.

See [audio-core.md](audio-core.md) for protocol and DSP details.

## Effects

Native effects are RNNoise, high-pass, eight-band EQ, compressor, gate,
limiter, and Karaoke Stage. RNNoise uses one state for mono microphones and
duplicates the processed result to the stereo public source. Standard effects
except EQ and Karaoke expose exactly one user-facing Strength control. Existing
advanced parameter values remain schema-compatible and are normalized when the
Strength control is changed.

Parameter edits update saved config and schedule a debounced channel sync.
The core prepares the replacement chain off the callback, replaces pending
work with the newest revision, and crossfades when ready. A burst of edits must
not unload modules or remove a public source.

## Devices And Routing

Automatic device ranking is:

1. USB input/output
2. Bluetooth headphones
3. internal headphone jack
4. internal headset microphone
5. internal microphone
6. internal speakers

An HDA jack whose active port is explicitly `not available` is unroutable.
`availability unknown` remains eligible for USB and platform devices that do
not report jack state. A selected unavailable microphone falls back temporarily
and is restored only after 750 ms of stable availability.

Playback stream events are settled briefly and coalesced before routing. The
engine uses Pulse compatibility commands only to move third-party Pulse
streams; it preserves stream volume across failed moves.

## Adaptive Latency

Core channels support live targets of 28, 40, 60, 80, 100, and 120 ms. A target
command changes the core buffer tap and PipeWire rate correction without
changing node ids or route revisions. A 20 ms dual-tap crossfade covers target
changes and overwrite recovery.

The controller raises latency from underrun evidence or sustained pressure and
recovers with 30-second clean and 15-second step hysteresis. Health reports
target, fill, rate correction, process time, dropped frames, and underruns.

Adaptive changes happen inside the persistent channel and mix taps. Changing a
target does not replace the direct native physical streams, unload a module, or
change graph topology.

## Frontend State Delivery

The UI bootstraps once from the backend and then consumes:

- `wavelinux://state-delta` for config and runtime revisions.
- `wavelinux://meters` for visibility-aware meter updates.
- `wavelinux://operation` for versioned mutation success/failure acknowledgements.

High-frequency mixer, routing, stream, settings, and effect mutations include a
frontend-generated request id. Their command response and operation event carry
the operation protocol version plus monotonic operation, state, config, and
graph revisions. The frontend keeps its optimistic update on success and uses
the existing authoritative refresh path to revert a rejected mutation.

`src/state.ts` exposes selector-based `useSyncExternalStore` hooks. Meter bars
interpolate attack and release at display refresh rate and update compositor
transforms directly instead of rerendering the full mixer. WaveLinux 6 channel
and mix callbacks publish peak/RMS snapshots through atomics already owned by
the persistent audio core. A non-real-time core thread streams all logical slots
at 30 Hz over meter protocol v1 while the mixer is visible, so displaying it
does not create PipeWire recorder clients or repeatedly open and parse JSON
control requests. Mix snapshots are exact while a mix source is consumed and
are estimated from channel values plus current bus/master gains while idle.

The older shared PipeWire reader remains a compatibility fallback for legacy
graph namespaces. Its callback publishes RMS samples through atomics and does
not take a mutex or allocate. Browser-only demo data is loaded through a
development-build dynamic import and is absent from production bundles.

Tauri's Tokio runtime defaults to four worker threads because audio processing,
core control, and engine reconciliation use dedicated threads. This avoids a
CPU-count-sized idle executor pool on high-core-count systems. Advanced users
can override the default with `TOKIO_WORKER_THREADS` before launch.

## Logs And Health

```text
~/.config/wavelinux6/wavelinux-engine.log
~/.config/wavelinux6/wavelinux6-audio-core.log
~/.config/wavelinux6/wavelinux6-chain-<channel>.log
```

Logs rotate at bounded sizes. Startup identity logs include app version,
AppImage path, config/data paths, and installed-version comparison. Relevant
areas are `repair.*`, `runtime.refresh`, `route.streams`, `hotplug.*`,
`effects.sync`, `meters.supervisor`, and `default.input`.

The Health report should be the first debugging artifact. It includes refresh
phase timing, recent PipeWire warnings, route repairs, core process latency,
adaptive buffering, and archived effect failures with timestamps.

## Packaging

AppImages bundle the UI stack, `wavelinux6-audio-core`,
`wavelinux6-peripheral-plugin`, RNNoise, and standard effects. They deliberately
exclude PipeWire client libraries, GStreamer PipeWire plugins, partial
SPA/PipeWire module trees, and Wayland client libraries. Those must match the
host PipeWire daemon and Mesa/EGL driver stack.

`scripts/build-local.sh` stages runtime assets, runs Tauri, sanitizes the AppDir,
and rebuilds the final AppImage. Distributable artifacts are built through the
pinned `scripts/build-portable.sh` container path and must pass embedded-binary
glibc and package-content probes before promotion. `scripts/install-local.sh` stops only known
WaveLinux5/6 processes, unloads owned modules, installs WaveLinux 6 into user
XDG paths, and removes the replaced WaveLinux5 installation.

## Remaining Architecture Work

These WaveLinux 6 plan items are not complete and must not be described as
shipping features:

- Replace the current persistent `pw-dump` registry adapter with an in-process
  PipeWire registry binding after its cross-distro behavior is covered by the
  isolated integration suite. Reconciliation already uses the immutable cache,
  and native stream moves no longer depend on Pulse compatibility ids.
- Continue splitting the large engine and `App.tsx`; the external state store,
  vertical EQ, and Health report are already isolated, while routing, devices,
  effects, settings, and mixer views still share the root module.
- Expand frontend Vitest/React Testing Library and Playwright interaction,
  screenshot, focus, and broader mobile coverage beyond the active scaling and
  FX-scroll regression suite.
- Qualify isolated CUDA, OpenVINO, and AMD provider packs on representative
  hardware. Qualified RNNoise neural stages can run on channel workers with
  exact CPU fallback; ordinary filters, dynamics, delays, and mixing remain CPU.
- Complete the final isolated 60-minute audio discontinuity stress gate for the
  exact stable release artifact; continuity fixtures must never be injected
  into a desktop graph with a physical monitor target.

## Change Rules

- Preserve stable public node names during parameter and effect changes.
- Keep allocations, locks, logging, subprocesses, and file I/O out of RT code.
- Keep every graph mutation under `audio_commands` and coalesce redundant work.
- Treat explicit unavailable HDA ports as unroutable without rejecting unknown
  USB availability.
- Preserve host-bound PipeWire libraries in every AppImage.
- Add regression tests for any migration, routing, retry, or DSP behavior.
- Do not publish a release until local audio, safe tests, package checks, and
  distro smoke gates pass.
