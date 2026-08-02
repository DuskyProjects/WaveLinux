# WaveLinux Hardware Profiles v1

Hardware profiles are declarative JSON records that let WaveLinux select safe
device priorities, Bluetooth policy, and latency floors without hard-coding a
specific computer. They never execute commands or modify host configuration.

## Catalog Layers

Profiles are merged in this order, with the later layer taking precedence when
the same profile id has a newer or equal revision:

1. shipped profiles embedded from `profiles/v1/devices/*.json` at compile time;
2. verified remote profiles cached under
   `~/.config/wavelinux6/hardware-profiles/v1/remote`;
3. user profiles under
   `~/.config/wavelinux6/hardware-profiles/v1/local`.

The safe generic fallback remains available when nothing matches. A remote
failure never removes the embedded or last-known-good catalog.

Each file in `devices` contains one profile object. Bundled
`{ "profiles": [...] }` files are accepted only for local compatibility; use
one-device files for shipped and remote changes so ownership and review remain
clear.

## Signed Remote Updates

Production builds fetch `profiles/v1/index.json` and `index.json.sig` from the
WaveLinux repository only when a detected device needs a missing or newer
profile. The index is verified with the profile-catalog Ed25519 public key
compiled into `crates/engine/src/hardware_profiles.rs`. Every index entry also
contains the SHA-256 digest of its device asset; an asset is cached only when
its path, size, digest, schema, and profile id all match the signed entry.

The index cache has a 24-hour TTL, download failures back off for 30 minutes,
and network operations have a five-second timeout. Set
`WAVELINUX_DISABLE_PROFILE_DOWNLOADS=1` to use only shipped and local profiles.
`WAVELINUX_PROFILE_BASE_URL` is a developer/test override and does not change
signature verification.

Prewarm matching devices without opening the UI or mutating the audio graph:

```bash
wavelinux6 --prewarm-hardware-profiles
```

The command reports installed matches and newly fetched assets. Normal startup
runs the same synchronization asynchronously.

## Authoring

Start from `examples/local-usb-microphone.json`. Prefer exact `vendor_id` and
`product_id` rules plus audio-specific PipeWire/ALSA text. Receivers, docks,
webcams, and capture cards often expose non-audio interfaces with the same USB
identity, so a VID/PID rule alone may be unsafe.

Important fields:

- `id`, `revision`, and `name`: stable identity and update ordering.
- `matches`: bus, vendor/product id, node, description, driver, ALSA, or
  Bluetooth modalias criteria.
- `capabilities`: input/output/duplex, USB audio, and Bluetooth capabilities.
- `latency_policy`: conservative and low-latency route floors in milliseconds.
- `routing_policy`: input/output eligibility and safe priority adjustments.
- `bluetooth_mic_policy`: normally `never_if_hfp` for consumer headsets.
- `codec_policy`: preferred/avoided codecs and codec-specific latency floors.
- `confidence`: `low`, `medium`, or `high`.

At least one audio input or output capability is required. Profiles containing
fields such as `command`, `exec`, `shell`, `script`, or `hook` are rejected.
Local profiles cannot bypass unavailable HDA jack handling or force an HFP/HSP
microphone when doing so would degrade active A2DP playback.

## Bluetooth Floors

Use measured endpoint-specific values. The normal starting ranges are:

| Codec | Suggested floor |
| --- | --- |
| AAC | 80-140 ms |
| SBC-XQ | 100-160 ms |
| SBC | 70-120 ms |
| LDAC | 120-180 ms |

Prefer a small profile-specific increase over broad 240-300 ms defaults. LDAC
is quality-first, not latency-first; avoid maximum-bitrate modes on unstable
links.

## Validation And Signing

Run profile and catalog tests through:

```bash
bash scripts/test-all.sh
```

Release maintainers update hashes/revisions in `index.json`, then sign the
exact index bytes with the dedicated catalog key:

```bash
bash scripts/sign-profile-catalog.sh
```

Never commit the private signing key. A profile change is incomplete until the
index digest, signature, shipped-catalog tests, and distro package checks pass.
