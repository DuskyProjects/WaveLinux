# WaveLinux
<!-- Keep this screenshot in the README as the permanent project preview. -->
<img width="1917" height="1093" alt="image" src="https://github.com/user-attachments/assets/63e32eed-16fe-43be-b86c-6b172a88f3bb" />


WaveLinux is a Linux-first creator audio mixer built with Rust, Tauri, React,
TypeScript, and PipeWire. It is a native desktop app for software mixing,
hardware-aware routing, creator audio workflows, and multiple selectable UI
surfaces.

The goal is Wave Link-style software mixing for Linux: virtual source faders,
separate Monitor and Stream mixes, app routing, microphone processing,
diagnostics, tray behavior, package builds, open-source DSP replacements, and
frontend surfaces that can evolve without coupling theme work to the Rust audio
engine.

WaveLinux is not an Elgato hardware control panel. Vendor-specific microphone
features, Stream Deck integration, proprietary marketplace effects, and
hardware Clipguard behavior are intentionally out of scope for WaveLinux.
Standard Linux audio hardware is the target.

## Highlights

- WaveLinux now has a multi-surface UI system. The original WaveLinux interface
  remains available as `wavelink2`, while the newer Wave Link 3-style matrix is
  available in light and dark variants.
- Custom UI theme JSON files can be dropped into the app theme folder and
  selected from Settings without editing the Rust audio engine or rebuilding
  WaveLinux.
- Hardware profiles are individual JSON device files under `profiles/v1`
  with schema docs, examples, local overrides, signed remote bundle support,
  install-time hardware prewarm, and an editable safe generic fallback profile.
- Profiles stay audio-only and cannot execute commands, write host config, or
  bypass Bluetooth/profile guardrails.
- Bluetooth headset policy protects A2DP playback. HFP/HSP microphones are
  refused as an optimization when they would destroy playback quality, and
  capture is routed to DJI, USB, internal, or other non-Bluetooth microphones
  when available.
- The Settings page contains Profiles and Health tabs, while the main mixer
  navigation stays focused on mixing, routing, effects, and settings.
- Profile and route selectors are searchable, anchored to their controls, and
  sized for readable hardware/profile names.
- Mixer commands use optimistic UI updates and coalesced backend refreshes so
  faders, toggles, and low-latency settings no longer freeze the app while audio
  commands run.
- Hardware input meters show the real selected microphone before effects,
  then the microphone-only post-FX signal when effects are active.
- If an effect-chain helper exits after restart, WaveLinux repairs the
  stale route instead of leaving app routing stuck on a missing `wavelinux-mic`
  source.
- Bluetooth profile floors target the lowest stable profile-defined buffer
  range observed locally, rather than falling back below AAC or pushing latency
  high enough to exhaust PipeWire buffers.
- Auto hardware repair updates only real device routes, keeping effect
  chains and app/channel routes alive during output reconnects.
- Channel Stream/Monitor meters follow the effective channel-send and
  destination mix/master level, so source strip VUs and mix VUs agree.
- The effects microphone export is named `wavelinux-mic` / `WaveLinux-mic` so
  Discord, OBS, and browser capture menus are easier to understand.
- Startup repair resets real non-Bluetooth microphone sources to 100% and
  unmuted, while ignoring WaveLinux virtual/monitor sources.
- Startup skips cleanup and repair when the existing graph already matches the
  current profiles, routes, and effect-chain revision.
- Common Bluetooth headset profiles carry conservative A2DP latency floors per
  codec, and the editable generic fallback profile uses a safer Bluetooth floor
  for unknown devices.

Release history lives in `RELEASE_NOTES.md`.

## Features

- PipeWire and WirePlumber audio graph management
- Up to 5 virtual mixes
- Up to 8 software routes plus hardware input routes
- Two faders per source: Monitor and Stream
- Built-in UI surfaces for the original WaveLinux/Wave Link 2-style mixer and
  the Wave Link 3-style matrix mixer
- Persistent interface selection with custom JSON theme files loaded from the
  app config theme folder
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
- Diagnostics, sound checks, graph repair, and cleanup
- Close-to-tray desktop behavior with tray Quit for full graph cleanup
- AppImage, deb, rpm, and AUR packaging
- Signed in-app update checks for AppImage installs

## Supported Platforms

WaveLinux targets PipeWire-based Linux desktops.

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
~/.local/share/wavelinux/WaveLinux_*_amd64.AppImage
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

## Interface Themes

WaveLinux separates UI selection from the audio engine. The Settings page has
an Interface selector under General with built-in choices:

- `WaveLinux Original (Wave Link 2-style)`: the original WaveLinux mixer
  surface
- `Wave Link 3-style Matrix`: the newer matrix mixer surface
- `Wave Link 3-style Matrix Dark`: the same matrix workflow with dark tokens

The selected interface is saved and restored on the next launch. Theme files do
not alter mixer config, PipeWire graph behavior, hardware profiles, effects, or
backend commands.

Custom themes are JSON files loaded from the app config `themes` directory.
Open Settings > General > Interface > Folder to reveal the directory, add one
theme file per `.json`, then press Refresh or restart WaveLinux. On current
Linux desktop builds the folder is typically:

```bash
~/.config/io.github.duskyprojects.WaveLinux/themes
```

Custom files choose one of the shipped UI surfaces and override WaveLinux CSS
tokens such as background, panel, accent, text, border, danger, and effect LED
colors. See `docs/themes.md` for the full file format, examples, and authoring
notes.

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
VERSION="$(node -p "require('./package.json').version")"
python3 scripts/build-updater-manifest.py \
  --artifact "target/release/bundle/appimage/WaveLinux_${VERSION}_amd64.AppImage.tar.gz" \
  --version "$VERSION" \
  --repo DuskyProjects/WaveLinux \
  --tag "v$VERSION" \
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
- `crates/engine`: config, diagnostics, graph orchestration, and state
- `crates/model`: shared data model and migrations
- `crates/pw`: PipeWire/PulseAudio command planning, parsing, and DSP rendering
- `profiles/v1`: hardware profile schema, examples, author docs, and device seeds
- `src`: React/TypeScript UI
- `docs/themes.md`: custom UI theme file format and authoring guide
- `scripts`: installers, release helpers, dependency checks, and validation
- `packaging/aur`: Arch/AUR package metadata

## License

WaveLinux is licensed under GPL-3.0-only.
