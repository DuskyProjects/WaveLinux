# Hardware Profile Runtime

WaveLinux uses hardware profiles to refine device ranking, Bluetooth policy,
and latency floors while keeping routing guardrails in code. The authoring
format and signing workflow are documented in
[profiles/v1/README.md](../profiles/v1/README.md).

## Resolution

The engine loads embedded shipped profiles, last-known-good verified remote
profiles, and local user overrides. Matching is scored from concrete audio
identity fields; broad bus-only or computer-model-only records are rejected.
Manual profile assignments win over automatic matches, and the generic
fallback remains available for unknown hardware.

Profile application may change safe priorities and latency floors. It cannot:

- execute a command or install a package;
- make an explicitly unavailable HDA port routable;
- select a non-audio USB/HID interface as a microphone;
- force HFP/HSP while normal Bluetooth playback should remain A2DP;
- alter WaveLinux node ownership or effect topology.

## Remote Trust Boundary

The remote index is Ed25519 signed with a dedicated catalog key. Device files
are authorized by id, revision, asset filename, and SHA-256 digest in that
signed index. Verification happens before a downloaded profile enters the
catalog; invalid or unavailable remote data leaves the embedded and cached
catalog intact.

Remote synchronization runs outside the real-time audio path. It is requested
only for unmatched or stale detected devices and has bounded timeout/backoff.
Health diagnostics report failures without blocking graph startup.

## Developer Checks

When changing a profile:

1. Keep one device per file and increase its revision.
2. Use audio-interface identity, not a broad host/controller match.
3. Update and sign `profiles/v1/index.json`.
4. Run `cargo test -p wavelinux-engine hardware_profiles`.
5. Run `bash scripts/test-all.sh` before packaging.

The private signing key is release infrastructure and must never enter the
repository or application data.
