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

## Highlights

- PipeWire/WirePlumber graph management with up to 5 virtual mixes.
- App routing, saved app rules, per-source Monitor and Stream faders, and live
  metering.
- Microphone processing through open LADSPA/PipeWire effects, including
  DeepFilterNet3, RNNoise, EQ, compressor, gate, and limiter entries.
- Hardware profile matching for common USB, Bluetooth, PCI, and platform audio
  endpoints.
- Optional Elgato Wave XLR controls when supported hardware is detected.
- Stream Deck-style HID and MIDI streamer device detection with safe mixer
  bindings.
- AppImage, deb, rpm, and AUR packaging with signed AppImage update checks.

## Install

Download the latest release:

```text
https://github.com/DuskyProjects/WaveLinux/releases/latest
```

Available formats are AppImage, deb, rpm, and AUR metadata. AppImage is the
portable build and primary self-update format.

For a local install from a checkout:

```bash
yarn install
yarn desktop:build
yarn install:local
```

Launch from the app menu or run:

```bash
wavelinux
```

## Documentation

- [Architecture notes](docs/architecture.md)
- [WaveLinux5 hardware-acceleration test line](docs/wavelinux5-hardware-acceleration.md)
- [Setup and development](docs/setup.md)
- [Theme authoring](docs/themes.md)
- [Test suites](docs/testing.md)
- [Hardware profile authoring](profiles/v1/README.md)
- [Release notes](RELEASE_NOTES.md)
- [License and open-source credits](LICENSE)

## Requirements

WaveLinux targets PipeWire-based Linux desktops. It expects PipeWire,
WirePlumber, pipewire-pulse, `pactl`, `wpctl`, `pw-cli`, `pw-dump`, and the
normal desktop WebKit/GTK runtime pieces for your distro. Optional effect
packages include SWH LADSPA plugins, RNNoise LADSPA/noise-suppression-for-voice,
and DeepFilterNet3 LADSPA/PipeWire plugin support.

## Updates

AppImage installs can check signed release metadata from inside Settings. Stable
updates use the latest GitHub release feed; package-managed installs should
update through their package manager.

Release history lives in [RELEASE_NOTES.md](RELEASE_NOTES.md). The GitHub
Releases page is kept focused on the latest downloadable release, while stable
git tags remain as source history.

## License

WaveLinux is licensed under GPL-3.0-only. See [LICENSE](LICENSE) for license
details and open-source credits.
