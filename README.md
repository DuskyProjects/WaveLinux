# WaveLinux

WaveLinux is a PyQt6 PipeWire mixer for Linux with Wave Link-style routing:

- Monitor mix (what you hear)
- Stream mix (`WaveLinux-Stream` for OBS)
- Per-channel MON/STR faders with optional link
- Per-app routing persistence
- Per-channel FX (RNNoise, HPF, EQ, compressor, gate, limiter)

## Release Model

WaveLinux is now AppImage-first for end users.

- The release AppImage is the primary multi-distro build for Arch, Fedora, Ubuntu, and other XDG desktops.
- In-app updates are notify-only. WaveLinux checks GitHub releases, then sends you to the release page instead of mutating local files in place.
- Host PipeWire tools are still required even when using the AppImage.

## Host Requirements

WaveLinux expects these host commands to exist:

- `pactl`
- `pw-dump`
- `wpctl`
- `parec`
- `pipewire`
- `pw-cli`

You also need a running PipeWire + WirePlumber session.

## AppImage Install

1. Download the latest `WaveLinux-<version>-x86_64.AppImage` from GitHub Releases.
2. Make it executable:

```bash
chmod +x WaveLinux-*.AppImage
```

3. Run it:

```bash
./WaveLinux-*.AppImage
```

4. Inside WaveLinux, open `Settings -> Updates` and use `Install This AppImage` if you want a desktop launcher copied into `~/.local/bin` and `~/.local/share/applications`.

WaveLinux uses host LADSPA plugins for FX by default, even in the AppImage. This is intentional: PipeWire loads those plugins from the host side, so AppImage-bundled LADSPA copies are less reliable across distros.

If you explicitly want to try bundled LADSPA plugins in a custom build, set `WAVELINUX_BUNDLE_LADSPA=1` when building and `WAVELINUX_ENABLE_BUNDLED_LADSPA=1` at runtime.

## Source Checkout Install

For development or running directly from a git checkout:

```bash
git clone https://github.com/DuskyProjects/WaveLinux.git
cd WaveLinux
./install.sh
```

That installs a desktop launcher pointing at the current checkout. On Arch-based systems you can also install known runtime dependencies automatically:

```bash
./install.sh --arch-deps
```

Run directly without installing a launcher:

```bash
python3 main.py
```

Remove launcher/config state:

```bash
./uninstall.sh
```

If you also want the Arch helper to remove optional FX packages:

```bash
./uninstall.sh --remove-arch-deps
```

## Arch Package

Arch users can still build the package:

```bash
makepkg -si
```

## Building the AppImage

The release AppImage is built with PyInstaller plus AppImageKit:

```bash
python3 -m pip install PyInstaller PyQt6
./scripts/build_appimage.sh
```

If you build from a virtualenv, point the script at that exact interpreter so
`PyInstaller` and `PyQt6` come from the same environment:

```bash
PYTHON_BIN=/path/to/venv/bin/python ./scripts/build_appimage.sh
```

Artifacts are written to `dist/`:

- `WaveLinux-<version>-x86_64.AppImage`
- `sha256sums.txt`

GitHub Actions release CI is defined in `.github/workflows/release.yml`.

## Paths

- Config: `~/.config/wavelinux/config.json`
- Log: `~/.config/wavelinux/wavelinux.log`
- FX logs: `~/.config/wavelinux/fx-logs/`

## OBS

1. Launch WaveLinux.
2. Add **Audio Input Capture** in OBS.
3. Select `WaveLinux-Stream` or the monitor variant you want.
