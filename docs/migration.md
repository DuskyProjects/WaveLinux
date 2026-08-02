# WaveLinux 5 To 6 Migration

WaveLinux 6 intentionally uses a new application and PipeWire namespace. The
local installer replaces WaveLinux5 rather than keeping both graphs active.

## Config Migration

When `~/.config/wavelinux6/config.json` does not exist, the engine looks for the
WaveLinux5 config through its XDG application path. It then:

1. parses the old config;
2. rewrites owned node names and properties to `wavelinux6`;
3. migrates legacy effects, including DeepFilterNet entries to RNNoise;
4. clears transient live route/module ids during normalization;
5. writes schema 14 with write, fsync, atomic rename, and directory fsync;
6. records a pending migration marker containing the old config path.

The old config is removed only after startup graph repair validates the new
WaveLinux 6 graph. A failed startup leaves the old installation data available
for another attempt. Once validation succeeds, no long-lived migration backup
is retained.

## Client Devices

WaveLinux 6 exposes only `wavelinux6-*` and `wavelinux6_*` nodes. Applications
that saved `wavelinux5-mic` or a WaveLinux5 Stream source may require one manual
selection of `wavelinux6-mic` or `wavelinux6_mix_stream_source`.

## Installer Scope

The installer stops known WaveLinux5/6 app and helper processes, unloads only
modules carrying WaveLinux ownership, removes WaveLinux5 launchers/icons/data,
and installs WaveLinux 6 in user XDG paths. Process matching tests ensure it
does not target unrelated `wavelinux` text or general PipeWire services.

The installer does not remove arbitrary user audio configuration. The managed
ALSA alias block is updated independently and is marked so uninstall can remove
only WaveLinux-owned lines.
