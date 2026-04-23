# 🌊 WaveLinux: Project Roadmap & Status

WaveLinux is a professional-grade PipeWire audio mixer for CachyOS/KDE, designed to provide stream-deck level audio control with a premium visual experience.

---

## ✅ Completed Milestones

### 🎨 UI & Aesthetics
- [x] **Premium Dark Theme**: Sleek, modern interface using custom CSS/QSS tokens.
- [x] **Icon Centering**: Finalized 512x512 pre-centered app icon for perfect taskbar and menu alignment.
- [x] **Responsive Layout**: Dynamic layout adjusting for varying numbers of application audio sources.
- [x] **Tray Integration**: Functional system tray icon for background operation.

### ⚙️ Core Audio Engine (PipeWire)
- [x] **Virtual Channel Matrix**: Creation of dedicated virtual sinks (Music, Game, Web Browser, etc.).
- [x] **Master Output Mixes**:
    - **Monitor**: Dedicated mix for the user's headphones/speakers.
    - **Stream**: Dedicated mix and **Virtual Recording Source** for OBS/Broadcasting.
- [x] **Robust Loopback System**: Low-latency audio routing from apps/channels to master mixes.
- [x] **Cleanup Engine**: Intelligent module cleanup that prevents duplicate devices on restart.
- [x] **Hardware Persistence**: Remembers which headphones/speakers are assigned to which master mix.

### 📱 Application Management
- [x] **Container Awareness**: Reliable identification of Flatpak, Snap, and native applications via process inspection.
- [x] **App Persistence (Caching)**: Applications stay in the mixer (as "Offline") even when closed, preserving their routing settings.
- [x] **Volume/Mute Synchronization**: Real-time sync between the UI and PipeWire's internal states.

### 📦 Deployment & DevOps
- [x] **Automated Installer**: `install.sh` script to handle desktop integration, icons, and permissions.
- [x] **GitHub Integration**: Repository initialized at `excalprimeacct-gif/WaveLinux`.
- [x] **Desktop Export**: Project snapshot exported to `~/Desktop/WaveLinux`.

---

## 🚧 Current Backlog (Pending Features)

### 1. Advanced Audio Processing (High Priority)
- [ ] **FX UI Panel**: Build the interface for the existing backend hooks (Compressor, Limiter, EQ).
- [ ] **Noise Suppression Settings**: Add controls for the RNNoise threshold and toggle.
- [ ] **Audio Visualizers**: Real-time spectral meters or volume peak bars for each channel.

### 2. Hardware & Compatibility (Medium Priority)
- [ ] **ALSA Profiles**: Support for switching hardware profiles (e.g., Analog Stereo vs. Pro Audio) directly from the dropdown.
- [ ] **Hot-Plug Support**: Dynamic UI updates when new USB headsets or microphones are plugged in.
- [ ] **Virtual Channel Renaming**: Allow users to right-click and rename "Music" or "Game" channels.

### 3. Workflow & UX (Medium Priority)
- [ ] **Routing Profiles**: Save/Load profiles (e.g., "Gaming Profile" vs "Music Production Profile").
- [ ] **Keybindings**: Support for global hotkeys (Mute Mic, Volume Up/Down).
- [ ] **Auto-Launch on Boot**: Option to add WaveLinux to system startup automatically.

### 4. Packaging & Distribution (Long Term)
- [ ] **AUR Package**: Create a `PKGBUILD` for Arch/CachyOS users.
- [ ] **Flatpak Packaging**: Sandbox the application for broader Linux distribution.

---

## 🛠️ Architecture Notes
- **Language**: Python 3.14+
- **GUI**: PyQt6 (with fallback logic for PyQt5)
- **Audio Backend**: PipeWire via `pactl` (PulseAudio compatibility layer) and `pw-dump`.
- **Config Path**: `~/.config/wavelinux/config.json`
