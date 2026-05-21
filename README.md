# WaveLinux 4.0

WaveLinux 4.0 is a new Linux-first, open-source creator audio mixer built with
Rust, Tauri, React, and PipeWire. It carries the WaveLinux name, but the app,
architecture, and product direction start fresh.

The v1 target is software parity for generic audio hardware and desktop audio:

- Up to 5 virtual mixes
- Up to 8 software channels plus 4 hardware input channels
- Hardware input routing for any PipeWire capture source: USB interfaces,
  headset microphones, capture cards, line inputs, Bluetooth sources, monitor
  sources, and other non-WaveLinux audio sources
- Unlimited app streams grouped into channels
- Per-channel/per-mix volume and mute
- Virtual mix sources for OBS, Discord, Teams, games, and browsers
- Open DSP chains through PipeWire filter-chain/LADSPA/LV2 replacements
- Scenes, diagnostics, startup restore, and packaged Linux desktop builds

Vendor-specific device features such as proprietary gain control, hardware clip
protection, Stream Deck integration, and marketplace effects are outside the v1
scope. WaveLinux should work with standard Linux audio devices instead of
special-casing one hardware family.

## Desktop Development

WaveLinux is a local Tauri desktop app. The browser/Vite target exists only as
a quick UI preview and should not be treated as the product runtime.

Host requirements:

- Rust 1.80 or newer
- Node + Yarn
- PipeWire, WirePlumber, and pipewire-pulse
- `pactl`, `wpctl`, `pw-cli`, and `pw-dump`

Install frontend dependencies:

```bash
yarn install
```

Run the core tests:

```bash
cargo test -p wavelinux-model -p wavelinux-pw -p wavelinux-engine
```

Run the desktop app:

```bash
yarn dev
```

Set `WAVELINUX_DRY_RUN=1` to inspect planned PipeWire commands without
creating or moving audio nodes.

Run the browser-only UI preview, when you explicitly want demo mode:

```bash
yarn web:dev
```

## Packaging

Tauri is configured for AppImage, deb, and rpm bundles:

```bash
yarn build
```

The build script sets `NO_STRIP=1` for linuxdeploy so AppImage bundling works
on modern distributions whose system libraries use newer ELF sections. It also
installs the freshly built AppImage into `~/.local/bin/wavelinux` and refreshes
the local desktop entry/icons.

Install the current build without rebuilding:

```bash
yarn install:local
```

Flatpak packaging is intentionally deferred because virtual audio device
management and PipeWire graph mutation are much more constrained in a sandbox.
