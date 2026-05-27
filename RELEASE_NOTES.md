# WaveLinux 4.1.3

WaveLinux 4.1.3 is a focused stability release for hotplug routing, Bluetooth
monitor output recovery, and documentation cleanup.

## Fixes

- Rebuilds only the final Bluetooth monitor route when a Bluetooth output
  reconnects, changes profile/codec identity, or leaves duplicate monitor
  loopbacks behind.
- Waits briefly for A2DP transport to settle before reconnecting the monitor
  route, reducing silent-output races during Bluetooth reconnects.
- Restores default input/output locks without running a full graph repair when
  only the app-facing default device changed.
- Backs off failed app stream moves for disappeared streams so stale PipeWire
  stream IDs do not create repeated move failures.
- Adds `adjust_time=0` to managed WaveLinux loopbacks and bumps route revisions
  so old routes are rebuilt with the new arguments.
- Keeps route latency decisions profile-driven while preserving conservative
  fallbacks for unknown hardware.

## Documentation

- Keeps the README version-neutral and moves release-specific detail back into
  release notes.
- Simplifies code comments that had drifted into internal changelog wording.
- Removes version-specific wording from the test documentation.
