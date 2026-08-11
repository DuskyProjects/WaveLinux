# WaveLinux 6 Setup And Development

WaveLinux 6 targets PipeWire desktop sessions. The app can start missing user
services, but it cannot replace missing host libraries or a distro configured
for PulseAudio without PipeWire compatibility.

For normal installation, use the checksum-verified standalone installer shown
first in the project README. Run it as the desktop user; it elevates only its
package-manager transaction and verifies the live audio graph before reporting
success.

## Host Requirements

Required runtime pieces are:

- PipeWire, `pipewire-pulse`, and WirePlumber;
- `pactl`, `wpctl`, `pw-cli`, `pw-dump`, `pw-metadata`, and `pw-top`;
- Procps (`ps` and `pgrep`) for exact WaveLinux process ownership checks;
- ALSA tools for legacy-device discovery;
- GTK/WebKitGTK, portals, fonts, and graphics libraries required by Tauri;
- RNNoise, supplied by the WaveLinux release/runtime bundle.

Check the current host without changing it:

```bash
yarn deps:check
```

Request package installation through the supported distro helper:

```bash
yarn deps:install
```

Package installation is an explicit privileged action. Standard WaveLinux
effects are user-installed with the app and must not require `sudo`.

## Build And Install

Install JavaScript dependencies and build the local AppImage:

```bash
yarn install
yarn desktop:build
yarn install:local
```

`yarn desktop:build` is a host-native development build. Before sharing an
artifact, use the pinned Debian builder instead:

```bash
WAVELINUX_REBUILD_PORTABLE_IMAGE=1 bash scripts/build-portable.sh
```

The portable build uses Podman when available, otherwise Docker. Outputs remain
under `target/portable/release`; after all package and glibc checks pass, the
script promotes the packages to `target/release` and stages the exact files used
by distro smoke tests under `target/release/smoke-assets`.

The installer replaces local WaveLinux5 and installs:

```text
~/.local/bin/wavelinux6
~/.local/bin/wavelinux6-audio-core
~/.local/share/wavelinux6/WaveLinux6_<version>_amd64.AppImage
~/.local/share/applications/wavelinux6.desktop
~/.config/wavelinux6/config.json
```

Start it from the desktop menu or:

```bash
wavelinux6
```

The installer stops WaveLinux5/6 app and helper processes and unloads only
WaveLinux-owned modules. It must never match unrelated PipeWire processes.

Uninstall the local development build with:

```bash
yarn uninstall:local
```

## First Launch And Migration

If no WaveLinux 6 config exists, startup can import WaveLinux5 schema data,
normalize it to schema 14, rewrite owned node names to `wavelinux6`, and clear
transient route ids. WaveLinux5 config and installation artifacts are removed
after the new graph validates. There is no long-lived migration backup.

DeepFilterNet effect entries migrate to RNNoise. DeepFilterNet packages or
models are not downloaded.

## AppImage Runtime Preflight

Check a built AppImage directly:

```bash
APPIMAGE_EXTRACT_AND_RUN=1 \
  target/release/bundle/appimage/WaveLinux6_6.0.0_amd64.AppImage \
  --check-runtime-dependencies
```

Request host package installation:

```bash
APPIMAGE_EXTRACT_AND_RUN=1 \
  target/release/bundle/appimage/WaveLinux6_6.0.0_amd64.AppImage \
  --install-runtime-dependencies
```

Startup probes `pactl info`. If the user audio stack is stopped, the launcher
tries PipeWire/Pulse/WirePlumber user services, then a non-systemd fallback. Set
`WAVELINUX_SKIP_AUDIO_SERVICE_START=1` only when another supervisor owns those
services.

AppImages must not bundle `libpipewire-0.3`, the GStreamer PipeWire plugin, a
partial `pipewire-0.3`/`spa-0.2` tree, or `libwayland*.so`. PipeWire and
Wayland/EGL must use a matching host stack. Run:

```bash
bash scripts/sanitize-appimage-pipewire.sh --check <AppImage-or-AppDir>
bash scripts/sanitize-appimage-wayland.sh --check <AppImage-or-AppDir>
```

## ALSA-Only Applications

PipeWire/Pulse applications should select `wavelinux6-mic` or
`wavelinux6_mix_stream_source`. The local installer also maintains a marked,
user-owned block in `~/.asoundrc` for ALSA-only applications.

Refresh it with:

```bash
yarn install:alsa-aliases
```

Typical ALSA aliases are `wavelinux6_mic`, `wavelinux6_mix_stream`, and
`wavelinux6_mix_monitor`. Prefer the PipeWire or Pulse host in Audacity when it
is available; ALSA aliases are compatibility paths.

## Hardware Profiles

Profile resolution prefers:

1. user overrides under `~/.config/wavelinux6/hardware-profiles/v1/local`;
2. signed cached catalog entries;
3. profiles embedded from `profiles/v1/devices` at compile time;
4. the safe generic profile.

See [profiles.md](profiles.md) for runtime behavior and
[profiles/v1/README.md](../profiles/v1/README.md) before changing match rules,
port availability policy, or device ranking.

## Development Commands

Run the Tauri desktop in development mode:

```bash
yarn dev
```

Run only the web UI:

```bash
yarn web:dev
```

Render graph commands without mutating the host:

```bash
WAVELINUX_DRY_RUN=1 yarn dev
```

Run the safe validation suite:

```bash
yarn test:all
```

Run live graph tests only in a disposable or explicitly prepared user audio
session:

```bash
WAVELINUX_RUN_LIVE_TESTS=1 bash scripts/test-all.sh
```

## Runtime Overrides

These are developer diagnostics, not normal user settings:

```text
WAVELINUX_AUDIO_RUNTIME=pipewire_filter_chain|dsp_cpu|dsp_auto|dsp_accelerated
WAVELINUX_DSP_PROVIDER=auto|cuda|openvino|migraphx|cpu
WAVELINUX_DRY_RUN=1
WAVELINUX_SKIP_AUDIO_SERVICE_START=1
```

Provider probes do not activate production GPU processing. Keep the CPU path
unless an isolated provider pack has passed workload qualification.

## Build Pipeline

`yarn desktop:build` runs `scripts/build-local.sh`, which:

1. builds `wavelinux6-audio-core` in release mode;
2. stages the core and runtime libraries into the AppDir;
3. runs the Tauri bundle build;
4. retries linuxdeploy with host `strip` when required;
5. removes host-incompatible PipeWire artifacts;
6. rebuilds and verifies the final AppImage.

`scripts/build-portable.sh` runs that pipeline inside
`containers/release/Containerfile`, verifies every embedded WaveLinux ELF
against the configured glibc ceiling (2.39 by default), and runs package-content
checks before promotion. The AppImage keeps its bundled Fontconfig library and
configuration on the same builder baseline. A process-local, user-runtime
sysroot isolates parser rules while links retain host fonts and writable user
caches, so newer distro rules cannot break startup or flood logs with warnings.
`--probe-binary` is an early, display-free executable
probe used by clean distro containers; it verifies that the actual packaged
binary can load, rather than accepting metadata alone.

Use `APPIMAGE_EXTRACT_AND_RUN=1` when FUSE is unavailable. Publishing and signing
are separate release steps; a local build must not be pushed implicitly.

## Installer Verification And Recovery

The standalone installer waits for PipeWire and Pulse connectivity, a running
`wavelinux6-audio-core`, its user-only control socket, and all public microphone,
mix, and channel nodes. Re-run the installed verifier with:

```bash
~/.local/share/wavelinux6/verify-install.sh --timeout 30
```

Installed AppImage logs are under `~/.config/wavelinux6`; package/service status
and graph inspection commands are printed when verification fails. A file-only
installation for imaging or offline preparation is available with
`--no-launch`. openSUSE should use the standalone installer rather than the
Fedora-targeted RPM.
