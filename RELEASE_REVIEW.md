# Production Readiness Review (2026-05-11)

Scope reviewed:
- main.py
- pipewire_engine.py
- install.sh/start.sh/uninstall.sh
- PKGBUILD
- README.md/ROADMAP.md

## Summary
WaveLinux is close to production for a single-user desktop app, but there are **release blockers** around update integrity and packaging assumptions, plus a few medium-risk reliability issues.

## Blockers
1. In-app updater has no authenticity/integrity validation.
   - It downloads Python files from GitHub and replaces local runtime files directly.
   - No signature/hash pinning means any compromised release/tag path can execute arbitrary code on user machines.

2. Updater only replaces 3 files, not whole app state.
   - `_UPDATE_FILES = ["main.py", "pipewire_engine.py", "wavelinux_theme.py"]` can drift from install artifacts (scripts, metadata), leaving mixed versions.

3. App relies on shell tools but install path validation is partial.
   - Runtime depends on pactl/wpctl/pw-dump/parec; failure paths are mostly graceful, but a preflight dependency check at startup is missing.

## High-priority fixes
1. Add release signing/checksum verification for updater payloads.
2. Move from file-level updater to packaged release updater (or disable updater in packaged installs).
3. Add startup diagnostics panel with dependency and permission checks (PipeWire, WirePlumber, LADSPA, rtkit).
4. Add CI smoke tests (import + static lint + minimal subprocess mocks).

## Medium risk findings
1. `logging.basicConfig(...)` in library module (`pipewire_engine.py`) can override host logging configuration.
2. Background threads (updater, profile loader, FX rebuild orchestration) are robust but still rely on polling and eventual consistency; race windows are acknowledged in comments and should be covered by regression tests.
3. Single-instance lockfile behavior should be tested against crash/restart on KDE/Wayland sessions.

## Dead/missing code indicators
- No obvious dead blocks from static pass.
- No unresolved TODO/FIXME markers requiring immediate implementation.
- `ruff check` passes and bytecode compile succeeds.

## Production release checklist (recommended)
- [ ] Disable or harden in-app updater before release.
- [ ] Add versioned migration checks for config schema.
- [ ] Add recovery path for corrupted config.
- [ ] Add CI: ruff, py_compile, minimal integration script with mocked `pactl/pw-dump/wpctl`.
- [ ] Validate install/uninstall idempotency on clean Arch/CachyOS image.

