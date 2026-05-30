# WaveLinux
<!-- Keep this screenshot in the README as the permanent project preview. -->
<img width="1917" height="1093" alt="image" src="https://github.com/user-attachments/assets/63e32eed-16fe-43be-b86c-6b172a88f3bb" />


WaveLinux is a Linux-first creator audio mixer built with Rust, Tauri, React,
TypeScript, and PipeWire. It creates virtual sources and mixes, routes app
audio, applies open-source microphone effects, and provides selectable UI
surfaces for Linux desktop audio workflows.

WaveLinux is not an Elgato hardware control panel. Vendor-specific microphone
features, Stream Deck integration, proprietary marketplace effects, and
hardware Clipguard behavior are intentionally out of scope for WaveLinux.
Standard Linux audio hardware is the target.

## Features

- PipeWire and WirePlumber audio graph management
- Up to 5 virtual mixes
- Up to 8 software routes plus hardware input routes
- Two faders per source: Monitor and Stream
- Built-in UI surface `wavelink3_dark`: Wave Link 3-style matrix, dark,
  default for new installs
- Built-in UI surface `wavelink3`: Wave Link 3-style matrix, light
- Built-in UI surface `wavelink2`: original WaveLinux/Wave Link 2-style mixer
- Custom JSON theme files loaded from the app config theme folder
- Virtual mix outputs for OBS, Discord, browsers, games, meetings, and tools
- App stream discovery, saved routing, app identity overrides, and offline rules
- Automatic monitor output policy: Bluetooth, USB audio, jack, then speakers
- Automatic microphone policy: USB mic, microphone jack, built-in mic, then
  Bluetooth input
- Bluetooth A2DP profile protection when possible
- Hardware profile matching for common USB, Bluetooth, PCI, and platform audio
  endpoints
- Editable safe generic default profile plus per-device manual profile assignment
- Local profile overrides that survive app updates without executable hooks
- Signed remote hardware profile downloads from GitHub Releases
- Hotplug recovery for inputs and outputs
- Real channel/mix metering with stale-sample decay
- Per-source effect chains through PipeWire filter-chain and LADSPA/open plugins
- DeepFilterNet3, RNNoise, high-pass, EQ, compressor, gate, and limiter catalog entries
- Diagnostics, sound checks, graph repair, and cleanup
- Close-to-tray desktop behavior with tray Quit for full graph cleanup
- AppImage, deb, rpm, and AUR packaging
- Signed in-app update checks for AppImage installs

Release history lives in `RELEASE_NOTES.md`.

## Requirements

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
- DeepFilterNet3 LADSPA/PipeWire plugin support when available for your distro

## Install

Download a release artifact from GitHub Releases:

```bash
https://github.com/DuskyProjects/WaveLinux/releases
```

Available formats:

- AppImage: portable desktop build and the primary self-update format
- deb: Debian and Ubuntu-family package
- rpm: Fedora/openSUSE-family package
- AUR metadata: Arch package recipe

For a local install from a checkout:

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

## Run

Launch WaveLinux from the app menu or:

```bash
wavelinux
```

WaveLinux opens without mutating the audio graph unless startup restore is
enabled. Use Start Audio to create the virtual devices, Repair to rebuild stale
routes, and Stop/Cleanup to unload managed nodes.

Closing the window hides it to the tray so audio can keep running. Use Quit
from the tray menu to exit fully and remove WaveLinux-managed PipeWire nodes.

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
The desktop app also exposes the same flow in Settings -> Health -> Effect
Availability. Use Install FX to install missing optional LADSPA plugins through
the detected package manager, then WaveLinux re-checks that DeepFilterNet3,
RNNoise, and SWH dynamics are actually available.

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

Profile authoring files:

- `profiles/v1/schema.json`
- `profiles/v1/examples`
- `profiles/v1/README.md`

## Interface Themes

WaveLinux separates UI selection from the audio engine. The Settings page has
an Interface selector under General with built-in choices:

- `WaveLinux Original (Wave Link 2-style)`: the original WaveLinux mixer
  surface
- `Wave Link 3-style Matrix`: the newer matrix mixer surface
- `Wave Link 3-style Matrix Dark`: the same matrix workflow with dark tokens

New installs default to `Wave Link 3-style Matrix Dark`. The selected interface
is saved and restored on the next launch. Theme files do not alter mixer config,
PipeWire graph behavior, hardware profiles, effects, or backend commands.

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
Package-managed installs should update through their package manager.

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

## Build

Build web UI only:

```bash
yarn web:build
```

Build local desktop bundles:

```bash
yarn desktop:build
```

Stage AUR files:

```bash
yarn aur:build
```

Build signed release bundles and updater signatures:

```bash
yarn release:key
yarn desktop:release
```

The GitHub release workflow builds AppImage, deb, rpm, updater metadata, and AUR
package files when a `v*` tag is pushed.

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
