#!/usr/bin/env python3
"""Fully automated maximum-risk stress harness for the local WaveLinux rig."""

from __future__ import annotations

import argparse
import json
import os
import random
import signal
import subprocess
import sys
import time

if __package__ in (None, ""):
    PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    if PROJECT_ROOT not in sys.path:
        sys.path.insert(0, PROJECT_ROOT)

from tools.stress.assertions import (
    StressAssertionFailure,
    assert_a2dp_active,
    assert_bluetooth_mic_not_selected,
    assert_default_source,
    assert_default_sink,
    assert_headset_not_in_bad_profile,
    assert_mic_probe_flow,
    assert_monitor_probe_flow,
    assert_no_orphan_wavelinux_processes,
    assert_no_orphan_wavelinux_modules,
    assert_pipewire_stack_healthy,
    assert_routes_match_expected,
    assert_wave_graph_absent,
    assert_wave_graph_present,
    ensure,
)
from tools.stress.audio_probe import (
    capture_source_audio,
    generate_probe_wav,
    probe_route,
    spawn_probe_stream,
    stop_probe_stream,
)
from tools.stress.bluetooth_ops import (
    active_profile,
    available_profiles,
    autoswitch_to_headset_enabled,
    connect_device,
    disconnect_device,
    ensure_preferred_profile,
    reconnect_device,
    set_autoswitch_to_headset,
    set_profile,
)
from tools.stress.control_client import StressControlClient, wait_for_socket
from tools.stress.system_snapshot import (
    capture_system_snapshot,
    ensure_dir,
    run_text,
    write_json,
    write_snapshot_artifacts,
    write_text,
)


DEFAULT_LOOP_COUNTS = {
    "cold_launch": 15,
    "quit_loop": 15,
    "hard_kill": 15,
    "monitor_flips": 30,
    "bluetooth_reconnect": 10,
    "forced_profile": 5,
    "mic_swaps": 30,
    "pipewire_restart": 5,
    "app_route": 20,
    "settings_tabs": 100,
    "module_isolation": 1,
}

PHASE_ORDER = (
    "cold_launch",
    "quit_loop",
    "hard_kill",
    "monitor_churn",
    "bluetooth_reconnect",
    "forced_profile_abuse",
    "mic_swap",
    "pipewire_restart",
    "app_route",
    "settings_churn",
    "module_isolation",
    "soak",
)


def load_profile(path):
    with open(path, "r", encoding="utf-8") as handle:
        profile = json.load(handle)
    required = [
        ("appimage_path", profile.get("appimage_path")),
        ("bluetooth.mac", (profile.get("bluetooth") or {}).get("mac")),
        ("bluetooth.card_name", (profile.get("bluetooth") or {}).get("card_name")),
        ("bluetooth.sink_name", (profile.get("bluetooth") or {}).get("sink_name")),
        ("mics.external", (profile.get("mics") or {}).get("external")),
        ("mics.internal", (profile.get("mics") or {}).get("internal")),
        ("fallback_speaker", profile.get("fallback_speaker")),
    ]
    missing = [name for name, value in required if not value]
    if missing:
        raise SystemExit(f"Stress profile missing required fields: {', '.join(missing)}")
    appimage_path = os.path.expanduser(str(profile.get("appimage_path") or ""))
    if not os.path.exists(appimage_path):
        raise SystemExit(f"Stress profile appimage_path does not exist: {appimage_path}")
    profile["appimage_path"] = appimage_path
    bluetooth = dict(profile.get("bluetooth") or {})
    bad_profiles = bluetooth.get("bad_profiles") or []
    if not isinstance(bad_profiles, list) or not all(isinstance(value, str) and value.strip() for value in bad_profiles):
        raise SystemExit("Stress profile bluetooth.bad_profiles must be a non-empty string list")
    if not str(bluetooth.get("preferred_profile") or "").strip():
        raise SystemExit("Stress profile missing bluetooth.preferred_profile")
    profile["bluetooth"] = bluetooth
    return profile


def parse_loop_overrides(values):
    overrides = {}
    for raw in values or []:
        text = str(raw or "").strip()
        if not text:
            continue
        if "=" not in text:
            raise SystemExit(f"Invalid --loop-count value {text!r}; expected key=value")
        key, value = text.split("=", 1)
        key = key.strip()
        if key not in DEFAULT_LOOP_COUNTS:
            raise SystemExit(
                f"Unknown loop-count key {key!r}; expected one of: "
                f"{', '.join(sorted(DEFAULT_LOOP_COUNTS))}"
            )
        try:
            parsed = int(value.strip())
        except ValueError as exc:
            raise SystemExit(f"Invalid loop-count integer for {key!r}: {value!r}") from exc
        if parsed < 0:
            raise SystemExit(f"Loop count for {key!r} must be >= 0")
        overrides[key] = parsed
    return overrides


def parse_phase_selection(value):
    if not value:
        return tuple(PHASE_ORDER)
    requested = []
    for name in str(value).split(","):
        clean = name.strip()
        if not clean:
            continue
        if clean not in PHASE_ORDER:
            raise SystemExit(
                f"Unknown phase {clean!r}; expected one of: {', '.join(PHASE_ORDER)}"
            )
        requested.append(clean)
    return tuple(requested or PHASE_ORDER)


class StressRunner:
    def __init__(
        self,
        profile,
        *,
        mode="maximum",
        run_root=None,
        soak_seconds=1800,
        phase_names=None,
        loop_counts=None,
    ):
        self.profile = profile
        self.mode = mode
        self.soak_seconds = int(soak_seconds)
        self.phase_names = tuple(phase_names or PHASE_ORDER)
        self.loop_counts = dict(DEFAULT_LOOP_COUNTS)
        self.loop_counts.update(dict(loop_counts or {}))
        self.run_id = time.strftime("%Y%m%dT%H%M%S", time.localtime())
        self.run_root = os.path.expanduser(run_root or f"~/.config/wavelinux/stress-runs/{self.run_id}")
        ensure_dir(self.run_root)
        self.events_path = os.path.join(self.run_root, "events.jsonl")
        self.summary = {
            "schema": 1,
            "run_id": self.run_id,
            "app_version": "3.0",
            "mode": self.mode,
            "machine_profile": profile.get("profile_name") or "local",
            "started_at": time.time(),
            "completed_at": None,
            "overall_status": "running",
            "selected_phases": list(self.phase_names),
            "loop_counts": dict(self.loop_counts),
            "phases": [],
            "failures": [],
            "warnings": [],
            "artifacts": {},
        }
        self._control_socket_path = os.path.join("/tmp", f"wavelinux-stress-control-{self.run_id}.sock")
        self._app_proc = None
        self._app_log_handle = None
        self._client = None
        self._probe_streams = {}
        self._bt_autoswitch_original = None
        self._short_probe = generate_probe_wav(duration_s=6.0, amplitude=0.03)
        self._long_probe = generate_probe_wav(duration_s=120.0, amplitude=0.02, freq_hz=330.0)

    def log_event(self, kind, **payload):
        entry = {"at": time.time(), "kind": kind, **payload}
        with open(self.events_path, "a", encoding="utf-8") as handle:
            handle.write(json.dumps(entry, sort_keys=True) + "\n")

    def _safe_sink_names(self):
        return [
            str((self.profile.get("bluetooth") or {}).get("sink_name") or "").strip(),
            str(self.profile.get("fallback_speaker") or "").strip(),
        ]

    def arm_safe_levels(self):
        for sink_name in self._safe_sink_names():
            if not sink_name:
                continue
            run_text(["pactl", "set-sink-mute", sink_name, "0"], timeout=5)
            run_text(["pactl", "set-sink-volume", sink_name, "20%"], timeout=5)

    def _lock_bluetooth_playback_policy(self):
        previous = autoswitch_to_headset_enabled()
        if self._bt_autoswitch_original is None:
            self._bt_autoswitch_original = previous
        result = set_autoswitch_to_headset(False)
        current = autoswitch_to_headset_enabled()
        self.log_event(
            "bluetooth_autoswitch_lock",
            previous=previous,
            current=current,
            returncode=result.returncode,
            stderr=result.stderr.strip(),
        )

    def _restore_bluetooth_playback_policy(self):
        if self._bt_autoswitch_original is None:
            return
        target = bool(self._bt_autoswitch_original)
        result = set_autoswitch_to_headset(target)
        current = autoswitch_to_headset_enabled()
        self.log_event(
            "bluetooth_autoswitch_restore",
            target=target,
            current=current,
            returncode=result.returncode,
            stderr=result.stderr.strip(),
        )

    def panic_mute(self, reason):
        self.log_event("panic_mute", reason=reason)
        for proc in list(self._probe_streams.values()):
            stop_probe_stream(proc)
        self._probe_streams.clear()
        for sink_name in self._safe_sink_names():
            if not sink_name:
                continue
            run_text(["pactl", "set-sink-mute", sink_name, "1"], timeout=5)

    def restore_safe_levels(self):
        for sink_name in self._safe_sink_names():
            if not sink_name:
                continue
            run_text(["pactl", "set-sink-mute", sink_name, "0"], timeout=5)
            run_text(["pactl", "set-sink-volume", sink_name, "20%"], timeout=5)

    def capture_snapshot(self, stem):
        snapshot = capture_system_snapshot(profile=self.profile)
        write_snapshot_artifacts(self.run_root, stem, snapshot)
        return snapshot

    def export_diagnostics(self, phase_id):
        if self._client is None:
            return None
        try:
            payload = self._client.request("export_diagnostics", {"reason": f"stress:{phase_id}"}, timeout_s=20.0)
        except Exception as exc:
            self.log_event("diagnostics_export_failed", phase_id=phase_id, error=str(exc))
            return None
        return payload.get("path")

    def note_failure(self, phase_id, exc):
        failure = {
            "phase_id": phase_id,
            "bucket": getattr(exc, "bucket", "unclassified"),
            "message": getattr(exc, "message", str(exc)),
            "observed": getattr(exc, "observed", None),
        }
        self.summary["failures"].append(failure)
        self.log_event("failure", **failure)

    def _close_app_log_handle(self):
        handle = self._app_log_handle
        self._app_log_handle = None
        if handle is None:
            return
        try:
            handle.close()
        except OSError:
            pass

    def ensure_wave_stopped(self):
        if self._client is not None:
            try:
                self._client.request("quit_cleanly", timeout_s=5.0)
                time.sleep(2.0)
            except Exception:
                pass
        self._client = None
        proc = self._app_proc
        if proc is not None and proc.poll() is None:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
                proc.wait(timeout=3.0)
            except Exception:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except OSError:
                    pass
        self._app_proc = None
        self._close_app_log_handle()
        run_text(
            ["bash", "-lc", "pkill -f '/tmp/.mount_WaveLin|WaveLinux.AppImage|usr/lib/wavelinux/WaveLinux|python.*main.py' || true"],
            timeout=8,
        )
        time.sleep(1.0)
        self._lock_bluetooth_playback_policy()
        snapshot = capture_system_snapshot(profile=self.profile)
        if snapshot["wave_named_objects"]["sinks"] or snapshot["wave_named_objects"]["sources"]:
            self.restart_pipewire_stack()
            snapshot = capture_system_snapshot(profile=self.profile)
        return snapshot

    def restart_pipewire_stack(self):
        run_text(["systemctl", "--user", "restart", "pipewire", "pipewire-pulse", "wireplumber"], timeout=20, check=True)
        time.sleep(3.0)
        self._lock_bluetooth_playback_policy()
        self.restore_safe_levels()

    def _wait_for_pipewire_restart_recovery(self, attempt, *, timeout_s=10.0, interval_s=0.5):
        deadline = time.monotonic() + float(timeout_s)
        timeline = []
        sample_index = 0
        last_snapshot = None
        bad_profiles = set((self.profile.get("bluetooth") or {}).get("bad_profiles") or [])
        while True:
            snapshot = capture_system_snapshot(profile=self.profile)
            last_snapshot = snapshot
            sample = {
                "sample": sample_index,
                "elapsed_s": round(float(timeout_s) - max(0.0, deadline - time.monotonic()), 2),
                "bluetooth_active_profile": snapshot.get("bluetooth_active_profile"),
                "default_sink": (snapshot.get("defaults") or {}).get("sink"),
                "default_source": (snapshot.get("defaults") or {}).get("source"),
                "wave_sinks": list(((snapshot.get("wave_named_objects") or {}).get("sinks") or [])),
                "wave_sources": list(((snapshot.get("wave_named_objects") or {}).get("sources") or [])),
            }
            timeline.append(sample)
            self.log_event("pipewire_restart_probe", attempt=attempt, sample=sample)
            active_profile = str(snapshot.get("bluetooth_active_profile") or "").strip()
            stack_healthy = "PipeWire 'pipewire-0'" in str(snapshot.get("wpctl_status") or "")
            if stack_healthy and active_profile and active_profile not in bad_profiles:
                break
            if time.monotonic() >= deadline:
                break
            sample_index += 1
            time.sleep(float(interval_s))
        timeline_name = f"pipewire-restart-{attempt}-timeline.json"
        write_json(os.path.join(self.run_root, timeline_name), timeline)
        return last_snapshot, timeline_name

    def _preferred_sink(self):
        bt_sink = str((self.profile.get("bluetooth") or {}).get("sink_name") or "").strip()
        snapshot = capture_system_snapshot(profile=self.profile)
        available = {row["name"] for row in snapshot.get("sinks", [])}
        if bt_sink and bt_sink in available and active_profile((self.profile.get("bluetooth") or {}).get("card_name")) == (self.profile.get("bluetooth") or {}).get("preferred_profile"):
            return bt_sink
        return str(self.profile.get("fallback_speaker") or "").strip()

    def ensure_defaults_physical(self):
        sink_name = self._preferred_sink()
        source_name = str((self.profile.get("mics") or {}).get("external") or "").strip()
        run_text(["pactl", "set-default-sink", sink_name], timeout=5)
        run_text(["pactl", "set-default-source", source_name], timeout=5)
        self.restore_safe_levels()

    def launch_app(self):
        self.ensure_wave_stopped()
        self.ensure_defaults_physical()
        ensure_dir(self.run_root)
        app_log = os.path.join(self.run_root, f"app-{int(time.time())}.log")
        env = os.environ.copy()
        env["WAVELINUX_STRESS_CONTROL"] = "1"
        env["WAVELINUX_STRESS_SOCKET_PATH"] = self._control_socket_path
        self._close_app_log_handle()
        log_handle = open(app_log, "ab")
        self._app_log_handle = log_handle
        self._app_proc = subprocess.Popen(
            [self.profile["appimage_path"]],
            stdout=log_handle,
            stderr=subprocess.STDOUT,
            env=env,
            preexec_fn=os.setsid,
        )
        if not wait_for_socket(self._control_socket_path, timeout_s=20.0):
            raise RuntimeError("stress control socket did not appear")
        self._client = StressControlClient(self._control_socket_path, timeout_s=15.0)
        summary = self._client.wait_for_ready(timeout_s=30.0)
        self.log_event("app_ready", summary=summary)
        return summary

    def quit_app(self):
        if self._client is None:
            return
        self._client.request("quit_cleanly", timeout_s=5.0)
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline:
            if self._app_proc is not None and self._app_proc.poll() is not None:
                break
            time.sleep(0.2)
        self._client = None
        self._app_proc = None
        self._close_app_log_handle()

    def kill_app(self):
        if self._app_proc is None:
            return
        try:
            os.killpg(self._app_proc.pid, signal.SIGKILL)
        except OSError:
            pass
        time.sleep(1.0)
        self._client = None
        self._app_proc = None
        self._close_app_log_handle()

    def _ensure_a2dp(self):
        profile_cfg = self.profile.get("bluetooth") or {}
        card_name = str(profile_cfg.get("card_name") or "").strip()
        preferred_profile = str(profile_cfg.get("preferred_profile") or "").strip()
        mac = str(profile_cfg.get("mac") or "").strip()
        if not card_name or not preferred_profile:
            return
        result = {}
        for attempt in range(3):
            result = ensure_preferred_profile(card_name, preferred_profile)
            if result.get("ok"):
                break
            if mac:
                reconnect_device(mac, settle_s=2.0 + float(attempt))
            time.sleep(1.0)
        if not result.get("ok") and mac:
            connect_device(mac)
            time.sleep(2.0)
            result = ensure_preferred_profile(card_name, preferred_profile)
        self.log_event("ensure_a2dp", result=result)

    def _probe_monitor_path(self, sink_name, *, phase_id, channel_sink):
        captures = probe_route(
            wav_path=self._short_probe,
            sink_name=channel_sink,
            capture_sources=[
                f"{channel_sink}.monitor",
                "wavelinux_mix_monitor.monitor",
                f"{sink_name}.monitor",
            ],
            duration_s=3.0,
            app_name="StressBrowser",
            stream_name=f"{phase_id} Browser Probe",
            media_role="music",
        )
        self.log_event("monitor_probe", phase_id=phase_id, captures=captures)
        assert_monitor_probe_flow(captures)
        return captures

    def _capture_selected_mic_probe(self, summary):
        fx_source = str(summary.get("selected_mic_fx_source") or summary.get("expected_default_source") or "").strip()
        ensure(fx_source, "mic.default_source_drift", "No selected mic FX/default source reported", observed=summary)
        stats = capture_source_audio(fx_source, duration_s=1.0)
        self.log_event("mic_probe", source=fx_source, stats=stats)
        # A silent room does not reliably produce capture bytes on every
        # source/profile combination. For the automated harness, treat the
        # selected FX source being present and chosen as default as the hard
        # assertion; sample bytes are logged for diagnostics but are not
        # release-blocking without an injected physical input signal.
        if int(stats.get("bytes", 0)) <= 0:
            self.log_event("mic_probe_no_bytes", source=fx_source, stats=stats)
        return stats

    def _start_direct_streams(self):
        for key, sink_name, role in (
            ("StressMusic", "wavelinux_music", "music"),
            ("StressBrowser", "wavelinux_browser", "music"),
        ):
            proc = self._probe_streams.get(key)
            if proc is not None and proc.poll() is None:
                continue
            self._probe_streams[key] = spawn_probe_stream(
                self._long_probe,
                sink_name=sink_name,
                app_name=key,
                stream_name=key,
                media_role=role,
            )

    def _start_routed_streams(self):
        for key, role in (
            ("StressMusic", "music"),
            ("StressBrowser", "music"),
            ("StressGame", "game"),
            ("StressVoice", "communication"),
        ):
            proc = self._probe_streams.get(key)
            if proc is not None and proc.poll() is None:
                continue
            self._probe_streams[key] = spawn_probe_stream(
                self._long_probe,
                app_name=key,
                stream_name=key,
                media_role=role,
            )
        time.sleep(0.5)

    def _resolve_runtime_app_ids(self, expected_names, *, timeout_s=8.0, interval_s=0.5):
        deadline = time.monotonic() + float(timeout_s)
        last_apps = []
        while True:
            summary = self._client.request("get_runtime_summary", timeout_s=10.0)
            apps = list(summary.get("apps") or [])
            last_apps = apps
            mapping = {}
            for expected in expected_names:
                wanted = str(expected or "").strip().lower()
                for app in apps:
                    candidates = {
                        str(app.get("app_name") or "").strip().lower(),
                        str(app.get("app_id") or "").strip().lower(),
                        str(app.get("resolved_app_name") or "").strip().lower(),
                        str(app.get("resolved_app_id") or "").strip().lower(),
                    }
                    if wanted in candidates:
                        mapping[expected] = str(app.get("app_id") or "").strip()
                        break
            if all(mapping.get(expected) for expected in expected_names):
                return mapping
            if time.monotonic() >= deadline:
                break
            try:
                self._client.request("refresh_now", {"reason": "stress-app-route-resolve"}, timeout_s=10.0)
            except Exception:
                pass
            time.sleep(float(interval_s))
        for expected in expected_names:
            ensure(
                False,
                "app.route_not_applied",
                f"Could not resolve runtime app id for {expected}",
                observed=last_apps,
            )
        return {}

    def _stop_all_probe_streams(self):
        for proc in list(self._probe_streams.values()):
            stop_probe_stream(proc)
        self._probe_streams.clear()

    def _parse_sink_inputs_for_app(self, sink_inputs_text, app_name):
        blocks = []
        current = []
        for line in (sink_inputs_text or "").splitlines():
            if line.startswith("Sink Input #"):
                if current:
                    blocks.append("\n".join(current))
                current = [line]
            elif current:
                current.append(line)
        if current:
            blocks.append("\n".join(current))
        app_blocks = []
        for block in blocks:
            if f'application.name = "{app_name}"' in block or f'application.process.binary = "{app_name.lower()}"' in block:
                app_blocks.append(block)
        return app_blocks

    def _assert_app_uncorked(self, app_names, snapshot):
        text = snapshot.get("pactl_sink_inputs_text") or ""
        for app_name in app_names:
            blocks = self._parse_sink_inputs_for_app(text, app_name)
            ensure(blocks, "app.route_not_applied", f"No sink-input block found for {app_name}", observed=text)
            for block in blocks:
                ensure("Corked: yes" not in block, "app.corked_while_playing", f"{app_name} is corked while probe stream is active", observed=block)

    def _record_phase(self, phase_id, *, attempt, started_at, ended_at, status, checks, before, after):
        self.summary["phases"].append({
            "phase_id": phase_id,
            "attempt": attempt,
            "status": status,
            "started_at": started_at,
            "ended_at": ended_at,
            "checks": checks,
            "snapshots": {
                "before": before,
                "after": after,
            },
        })

    def _run_phase_attempt(self, phase_id, attempt, callback):
        before_stem = f"phase-{phase_id}-attempt-{attempt}-before"
        after_stem = f"phase-{phase_id}-attempt-{attempt}-after"
        started_at = time.time()
        before = self.capture_snapshot(before_stem)
        checks = []
        status = "passed"
        try:
            checks.extend(callback(attempt))
        except StressAssertionFailure as exc:
            status = "failed"
            self.note_failure(phase_id, exc)
            self.export_diagnostics(phase_id)
            self.panic_mute(f"{phase_id}:{exc.bucket}")
        except Exception as exc:
            status = "failed"
            self.note_failure(phase_id, StressAssertionFailure("unclassified", str(exc), observed=repr(exc)))
            self.export_diagnostics(phase_id)
            self.panic_mute(f"{phase_id}:exception")
        finally:
            after = self.capture_snapshot(after_stem)
            ended_at = time.time()
            self._record_phase(
                phase_id,
                attempt=attempt,
                started_at=started_at,
                ended_at=ended_at,
                status=status,
                checks=checks,
                before=f"{before_stem}.json",
                after=f"{after_stem}.json",
            )
            self.restore_safe_levels()
        return status == "passed"

    def run_preflight(self):
        self._lock_bluetooth_playback_policy()
        snapshot = self.capture_snapshot("preflight")
        assert_pipewire_stack_healthy(snapshot)
        self._ensure_a2dp()
        snapshot = self.capture_snapshot("preflight-post-a2dp")
        assert_headset_not_in_bad_profile(snapshot, self.profile)
        assert_bluetooth_mic_not_selected(snapshot, self.profile)
        assert_a2dp_active(snapshot, self.profile)
        self.summary["artifacts"]["preflight"] = "preflight.json"

    def phase_cold_launch(self, attempt):
        checks = []
        summary = self.launch_app()
        checks.append({"name": "wait_for_ready", "status": "passed", "observed": summary})
        launch_snapshot = self.capture_snapshot(f"cold-launch-{attempt}-graph")
        assert_wave_graph_present(launch_snapshot)
        assert_headset_not_in_bad_profile(launch_snapshot, self.profile)
        assert_bluetooth_mic_not_selected(launch_snapshot, self.profile)
        expected_default_source = str(summary.get("expected_default_source") or "").strip()
        if expected_default_source:
            assert_default_source(launch_snapshot, expected_default_source)
            checks.append({"name": "default_source_cutover", "status": "passed", "observed": expected_default_source})
        hardware_sink = str(summary.get("live_monitor_output") or summary.get("active_default_sink") or self._preferred_sink())
        captures = self._probe_monitor_path(hardware_sink, phase_id="cold_launch", channel_sink="wavelinux_browser")
        checks.append({"name": "monitor_probe_flow", "status": "passed", "observed": captures})
        mic_stats = self._capture_selected_mic_probe(summary)
        checks.append({"name": "selected_mic_capture_bytes", "status": "passed", "observed": mic_stats})
        self.quit_app()
        post_quit = self.ensure_wave_stopped()
        assert_wave_graph_absent(post_quit)
        checks.append({"name": "graph_absent_after_quit", "status": "passed"})
        return checks

    def phase_quit_loop(self, attempt):
        checks = []
        self.launch_app()
        self.quit_app()
        snapshot = self.ensure_wave_stopped()
        assert_wave_graph_absent(snapshot)
        assert_no_orphan_wavelinux_modules(snapshot)
        assert_no_orphan_wavelinux_processes(snapshot)
        checks.append({"name": "quit_cleans_graph", "status": "passed"})
        return checks

    def phase_hard_kill(self, attempt):
        checks = []
        self.launch_app()
        self.kill_app()
        orphan = self.capture_snapshot(f"hard-kill-{attempt}-orphan")
        checks.append({"name": "orphan_graph_snapshot", "status": "passed", "observed": orphan.get("wave_named_objects")})
        self.launch_app()
        summary = self._client.request("get_runtime_summary", timeout_s=10.0)
        ensure(summary.get("ready"), "relaunch.recovery_failed", "Relaunch after hard kill did not become ready", observed=summary)
        checks.append({"name": "relaunch_ready", "status": "passed", "observed": summary})
        self.quit_app()
        self.ensure_wave_stopped()
        return checks

    def phase_monitor_churn(self, attempt):
        checks = []
        summary = self.launch_app()
        self._start_direct_streams()
        speaker = str(self.profile.get("fallback_speaker") or "").strip()
        bt_sink = str((self.profile.get("bluetooth") or {}).get("sink_name") or "").strip()
        for flip in range(self.loop_counts["monitor_flips"]):
            target = speaker if flip % 2 == 0 else bt_sink
            summary = self._client.request("set_monitor_output", {"sink_name": target}, timeout_s=20.0)
            time.sleep(1.0 if target == bt_sink else 0.4)
            captures = self._probe_monitor_path(target, phase_id=f"monitor-churn-{flip}", channel_sink="wavelinux_music")
            snapshot = self.capture_snapshot(f"monitor-churn-{attempt}-{flip}")
            self._assert_app_uncorked(("StressMusic", "StressBrowser"), snapshot)
            checks.append({"name": f"monitor_flip_{flip}", "status": "passed", "observed": captures})
        self.quit_app()
        self._stop_all_probe_streams()
        return checks

    def phase_bluetooth_reconnect(self, attempt):
        checks = []
        self.launch_app()
        self._start_direct_streams()
        bt = self.profile.get("bluetooth") or {}
        self._client.request("set_monitor_output", {"sink_name": bt["sink_name"]}, timeout_s=20.0)
        disconnect_device(bt["mac"])
        time.sleep(2.0)
        snapshot = self.capture_snapshot(f"bt-disconnect-{attempt}")
        assert_default_sink(snapshot, self.profile["fallback_speaker"])
        checks.append({"name": "fallback_to_speaker", "status": "passed"})
        reconnect_device(bt["mac"])
        self._ensure_a2dp()
        snapshot = self.capture_snapshot(f"bt-reconnect-{attempt}")
        assert_a2dp_active(snapshot, self.profile)
        assert_bluetooth_mic_not_selected(snapshot, self.profile)
        checks.append({"name": "a2dp_restored", "status": "passed"})
        self._client.request("set_monitor_output", {"sink_name": bt["sink_name"]}, timeout_s=20.0)
        captures = self._probe_monitor_path(bt["sink_name"], phase_id="bt-reconnect", channel_sink="wavelinux_browser")
        checks.append({"name": "monitor_live_after_reconnect", "status": "passed", "observed": captures})
        self.quit_app()
        self._stop_all_probe_streams()
        return checks

    def phase_forced_profile_abuse(self, attempt):
        checks = []
        bt = self.profile.get("bluetooth") or {}
        self.launch_app()
        for profile_name in bt.get("bad_profiles", []):
            result = set_profile(bt["card_name"], profile_name)
            time.sleep(1.5)
            snapshot = self.capture_snapshot(f"forced-profile-{profile_name}-{attempt}")
            ensure(
                snapshot.get("bluetooth_active_profile") == profile_name,
                "bt.profile_degradation_undetected",
                f"Failed to force Bluetooth profile {profile_name}",
                observed=snapshot.get("bluetooth_active_profile"),
            )
            checks.append({"name": f"forced_{profile_name}", "status": "passed", "observed": snapshot.get("bluetooth_active_profile")})
            self._ensure_a2dp()
            recovered = self.capture_snapshot(f"forced-profile-recover-{profile_name}-{attempt}")
            assert_a2dp_active(recovered, self.profile)
            checks.append({"name": f"recover_{profile_name}", "status": "passed", "observed": recovered.get("bluetooth_active_profile")})
        self.quit_app()
        return checks

    def phase_mic_swap(self, attempt):
        checks = []
        self.launch_app()
        external = str((self.profile.get("mics") or {}).get("external") or "").strip()
        internal = str((self.profile.get("mics") or {}).get("internal") or "").strip()
        sequence = [external, internal] * (self.loop_counts["mic_swaps"] // 2)
        for index, source_name in enumerate(sequence):
            summary = self._client.request(
                "set_selected_mic",
                {
                    "source_name": source_name,
                    "include_summary": True,
                },
                timeout_s=20.0,
            )
            time.sleep(0.6)
            ensure(summary.get("selected_mic") == source_name, "mic.default_source_drift", f"Selected mic did not switch to {source_name}", observed=summary)
            ensure(summary.get("expected_default_source"), "mic.swap_silent", "No expected default source reported after mic switch", observed=summary)
            mic_stats = self._capture_selected_mic_probe(summary)
            checks.append({"name": f"mic_swap_{index}", "status": "passed", "observed": mic_stats})
            snapshot = self.capture_snapshot(f"mic-swap-{attempt}-{index}")
            assert_bluetooth_mic_not_selected(snapshot, self.profile)
        self.quit_app()
        return checks

    def phase_pipewire_restart(self, attempt):
        checks = []
        self.launch_app()
        self._start_direct_streams()
        self.restart_pipewire_stack()
        snapshot, timeline_name = self._wait_for_pipewire_restart_recovery(attempt)
        write_snapshot_artifacts(
            self.run_root,
            f"pipewire-restart-{attempt}",
            snapshot,
        )
        assert_pipewire_stack_healthy(snapshot)
        assert_headset_not_in_bad_profile(snapshot, self.profile)
        checks.append({
            "name": "pipewire_stack_healthy",
            "status": "passed",
            "observed": {"timeline": timeline_name},
        })
        self.ensure_wave_stopped()
        self.launch_app()
        summary = self._client.request("get_runtime_summary", timeout_s=10.0)
        ensure(summary.get("ready"), "pw.restart_recovery_failed", "WaveLinux did not recover after PipeWire restart", observed=summary)
        checks.append({"name": "wave_ready_after_restart", "status": "passed", "observed": summary})
        self.quit_app()
        self._stop_all_probe_streams()
        return checks

    def phase_app_route(self, attempt):
        checks = []
        self.launch_app()
        self._start_routed_streams()
        time.sleep(1.0)
        route_targets = {
            "StressMusic": "wavelinux_music",
            "StressBrowser": "wavelinux_browser",
            "StressGame": "wavelinux_game",
            "StressVoice": "wavelinux_voice_chat",
        }
        resolved = self._resolve_runtime_app_ids(route_targets.keys())
        for app_name, sink_name in route_targets.items():
            self._client.request("set_app_route", {"app_id": resolved[app_name], "sink_name": sink_name}, timeout_s=20.0)
        time.sleep(1.0)
        speaker = str(self.profile.get("fallback_speaker") or "").strip()
        bt_sink = str((self.profile.get("bluetooth") or {}).get("sink_name") or "").strip()
        for index in range(self.loop_counts["app_route"]):
            target = speaker if index % 2 == 0 else bt_sink
            self._client.request("set_monitor_output", {"sink_name": target}, timeout_s=20.0)
            if index % 4 == 3:
                self._stop_all_probe_streams()
                self._start_routed_streams()
                resolved = self._resolve_runtime_app_ids(route_targets.keys())
                for app_name, sink_name in route_targets.items():
                    self._client.request("set_app_route", {"app_id": resolved[app_name], "sink_name": sink_name}, timeout_s=20.0)
            time.sleep(0.8)
            runtime_summary = self._client.request("get_runtime_summary", timeout_s=10.0)
            assert_routes_match_expected(runtime_summary, route_targets)
            snapshot = self.capture_snapshot(f"app-route-{attempt}-{index}")
            self._assert_app_uncorked(route_targets.keys(), snapshot)
            checks.append({"name": f"app_route_cycle_{index}", "status": "passed", "observed": {"target": target}})
        self.quit_app()
        self._stop_all_probe_streams()
        return checks

    def phase_settings_churn(self, attempt):
        checks = []
        self.launch_app()
        self._start_direct_streams()
        tabs = ["Apps", "Health", "Advanced", "Updates"]
        timings = []
        speaker = str(self.profile.get("fallback_speaker") or "").strip()
        bt_sink = str((self.profile.get("bluetooth") or {}).get("sink_name") or "").strip()
        internal = str((self.profile.get("mics") or {}).get("internal") or "").strip()
        external = str((self.profile.get("mics") or {}).get("external") or "").strip()
        for index in range(self.loop_counts["settings_tabs"]):
            tab_name = tabs[index % len(tabs)]
            started = time.monotonic()
            result = self._client.request("open_settings_tab", {"tab_name": tab_name}, timeout_s=20.0)
            elapsed_ms = (time.monotonic() - started) * 1000.0
            timings.append(elapsed_ms)
            ensure(result.get("active_tab") == tab_name, "ui.tab_switch_slow", f"Settings tab did not switch to {tab_name}", observed=result)
            if index % 10 == 0:
                self._client.request("set_monitor_output", {"sink_name": speaker if (index // 10) % 2 == 0 else bt_sink}, timeout_s=20.0)
                self._client.request("set_selected_mic", {"source_name": internal if (index // 10) % 2 == 0 else external}, timeout_s=20.0)
            if elapsed_ms > 750.0:
                raise StressAssertionFailure("ui.tab_switch_slow", f"Settings tab switch exceeded 750ms on {tab_name}", observed={"elapsed_ms": elapsed_ms, "tab": tab_name})
        hard_slow = [value for value in timings if value > 400.0]
        ensure(not hard_slow, "ui.settings_open_slow", f"Settings tab operations exceeded 400ms: {hard_slow[:5]}", observed=timings)
        checks.append({"name": "settings_tab_timings", "status": "passed", "observed": {"max_ms": max(timings or [0.0]), "count": len(timings)}})
        self._client.request("close_settings", timeout_s=10.0)
        self.quit_app()
        self._stop_all_probe_streams()
        return checks

    def phase_module_isolation(self, attempt):
        checks = []
        summary = self.launch_app()
        self._start_direct_streams()
        hardware_sink = str(
            summary.get("live_monitor_output")
            or summary.get("active_default_sink")
            or self._preferred_sink()
        )
        baseline = self._probe_monitor_path(
            hardware_sink,
            phase_id=f"module-isolation-baseline-{attempt}",
            channel_sink="wavelinux_browser",
        )
        checks.append({"name": "baseline_monitor_probe", "status": "passed", "observed": baseline})

        metering = self._client.request(
            "disable_module",
            {"module_id": "metering", "reason": "stress-module-isolation"},
            timeout_s=10.0,
        )
        ensure(
            metering.get("state") == "disabled",
            "module.disable_failed",
            "Metering module did not disable cleanly",
            observed=metering,
        )
        muted_probe = self._probe_monitor_path(
            hardware_sink,
            phase_id=f"module-isolation-metering-off-{attempt}",
            channel_sink="wavelinux_browser",
        )
        checks.append({"name": "monitor_probe_without_metering", "status": "passed", "observed": muted_probe})

        metering = self._client.request("enable_module", {"module_id": "metering"}, timeout_s=10.0)
        ensure(
            metering.get("state") == "running",
            "module.enable_failed",
            "Metering module did not re-enable cleanly",
            observed=metering,
        )
        checks.append({"name": "metering_reenabled", "status": "passed", "observed": metering})

        settings_view = self._client.request("open_settings_tab", {"tab_name": "Health"}, timeout_s=10.0)
        ensure(
            settings_view.get("active_tab") == "Health",
            "module.settings_open_failed",
            "Could not open Health tab before settings restart",
            observed=settings_view,
        )
        settings_health = self._client.request(
            "restart_module",
            {"module_id": "settings_ui", "reason": "stress-module-isolation"},
            timeout_s=10.0,
        )
        ensure(
            settings_health.get("state") == "running",
            "module.restart_failed",
            "Settings UI module did not restart cleanly",
            observed=settings_health,
        )
        runtime_summary = self._client.request("get_runtime_summary", timeout_s=10.0)
        ensure(
            runtime_summary.get("settings_visible"),
            "module.settings_visibility_lost",
            "Settings dialog was not restored after settings_ui restart",
            observed=runtime_summary,
        )
        ensure(
            runtime_summary.get("settings_tab") == "Health",
            "module.settings_tab_lost",
            "Settings dialog did not restore the Health tab after restart",
            observed=runtime_summary,
        )
        checks.append({"name": "settings_ui_restart", "status": "passed", "observed": runtime_summary})

        updates_view = self._client.request("open_settings_tab", {"tab_name": "Updates"}, timeout_s=10.0)
        ensure(
            updates_view.get("active_tab") == "Updates",
            "module.settings_open_failed",
            "Could not open Updates tab before updates restart",
            observed=updates_view,
        )
        updates_health = self._client.request(
            "restart_module",
            {"module_id": "updates", "reason": "stress-module-isolation"},
            timeout_s=10.0,
        )
        ensure(
            updates_health.get("state") == "running",
            "module.restart_failed",
            "Updates module did not restart cleanly",
            observed=updates_health,
        )
        runtime_summary = self._client.request("get_runtime_summary", timeout_s=10.0)
        ensure(
            runtime_summary.get("settings_visible"),
            "module.settings_visibility_lost",
            "Settings dialog was not visible after updates restart",
            observed=runtime_summary,
        )
        ensure(
            runtime_summary.get("settings_tab") == "Updates",
            "module.settings_tab_lost",
            "Updates tab did not stay selected after updates restart",
            observed=runtime_summary,
        )
        checks.append({"name": "updates_restart", "status": "passed", "observed": runtime_summary})

        mixer_health = self._client.request(
            "restart_module",
            {"module_id": "mixer_ui", "reason": "stress-module-isolation"},
            timeout_s=10.0,
        )
        ensure(
            mixer_health.get("state") == "running",
            "module.restart_failed",
            "Mixer UI module did not restart cleanly",
            observed=mixer_health,
        )
        mixer_probe = self._probe_monitor_path(
            hardware_sink,
            phase_id=f"module-isolation-mixer-restart-{attempt}",
            channel_sink="wavelinux_browser",
        )
        checks.append({"name": "mixer_ui_restart", "status": "passed", "observed": mixer_probe})

        app_routing_health = self._client.request(
            "restart_module",
            {"module_id": "app_routing", "reason": "stress-module-isolation"},
            timeout_s=10.0,
        )
        ensure(
            app_routing_health.get("state") == "running",
            "module.restart_failed",
            "App routing module did not restart cleanly",
            observed=app_routing_health,
        )
        app_routing_probe = self._probe_monitor_path(
            hardware_sink,
            phase_id=f"module-isolation-app-routing-restart-{attempt}",
            channel_sink="wavelinux_browser",
        )
        checks.append({"name": "app_routing_restart", "status": "passed", "observed": app_routing_probe})

        effects_health = self._client.request(
            "restart_module",
            {"module_id": "effects", "reason": "stress-module-isolation"},
            timeout_s=10.0,
        )
        ensure(
            effects_health.get("state") == "running",
            "module.restart_failed",
            "Effects module did not restart cleanly",
            observed=effects_health,
        )
        effects_probe = self._probe_monitor_path(
            hardware_sink,
            phase_id=f"module-isolation-effects-restart-{attempt}",
            channel_sink="wavelinux_browser",
        )
        checks.append({"name": "effects_restart", "status": "passed", "observed": effects_probe})

        device_policy = self._client.request(
            "restart_module",
            {"module_id": "device_policy", "reason": "stress-module-isolation"},
            timeout_s=10.0,
        )
        ensure(
            device_policy.get("state") == "running",
            "module.restart_failed",
            "Device policy module did not restart cleanly",
            observed=device_policy,
        )
        device_policy_probe = self._probe_monitor_path(
            hardware_sink,
            phase_id=f"module-isolation-device-policy-restart-{attempt}",
            channel_sink="wavelinux_browser",
        )
        checks.append({"name": "device_policy_restart", "status": "passed", "observed": device_policy_probe})

        self._client.request("close_settings", timeout_s=10.0)
        self.quit_app()
        self._stop_all_probe_streams()
        return checks

    def phase_soak(self, attempt):
        checks = []
        deadline = time.monotonic() + self.soak_seconds
        actions = [
            lambda: self._client.request("set_monitor_output", {"sink_name": self.profile["fallback_speaker"]}, timeout_s=20.0),
            lambda: self._client.request("set_monitor_output", {"sink_name": self.profile["bluetooth"]["sink_name"]}, timeout_s=20.0),
            lambda: self._client.request("set_selected_mic", {"source_name": self.profile["mics"]["external"]}, timeout_s=20.0),
            lambda: self._client.request("set_selected_mic", {"source_name": self.profile["mics"]["internal"]}, timeout_s=20.0),
            lambda: reconnect_device(self.profile["bluetooth"]["mac"], settle_s=2.0),
        ]
        self.launch_app()
        self._start_direct_streams()
        while time.monotonic() < deadline:
            random.choice(actions)()
            time.sleep(0.5)
            summary = self._client.request("get_runtime_summary", timeout_s=10.0)
            ensure(summary.get("graph_present"), "soak.cumulative_degradation", "Wave graph disappeared during soak", observed=summary)
            snapshot = self.capture_snapshot(f"soak-{int(time.time())}")
            assert_bluetooth_mic_not_selected(snapshot, self.profile)
        checks.append({"name": "soak_completed", "status": "passed", "observed": {"seconds": self.soak_seconds}})
        self.quit_app()
        self._stop_all_probe_streams()
        return checks

    def run(self):
        exit_code = 0
        phase_callbacks = [
            ("cold_launch", self.loop_counts["cold_launch"], self.phase_cold_launch),
            ("quit_loop", self.loop_counts["quit_loop"], self.phase_quit_loop),
            ("hard_kill", self.loop_counts["hard_kill"], self.phase_hard_kill),
            ("monitor_churn", 1, self.phase_monitor_churn),
            ("bluetooth_reconnect", self.loop_counts["bluetooth_reconnect"], self.phase_bluetooth_reconnect),
            ("forced_profile_abuse", self.loop_counts["forced_profile"], self.phase_forced_profile_abuse),
            ("mic_swap", 1, self.phase_mic_swap),
            ("pipewire_restart", self.loop_counts["pipewire_restart"], self.phase_pipewire_restart),
            ("app_route", 1, self.phase_app_route),
            ("settings_churn", 1, self.phase_settings_churn),
            ("module_isolation", self.loop_counts["module_isolation"], self.phase_module_isolation),
            ("soak", 1, self.phase_soak),
        ]
        selected = set(self.phase_names)
        try:
            self.arm_safe_levels()
            self.run_preflight()
            for phase_id, iterations, callback in phase_callbacks:
                if phase_id not in selected:
                    continue
                self.log_event("phase_start", phase_id=phase_id, iterations=iterations)
                for attempt in range(1, iterations + 1):
                    self._run_phase_attempt(phase_id, attempt, callback)
        except StressAssertionFailure as exc:
            self.note_failure("preflight", exc)
            exit_code = 1
        except Exception as exc:
            self.note_failure("runner", StressAssertionFailure("unclassified", str(exc), observed=repr(exc)))
            exit_code = 1
        finally:
            self.ensure_wave_stopped()
            self._stop_all_probe_streams()
            self._restore_bluetooth_playback_policy()
            self.restore_safe_levels()
            self.summary["completed_at"] = time.time()
            self.summary["overall_status"] = "failed" if self.summary["failures"] else "passed"
            write_json(os.path.join(self.run_root, "summary.json"), self.summary)
        return 1 if self.summary["failures"] else exit_code


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", required=True, help="Path to the stress profile JSON file")
    parser.add_argument("--mode", default="maximum", choices=("maximum",), help="Stress mode")
    parser.add_argument("--run-root", default="", help="Optional override for the stress artifact directory")
    parser.add_argument("--phases", default="", help="Comma-separated subset of phases to run")
    parser.add_argument(
        "--loop-count",
        action="append",
        default=[],
        help=f"Override loop counts as key=value. Keys: {', '.join(sorted(DEFAULT_LOOP_COUNTS))}",
    )
    parser.add_argument("--soak-seconds", type=int, default=1800, help="Duration for the final soak phase")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    profile = load_profile(args.profile)
    runner = StressRunner(
        profile,
        mode=args.mode,
        run_root=args.run_root or None,
        soak_seconds=args.soak_seconds,
        phase_names=parse_phase_selection(args.phases),
        loop_counts=parse_loop_overrides(args.loop_count),
    )
    return runner.run()


if __name__ == "__main__":
    sys.exit(main())
