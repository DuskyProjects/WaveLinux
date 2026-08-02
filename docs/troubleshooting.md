# WaveLinux 6 Troubleshooting

Start with Settings > Health and the current logs. Archived effect failures are
historical unless their timestamp matches the current session.

## Log Locations

```text
~/.config/wavelinux6/wavelinux-engine.log
~/.config/wavelinux6/wavelinux6-audio-core.log
```

Inspect only the current startup:

```bash
start=$(rg -n '\[engine.start\]' ~/.config/wavelinux6/wavelinux-engine.log | tail -n1 | cut -d: -f1)
sed -n "${start},\$p" ~/.config/wavelinux6/wavelinux-engine.log
```

Check the host services and public nodes:

```bash
pactl info
pactl list short sinks
pactl list short sources
wpctl status
```

## Stuck On Starting Audio Engine

1. Run `yarn deps:check` from the source tree or the AppImage
   `--check-runtime-dependencies` command.
2. Confirm `pactl info` connects to PipeWire Pulse.
3. Look for `repair.end`; `failed=0` is expected.
4. Confirm one `wavelinux6-audio-core --run-core` process exists.
5. Check the core log for all channel capture/playback states reaching Paused or
   Streaming.

Do not repeatedly press repair while startup is still running. Graph mutations
are serialized, so repeated requests are intentionally coalesced.

## No Meter Movement

WaveLinux 6 meters come from the persistent audio core and do not require a
recorder client for each node. Check:

```bash
rg 'meters.supervisor' ~/.config/wavelinux6/wavelinux-engine.log | tail
```

A healthy mixer reports meter protocol `1` as connected in Testing Health, the
expected logical target count, and no `wavelinux6-meter-*` clients in `pw-dump`.
Confirm both private sockets exist:

```bash
ls -l "$XDG_RUNTIME_DIR/wavelinux6/control/wavelinux6-"*.sock
```

The JSON compatibility request below isolates core atomics when the binary
transport cannot connect:

```bash
printf '%s' '{"protocol_version":3,"command":"get_meters"}' \
  | socat - UNIX-CONNECT:"$XDG_RUNTIME_DIR/wavelinux6/control/wavelinux6-audio-core.sock"
```

If the control socket is missing, inspect `wavelinux6-audio-core.log`. Also
verify that the AppImage did not bundle an incompatible PipeWire library tree:

```bash
bash scripts/sanitize-appimage-pipewire.sh --check <AppImage>
```

## AppImage Opens To A White Window

Run the AppImage from a terminal. `Could not create default EGL display:
EGL_BAD_PARAMETER` means the WebKit render process loaded an AppImage Wayland
library beside the host Mesa/EGL driver. Release AppImages must pass:

```bash
bash scripts/sanitize-appimage-wayland.sh --check <AppImage>
```

The package gate also launches the AppImage as a non-root user in a clean
CachyOS image and rejects screenshots that do not contain the rendered mixer.

If backend meter values exist but bars do not move, inspect `src/state.ts` event
delivery and the direct meter transform bindings before changing audio routing.

## Microphone Source Is Silent

Select `wavelinux6-mic` in PipeWire/Pulse applications. For ALSA-only clients,
refresh aliases with `yarn install:alsa-aliases` and use `wavelinux6_mic`.

Check that:

- the selected physical input's active port is available;
- a WaveLinux input-to-channel loopback exists;
- `wavelinux6-mic` exists and is not muted;
- core `captured_frames` and `rendered_frames` increase;
- `dropped_frames` and `underrun_frames` remain zero.

If disabling all effects restores audio, inspect `effects.sync` and core chain
acknowledgement. Enabling effects must swap a native chain, not remove the
public microphone source.

## Wrong Microphone After Hotplug

An unplugged PCI/HDA headset port reported as `not available` must not win auto
selection. USB devices with `availability unknown` remain eligible. Manual
choices fall back temporarily and restore after 750 ms of stable availability.

Search current logs for `default.input`, `hotplug.input`, and `auto_devices`.
Do not add a broad bad-jack exception; that previously selected phantom headset
microphones.

## Browser Is Missing From Stream

The browser playback stream must first appear in PipeWire, then be moved to the
configured channel. Check `route.streams` and `route.streams.fast` after pressing
play. The Stream recording source is `wavelinux6_mix_stream_source`.

If a browser was dormant for a long time, confirm the Pulse subscription worker
is alive and the event moved the new stream within the 100 ms routing target.
The 120-second watchdog is only a lost-event recovery path.

## Effect Edits Cause Silence

Rapid edits should produce one latest-wins chain swap and a 20 ms crossfade.
Healthy logs show `effects.sync` with a native-core command and increasing
`acknowledged_generation`; they do not show module unload/reload churn.

Check for unsupported effect ids or RNNoise load errors in the core log. The
current catalog contains RNNoise, high-pass, EQ, compressor, gate, limiter, and
Karaoke Stage. DeepFilterNet is not supported.

## High CPU At Silence

Check `native_stats last_process_us` and the per-thread view:

```bash
core=$(ps -eo pid=,args= | awk '$2 ~ /wavelinux6-audio-core$/ && $3 == "--run-core" {print $1; exit}')
top -H -p "$core"
```

Near-silent processing should remain cheap. If one data-loop thread rises
dramatically, verify recursive filters still flush state below `1e-20`. A loud
benchmark alone will not reproduce a denormal slowdown.

## Clicks Under Disk, Network, Or Game Load

Correlate the event timestamp across:

```bash
rg 'native_stats' ~/.config/wavelinux6/wavelinux6-audio-core.log | tail
journalctl --user --since '5 minutes ago' --no-pager | \
  rg -i 'pipewire|out of buffers|xrun|underrun|resync|link'
```

Health should raise adaptive latency after real underrun evidence without a
`repair.*` event, public-node replacement, or helper restart. CPU pressure alone
raises one level at a time; real discontinuities can jump two.

## Socket ACL Errors

Core control sockets must be owned by the current user and mode `0600`:

```bash
find "${XDG_RUNTIME_DIR:-/tmp/wavelinux-$(id -u)}/wavelinux6/control" \
  -name '*.sock' -printf '%m %u %p\n'
```

Delete stale sockets only while WaveLinux 6 is stopped. Startup recreates them.
Do not relax them to group/world access to work around an ownership mistake.

## Reinstall A Local Build

Use the supported path so stale modules and helpers are cleaned in dependency
order:

```bash
yarn desktop:build
yarn install:local
wavelinux6
```

The installer must not kill unrelated PipeWire services or non-WaveLinux
applications.
