# WaveLinux Hardware Profiles v1

WaveLinux hardware profiles are JSON files that help the backend pick safe, fast audio settings automatically when matching audio hardware appears.

Profile source files live as one device per file in:

```text
profiles/v1/devices/*.json
```

These files are source data for GitHub Release assets and local testing; they are not embedded into the app binary. Device files should contain a single profile object, not a `{ "profiles": [...] }` bundle. Remote release bundles and local experiments may still be loaded as bundles, but one-device files are preferred because they make review, ownership, and community fixes much easier.

The app downloads a signed lightweight index when it sees an unmatched audio device:

```text
hardware-profiles-v1-index.json
hardware-profiles-v1-index.json.sig
```

When an index entry matches detected hardware, WaveLinux downloads only that matching signed profile asset from GitHub Releases and caches it in `~/.config/wavelinux/hardware-profiles/v1/remote/`. It does not ship or load a full built-in profile catalog.

Installers may run `wavelinux --prewarm-hardware-profiles` after install. That command performs a read-only audio hardware check, downloads any signed matching remote profiles into the same cache, and exits without opening the UI or changing PipeWire routing. If hardware, PipeWire, or GitHub is unavailable during install, WaveLinux logs the reason and retries from the normal background detector when the app starts.

Local profiles go here:

```text
~/.config/wavelinux/hardware-profiles/v1/local/*.json
```

Profiles are data only. They cannot run commands, install files, edit PipeWire/WirePlumber config, or override WaveLinux hard guardrails. In particular, local profiles cannot force a Bluetooth headset microphone when doing so would switch the device to HFP/HSP and degrade A2DP playback.

Use `examples/local-usb-microphone.json` as a starting point. Prefer exact `vendor_id` and `product_id` matches when the device is a known audio endpoint. Profiles must describe an audio input and/or output endpoint; HID receivers, keyboards, mice, lighting controllers, video-only webcams, Bluetooth controllers, and other non-audio hardware do not belong in this catalog. For receivers, docks, webcams, and capture devices that can expose different USB interfaces in different modes, include audio identity text such as PipeWire node names, ALSA descriptions, or `Wireless Microphone RX`-style source names so WaveLinux does not treat a control/firmware interface as a microphone. Broad profiles without meaningful match rules are ignored.

## Important Fields

- `matches`: device identity rules, such as bus, vendor/product ID, node-name text, description text, driver text, or Bluetooth modalias text.
- `capabilities`: whether the audio endpoint is input, output, duplex, USB audio class, Bluetooth A2DP, Bluetooth HFP, or true duplex A2DP. At least one of `input` or `output` must be true.
- `latency_policy`: conservative and low-latency loopback choices in milliseconds.
- `routing_policy`: auto-selection priorities and whether the device should be considered for input/output.
- `bluetooth_mic_policy`: Bluetooth microphone safety policy. Use `never_if_hfp` for normal Bluetooth headsets.
- `codec_policy`: preferred/avoided Bluetooth codecs plus optional
  `latency_floor_msec` values keyed by codec, such as `aac`, `ldac`, and
  `sbc_xq`.
- `confidence`: `low`, `medium`, or `high`.

## Bluetooth Latency Floors

Bluetooth profiles should preserve the best stable A2DP codec before falling
back to lower-quality codecs. Do not use SBC as the first crackle fix when AAC,
SBC-XQ, or LDAC is available; raise the codec latency floor first.

Use these stability-first floors unless a device-specific trace proves a lower
value is reliable:

- `aac`: 320 ms
- `sbc_xq`: 360 ms
- `sbc`: 280 ms
- `ldac`: 500 ms

LDAC is quality-first, not latency-first. Profiles should avoid maximum-bitrate
LDAC modes on unstable links and should keep HFP/HSP out of music playback.

## Guardrails

WaveLinux ignores profiles that contain executable fields such as `command`, `exec`, `shell`, `script`, or `hook`.

WaveLinux also ignores profiles that do not describe an audio input or output endpoint.

WaveLinux also clamps unsafe Bluetooth microphone policies. If a profile describes a Bluetooth HFP device without true duplex A2DP support, the backend treats its microphone policy as `never_if_hfp`.
