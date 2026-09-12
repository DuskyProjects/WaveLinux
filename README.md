# WaveLinux

<!-- Keep this screenshot in the README as the permanent project preview. -->
<img width="1917" height="1093" alt="WaveLinux mixer" src="https://github.com/user-attachments/assets/63e32eed-16fe-43be-b86c-6b172a88f3bb" />

WaveLinux is a Linux-first creator audio mixer built with Rust, Tauri, React,
TypeScript, and PipeWire. WaveLinux 6 provides persistent virtual channels,
Monitor and Stream mixes, automatic application routing, live meters, and a
native real-time microphone DSP core.

WaveLinux 6 replaces a local WaveLinux5 installation and intentionally uses a
new `wavelinux6` application, configuration, and PipeWire namespace.

## Features

- Six persistent input buses with per-Monitor and per-Stream levels.
- Event-driven application routing with saved routing rules.
- Jack-aware microphone and speaker selection for USB, Bluetooth, HDA jack,
  and internal devices.
- Native RNNoise, DeepFilterNet 3, high-pass, eight-band EQ, compressor, gate, limiter, and
  Karaoke Stage processing in `wavelinux6-audio-core`.
- Stable `wavelinux6-mic` and `wavelinux6_mix_stream_source` recording sources.
- Live effect parameter and topology updates without replacing public client
  nodes.
- Adaptive 28/40/60/80/100/120 ms core buffering and Health diagnostics.
- AppImage, deb, rpm, and AUR packaging with host-compatible PipeWire use.

WaveLinux 6.1.0 adds **DeepFilterNet 3** as a second noise-cleanup
option. Open your microphone’s **FX**, select **DeepFilterNet 3**, and start with
**Gentle** for faint hiss or fan noise. Choose Balanced or Strong if needed.
Selecting it replaces RNNoise on that channel. Processing and the model are
included locally; no Python setup or model download is needed.

The EQ’s gray spectrum shows the selected channel **after its effects**.
The compressor shows that effect’s live input, output and gain reduction for
the selected channel, with a vertical threshold control.

## Install or update

**Copy and paste this into a terminal on your Linux desktop:**

```bash
curl -fL https://github.com/DuskyProjects/WaveLinux/releases/latest/download/install.sh -o wavelinux6-install.sh \
  && bash wavelinux6-install.sh
```

This downloads the latest stable `.sh` installer, verifies its checksum, installs
missing dependencies, and starts WaveLinux. Run the same command to update an
existing WaveLinux 6 installation; your saved settings are kept. No source
checkout, Rust, Node.js, or Yarn is needed.

Run it as your **normal desktop user**. The installer asks for administrator
permission only when system dependencies need installing.

Prefer to download it yourself? Get
[WaveLinux6_amd64_Installer.sh](https://github.com/DuskyProjects/WaveLinux/releases/latest/download/WaveLinux6_amd64_Installer.sh)
from the [latest release](https://github.com/DuskyProjects/WaveLinux/releases/latest),
then run `bash ~/Downloads/WaveLinux6_amd64_Installer.sh`.

### Supported systems

The installer supports **64-bit Intel/AMD Linux desktops** with PipeWire:

- Debian 13, Ubuntu 24.04, and compatible `apt` distributions;
- Fedora and compatible `dnf` distributions;
- Arch Linux, CachyOS, Manjaro, EndeavourOS, and compatible `pacman` distributions;
- openSUSE using `zypper`.

It checks the installed app and audio services before reporting success. If
something fails, it identifies the missing component and points to logs under
`~/.config/wavelinux6`. See [Troubleshooting](docs/troubleshooting.md).

<details>
<summary>Other installation options</summary>

After downloading `wavelinux6-install.sh` with the command above:

```bash
# Install without starting the app.
bash wavelinux6-install.sh --no-launch

# Preview the selected download without installing anything.
bash wavelinux6-install.sh --dry-run

# Install a specific release.
bash wavelinux6-install.sh --tag v6.1.0

# Native packages for Debian/Ubuntu or Fedora.
bash wavelinux6-install.sh --tag v6.1.0 --format deb
bash wavelinux6-install.sh --tag v6.1.0 --format rpm
```

The release also includes a direct AppImage and AUR metadata. openSUSE should
use the `.sh` installer; the RPM uses Fedora package names.

To verify a manually downloaded standalone installer before running it:

```bash
curl -fLO https://github.com/DuskyProjects/WaveLinux/releases/download/v6.1.0/WaveLinux6_amd64_Installer.sh
curl -fLO https://github.com/DuskyProjects/WaveLinux/releases/download/v6.1.0/SHA256SUMS
grep 'WaveLinux6_amd64_Installer.sh$' SHA256SUMS | sha256sum -c - \
  && bash WaveLinux6_amd64_Installer.sh
```

</details>

### Building the standalone installer

A maintainer with a fully working local build can create one self-extracting
installer containing that exact AppImage, audio core, peripheral helper,
launcher, icons, profiles, dependency detector, and installation scripts:

```bash
yarn install
yarn desktop:build
bash scripts/build-standalone-installer.sh
```

The generated file is:

```text
dist/WaveLinux6_<version>_amd64_Installer.sh
```

The generated versioned file and stable alias are independently extractable and
contain payload checksums. Install either as the normal desktop user:

```bash
chmod +x WaveLinux6_<version>_amd64_Installer.sh
./WaveLinux6_<version>_amd64_Installer.sh
```

The WaveLinux application payload is embedded in the installer, so the target
computer does not need the source tree, Rust, Node.js, or Yarn. The installer
automatically detects `apt`, `dnf`, `pacman`, or `zypper` and may use the
computer's configured package repositories to obtain missing system
dependencies.

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
`pw-cli`, `pw-dump`, `pw-metadata`, and `pw-top`, plus Procps (`ps` and
`pgrep`) and the normal GTK/WebKit desktop runtime. Standard
effects and RNNoise are bundled for release builds; installing microphone
effects must not require administrator access.

AppImages deliberately use the host PipeWire client stack. Bundling a partial
PipeWire or SPA module tree can make streams and meters disappear when its
version differs from the host daemon.

## License

WaveLinux is licensed under GPL-3.0-only. See [LICENSE](LICENSE) for the license
and open-source credits.
