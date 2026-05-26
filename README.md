# WaveLinux 4.1
<!-- Keep this screenshot in the README as the permanent project preview. -->
<img width="1895" height="1108" alt="image" src="https://github.com/user-attachments/assets/22b52de8-d97d-4664-9772-c1c358122144" />

WaveLinux 4.1 is a Linux-first creator audio mixer built with Rust, Tauri,
React, TypeScript, and PipeWire. It is a fresh application that carries the
WaveLinux name forward while replacing the old Python implementation with a
new native desktop stack.

The goal is Wave Link-style software mixing for Linux: virtual source faders,
separate Monitor and Stream mixes, app routing, microphone processing, scenes,
diagnostics, tray behavior, package builds, and open-source DSP replacements.

WaveLinux is not an Elgato hardware control panel. Vendor-specific microphone
features, Stream Deck integration, proprietary marketplace effects, and
hardware Clipguard behavior are intentionally out of scope for WaveLinux.
Standard Linux audio hardware is the target.

## WaveLinux 4.1 Release Notes

WaveLinux 4.1 is the background optimization and hardware profile release. It
adds safe audio-only hardware profiles, Bluetooth headset protection, searchable
profile assignment, lower-lag mixer controls, better metering, and release
packaging updates without changing the mixer-first workflow.

4.1.1 adds a focused XM4 stability profile update: the Sony WH-1000XM4 profile
now prefers AAC/SBC-XQ/SBC before LDAC, raises the Bluetooth latency floor to
120 ms, and treats high-bitrate LDAC as something to avoid unless the link is
excellent. It also publishes signed hardware profile assets with GitHub
releases so profile fixes can land without waiting for a larger feature build.

4.1.2 is a stability and profile-authority fix release. It removes the
refresh-loop stalls caused by large `pactl` output, keeps faders and toggles
responsive under drag spam, makes the hardware input VU visually swap from raw
mic to post-FX `wavelinux-mic`, and lets active hardware profiles decide route
latency floors before any generic fallback is used.

Highlights:

- Hardware profiles are now individual JSON device files under `profiles/v1`
  with schema docs, examples, local overrides, signed remote bundle support,
  install-time hardware prewarm, and an editable safe generic fallback profile.
- Profiles stay audio-only and cannot execute commands, write host config, or
  bypass Bluetooth/profile guardrails.
- Bluetooth headset policy protects A2DP playback. HFP/HSP microphones are
  refused as an optimization when they would destroy playback quality, and
  capture is routed to DJI, USB, internal, or other non-Bluetooth microphones
  when available.
- The Settings page now contains Profiles and Health tabs, while the main mixer
  navigation stays focused on mixing, routing, effects, scenes, and settings.
- Profile and route selectors are searchable, anchored to their controls, and
  sized for readable hardware/profile names.
- Mixer commands use optimistic UI updates and coalesced backend refreshes so
  faders, toggles, and low-latency settings no longer freeze the app while audio
  commands run.
- Hardware input meters now show the real selected microphone before effects,
  then the microphone-only post-FX signal when effects are active.
- Channel Stream/Monitor meters now follow the effective channel-send and
  destination mix/master level, so source strip VUs and mix VUs agree.
- The effects microphone export is named `wavelinux-mic` / `WaveLinux-mic` so
  Discord, OBS, and browser capture menus are easier to understand.
- Startup repair resets real non-Bluetooth microphone sources to 100% and
  unmuted, while ignoring WaveLinux virtual/monitor sources.
- Release, local-native, AUR, and profile asset scripts were updated for the
  new profile system and 4.1 packaging.

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
- Hardware profile matching for common USB, Bluetooth, PCI, and platform audio endpoints
- Editable safe generic default profile plus per-device manual profile assignment
- Local profile overrides that survive app updates without executable hooks
- Signed remote hardware profile downloads from GitHub Releases
- Hotplug recovery for inputs and outputs
- Real channel/mix metering with stale-sample decay
- Per-source effect chains through PipeWire filter-chain and LADSPA/open plugins
- DeepFilterNet, RNNoise, high-pass, EQ, compressor, gate, and limiter catalog entries
- Scenes, setup templates, diagnostics, sound checks, graph repair, and cleanup
- Close-to-tray desktop behavior with tray Quit for full graph cleanup
- AppImage, deb, rpm, and AUR packaging
- Signed in-app update checks for AppImage installs

## Supported Platforms

WaveLinux 4.1 targets PipeWire-based Linux desktops.

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
~/.local/share/wavelinux/WaveLinux_4.1.2_amd64.AppImage
~/.local/bin/wavelinux
```

It also installs the desktop entry and icons under the usual XDG user paths.
By default, local development installs seed the checkout's audio hardware
profiles into:

```bash
~/.config/wavelinux/hardware-profiles/v1/local/wavelinux-local-seed
```

The installer also runs a hardware profile prewarm check so WaveLinux can fetch
signed remote profile bundles for detected audio devices when release assets
are available.

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

## Hardware Profiles

WaveLinux uses background hardware profiles to choose safer default routing,
latency, codec, and Bluetooth microphone behavior without requiring new mixer
controls. Profiles are data-only JSON files; they cannot run commands, write
host audio configuration, or bypass hard safety guardrails.

Profile resolution prefers the safest local data first:

- Local user profiles in `~/.config/wavelinux/hardware-profiles/v1/local`
- Signed remote profiles cached from GitHub Releases
- The editable safe generic default profile, `default.generic-audio`

The Settings page includes Profiles under its tab bar. It lists real hardware
audio devices only, lets you assign a profile to a device, and edits the
currently selected profile in the side editor. Editing a downloaded or seeded
profile creates a safe local override under:

```bash
~/.config/wavelinux/hardware-profiles/v1/local/wavelinux-user-overrides
```

Unknown devices fall back to `default.generic-audio`. That profile is intended
to be conservative enough for nearly any audio endpoint and can be edited from
the same Profiles view.

Bluetooth headset profiles protect playback quality. If a headset microphone
would force HFP/HSP and degrade A2DP playback, WaveLinux keeps the headset on
A2DP and routes capture to a non-Bluetooth microphone when one is available.
HFP/HSP remains a compatibility fallback, not a performance optimization.

Profile authoring files live in `profiles/v1`:

- `profiles/v1/schema.json`
- `profiles/v1/examples`
- `profiles/v1/README.md`

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
  --artifact target/release/bundle/appimage/WaveLinux_4.1.2_amd64.AppImage.tar.gz \
  --version 4.1.2 \
  --repo DuskyProjects/WaveLinux \
  --tag v4.1.2 \
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
- `profiles/v1`: hardware profile schema, examples, author docs, and device seeds
- `src`: React/TypeScript UI
- `scripts`: installers, release helpers, dependency checks, and validation
- `packaging/aur`: Arch/AUR package metadata

## License

WaveLinux 4.1 is licensed under GPL-3.0-only.
