# WaveLinux

WaveLinux is a PyQt6 PipeWire mixer for Linux with Wave Link-style routing:

- Monitor mix (what you hear)
- Stream mix (`WaveLinux-Stream` for OBS)
- Per-channel MON/STR faders with optional link
- Per-app routing persistence
- Per-channel FX (RNNoise, HPF, EQ, compressor, gate, limiter)

## Install (Arch/CachyOS)

```bash
git clone https://github.com/excalprimeacct-gif/WaveLinux.git
cd WaveLinux
./install.sh
```

Or build package:

```bash
makepkg -si
```

Run locally:

```bash
python3 main.py
```

## Paths

- Config: `~/.config/wavelinux/config.json`
- Log: `~/.config/wavelinux/wavelinux.log`
- FX logs: `~/.config/wavelinux/fx-logs/`

## OBS

1. Launch WaveLinux.
2. Add **Audio Input Capture** in OBS.
3. Select `WaveLinux-Stream` (or monitor variant).

## Notes

- Depends on PipeWire tools (`pactl`, `pw-dump`, `wpctl`, `parec`).
- Effects that rely on LADSPA require installed plugins.
