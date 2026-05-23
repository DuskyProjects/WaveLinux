# WaveLinux 4.0
<img width="1895" height="1108" alt="image" src="https://github.com/user-attachments/assets/22b52de8-d97d-4664-9772-c1c358122144" />

WaveLinux 4.0 is a Linux-first creator audio mixer built with Rust, Tauri,
React, TypeScript, and PipeWire. It is a fresh application that carries the
WaveLinux name forward while replacing the old Python implementation with a
new native desktop stack.

The goal is Wave Link-style software mixing for Linux: virtual source faders,
separate Monitor and Stream mixes, app routing, microphone processing, scenes,
diagnostics, tray behavior, package builds, and open-source DSP replacements.

WaveLinux is not an Elgato hardware control panel. Vendor-specific microphone
features, Stream Deck integration, proprietary marketplace effects, and
hardware Clipguard behavior are intentionally out of scope for 4.0. Standard
Linux audio hardware is the target.

## Features

- PipeWire and WirePlumber audio graph management
- Up to 5 virtual mixes
- Up to 8 software routes plus hardware input routes
- Two faders per source: Monitor and Stream
- Virtual mix outputs for OBS, Discord, browsers, games, meetings, and tools
- App stream discovery, saved routing, app identity overrides, and offline rules
- Automatic monitor output policy: Bluetooth, USB audio, jack, then speakers
- Automatic microphone policy: USB mic, microphone jack, built-in mic, then Bluetooth input
- Bluetooth A2DP profile protection when possible
- Hotplug recovery for inputs and outputs
- Real channel/mix metering with stale-sample decay
- Per-source effect chains through PipeWire filter-chain and LADSPA/open plugins
- DeepFilterNet, RNNoise, high-pass, EQ, compressor, gate, and limiter catalog entries
- Scenes, setup templates, diagnostics, sound checks, graph repair, and cleanup
- Close-to-tray desktop behavior with tray Quit for full graph cleanup
- AppImage, deb, rpm, and AUR packaging
- Signed in-app update checks for AppImage installs

## Supported Platforms

WaveLinux 4.0 targets PipeWire-based Linux desktops.

Required host services and tools:

- PipeWire
- WirePlumber
- pipewire-pulse
- `pactl`
- `wpctl`
- `pw-cli`
- `pw-dump`

Recommended optional effect packages:

- SWH LADSPA plugins for compressor, gate, and limiter support
- RNNoise LADSPA/noise-suppression-for-voice
- DeepFilterNet LADSPA support when available for your distro

## Install

Download the latest release artifact from GitHub Releases:

```bash
https://github.com/DuskyProjects/WaveLinux/releases
```

Available formats:

- AppImage: portable desktop build and the primary self-update format
- deb: Debian and Ubuntu-family package
- rpm: Fedora/openSUSE-family package
- AUR metadata: Arch package recipe

For local development installs from a checkout:

```bash
yarn install
yarn desktop:build
yarn install:local
```

The local installer places the AppImage and launcher here:

```bash
~/.local/share/wavelinux/WaveLinux_4.0.0_amd64.AppImage
~/.local/bin/wavelinux
```

It also installs the desktop entry and icons under the usual XDG user paths.

## Dependency Checks

Check runtime dependencies and optional effect plugins:

```bash
yarn deps:check
```

Install missing runtime dependencies and optional effect packages when a
supported package manager is available:

```bash
yarn deps:install
```

Install only optional effect packages:

```bash
yarn effects:install
```

The dependency installer checks first. It does not install packages unless you
explicitly run the install command or set the corresponding environment flags.

## ALSA-Only Apps

Most apps should see WaveLinux devices through PipeWire/PulseAudio. For legacy
ALSA-only applications that cannot see those devices, install optional ALSA
aliases:

```bash
yarn install:alsa-aliases
```

This is opt-in and uses a marked block in `~/.asoundrc` so uninstall can remove
only WaveLinux-owned aliases.

## Daily Use

Launch WaveLinux from your app menu or:

```bash
wavelinux
```

WaveLinux opens without mutating the audio graph unless startup restore is
enabled. Use Start Audio to create the virtual devices, Repair to rebuild stale
routes, and Stop/Cleanup to unload managed nodes.

Closing the window hides it to the tray so audio can keep running. Use Quit
from the tray menu to exit fully and remove WaveLinux-managed PipeWire nodes.

## Updates

AppImage installs can check signed release metadata from inside Settings.

Stable channel:

```text
https://github.com/DuskyProjects/WaveLinux/releases/latest/download/latest.json
```

Pre-release channel:

```text
https://github.com/DuskyProjects/WaveLinux/releases/download/prerelease/latest.json
```

If update metadata has not been published yet, the app reports that no signed
metadata is available for the channel. Package-managed installs should update
through their package manager.

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

Build signed release bundles and updater signatures:

```bash
yarn release:key
yarn desktop:release
```

Generate updater metadata:

```bash
python3 scripts/build-updater-manifest.py \
  --artifact target/release/bundle/appimage/WaveLinux_4.0.0_amd64.AppImage \
  --version 4.0.0 \
  --repo DuskyProjects/WaveLinux \
  --tag v4.0.0 \
  --output target/release/bundle/latest.json
```

Stage AUR files:

```bash
yarn aur:build
```

The GitHub release workflow builds AppImage, deb, rpm, updater metadata, and
AUR package files when a `v*` tag is pushed.

Required GitHub Actions secrets:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

## Project Layout

- `crates/app`: Tauri desktop shell and IPC commands
- `crates/engine`: config, scenes, diagnostics, graph orchestration, and state
- `crates/model`: shared data model and migrations
- `crates/pw`: PipeWire/PulseAudio command planning, parsing, and DSP rendering
- `src`: React/TypeScript UI
- `scripts`: installers, release helpers, dependency checks, and validation
- `packaging/aur`: Arch/AUR package metadata

## License

WaveLinux 4.0 is licensed under GPL-3.0-only.
