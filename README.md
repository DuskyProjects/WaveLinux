# WaveLinux 4.0

WaveLinux 4.0 is a new Linux-first, open-source creator audio mixer built with
Rust, Tauri, React, and PipeWire. It carries the WaveLinux name, but the app,
architecture, and product direction start fresh.

The v1 target is software parity for generic microphones and desktop audio:

- Up to 5 virtual mixes
- Up to 8 software/non-Wave channels
- Unlimited app streams grouped into channels
- Per-channel/per-mix volume and mute
- Virtual mix sources for OBS, Discord, Teams, games, and browsers
- Open DSP chains through PipeWire filter-chain/LADSPA/LV2 replacements
- Scenes, diagnostics, startup restore, and packaged Linux desktop builds

Hardware-specific Elgato features such as Clipguard, Wave device gain control,
Wave FX Processor, Stream Deck integration, and Marketplace effects are outside
the v1 scope.

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
