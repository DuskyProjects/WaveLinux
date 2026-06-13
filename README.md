# WaveLinux
<!-- Keep this screenshot in the README as the permanent project preview. -->
<img width="1917" height="1093" alt="image" src="https://github.com/user-attachments/assets/63e32eed-16fe-43be-b86c-6b172a88f3bb" />


WaveLinux is a Linux-first creator audio mixer built with Rust, Tauri, React,
TypeScript, and PipeWire. It creates virtual sources and mixes, routes app
audio, applies open-source microphone effects, and provides selectable UI
surfaces for Linux desktop audio workflows.

WaveLinux targets standard Linux audio hardware first, with optional
device-specific controls where the protocol is open enough to support safely.
Stream Deck integration, proprietary marketplace effects, and hardware
Clipguard behavior remain out of scope.

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
- Elgato Wave XLR gain, mute, headphone volume, and low-impedance controls when
  supported Elgato hardware is detected
- Stream Deck-style HID and MIDI streamer hardware detection with mixer binding
  profiles that stay hidden until supported hardware is connected
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
- Host display/session pieces: Xwayland when needed, Mesa/GL/GBM/DRM libraries,
  fonts, and a desktop portal backend
- Linux hidraw/sysfs access for Stream Deck-style streamer device detection
- ALSA sequencer client listing and `aseqdump` for MIDI streamer device binding

Recommended effect packages:

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

AppImage releases bundle WebKitGTK/GTK, GStreamer media support, WebKit sandbox
helpers, libusb for optional Elgato controls, and any supported LADSPA effect
plugins present on the release builder. First launch still runs a runtime
preflight before WebKit starts for host-bound pieces such as PipeWire, desktop
display/GL libraries, fonts, portals, and distro-provided effect packages. If a
fresh Linux install is missing those packages, WaveLinux prompts for admin
permission through the desktop and installs the distro packages it can verify
with apt, dnf, pacman, or zypper. If core runtime setup cannot complete, it exits
with a copyable manual command instead of opening a half-broken WebKit process;
missing effect plugins are reported and can be installed later.

## Run

Launch WaveLinux from the app menu or:

```bash
wavelinux
```

WaveLinux opens without mutating the audio graph unless startup restore is
enabled. When startup restore is enabled, WaveLinux creates or restores its
virtual devices on launch and removes WaveLinux-managed PipeWire nodes when
the app fully quits.

Closing the window hides it to the tray so audio can keep running. Use Quit
from the tray menu to exit fully and remove WaveLinux-managed PipeWire nodes.

## Dependency Checks

Check runtime dependencies and effect plugins:

```bash
yarn deps:check
```

Install missing runtime dependencies and effect packages when a
supported package manager is available:

```bash
yarn deps:install
```

Install only effect packages:

```bash
yarn effects:install
```

The dependency installer checks first. Source-tree installs do not install
packages unless you explicitly run the install command or set the corresponding
environment flags. AppImage releases run a slimmer host runtime check on first
launch and can install required system packages before WebKit starts. To run that
preflight manually:

```bash
./WaveLinux_4.3.1_amd64.AppImage --check-runtime-dependencies
./WaveLinux_4.3.1_amd64.AppImage --install-runtime-dependencies
```

Use `WAVELINUX_SKIP_RUNTIME_INSTALL=1` to skip the AppImage preflight, or
`WAVELINUX_ASSUME_RUNTIME_DEPS=1` when a packager has already provided all host
runtime dependencies. The desktop app also exposes the plugin flow in
Settings -> Health -> Effect Availability. Use Install FX to install missing
LADSPA plugins through the detected package manager, then WaveLinux re-checks
that DeepFilterNet3, RNNoise, and SWH dynamics are actually available.

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

## Elgato Controls

When WaveLinux detects an Elgato audio device, Settings shows an Elgato tab.
Wave XLR hardware controls are available there for microphone gain, mute,
headphone volume, and low-impedance mode. The tab is hidden on systems without
detected Elgato hardware, and the libusb control path is loaded only after a
supported Wave XLR is detected. The Wave XLR USB protocol details are based on
the OpenWave project: https://github.com/rikkichy/openwave

For zero-latency self monitoring on a Wave XLR, enable Hardware direct mic
monitor in Settings -> Sync and listen through the Wave XLR headphone output.
WaveLinux keeps routing the microphone to stream/record mixes, but skips the
software mic route into the Monitor mix so hardware sidetone is not doubled with
a delayed software copy.

## Streamer Device Bindings

When WaveLinux detects supported streamer hardware, Settings shows a Streamers
tab. It is hidden when no supported Stream Deck-style HID, RODE/GoXLR-style
MIDI, or recognized streamer audio/control device is connected. Device discovery
uses cheap Linux sysfs, hidraw, PipeWire, and ALSA sequencer inspection first;
WaveLinux only keeps hidraw devices open or starts `aseqdump` MIDI capture when
a detected device has enabled bindings.

Bindings can target mixer mute and volume controls, source-to-mix controls, and
the safer stale-audio prune action. Full audio graph start and shutdown stay tied
to opening and quitting WaveLinux. A safe preset is created the first time a
bindable device is seen, and hardware access reports permission, busy, missing
runtime, or unsupported protocol states instead of showing inactive controls.

For hidraw permissions, packaged installs may include:

```text
packaging/udev/70-wavelinux-streamer-devices.rules
```

After installing udev rules manually, reload rules with your distribution's
standard `udevadm control --reload-rules && udevadm trigger` flow and reconnect
the device.

## Testing Health Reports

For beta testing and GitHub issues, use `Settings -> Health -> Testing Health
Report`. It creates one copyable Markdown block with engine state, update
channel/feed/status, diagnostics, audio device summaries, Elgato detection,
streamer-device detection, and recent debug-log lines.

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
The updater includes a Beta updates checkbox that tracks the single moving
`prerelease` feed for WaveLinux Testing; leave it unchecked to stay on stable
releases.
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

## Open Source Credits

WaveLinux is GPL-3.0-only, but it is built with and integrates with other open
source projects. This acknowledgement is intentionally human-readable; release
builders should still preserve third-party license files for bundled Cargo and
npm dependencies from `Cargo.lock` and `yarn.lock`.

Direct code, protocol, and runtime dependencies:

| Project | License | WaveLinux use |
| --- | --- | --- |
| [OpenWave](https://github.com/rikkichy/openwave) | MIT | Wave XLR USB control-transfer protocol notes and behavior used for optional Elgato controls. |
| [Elgato Stream Deck HID documentation](https://docs.elgato.com/streamdeck/hid/intro) | Reference documentation | Stream Deck HID device model and packet behavior used to guide lazy HID detection and raw report binding IDs. |
| [Tauri](https://tauri.app/) and Tauri plugins | MIT OR Apache-2.0 | Desktop shell, IPC, tray, updater, opener, shell, and single-instance support. |
| [WebKitGTK](https://webkitgtk.org/) and [GTK](https://gtk.org/) | LGPL-2.1-or-later plus WebKit third-party notices | Linux webview/runtime stack used by Tauri desktop builds. |
| [React](https://react.dev/) and React DOM | MIT | Frontend UI framework. |
| [TypeScript](https://www.typescriptlang.org/) | Apache-2.0 | Frontend type system and compiler. |
| [Vite](https://vite.dev/) and `@vitejs/plugin-react` | MIT | Frontend development server and production build tooling. |
| [Lucide](https://lucide.dev/) / `lucide-react` | ISC | UI icon set. |
| [PipeWire](https://pipewire.org/) and `pipewire-rs` / libspa bindings | MIT | Linux audio graph integration, device discovery, routing, and metering. |
| [WirePlumber](https://pipewire.pages.freedesktop.org/wireplumber/) | MIT | Host session-manager integration target for PipeWire desktops. |
| [bubblewrap](https://github.com/containers/bubblewrap) and [xdg-dbus-proxy](https://github.com/flatpak/xdg-dbus-proxy) | LGPL-2.0-or-later / LGPL-2.1-or-later | WebKitGTK sandbox helpers staged into AppImage releases when available from the build host. |
| [xdg-desktop-portal](https://github.com/flatpak/xdg-desktop-portal) | LGPL-2.1-or-later | Host desktop portal integration used by Linux desktop sessions. |
| [Mesa](https://www.mesa3d.org/), libdrm, libglvnd, and Xwayland | MIT/BSD/X11-style licenses | Host GL/EGL/GBM/DRM and Xwayland runtime pieces needed by WebKitGTK AppImage launches on common Linux desktops. |
| [GStreamer](https://gstreamer.freedesktop.org/) base/good plugins | LGPL-2.1-or-later | Host media stack used by WebKitGTK and desktop audio/video runtime dependencies. |
| [Noto Fonts](https://notofonts.github.io/) | OFL-1.1 | Host fallback fonts installed by dependency preflight on minimal desktops. |
| [libusb](https://github.com/libusb/libusb) | LGPL-2.1-or-later | Dynamically loaded shared library for optional Elgato Wave XLR controls; staged into AppImage releases when available from the build host. |
| [ALSA utilities](https://www.alsa-project.org/) (`aseqdump`) | GPL-2.0-or-later | Host MIDI event capture for connected streamer control surfaces, started only for enabled detected MIDI devices. |
| Rust support crates: `anyhow`, `base64`, `directories`, `libc`, `serde`, `serde_json`, `tempfile`, `thiserror`, `time`, `url`, `uuid` | MIT OR Apache-2.0 | Serialization, errors, paths, test files, URLs, timestamps, identifiers, and libc bindings. |
| Rust support crate: `include_dir` | MIT | Embeds packaged hardware profile assets. |
| Rust support crate: `libloading` | ISC | Lazy runtime loading for the optional libusb control path. |

Open-source integrations that WaveLinux can detect, configure, bundle when
available from the release builder, or install from distro packages:

| Project | License | Notes |
| --- | --- | --- |
| [SWH LADSPA plugins](https://github.com/swh/ladspa) | GPL-2.0 | Compressor, gate, and limiter plugin support bundled in AppImage releases when present on the builder, otherwise installed from distro packages. |
| [noise-suppression-for-voice](https://github.com/werman/noise-suppression-for-voice) / RNNoise | GPL-3.0 | RNNoise LADSPA noise suppression support bundled in AppImage releases when present on the builder, otherwise installed from distro packages. |
| [DeepFilterNet3 LADSPA/PipeWire plugins](https://github.com/Rikorose/DeepFilterNet) | MIT OR Apache-2.0 | DeepFilterNet noise suppression support bundled or installed when a distro package is available. |
| [OpenDeck](https://github.com/nekename/OpenDeck) | GPL-3.0-or-later | Open-source Linux Stream Deck implementation used as a compatibility and udev-permission reference. |
| [Bitfocus Companion](https://github.com/bitfocus/companion) | MIT | Open-source streamer surface ecosystem reference for Stream Deck, Loupedeck, X-keys, and similar devices. |
| [GoXLR Utility](https://github.com/GoXLR-on-Linux/goxlr-utility) | MIT | Open-source GoXLR control software reference for Linux behavior and control-surface expectations. |
| PulseAudio-compatible tools (`pactl`) and PipeWire tools (`wpctl`, `pw-cli`, `pw-dump`) | LGPL-2.1-or-later / MIT | Host command-line tools used for graph inspection, routing, and diagnostics. |

## License

WaveLinux is licensed under GPL-3.0-only.
