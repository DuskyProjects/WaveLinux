# Maintainer: WaveLinux contributors <https://github.com/DuskyProjects/WaveLinux>
pkgname=wavelinux
pkgver=2.0.6
pkgrel=1
pkgdesc="Elgato Wave Link–style PipeWire mixer for Linux (virtual channels, Monitor/Stream buses, per-channel FX)"
arch=('any')
url="https://github.com/DuskyProjects/WaveLinux"
license=('MIT')
depends=(
  'python'
  'python-pyqt6'
  'pipewire'
  'pipewire-pulse'
  'wireplumber'     # provides wpctl (used for BT autoswitch lock + volume)
  'libpulse'        # provides pactl, parec
  'swh-plugins'     # compressor (sc4m_1916) and gate (gate_1410) LADSPA plugins
  'procps-ng'       # provides pkill (used by uninstall.sh and the orphan-FX reaper)
)
optdepends=(
  'noise-suppression-for-voice: RNNoise filter-chain backend (AUR)'
)
makedepends=('git')
source=("git+https://github.com/DuskyProjects/WaveLinux.git#tag=v$pkgver")
sha256sums=('SKIP')

package() {
  cd "$srcdir/WaveLinux"

  # App sources live under /usr/share/wavelinux.
  install -d "$pkgdir/usr/share/wavelinux"
  install -Dm644 main.py            "$pkgdir/usr/share/wavelinux/main.py"
  install -Dm644 distribution.py    "$pkgdir/usr/share/wavelinux/distribution.py"
  install -Dm644 pipewire_engine.py "$pkgdir/usr/share/wavelinux/pipewire_engine.py"
  install -Dm644 wavelinux_theme.py "$pkgdir/usr/share/wavelinux/wavelinux_theme.py"
  install -Dm644 tray_icon.png      "$pkgdir/usr/share/wavelinux/tray_icon.png"
  install -d "$pkgdir/usr/share/wavelinux/audio_runtime"
  install -Dm644 audio_runtime/__init__.py    "$pkgdir/usr/share/wavelinux/audio_runtime/__init__.py"
  install -Dm644 audio_runtime/adapter.py     "$pkgdir/usr/share/wavelinux/audio_runtime/adapter.py"
  install -Dm644 audio_runtime/controller.py  "$pkgdir/usr/share/wavelinux/audio_runtime/controller.py"
  install -Dm644 audio_runtime/diagnostics.py "$pkgdir/usr/share/wavelinux/audio_runtime/diagnostics.py"
  install -Dm644 audio_runtime/executor.py    "$pkgdir/usr/share/wavelinux/audio_runtime/executor.py"
  install -Dm644 audio_runtime/models.py      "$pkgdir/usr/share/wavelinux/audio_runtime/models.py"
  install -Dm644 audio_runtime/planner.py     "$pkgdir/usr/share/wavelinux/audio_runtime/planner.py"
  install -Dm644 icon.png           "$pkgdir/usr/share/icons/hicolor/512x512/apps/wavelinux.png"
  install -Dm644 README.md          "$pkgdir/usr/share/doc/$pkgname/README.md"
  install -Dm644 ROADMAP.md         "$pkgdir/usr/share/doc/$pkgname/ROADMAP.md"

  # /usr/bin/wavelinux launcher.
  install -d "$pkgdir/usr/bin"
  cat > "$pkgdir/usr/bin/wavelinux" <<'SH'
#!/bin/sh
exec python3 /usr/share/wavelinux/main.py "$@"
SH
  chmod 755 "$pkgdir/usr/bin/wavelinux"

  # Desktop launcher.
  install -d "$pkgdir/usr/share/applications"
  cat > "$pkgdir/usr/share/applications/wavelinux.desktop" <<'DESKTOP'
[Desktop Entry]
Name=WaveLinux
Comment=PipeWire Audio Router & Mixer
Exec=wavelinux
Icon=wavelinux
Type=Application
Categories=AudioVideo;Audio;Mixer;
Keywords=audio;mixer;pipewire;routing;
StartupNotify=true
DESKTOP
}
