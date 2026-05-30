# WaveLinux 4.3.0

WaveLinux 4.3.0 adds optional Elgato hardware control support while keeping the
main mixer path device-neutral.

## Features

- Adds an Elgato Settings tab only when Elgato audio hardware is detected.
- Adds Wave XLR gain, mute, headphone volume, low-impedance, firmware, API, and
  serial controls using the OpenWave USB control-transfer approach.
- Loads libusb only inside the detected Wave XLR control path so systems without
  Elgato hardware do not load the extra shared library during normal startup.
- Adds libusb to release packaging and dependency checks.

# WaveLinux 4.2.1

WaveLinux 4.2.1 is a follow-up stability and polish release for the new
multi-surface UI system, custom theme loading, effect plugin setup, and
DeepFilterNet3 microphone processing.

## Fixes

- Defaults new installs to the Wave Link 3-style Matrix Dark interface while
  preserving any saved user interface choice.
- Adds a built-in Settings > Health > Effect Availability installer for missing
  optional LADSPA plugins, including DeepFilterNet3, RNNoise, and SWH dynamics.
- Verifies DeepFilterNet LADSPA availability by checking for a DeepFilterNet3
  model marker instead of accepting ambiguous legacy plugins.
- Tunes DeepFilterNet3 defaults for live mic use with a less lossy input/output
  gain stage, a lower reduction limit, a quieter-speech threshold, balanced
  Voice/Natural/Noisy Room presets, and a larger realtime processing buffer.
- Keeps the system default capture device on `wavelinux-mic` instead of the
  stream mix, so microphone effects remain available without making stream mix
  audio the default input.
- Adds source/channel icon editing and keeps mix/source icon choices normalized
  and persistent.
- Moves mixer-side editing controls into flyout panels for app routing, source
  settings, output settings, and FX workflows.
- Fixes light theme contrast issues, mute button styling, matrix scroll/padding
  problems, and effect active indicators.
- Removes the unused Scenes capability from the UI/code path.
- Updates the app icon set and simplifies the README into setup-focused project
  documentation.

# WaveLinux 4.2.0

WaveLinux 4.2.0 is a feature release that adds selectable UI surfaces and a
frontend-owned theme system for custom user-interface files without coupling
theme data to the Rust audio engine config.

## Features

- Adds the original WaveLinux interface as the Wave Link 2-style surface and
  adds Wave Link 3-style Matrix light and dark surfaces.
- Moves UI theme selection into a frontend theme registry backed by local app
  storage, keeping it separate from the Rust mixer engine.
- Loads user-created UI theme JSON files from the app themes folder; valid files
  appear in the Interface selector after refresh or restart.
- Exposes `--wl-*` theme tokens for Matrix shell colors, panels, borders,
  text, accent colors, danger states, and active LED color.
- Adds Wave Link 3-style matrix refinements including shrink/expand mode,
  input-first source creation, mix templates, per-cell route assignment, active
  app chips, FX LEDs, and multi-output mix routing.
- Persists user-selectable mix icons so custom Matrix mixes keep their visual
  identity across restarts.

# WaveLinux 4.1.3

WaveLinux 4.1.3 is a focused stability release for hotplug routing, Bluetooth
monitor output recovery, and documentation cleanup.

## Fixes

- Rebuilds only the final Bluetooth monitor route when a Bluetooth output
  reconnects, changes profile/codec identity, or leaves duplicate monitor
  loopbacks behind.
- Waits briefly for A2DP transport to settle before reconnecting the monitor
  route, reducing silent-output races during Bluetooth reconnects.
- Restores default input/output locks without running a full graph repair when
  only the app-facing default device changed.
- Backs off failed app stream moves for disappeared streams so stale PipeWire
  stream IDs do not create repeated move failures.
- Adds `adjust_time=0` to managed WaveLinux loopbacks and bumps route revisions
  so old routes are rebuilt with the new arguments.
- Keeps route latency decisions profile-driven while preserving conservative
  fallbacks for unknown hardware.

## Documentation

- Keeps the README version-neutral and moves release-specific detail back into
  release notes.
- Simplifies code comments that had drifted into internal changelog wording.
- Removes version-specific wording from the test documentation.
