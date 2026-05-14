# WaveLinux

WaveLinux is a PyQt6 PipeWire mixer for Linux with Wave Link-style routing for
desktop audio, streaming, and microphone FX.

## Current Capabilities

- Separate `Monitor` and `Stream` mixes
- `WaveLinux-Stream` virtual source for OBS
- Single active mic model with dedicated hardware `MIC` gain control
- Virtual channels for app grouping and mix isolation
- Per-channel `MON` / `STR` faders, mute, link, and peak meter
- Per-app routing with persistence, offline volume presets, and app icons
- Manual app identity pin / merge / reset for stubborn Chromium/Electron-style apps
- Per-channel FX: RNNoise, high-pass, EQ, compressor, gate, limiter
- Scenes, quick-start setup templates, Health diagnostics, and launcher repair
- Default-driven device startup with conservative monitor/mic fallback and restore actions
- Responsive single-row mixer layout that expands on wide windows and compacts cleanly on short windows
- Verified AppImage self-update with signed manifest validation and rollback

## Runtime Model

WaveLinux is AppImage-first for end users.

- The GitHub release AppImage is the primary supported desktop build.
- On each fresh launch, `Monitor` starts from the current system default sink
  and the active mic starts from the current system default source.
- After startup, newly connected devices do not silently steal control. If the
  active monitor sink or mic disappears, WaveLinux fails over to a viable
  hardware device and surfaces a restore action in `Settings -> Health` when
  the displaced device returns.
- `Stream` stays explicit and does not automatically follow `Monitor`.
- AppImage installs can update themselves from `Settings -> Updates` using a
  signed release manifest, checksum validation, smoke test, install, and
  rollback backup.
- Source checkouts and local bundled builds can repair or reinstall their own
  launcher state from the app.
- Package-managed installs can still check verified releases, but they do not
  replace themselves with an AppImage in place.
- Host PipeWire tools are always required, even when running the AppImage.

## Host Requirements

WaveLinux expects these host commands to exist:

- `pactl`
- `pw-dump`
- `wpctl`
- `parec`
- `pipewire`
- `pw-cli`

You also need a running PipeWire + WirePlumber session.

For full mic FX coverage, install the host-side PipeWire/LADSPA packages your
distro uses for RNNoise, compressor, and gate support.

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

4. Inside WaveLinux, open `Settings -> Updates` and use either:
   - `Install/Reinstall This AppImage`
   - the verified `Download && Install ...` action for the latest release

That installs or refreshes:

- `~/.local/bin/WaveLinux.AppImage`
- `~/.local/bin/wavelinux`
- `~/.local/share/applications/io.github.duskyprojects.WaveLinux.desktop`

Rollback uses:

- `~/.local/bin/WaveLinux.AppImage.bak`

WaveLinux uses host LADSPA plugins for FX by default. That is intentional:
PipeWire loads those plugins from the host side, and bundled copies are less
reliable across distros.

If you explicitly want to try bundled LADSPA plugins in a custom build:

- build with `WAVELINUX_BUNDLE_LADSPA=1`
- run with `WAVELINUX_ENABLE_BUNDLED_LADSPA=1`

## Source Checkout Install

For development or running directly from a git checkout:

```bash
git clone https://github.com/DuskyProjects/WaveLinux.git
cd WaveLinux
./install.sh
```

On Arch-based systems you can also install known runtime dependencies:

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

Package-managed installs should be updated through the package manager, not by
replacing them in place with an AppImage.

## Building the AppImage

The release AppImage is built with PyInstaller plus AppImageKit:

```bash
python3 -m pip install PyInstaller PyQt6 cryptography
./scripts/build_appimage.sh
```

If you build from a virtualenv, point the script at that exact interpreter so
`PyInstaller`, `PyQt6`, and `cryptography` come from the same environment:

```bash
PYTHON_BIN=/path/to/venv/bin/python ./scripts/build_appimage.sh
```

Artifacts are written to `dist/`:

- `WaveLinux-<version>-x86_64.AppImage`
- `sha256sums.txt`
- `wavelinux-release-manifest.json`
- `wavelinux-release-manifest.sig`

Release CI lives in `.github/workflows/release.yml`.

## Project Layout

- `main.py`: Qt UI, settings, scenes, update flow wiring
- `pipewire_engine.py`: PipeWire routing, device discovery, app identity, FX graph
- `audio_runtime/`: serialized runtime planner, executor, diagnostics, controller
- `updates.py`: signed manifest verification, download/install, rollback
- `distribution.py`: install paths, launcher repair, runtime mode detection
- `packaging/appimage/`: desktop metadata and AppImage assets

## Maintainer Release Checklist

Before tagging a release:

- Bump `APP_VERSION` in `main.py`.
- Update `pkgver` in `PKGBUILD` if the Arch package tracks the same release.
- Confirm the GitHub Actions secret `WAVELINUX_RELEASE_ED25519_PRIVATE_KEY_B64`
  is present and matches the public key embedded in `updates.py`.
- Run:

```bash
python3 -m unittest discover -s tests
PYTHON_BIN=/path/to/venv/bin/python ./scripts/build_appimage.sh
```

- Commit the version bump.
- Push the release tag as `vX.Y.Z`.

Normal branch pushes do not create GitHub releases. Release publishing only
happens from the tag-triggered workflow.

Expected release assets:

- `WaveLinux-<version>-x86_64.AppImage`
- `sha256sums.txt`
- `wavelinux-release-manifest.json`
- `wavelinux-release-manifest.sig`

Recommended updater validation path:

1. Start from an installed previous AppImage release.
2. Publish signed `v<new_version>`.
3. Open the older installed build and use `Settings -> Updates -> Download && Install`.
4. Confirm `~/.local/bin/WaveLinux.AppImage.bak` exists after install.
5. Restart into the new version.
6. Use `Restore Previous AppImage` if you need to roll back, then restart again.

Testing-only updater source overrides:

- `WAVELINUX_UPDATE_RELEASE_API_URL`
- `WAVELINUX_UPDATE_RELEASES_URL`

Those overrides let you point WaveLinux at staging release metadata and release
pages without adding UI settings. Signature, checksum, smoke-test, install, and
rollback rules stay the same.

## Paths

- Config: `~/.config/wavelinux/config.json`
- Log: `~/.config/wavelinux/wavelinux.log`
- Diagnostics: `~/.config/wavelinux/diagnostics/`
- FX logs: `~/.config/wavelinux/fx-logs/`
- Installed AppImage: `~/.local/bin/WaveLinux.AppImage`
- AppImage backup: `~/.local/bin/WaveLinux.AppImage.bak`

## OBS

1. Launch WaveLinux.
2. In OBS, add **Audio Input Capture**.
3. Select `WaveLinux-Stream` or the monitor variant you want.
