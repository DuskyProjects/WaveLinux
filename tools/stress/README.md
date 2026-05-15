# WaveLinux Stress Harness

This harness is for destructive local stress testing of the installed
`WaveLinux.AppImage` on the current desk rig.

It is designed to abuse:

- cold launch and quit loops
- hard-kill and relaunch recovery
- monitor output flips between speaker and Bluetooth
- Bluetooth disconnect/reconnect and profile churn
- selected mic swaps between DJI and internal mic
- PipeWire stack restarts
- app-route persistence and corking
- settings tab churn while runtime changes are active
- long randomized soak behavior

## Safety

The harness intentionally performs destructive operations.

- It keeps hardware sink volumes clamped to `20%`.
- It mutes both the Bluetooth sink and speaker sink if a phase fails.
- It may restart the user PipeWire stack.
- It may disconnect and reconnect the Bluetooth headset.
- It may kill WaveLinux with `SIGKILL`.

Do not run it while using the machine for anything else.

## Files

- `run_stress_suite.py`: orchestrates the phases and writes artifacts
- `control_client.py`: client for the app-side control socket
- `audio_probe.py`: synthetic playback and capture helpers
- `bluetooth_ops.py`: Bluetooth and profile helpers
- `system_snapshot.py`: system-state capture helpers
- `assertions.py`: failure buckets and phase assertions
- `profile.schema.json`: machine profile schema
- `profile.current-machine.example.json`: example profile for the current desk rig
- `profile.current-machine.json`: local concrete profile for this machine

## App-Side Control

WaveLinux exposes a test-only Unix socket when launched with:

```bash
WAVELINUX_STRESS_CONTROL=1
```

Optional explicit socket path:

```bash
WAVELINUX_STRESS_SOCKET_PATH=/tmp/wavelinux-stress.sock
```

Supported commands include:

- `ping`
- `get_runtime_summary`
- `get_health_summary`
- `export_diagnostics`
- `set_monitor_output`
- `set_stream_output`
- `set_selected_mic`
- `set_app_route`
- `open_settings_tab`
- `close_settings`
- `refresh_now`
- `quit_cleanly`
- `wait_for_ready`
- `list_known_sinks`
- `list_known_sources`

## Running

Full default battery:

```bash
python3 tools/stress/run_stress_suite.py \
  --profile tools/stress/profile.current-machine.json \
  --mode maximum
```

Short smoke run:

```bash
python3 tools/stress/run_stress_suite.py \
  --profile tools/stress/profile.current-machine.json \
  --phases cold_launch,quit_loop \
  --loop-count cold_launch=1 \
  --loop-count quit_loop=1 \
  --soak-seconds 0
```

Focused Bluetooth run:

```bash
python3 tools/stress/run_stress_suite.py \
  --profile tools/stress/profile.current-machine.json \
  --phases monitor_churn,bluetooth_reconnect,forced_profile_abuse \
  --loop-count bluetooth_reconnect=2 \
  --loop-count forced_profile=1 \
  --soak-seconds 0
```

## Artifacts

Each run writes to:

```text
~/.config/wavelinux/stress-runs/<run-id>/
```

Expected outputs include:

- `summary.json`
- `events.jsonl`
- `preflight.json`
- `phase-<name>-attempt-<n>-before.json`
- `phase-<name>-attempt-<n>-after.json`
- `*.wpctl-status.txt`
- `*.pactl-sinks.txt`
- `*.pactl-sources.txt`
- `*.pactl-sink-inputs.txt`
- `*.pactl-source-outputs.txt`
- `*.wavelinux-log-tail.txt`

If WaveLinux is still responsive when a phase fails, the harness also
asks it to export diagnostics and records that path in `events.jsonl`.
