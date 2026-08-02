# WaveLinux 6 Native Audio Core

`wavelinux6-audio-core` owns WaveLinux 6 channel processing. It is a persistent
process so changing an effect or latency target does not remove the source that
Discord, OBS, Audacity, or a browser has already opened.

## Process And Nodes

The engine writes one manifest to:

```text
~/.local/share/wavelinux6/effects/wavelinux6-audio-core.json
```

The manifest contains one `DspChannelConfig` per channel. The engine starts:

```bash
wavelinux6-audio-core --run-core --manifest <manifest-path>
```

The core creates all channel and mix streams on one PipeWire connection and
main loop. A single process therefore owns the complete native graph and exits
as one unit if that graph fails.

Hardware input captures `wavelinux6_channel_hardware_in` and exposes the public
stereo source `wavelinux6-mic`. Other channels expose
`wavelinux6_fx_<channel>_source`. The same process exposes
`wavelinux6_mix_monitor_source` and `wavelinux6_mix_stream_source`; each mix
reads channel histories directly, so there are no per-bus Pulse loopbacks.

## Real-Time Data Path

Each channel preallocates:

- a raw capture queue and input/shadow DSP scratch buffers;
- a fixed-capacity stereo history ring;
- active and pending DSP chains;
- RNNoise frame buffers and state;
- adaptive-latency transition state.

The PipeWire capture callback only decodes host samples and publishes them to a
fixed-capacity atomic queue. A named, non-real-time worker per channel drains
that queue in bounded blocks, processes the active chain, updates meters, and
publishes frames to the processed history. Playback reads a latency-adjusted
tap. During a chain swap or latency transition, both taps/chains are mixed with
a 20 ms equal-power crossfade.

Raw and processed frame sequences stay aligned. If a worker is delayed beyond
the complete raw queue capacity, it advances the processed sequence across the
missing region and reports the exact overrun count; it never replays stale
audio or silently shifts route-transition boundaries.

Real-time code must not allocate, lock a mutex, open files, call subprocesses,
or log. New control work is prepared off the callback and published atomically.

At block boundaries, recursive filter state below `1e-20` is reset to zero.
Do not remove this as an insignificant numeric cleanup: subnormal values caused
near-silent EQ/high-pass processing to consume almost half a CPU core.

## Native Effects

| Effect id | Implementation |
| --- | --- |
| `rnnoise` | Native feature/synthesis path with an explicit-state WaveLinux CPU neural stage derived from pinned `nnnoiseless` weights. |
| `highpass` | Stateful first-order high-pass. |
| `eq` | Eight peaking biquads at 63, 125, 250, 500, 1k, 2k, 4k, and 8k. |
| `compressor` | Peak detector, linear-domain transfer curve, attack/release smoothing. |
| `gate` | Linear threshold, hold, range, and attack/release smoothing. |
| `limiter` | Input gain and hard output ceiling. |
| `karaoke_stage` | Tone, delay/doubling, detune, and room processing. |

Mono microphones instantiate one RNNoise state. The processed mono signal is
copied to both public channels for client compatibility. Stereo channels use
two states.

DeepFilterNet is not implemented or installed. Legacy `deepfilternet` config
entries migrate to RNNoise before a chain is rendered.

## RNNoise Strength

The normal UI exposes one Strength slider. At 0-100%, it maps to:

```text
VAD threshold:        25 -> 95
hold:                 250 -> 75 ms
minimum voice level:  -65 -> -28 dB
dry mix:              0.12 -> 0.00
```

The minimum-level condition complements RNNoise speech probability: speech from
a television across the room may classify as speech, but an aggressive setting
also requires near-field energy. Advanced controls remain available for unusual
microphone gain or distance.

## Chain Updates

Effect edits write a new channel config and send `swap_chain` over that
channel's Unix socket. The control thread parses and constructs the new chain
before publishing it. If several revisions arrive before the callback accepts
one, the newest pending chain replaces the older pending chain.

The DSP worker acknowledges the generation after its crossfade. An input-mode
change is part of the prepared chain, so old and new channel mappings crossfade
with their corresponding effect state. Health exposes the acknowledged
generation, swaps, replacements, worker state, queue fill/capacity, and exact
overrun count. A parameter storm must not start another core process or unload
a Pulse module.

## Control Protocol

Protocol version is `3`. Each command is one JSON document followed by client
write shutdown; each response is one JSON line. Sockets have user-only `0600`
permissions and live under:

```text
$XDG_RUNTIME_DIR/wavelinux6/control/
```

When `XDG_RUNTIME_DIR` is unavailable, WaveLinux uses the user-specific
`/tmp/wavelinux-<uid>/wavelinux6/control` fallback. The containing directories
are mode `0700`. Generated manifests and channel configs remain persistent
under `~/.local/share/wavelinux6/effects`; socket files never do.

Get channel diagnostics:

```json
{
  "protocol_version": 3,
  "command": "get_diagnostics",
  "request_id": "uuid",
  "route_id": "hardware_in"
}
```

Change latency without changing topology:

```json
{
  "protocol_version": 3,
  "command": "set_target_latency",
  "request_id": "uuid",
  "route_id": "hardware_in",
  "target_msec": 60,
  "reason": "underrun"
}
```

Prepare a new chain:

```json
{
  "protocol_version": 3,
  "command": "swap_chain",
  "request_id": "uuid",
  "route_id": "hardware_in",
  "config_path": "/path/to/channel.json",
  "config_revision": "revision"
}
```

Mix bus and master changes use the core-wide socket and update only atomics:

```json
{
  "protocol_version": 3,
  "command": "set_mix_bus",
  "request_id": "uuid",
  "mix_id": "stream",
  "channel_id": "browser",
  "volume": 1.0,
  "muted": false,
  "enabled": true
}
```

```json
{
  "protocol_version": 3,
  "command": "set_mix_master",
  "request_id": "uuid",
  "mix_id": "stream",
  "volume": 1.0,
  "muted": false
}
```

Retarget a hardware input without replacing the core or any public node:

```json
{
  "protocol_version": 3,
  "command": "set_input_target",
  "request_id": "uuid",
  "route_generation": 42,
  "target_node_name": "alsa_input.usb-example.analog-mono"
}
```

Retarget an output mix. Up to four unique targets are accepted. The new hidden
endpoint is primed before a 20 ms equal-power transition retires the old one:

```json
{
  "protocol_version": 3,
  "command": "set_output_targets",
  "request_id": "uuid",
  "route_generation": 43,
  "target_node_names": ["alsa_output.usb-example.analog-stereo"]
}
```

Route generations are monotonic and latest-wins. Diagnostics expose submitted
and applied generations, current targets, and the last target error. Hardware
target names are deliberately excluded from the core topology revision, so a
device selection, fallback, restoration, or default-device change cannot
restart the core or replace public node ids.

Listeners block in `accept` and use one-second client read/write timeouts. This
keeps idle control threads asleep while still bounding malformed clients.

## Meter Stream Protocol

Meter protocol version `1` uses a separate user-only socket:

```text
$XDG_RUNTIME_DIR/wavelinux6/control/wavelinux6-meters.sock
```

On connection, the core sends one fixed-layout header and a stable descriptor
for every channel and mix slot. It then sends fixed-size little-endian frames at
30 Hz. Each frame contains a sequence number, a monotonic timestamp, and stereo
peak/RMS floats for every negotiated slot. The publisher runs on a non-real-time
thread and only reads callback-owned atomics.

The app holds one connection while the mixer window is visible and closes it
when hidden. It retries with bounded backoff and uses the JSON `get_meters`
control command at 2 Hz only when protocol v1 is unavailable. Health exposes
the negotiated protocol, connection state, slots, sequence, received frames,
disconnects, fallback polls, and errors.

## Adaptive Latency

Available targets are 28, 40, 60, 80, 100, and 120 ms. A target change updates
an atomic target, begins a dual-tap transition, and uses bounded PipeWire rate
matching to settle the history fill. Node names, ids, and route revisions stay
unchanged.

The reported fields include target, current fill, rate correction, captured and
rendered frames, dropped and underrun frames, worker queue state, process time, invalid DSP samples,
effect masks, automatic recoveries, chain replacement pressure, and last reason.

## Diagnostics And Benchmarks

Core logs are written by the engine's process supervisor to:

```text
~/.config/wavelinux6/wavelinux6-audio-core.log
```

Healthy `native_stats` lines have zero dropped/underrun frames, bounded buffer
fill near the target, and process time well below the PipeWire quantum budget.

Run the offline fixture benchmark with:

```bash
bash scripts/bench-audio-runtime.sh
```

The benchmark is useful for DSP regressions but does not replace a live stress
test. Release acceptance must also inspect core stats and PipeWire warnings
under microphone, game, disk, CPU, and network load.

## Provider Status

`WAVELINUX_AUDIO_RUNTIME` and `WAVELINUX_DSP_PROVIDER` remain development
overrides. The launcher defaults to `dsp_auto`; CUDA/OpenVINO/MIGraphX is still
ineligible unless a complete machine-local qualification record matches the
installed pack and model. Eligible RNNoise neural blocks run from the existing
non-real-time channel worker through provider protocol v1. PipeWire callbacks
only exchange preallocated audio history frames and never wait for a provider.

Every provider request carries the last committed recurrent state and a block
deadline. The worker commits a valid on-time result. Launch failure, process
exit, queue pressure, timeout, stale output, or invalid numerical output runs
the native neural stage from that exact state and leaves all public nodes and
route revisions unchanged. `get_diagnostics` exposes live provider PIDs,
blocks, fallbacks, deadline/validation failures, disabled states, and startup
errors for each channel.
