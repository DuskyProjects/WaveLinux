# WaveLinux Setup and Development

This page collects the operational details that do not need to live on the
front README.

For the engine, effect-routing, logging, and AppImage packaging design, see
[Architecture notes](architecture.md).

## Install From A Release

Download the latest release artifact:

```text
https://github.com/DuskyProjects/WaveLinux/releases/latest
```

Available formats:

- AppImage: portable desktop build and primary self-update format.
- deb: Debian and Ubuntu-family package.
- rpm: Fedora/openSUSE-family package.
- AUR metadata: Arch package recipe.

AppImage releases bundle WebKitGTK/GTK, GStreamer media support, WebKit sandbox
helpers, libusb for optional Elgato controls, and supported LADSPA effect
plugins present on the release builder. First launch still checks host-bound
pieces such as PipeWire, desktop display/GL libraries, fonts, portals, and
distro-provided effect packages.

PipeWire is intentionally host-bound for AppImage builds. Release packaging
must not bundle `libpipewire-0.3.so*`, the GStreamer PipeWire plugin, or partial
`pipewire-0.3`/`spa-0.2` module trees because version-mismatched client
libraries can prevent live meters from creating PipeWire streams.

## Local Install

From a checkout:

```bash
yarn install
yarn desktop:build
yarn install:local
```

The local installer places the AppImage and launcher here:

```bash
~/.local/share/wavelinux/WaveLinux_*_amd64.AppImage
~/.local/bin/wavelinux
```

It also installs the desktop entry and icons under the usual XDG user paths and
seeds local hardware profiles into:

```bash
~/.config/wavelinux/hardware-profiles/v1/local/wavelinux-local-seed
```

The WaveLinux5 test line installs side-by-side instead:

```bash
~/.local/share/wavelinux5/WaveLinux5_5.0.0_amd64.AppImage
~/.local/bin/wavelinux5
~/.config/wavelinux5/hardware-profiles/v1/local/wavelinux5-local-seed
```

## Runtime Checks

Check runtime dependencies and effect plugins:

```bash
yarn deps:check
```

Install missing runtime dependencies and effect packages when a supported
package manager is available:

```bash
yarn deps:install
```

Install only effect packages:

```bash
yarn effects:install
```

Run AppImage preflight manually:

```bash
./WaveLinux_4.3.7_amd64.AppImage --check-runtime-dependencies
./WaveLinux_4.3.7_amd64.AppImage --install-runtime-dependencies
```

The runtime check reports the host PipeWire client stack and fails if an
AppImage is shadowing it with bundled PipeWire libraries or modules.

AppImage startup also probes `pactl info` before the UI opens. If packages are
installed but the user audio stack is not running, WaveLinux tries to start
`pipewire`, `pipewire-pulse`, and `wireplumber` through `systemctl --user`; on
non-systemd sessions it falls back to launching those daemons directly. Set
`WAVELINUX_SKIP_AUDIO_SERVICE_START=1` to disable this startup recovery.

Use `WAVELINUX_SKIP_RUNTIME_INSTALL=1` to skip the AppImage preflight, or
`WAVELINUX_ASSUME_RUNTIME_DEPS=1` when a packager has already provided all host
runtime dependencies.

## ALSA-Only Apps

Most apps should see WaveLinux devices through PipeWire/PulseAudio. WaveLinux5
also installs user-scoped ALSA aliases during local install so legacy capture
tools can discover the test-line virtual sources through ALSA.

To refresh the aliases manually:

```bash
yarn install:alsa-aliases
```

This uses a marked block in `~/.asoundrc` so uninstall can remove only
WaveLinux-owned aliases. For WaveLinux5, Audacity's ALSA host should then show
entries such as `wavelinux5_mic`, `wavelinux5_mix_stream`,
`wavelinux5_mix_monitor`, and `wavelinux5_channel_hardware_in`.

## Hardware Profiles

Profile resolution prefers the safest local data first:

- Local user profiles in `~/.config/wavelinux/hardware-profiles/v1/local`.
- Remote profiles cached from the GitHub repo profile feed.
- The editable safe generic default profile, `default.generic-audio`.

The Settings page includes Profiles under its tab bar. Editing a downloaded or
seeded profile creates a safe local override under:

```bash
~/.config/wavelinux/hardware-profiles/v1/local/wavelinux-user-overrides
```

For profile authoring, see [profiles/v1/README.md](../profiles/v1/README.md).

## Elgato Controls

When WaveLinux detects an Elgato audio device, Settings shows an Elgato tab.
Wave XLR hardware controls are available there for microphone gain, mute,
headphone volume, and low-impedance mode. The libusb control path is loaded only
after a supported Wave XLR is detected.

For zero-latency self monitoring on a Wave XLR, enable Hardware direct mic
monitor in Settings > Sync and listen through the Wave XLR headphone output.

## Streamer Device Bindings

When WaveLinux detects supported streamer hardware, Settings shows a Streamers
tab. Device discovery uses Linux sysfs, hidraw, PipeWire, and ALSA sequencer
inspection first; WaveLinux only keeps hidraw devices open or starts `aseqdump`
MIDI capture when a detected device has enabled bindings.

Bindings can target mixer mute and volume controls, source-to-mix controls, and
the safer stale-audio prune action.

Packaged installs may include:

```text
packaging/udev/70-wavelinux-streamer-devices.rules
```

After installing udev rules manually, reload rules with your distribution's
standard `udevadm control --reload-rules && udevadm trigger` flow and reconnect
the device.

## Testing Health Reports

For beta testing and GitHub issues, use Settings > Health > Testing Health
Report. It creates one copyable Markdown block with engine state, update
channel/feed/status, diagnostics, audio device summaries, Elgato detection,
streamer-device detection, and recent debug-log lines.

## Development

Install dependencies:

```bash
yarn install
```

Run the desktop app:

```bash
yarn dev
```

Run the browser-only UI preview:

```bash
yarn web:dev
```

Dry-run audio graph commands without changing PipeWire:

```bash
WAVELINUX_DRY_RUN=1 yarn dev
```

Run all safe checks:

```bash
yarn test:all
```

Run live PipeWire integration tests only when you are ready for the test suite
to create, route, and clean up real audio nodes:

```bash
WAVELINUX_RUN_LIVE_TESTS=1 cargo test -p wavelinux-engine -- --ignored --test-threads=1
```

## Build And Release

Build web UI only:

```bash
yarn web:build
```

Build local desktop bundles:

```bash
yarn desktop:build
```

The local and release build scripts sanitize the generated AppDir, rebuild the
AppImage, and reject artifacts that contain bundled PipeWire client libraries,
PipeWire GStreamer plugins, or partial SPA/PipeWire module trees.

If Tauri's cached linuxdeploy AppImage fails while stripping newer ELF sections,
`scripts/build-local.sh` retries with
`scripts/rebuild-appimage-with-host-strip.sh`. That fallback extracts
linuxdeploy, replaces the embedded `strip` with the host `strip`, and reruns the
GTK/GStreamer plugin pass before `scripts/finalize-appimage.sh` rebuilds the
sanitized AppImage. For the full packaging flow, see
[Architecture notes](architecture.md#appimage-packaging).

Regenerate and stage AUR files:

```bash
yarn aur:build
```

Build signed release bundles and updater signatures:

```bash
yarn release:key
yarn desktop:release
```

The GitHub release workflow builds AppImage, deb, rpm, updater metadata, and AUR
package files when a `v*` tag is pushed. Hardware profiles are fetched from
`profiles/v1` in the repository instead of being uploaded as release assets.
Stable tags publish only the matching section from `RELEASE_NOTES.md`, prune
older GitHub release pages, and keep stable git tags for source history.
Testing tags containing `testing`, `beta`, `pre`, or `rc` publish to the moving
`prerelease` GitHub release instead of the stable `latest` release.

Required GitHub Actions secrets:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

## Project Layout

- `crates/app`: Tauri desktop shell and IPC commands.
- `crates/engine`: config, diagnostics, graph orchestration, and state.
- `crates/model`: shared data model and migrations.
- `crates/pw`: PipeWire/PulseAudio command planning, parsing, and DSP rendering.
- `profiles/v1`: hardware profile schema, examples, author docs, and device seeds.
- `src`: React/TypeScript UI.
- `docs`: architecture, setup, testing, and theme authoring docs.
- `scripts`: installers, release helpers, dependency checks, and validation.
- `packaging/aur`: Arch/AUR package metadata.
