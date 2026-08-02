# WaveLinux

<!-- Keep this screenshot in the README as the permanent project preview. -->
<img width="1917" height="1093" alt="WaveLinux mixer" src="https://github.com/user-attachments/assets/63e32eed-16fe-43be-b86c-6b172a88f3bb" />

WaveLinux is a Linux-first creator audio mixer built with Rust, Tauri, React,
TypeScript, and PipeWire. WaveLinux 6 provides persistent virtual channels,
Monitor and Stream mixes, automatic application routing, live meters, and a
native real-time microphone DSP core.

WaveLinux 6 is currently an alpha development line. It replaces a local
WaveLinux5 installation and intentionally uses a new `wavelinux6` application,
configuration, and PipeWire namespace.

## Features

- Six persistent input buses with per-Monitor and per-Stream levels.
- Event-driven application routing with saved routing rules.
- Jack-aware microphone and speaker selection for USB, Bluetooth, HDA jack,
  and internal devices.
- Native RNNoise, high-pass, eight-band EQ, compressor, gate, limiter, and
  Karaoke Stage processing in `wavelinux6-audio-core`.
- Stable `wavelinux6-mic` and `wavelinux6_mix_stream_source` recording sources.
- Live effect parameter and topology updates without replacing public client
  nodes.
- Adaptive 28/40/60/80/100/120 ms core buffering and Health diagnostics.
- AppImage, deb, rpm, and AUR packaging with host-compatible PipeWire use.

DeepFilterNet is not included. Existing DeepFilterNet config entries are
migrated to RNNoise and it does not appear in the effect catalog.

## Local Build

WaveLinux requires a PipeWire desktop session with `pipewire-pulse` and
WirePlumber. From a checkout:

```bash
yarn install
yarn desktop:build
yarn install:local
wavelinux6
```

The local install uses:

```text
~/.local/bin/wavelinux6
~/.local/bin/wavelinux6-audio-core
~/.local/share/wavelinux6/
~/.config/wavelinux6/
```

Run the complete safe test suite with:

```bash
yarn test:all
```

Build distributable AppImage, deb, and rpm artifacts in the pinned Debian
builder, enforce the glibc compatibility ceiling, and stage the exact distro
smoke assets with:

```bash
bash scripts/build-portable.sh
```

Live PipeWire tests are opt-in because they create and remove nodes in the
current user session. See [Test suites](docs/testing.md).

## Documentation

- [Architecture](docs/architecture.md)
- [Native audio core](docs/audio-core.md)
- [Setup and development](docs/setup.md)
- [Testing](docs/testing.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Theme authoring](docs/themes.md)
- [Hardware profile runtime](docs/profiles.md)
- [Hardware profile authoring](profiles/v1/README.md)
- [DSP acceleration policy](docs/acceleration.md)
- [Peripheral integrations](docs/integrations.md)
- [WaveLinux 5 migration](docs/migration.md)
- [Release procedure](docs/releasing.md)
- [Release history](RELEASE_NOTES.md)

## Runtime Requirements

WaveLinux expects PipeWire, WirePlumber, `pipewire-pulse`, `pactl`, `wpctl`,
`pw-cli`, `pw-dump`, `pw-metadata`, and `pw-top`, plus the normal GTK/WebKit
desktop runtime. Standard
effects and RNNoise are bundled for release builds; installing microphone
effects must not require administrator access.

AppImages deliberately use the host PipeWire client stack. Bundling a partial
PipeWire or SPA module tree can make streams and meters disappear when its
version differs from the host daemon.

## License

WaveLinux is licensed under GPL-3.0-only. See [LICENSE](LICENSE) for the license
and open-source credits.
