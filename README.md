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
- In-app AppImage updates now verify a signed GitHub release manifest, validate the downloaded checksum, smoke-test the new AppImage, then install it into `~/.local/bin`.
- Package-managed installs can still check for verified releases, but they do not replace themselves with an AppImage in place; update those through your package manager.
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

4. Inside WaveLinux, open `Settings -> Updates` and use the runtime-aware install button if you want a desktop launcher copied into `~/.local/bin` and `~/.local/share/applications`.

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

The same `Settings -> Updates` page can also reinstall the current source checkout launcher, or a local bundled build launcher, when you are running one of those modes.

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

GitHub Actions release CI is defined in `.github/workflows/release.yml`.

## Release Signing

Signed in-app updates require the GitHub Actions secret:

- `WAVELINUX_RELEASE_ED25519_PRIVATE_KEY_B64`

That secret must contain the base64-encoded raw 32-byte Ed25519 private key
that matches the public key embedded in `updates.py`. Without it, release CI
cannot produce `wavelinux-release-manifest.sig`, and the in-app updater will
refuse the release.

## Signed Release Checklist

Before tagging a release:

- Bump `APP_VERSION` in `main.py`.
- Update `PKGBUILD` if the Arch package version should track the same tag.
- Confirm the GitHub Actions secret `WAVELINUX_RELEASE_ED25519_PRIVATE_KEY_B64` is present and matches `updates.py`.
- Run `python3 -m unittest discover -s tests`.
- Build a fresh AppImage with `PYTHON_BIN=/path/to/venv/bin/python ./scripts/build_appimage.sh`.

Expected release assets:

- `WaveLinux-<version>-x86_64.AppImage`
- `sha256sums.txt`
- `wavelinux-release-manifest.json`
- `wavelinux-release-manifest.sig`

Recommended updater validation path:

1. Start from an installed `2.0.4` AppImage.
2. Publish signed `v2.0.6`.
3. Open installed `2.0.4`, then use `Settings -> Updates -> Download && Install v2.0.6`.
4. Confirm `~/.local/bin/WaveLinux.AppImage.bak` exists after install.
5. Restart into `2.0.6`.
6. Use `Restore Previous AppImage` if you need to roll back, then restart again.

Testing-only updater source overrides:

- `WAVELINUX_UPDATE_RELEASE_API_URL`
- `WAVELINUX_UPDATE_RELEASES_URL`

Those overrides let you point WaveLinux at staging release metadata and release pages without changing any UI settings. Signature, checksum, smoke-test, install, and rollback rules stay the same.

## Paths

- Config: `~/.config/wavelinux/config.json`
- Log: `~/.config/wavelinux/wavelinux.log`
- FX logs: `~/.config/wavelinux/fx-logs/`

## OBS

1. Launch WaveLinux.
2. Add **Audio Input Capture** in OBS.
3. Select `WaveLinux-Stream` or the monitor variant you want.
