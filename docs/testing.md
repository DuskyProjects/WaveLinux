# WaveLinux 6 Test Suites

Default tests are dry-run and must not create, move, or unload live PipeWire
nodes. Live integration tests are ignored unless explicitly enabled.

## Safe Suite

Run the same local gate used before packaging:

```bash
bash scripts/test-all.sh
```

It covers Rust formatting/tests, Clippy, frontend type/build checks, shell syntax,
ALSA alias isolation, profile validation, packaging metadata, and AppImage rules
available on the host. Missing optional host tools are reported explicitly.

Useful focused commands:

```bash
cargo test -p wavelinux-dsp
cargo test -p wavelinux-model
cargo test -p wavelinux-pw
cargo test -p wavelinux-engine
cargo test -p wavelinux-accelerator --all-features --all-targets
cargo clippy --workspace --all-targets -- -D warnings
yarn web:build
```

## Coverage Map

- `wavelinux-dsp`: native nodes, mono RNNoise, near-field gating, chain state,
  denormal handling, control buffering, adaptive latency, and provider policy.
- `wavelinux-model`: schema migration, effect catalog/defaults, namespaces,
  device ranking, app identity, and DeepFilterNet-to-RNNoise migration.
- `wavelinux-pw`: snapshot parsing, route planning, ownership, filter config,
  stream identity, and cleanup safety.
- `wavelinux-engine`: reconciliation, graph idempotency, hotplug, jack
  availability, fallback/restore, route backoff, effect coalescing, meter state,
  hardware profiles, Health, and process matching.
- `wavelinux-accelerator`: pack ownership/hash validation, machine-local
  qualification, fixed shared-memory queues, RNNoise numerical fixtures,
  isolated ONNX inference, provider termination, and exact CPU state fallback.
- Frontend: Vitest/React Testing Library covers effect-strength mapping and UI
  state contracts; Playwright covers FX scrolling and desktop layouts at 100,
  125, and 150% scaling; TypeScript and Vite protect production IPC/build
  contracts.
- Shell: installers, process matching, dependency logic, and ALSA alias edits.

Linux screenshot tests use `tests/e2e/fonts.conf` to select the DejaVu Sans
fallback used by the committed images, independently of the desktop's default
font. Install DejaVu Sans before running Playwright (`fonts-dejavu-core` on
Debian/Ubuntu, `ttf-dejavu` on Arch). An explicit `FONTCONFIG_FILE` overrides
this test setting. This affects only the test browser, not the installed app.

## Live PipeWire Tests

Live tests mutate the current user audio graph. Close recording/streaming work
or use an isolated PipeWire session first:

```bash
WAVELINUX_RUN_LIVE_TESTS=1 \
  cargo test -p wavelinux-engine -- --ignored --test-threads=1
```

Acceptance scenarios include:

- dormant browser stream activation and first-play routing;
- Monitor and Stream aggregation;
- `wavelinux6-mic` in Discord and Audacity;
- mono/USB/HDA microphone hotplug and 750 ms restoration debounce;
- rapid effect edits without public-node loss;
- live 28 -> 120 -> 28 ms latency changes without graph repair;
- stable node ownership alongside unrelated host nodes.

## DSP Benchmark

Run the deterministic offline fixture:

```bash
bash scripts/bench-audio-runtime.sh
```

Results are written under `target/bench`. Compare release builds on the same
machine and power state. The fixture catches algorithmic regressions but cannot
detect every real-time failure.

For a live run, record process CPU and inspect:

```bash
tail -f ~/.config/wavelinux6/wavelinux6-audio-core.log
journalctl --user -f -u pipewire -u pipewire-pulse -u wireplumber
```

Healthy core stats have zero dropped/underrun frames and callback time well
below the quantum budget. Near-silent microphone audio is a required benchmark
case because recursive filters can expose denormal-number regressions that loud
fixtures do not.

## Optional audio load diagnostics

Audio load diagnostics are available for investigating specific continuity or
performance problems. They are not part of `scripts/test-all.sh`, CI, or the
release requirements, and run only when explicitly requested. The default run
lasts 60 seconds; set `WAVELINUX_STRESS_DURATION_SEC` to choose another duration.

```bash
bash scripts/stress-audio-isolated.sh \
  target/release/bundle/appimage/WaveLinux6_6.0.3_amd64.AppImage
```

The harness records Stream and microphone sources under concurrent disk,
network, and CPU load, and retains its reports for diagnosis. It starts a
separate D-Bus, PipeWire, PipeWire-Pulse, and policy-only WirePlumber session.
Its monitor may target only a null/dummy sink, so the continuity pilot cannot
reach the desktop user's headphones or speakers.
`scripts/stress-audio-runtime.sh` rejects physical monitor targets by default;
its override is for unattended lab hardware only.

## Packaging And Distro Smoke

Build and validate portable local packages:

```bash
bash scripts/build-portable.sh
bash scripts/check-package-contents.sh target/portable/release/bundle
```

The package gate extracts the AppImage, deb, and rpm, checks their identity and
required helpers, and rejects embedded WaveLinux binaries newer than the glibc
2.39 ceiling. It also rejects bundled PipeWire and Wayland client libraries
that must match the host. Distro smoke then invokes the packaged app's
`--probe-binary` path, so a dynamically unloadable executable cannot pass on
metadata alone.

Run the exact locally staged artifacts across the supported matrix:

```bash
version="$(node -e 'console.log(require("./package.json").version)')"
WAVELINUX_SMOKE_ARTIFACT_VERSION="$version" \
WAVELINUX_SMOKE_LOCAL_ASSET_DIR="$PWD/target/release/smoke-assets" \
  bash scripts/distro-smoke.sh --all --target appimage --release-tag "v$version"
```

Repeat with `--target native` for Debian, Ubuntu, and Fedora. Arch uses the
AppImage/AUR path and intentionally skips native package installation.

Published prerelease assets can be tested in supported containers with:

```bash
bash scripts/distro-smoke.sh --all --target appimage \
  --release-tag <waveLinux6-prerelease-tag>
```

The smoke harness checks Debian 13, Ubuntu 24.04, current Fedora, Arch, and
CachyOS AppImage paths plus supported native deb/rpm paths. CachyOS additionally
launches the real WebKit window as a non-root user under Xvfb and rejects blank
or renderer-crashed screenshots. Containers do not replace live desktop audio
sessions.

## Regression Checklist

For every runtime change:

1. Run focused tests while editing.
2. Run the complete safe suite.
3. Build the release AppImage.
4. Install locally through `scripts/install-local.sh`.
5. Confirm public nodes, meters, selected devices, Stream aggregation, core
   stats, and current logs.
6. Keep the app running for user acceptance.
7. Do not push or publish until explicitly approved.
