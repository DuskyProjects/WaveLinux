# WaveLinux Test Suites

WaveLinux keeps all default tests dry-run and safe for CI. They must not create,
move, or unload live PipeWire nodes unless a test is explicitly marked ignored.

Run the full safe suite:

```sh
bash scripts/test-all.sh
```

Run live PipeWire regression tests only when you are ready to mutate the current
user audio graph:

```sh
WAVELINUX_RUN_LIVE_TESTS=1 bash scripts/test-all.sh
```

## Coverage Map

- `wavelinux-model`: config migration and normalization, app identity
  pin/merge/reset, wrapper app matching, device
  policy, effect catalog ranges, and legacy preset compatibility.
- `wavelinux-pw`: PipeWire/PulseAudio command planning, managed module parsing,
  app stream identity enrichment, source-output and sink-input route hydration,
  effect-chain rendering, plugin detection, and cleanup planning.
- `wavelinux-engine`: graph idempotency, stale route rescue, hotplug policy,
  Bluetooth profile rotation, targeted effect rebuilds, effect diagnostics,
  meter smoothing, app routing, and ignored live graph tests.
- Frontend: TypeScript and Vite builds catch IPC shape drift and UI compile
  regressions.
- Shell helpers: ALSA alias install/remove tests run against a temporary
  `.asoundrc` and never touch the real user audio config.

The ignored live tests cover the parts that cannot be proven in CI: virtual node
creation, volume/mute mutation, stale cleanup, per-channel music metering, and
PipeWire filter-chain startup/cleanup.

## Distro Smoke

Release smoke tests run the published assets in clean containers for the main
Linux families WaveLinux supports:

```sh
bash scripts/distro-smoke.sh --all --target appimage --release-tag v4.3.5
bash scripts/distro-smoke.sh --distro fedora --target native --release-tag v4.3.5
```

The AppImage target downloads the release AppImage, runs
`--install-runtime-dependencies`, and then verifies
`--check-runtime-dependencies` on Debian, Ubuntu, Fedora, and Arch. The native
target installs the release deb or rpm with the distro package manager and then
runs the same runtime check on Ubuntu and Fedora. Debian 12 is covered by the
AppImage path because the native deb is built on the current GitHub Linux runner
and may require a newer glibc than Debian 12 provides.

Containers are not full desktop sessions, so the smoke harness accepts the
container-only `bwrap usable sandbox` warning after package installation. It
still fails if runtime packages, WebKit helpers, PipeWire tools, ALSA tools, or
required graphics libraries are missing.
