# Peripheral Integrations

WaveLinux can map supported Elgato, HID, and MIDI controls to mixer actions.
These paths are optional and must remain idle when no enabled binding exists.

## Runtime Boundary

Device discovery is read-only. `wavelinux6-peripheral-plugin` runs hardware I/O
outside the Tauri and audio-core processes. The runtime controller starts one
isolated HID or MIDI child only when saved configuration contains an enabled
binding for that transport. Disabling the last binding stops that child.
Elgato control uses a short-lived child for each serialized command, so it has
no idle process. Audio routing and DSP never depend on a peripheral child.

Current backends are:

- Elgato Wave XLR control transfers through lazily loaded `libusb` in an
  on-demand child;
- HID reports from a configured `/dev/hidraw*` device in a lazy child;
- ALSA sequencer events read from `aseqdump` in a lazy MIDI child.

Permission errors are reported in Settings and Health. WaveLinux does not
silently change udev rules, install packages, or open every HID device at
startup.

## Actions

Bindings refer to stable model actions such as channel/mix volume, mute, and
monitor/stream selection. A learned raw event is normalized before it is saved;
the child emits only device id, control id, and value. The parent resolves that
event against the latest authoritative binding before the engine can mutate
state. A stale or forged action is therefore never accepted from a child.

Unknown devices receive status-only profiles. Vendor/product ids alone do not
grant control support, especially for capture devices and composite USB
hardware.

## Child-Plugin Protocol

Peripheral protocol v1 uses newline-framed JSON with a 16 KiB message limit.
Sockets live under `$XDG_RUNTIME_DIR/wavelinux6/peripherals` in a `0700`
directory; socket files are `0600`, and Linux peers must match the app's uid.
The handshake carries protocol version, transport kind, child pid, and
capabilities. HID/MIDI configuration is latest-state data from the parent, and
event delivery uses a bounded queue. Elgato requests carry monotonic request
ids and typed responses.

The helper receives no engine object, PipeWire ownership credentials, or
permission to execute mixer actions. A malformed version, transport mismatch,
wrong request id, oversized message, disconnect, or timeout ends only that
plugin session. HID/MIDI supervisors retry with bounded backoff; Elgato errors
return to the invoking command.

A plugin failure cannot stop the app, audio core, meters, or routing. Health
reports each active transport's state, protocol, pid, restart count, message,
and last error. Third-party plugin loading is not a shipping WaveLinux 6 API;
the current helper is an internal, packaged executable with a fixed protocol.

## Packaging

AppImage, DEB, and RPM builds must contain `wavelinux6-peripheral-plugin`.
`scripts/check-package-contents.sh` enforces this. Local installs place it at
`~/.local/bin/wavelinux6-peripheral-plugin`, and the launcher exports
`WAVELINUX_PERIPHERAL_PLUGIN`. The override is intended for development and
must point to a protocol-compatible helper.
