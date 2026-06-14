# WaveLinux 4.3.4

WaveLinux 4.3.4 hardens release packaging against clean Linux installs.

## Fixes

- Adds clean Debian, Ubuntu, Fedora, and Arch container smoke tests for release
  AppImage runtime dependency installs and native deb/rpm package installs.
- Bundles startup font and graphics libraries needed for the AppImage runtime
  dependency CLI to launch on minimal clean distro images.
- Renames the packaged Tauri binary to `wavelinux` so native packages expose the
  expected command.
- Adds missing deb/rpm runtime dependencies for PipeWire CLI and ALSA MIDI
  helper tools.

# WaveLinux 4.3.3

WaveLinux 4.3.3 moves hardware profiles out of GitHub release assets and into
the repository-backed profile feed.

## Fixes

- Downloads the remote hardware profile index from `profiles/v1/index.json` in
  the GitHub repository instead of from release assets.
- Downloads only matching device profiles from `profiles/v1/devices/` and
  keeps validating cached remote profile JSON before loading it.
- Stops building and uploading generated hardware profile JSON/signature assets,
  reducing future stable releases to the app, package, updater, and AUR files.

# WaveLinux 4.3.2

WaveLinux 4.3.2 is a focused microphone processing and monitor-control release
for stronger default noise gating and faster monitor input mute changes.

## Fixes

- Makes new Noise Gate instances more effective by default with a higher room
  mic threshold, deeper closed-gate range, and smoother hold/release timing.
- Migrates existing gates that still use the previous default profile to the
  stronger room mic profile while preserving custom gate settings.
- Expands the Noise Gate threshold and range controls to match the SWH LADSPA
  gate plugin limits more closely.
- Speeds up channel-to-monitor mute and volume changes by reusing cached route
  IDs and falling back to a lighter live lookup when the cache is stale.
- Keeps channel bus mute commands targeting both sides of the PipeWire loopback
  route when both are present, preserving route health after mute/unmute.

# WaveLinux 4.3.1

WaveLinux 4.3.1 is a focused routing and hardware-profile stability release for
Bluetooth headset quality, remote profile updates, and remembered-app matching.

## Fixes

- Keeps Bluetooth headphones on preferred A2DP playback profiles after startup,
  disconnect/reconnect, and hotplug without repeatedly forcing profile switches
  once the card is initialized.
- Improves Bluetooth codec matching so aptX, aptX HD, aptX Adaptive, SBC, and
  SBC XQ profile descriptions are matched without crossing variant boundaries.
- Refreshes cached remote hardware profile assets when the signed release index
  advertises a newer profile revision instead of reusing stale local cache files.
- Keeps the checked-in hardware profile index revisions aligned with the device
  profile files, including updated AirPods Pro, Bose QuietComfort Ultra,
  Sennheiser Momentum 4, Sony WH-1000XM5, Sony WH-1000XM4, and SteelSeries
  Arctis Nova Pro Wireless Bluetooth entries.
- Preserves stream volume when a route move fails, avoiding accidental double
  attenuation after an unsuccessful PipeWire move attempt.
- Restores legacy binary app matchers for streams that only report
  `process_name`, while keeping stricter app identity matching for newer route
  health checks.
- Adds regression coverage for Bluetooth reconnect initialization, remote
  profile cache refreshes, hardware profile index drift, codec matching, and
  legacy app matcher compatibility.

# WaveLinux 4.3.0

WaveLinux 4.3.0 promotes the recent testing work to stable with optional Elgato
hardware control support, stronger PipeWire route health repair, priority-based
auto hot-swap, and cleanup for stale remembered app entries.

## Features

- Adds an Elgato Settings tab only when Elgato audio hardware is detected.
- Adds Wave XLR gain, mute, headphone volume, low-impedance, firmware, API, and
  serial controls using the OpenWave USB control-transfer approach.
- Loads libusb only inside the detected Wave XLR control path so systems without
  Elgato hardware do not load the extra shared library during normal startup.
- Adds AppImage startup preflight for missing host runtime pieces before WebKit
  starts, with copyable install commands and native package-manager installs
  through apt, dnf, pacman, or zypper.
- Bundles more safe AppImage-side runtime pieces: GStreamer media support,
  WebKit sandbox helpers, and libusb for optional Elgato controls.
- Bundles supported LADSPA effect plugins into AppImage releases when present on
  the release builder, exposes the bundle through `LADSPA_PATH`, and includes
  distro effect packages in setup/install flows.
- Recognizes legacy `OpenWave_*` virtual audio nodes during managed graph
  cleanup so old testing graphs can be removed cleanly.
- Adds Hardware direct mic monitor mode so Wave XLR users can monitor through
  the interface hardware while WaveLinux keeps the mic in stream/record mixes
  and skips the delayed software Monitor copy.
- Ignores generic numbered `Stream 123` media labels when remembering apps so
  browser streams do not churn app history.
- Adds libusb and WebKit/AppImage runtime pieces to release packaging and
  dependency checks.
- Adds a Beta updates checkbox in the updater that tracks the single moving
  `prerelease` testing feed without changing stable update checks.
- Keeps unsupported, busy, permission-blocked, or missing-runtime streamer
  devices status-only so they do not expose non-working binding controls.
- Avoids tearing down the audio graph before a self-update has actually
  installed; restart shutdown handles cleanup after a successful update.
- Expands the Testing Health Report with update endpoint, release URL, current
  version, latest version, and install-support status for tester issue reports.

## Stability

- Repairs managed loopback routes when a route is stale, duplicated, missing its
  live source or sink endpoint, or missing either side of the PipeWire loopback.
- Adds route-health details to graph reports and diagnostics so missing,
  duplicated, or stale managed routes are visible instead of silently lingering.
- Keeps route repair focused on unsatisfied routes and rate-limits identical
  route-health repairs to reduce graph churn.
- Shows the resolved live Auto input/output device in the UI while keeping the
  saved setting as Auto, and keeps meters tied to the effective live source.
- Preserves priority-based Auto routing for hot-plugged microphones and outputs,
  including repair when the selected source disappears or a higher-priority
  valid device appears.
- Avoids selecting Bluetooth headset microphones while matching A2DP headphone
  output is available.
- Normalizes remembered app identities case-insensitively, collapses duplicate
  offline entries such as `RetroArch` / `retroarch`, and makes forget cleanup
  remove overlapping routes and volume presets.
- Falls back to the raw hardware mic route when a live mic effects helper is
  unhealthy, preventing stale effect processes from leaving app-facing mic
  capture silent until reboot.
- Refuses to reuse a graph that still has stale WaveLinux audio helper
  processes, so restart can cleanly recover without a full system reboot.
- Adds PipeWire health hints for recent underrun, buffer, and resync log
  clusters to help diagnose crackle without changing system PipeWire latency
  configuration.
- Rotates WaveLinux logs on update/startup and cleans old rotated logs so local
  logs do not grow indefinitely.

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
