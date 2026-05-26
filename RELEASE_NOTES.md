# WaveLinux 4.1.2

WaveLinux 4.1.2 is a stability and profile-authority fix release for the 4.1
hardware profile line.

## Fixes

- Fixed refresh-loop stalls caused by large `pactl --format=json list clients`
  output by draining stdout and stderr while waiting for command exit.
- Added per-command snapshot timing logs so future slow refreshes name the
  exact external command.
- Recovered hardware profile downloads from stale failure backoff, fetches the
  signed profile index, and keeps local fallback behavior when remote assets are
  unavailable.
- Kept faders, ordinary toggles, app volume changes, and low-latency monitoring
  controls optimistic and coalesced so drag spam does not freeze the UI or build
  a command queue.
- Updated meters immediately after volume and mute changes so fader state and
  VU state stay aligned.
- Made Bluetooth reconnect routing wait for the selected A2DP sink before
  moving monitor output, preventing silence while the sink is still appearing.
- Treated disappearing app streams during route moves as benign stale state.
- Fixed hardware input VU behavior so it displays the raw selected microphone
  until the FX source appears, then visually swaps to the post-FX
  `wavelinux-mic` source without changing routing.
- Applied active hardware profile latency floors to audio routes. Realtek
  ALC3254 speaker routing now uses the profile's safer low/stable latency
  floors instead of the old global 20 ms path.
- Made route latency profile-sourced: assigned or auto-matched hardware
  profiles decide first, and the editable generic fallback profile decides when
  no specific profile exists.

## Notes

- The dismissed `glib` Dependabot alert is upstream-pinned by Tauri's current
  Linux GTK/WebKit stack. `glib >= 0.20` cannot satisfy the current `gtk 0.18`
  constraints without removing Linux webview/tray functionality. Revisit this
  when upstream Tauri, wry, and gtk-rs provide a compatible patched line.
- The XM4 Bluetooth microphone guardrail from 4.1.1 is unchanged: HFP/HSP
  headset microphone mode is still treated as a compatibility fallback, not an
  optimization.
