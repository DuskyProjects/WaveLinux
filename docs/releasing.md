# Release Procedure

WaveLinux releases are promoted only from a clean, validated source state. A
local build/install never pushes, tags, or publishes by itself.

## Preflight

1. Confirm the intended branch and inspect every dirty file.
2. Run `bash scripts/test-all.sh`.
3. Build unsigned acceptance artifacts with `bash scripts/build-portable.sh`.
4. Run `bash scripts/check-package-contents.sh target/portable/release/bundle`
   and verify the reported glibc requirements do not exceed 2.39.
5. Run AppImage and native-package smoke tests for Debian, Ubuntu, Fedora,
   Arch, and CachyOS through `scripts/distro-smoke.sh`. Confirm the CachyOS
   WebKit pixel-render gate passes.
6. Install locally through `scripts/install-local.sh` and verify public nodes,
   routing, meters, microphone, Stream aggregation, effect swaps, hotplug, and
   current Health/log output.
7. Complete the 60-minute `scripts/stress-audio-runtime.sh` gate and retain its
   JSON result.

Any unexplained discontinuity, owned underrun, failed link, graph rebuild,
silent interval, or missed performance threshold blocks release.

## Version Promotion

Update the same version in:

- `package.json` and `yarn.lock` metadata;
- `crates/app/Cargo.toml` and `Cargo.lock`;
- `crates/app/tauri.conf.json`;
- release notes and any literal package examples.

Run all gates again after the version change. Verify AppImage, deb, rpm, desktop
entry, AUR metadata, updater metadata, binary names, and embedded audio-core
version before committing.

The signed release workflow may build on Ubuntu 24.04, but it must run the same
package-content and glibc gates. Host-native development artifacts are never
release assets. The distro matrix must consume one staged artifact set, not
independent per-distro rebuilds.

## Signing And Publishing

Release/update signing keys and the hardware-profile catalog key are separate.
Keep both outside the repository. Use the repository signing scripts and verify
the generated signatures before upload.

After explicit authorization:

1. create one intentional release commit;
2. push the validated branch to `master` without discarding remote history;
3. create and push the exact version tag;
4. allow the release workflow to build, validate, and upload the AppImage,
   Debian package, RPM package, updater artifacts, and AUR metadata;
5. publish versions containing a prerelease suffix as GitHub prereleases and
   stable versions as normal releases;
6. verify CI, distro smoke, release upload, checksums/signatures, and downloadable
   package contents from GitHub.

For WaveLinux 6 Alpha 1, the exact version is `6.0.0-alpha.1`, the exact tag is
`v6.0.0-alpha.1`, and the GitHub release title is `WaveLinux 6 Alpha 1`.

Do not mark the release complete while a required GitHub check is queued,
cancelled, or failing.
