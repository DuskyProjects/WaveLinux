"""Assertion helpers for WaveLinux stress runs."""

from __future__ import annotations


class StressAssertionFailure(RuntimeError):
    def __init__(self, bucket, message, *, observed=None):
        super().__init__(message)
        self.bucket = str(bucket)
        self.message = str(message)
        self.observed = observed


def ensure(condition, bucket, message, *, observed=None):
    if not condition:
        raise StressAssertionFailure(bucket, message, observed=observed)


def assert_pipewire_stack_healthy(snapshot):
    status = str(snapshot.get("wpctl_status") or "")
    ensure("PipeWire 'pipewire-0'" in status, "preflight.host_audio_bad", "PipeWire status is unavailable", observed=status)


def assert_a2dp_active(snapshot, profile):
    active = str(snapshot.get("bluetooth_active_profile") or "")
    preferred = str((profile.get("bluetooth") or {}).get("preferred_profile") or "")
    ensure(active == preferred, "bt.reconnect_hfp_only", f"Expected Bluetooth profile {preferred}, got {active or 'unknown'}", observed=active)


def assert_headset_not_in_bad_profile(snapshot, profile):
    active = str(snapshot.get("bluetooth_active_profile") or "")
    bad_profiles = set((profile.get("bluetooth") or {}).get("bad_profiles") or [])
    ensure(active not in bad_profiles, "bt.bad_profile_persists", f"Bluetooth profile is degraded: {active}", observed=active)


def assert_bluetooth_mic_not_selected(snapshot, profile):
    bad_source = str((profile.get("bluetooth") or {}).get("bad_source_name") or "")
    active = str((snapshot.get("defaults") or {}).get("source") or "")
    ensure(active != bad_source, "mic.bluetooth_mic_selected", f"Bluetooth microphone became default source: {active}", observed=active)


def assert_wave_graph_present(snapshot):
    wave_sinks = list(((snapshot.get("wave_named_objects") or {}).get("sinks") or []))
    ensure(bool(wave_sinks), "launch.graph_partial", "WaveLinux sinks are missing from the graph", observed=wave_sinks)


def assert_wave_graph_absent(snapshot):
    wave_sinks = list(((snapshot.get("wave_named_objects") or {}).get("sinks") or []))
    wave_sources = list(((snapshot.get("wave_named_objects") or {}).get("sources") or []))
    ensure(not wave_sinks and not wave_sources, "quit.graph_survives", "WaveLinux graph objects remain loaded", observed={"sinks": wave_sinks, "sources": wave_sources})


def assert_default_sink(snapshot, expected_sink):
    active = str((snapshot.get("defaults") or {}).get("sink") or "")
    ensure(active == expected_sink, "monitor.switch_wrong_sink", f"Expected default sink {expected_sink}, got {active}", observed=active)


def assert_default_source(snapshot, expected_source):
    active = str((snapshot.get("defaults") or {}).get("source") or "")
    ensure(active == expected_source, "launch.default_source_wrong", f"Expected default source {expected_source}, got {active}", observed=active)


def assert_monitor_probe_flow(captures):
    for name, stats in (captures or {}).items():
        ensure(
            int(stats.get("bytes", 0)) > 0 and int(stats.get("peak", 0)) > 0,
            "launch.monitor_silent",
            f"Expected monitor probe audio on {name}, got bytes={stats.get('bytes')} peak={stats.get('peak')}",
            observed=stats,
        )


def assert_mic_probe_flow(stats):
    ensure(
        int((stats or {}).get("bytes", 0)) > 0 and int((stats or {}).get("peak", 0)) > 0,
        "mic.swap_silent",
        "Expected microphone probe audio but capture was silent",
        observed=stats,
    )


def assert_no_orphan_wavelinux_processes(snapshot):
    processes = list(snapshot.get("wave_processes") or [])
    ensure(not processes, "kill.orphan_process", "WaveLinux-related processes remain alive", observed=processes)


def assert_no_orphan_wavelinux_modules(snapshot):
    wave_named = snapshot.get("wave_named_objects") or {}
    sinks = list(wave_named.get("sinks") or [])
    sources = list(wave_named.get("sources") or [])
    ensure(
        not sinks and not sources,
        "kill.orphan_graph",
        "WaveLinux sinks or sources remain loaded",
        observed={"sinks": sinks, "sources": sources},
    )


def assert_routes_match_expected(summary, expected_by_name):
    apps = list((summary or {}).get("apps") or [])
    actual = {}
    for app in apps:
        app_name = str(app.get("app_name") or "").strip()
        if app_name:
            actual[app_name] = app.get("current_sink")
    for app_name, expected_sink in dict(expected_by_name or {}).items():
        ensure(
            app_name in actual,
            "app.route_not_applied",
            f"Runtime summary does not contain app {app_name}",
            observed=actual,
        )
        ensure(
            actual.get(app_name) == expected_sink,
            "app.route_lost_after_churn",
            f"Expected {app_name} on {expected_sink}, got {actual.get(app_name)}",
            observed=actual,
        )
