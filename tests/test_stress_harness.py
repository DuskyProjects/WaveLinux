import json
import os
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock

from tools.stress.run_stress_suite import (
    PHASE_ORDER,
    StressRunner,
    load_profile,
    parse_loop_overrides,
    parse_phase_selection,
)
from tools.stress import audio_probe
from tools.stress import bluetooth_ops
from tools.stress import system_snapshot
from tools.stress.system_snapshot import write_snapshot_artifacts


class StressHarnessTests(unittest.TestCase):
    def _profile_payload(self):
        return {
            "profile_name": "test-rig",
            "appimage_path": "/bin/true",
            "bluetooth": {
                "mac": "00:11:22:33:44:55",
                "card_name": "bluez_card.test",
                "sink_name": "bluez_output.test.1",
                "preferred_profile": "a2dp-sink",
                "bad_profiles": ["headset-head-unit"],
                "bad_source_name": "bluez_input.test"
            },
            "mics": {
                "external": "alsa_input.usb_test",
                "internal": "alsa_input.internal_test"
            },
            "fallback_speaker": "alsa_output.speakers"
        }

    def test_load_profile_requires_fields(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            profile_path = os.path.join(temp_dir, "profile.json")
            with open(profile_path, "w", encoding="utf-8") as handle:
                json.dump({"profile_name": "bad"}, handle)
            with self.assertRaises(SystemExit):
                load_profile(profile_path)

    def test_load_profile_accepts_valid_profile(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            profile_path = os.path.join(temp_dir, "profile.json")
            with open(profile_path, "w", encoding="utf-8") as handle:
                json.dump(self._profile_payload(), handle)
            loaded = load_profile(profile_path)
        self.assertEqual(loaded["bluetooth"]["sink_name"], "bluez_output.test.1")

    def test_parse_loop_overrides_validates_keys(self):
        overrides = parse_loop_overrides(["cold_launch=1", "settings_tabs=4"])
        self.assertEqual(overrides["cold_launch"], 1)
        self.assertEqual(overrides["settings_tabs"], 4)
        with self.assertRaises(SystemExit):
            parse_loop_overrides(["nope=1"])

    def test_parse_phase_selection_validates_names(self):
        phases = parse_phase_selection("cold_launch,quit_loop")
        self.assertEqual(phases, ("cold_launch", "quit_loop"))
        self.assertEqual(parse_phase_selection(""), PHASE_ORDER)
        with self.assertRaises(SystemExit):
            parse_phase_selection("unknown")

    def test_write_snapshot_artifacts_writes_expected_files(self):
        snapshot = {
            "wpctl_status": "status",
            "wpctl_settings_text": "settings",
            "pactl_sinks_text": "sinks",
            "pactl_sources_text": "sources",
            "pactl_sink_inputs_text": "sink-inputs",
            "pactl_source_outputs_text": "source-outputs",
            "wavelinux_log_tail": "log-tail",
        }
        with tempfile.TemporaryDirectory() as temp_dir:
            write_snapshot_artifacts(temp_dir, "phase-test", snapshot)
            self.assertTrue(os.path.exists(os.path.join(temp_dir, "phase-test.json")))
            self.assertTrue(os.path.exists(os.path.join(temp_dir, "phase-test.wpctl-status.txt")))
            self.assertTrue(os.path.exists(os.path.join(temp_dir, "phase-test.wpctl-settings.txt")))
            self.assertTrue(os.path.exists(os.path.join(temp_dir, "phase-test.pactl-sink-inputs.txt")))

    def test_active_wavelinux_processes_filters_its_own_pgrep(self):
        payload = "\n".join(
            [
                "123 /home/dusky/.local/bin/WaveLinux.AppImage",
                "456 bash -lc pgrep -af 'WaveLinux|WaveLinux.AppImage|/usr/lib/wavelinux/WaveLinux|python.*main.py' || true",
                "789 python3 /home/dusky/Projects/WaveLinux/tools/stress/run_stress_suite.py --profile /tmp/profile.json",
            ]
        )
        with mock.patch.object(system_snapshot, "run_text", return_value=SimpleNamespace(stdout=payload)):
            processes = system_snapshot.active_wavelinux_processes()
        self.assertEqual(processes, ["123 /home/dusky/.local/bin/WaveLinux.AppImage"])

    def test_probe_route_attaches_captures_before_playback_and_collects_all_sources(self):
        events = []
        capture_procs = {
            "one": object(),
            "two": object(),
        }

        def _spawn_capture(source_name, sample_rate=48000):
            events.append(("capture", source_name, sample_rate))
            return capture_procs[source_name]

        def _spawn_playback(*args, **kwargs):
            events.append(("playback", kwargs.get("sink_name"), kwargs.get("app_name")))
            return object()

        def _collect_capture(proc, *, source_name):
            events.append(("collect", source_name))
            return {"source_name": source_name, "bytes": 123, "peak": 456}

        with mock.patch.object(audio_probe, "_spawn_capture_process", side_effect=_spawn_capture), \
                mock.patch.object(audio_probe, "spawn_probe_stream", side_effect=_spawn_playback), \
                mock.patch.object(audio_probe, "_collect_capture_process", side_effect=_collect_capture), \
                mock.patch.object(audio_probe, "stop_probe_stream"), \
                mock.patch.object(audio_probe.time, "sleep"):
            captures = audio_probe.probe_route(
                wav_path="/tmp/test.wav",
                sink_name="wavelinux_browser",
                capture_sources=["one", "two"],
                duration_s=1.0,
                app_name="StressBrowser",
                stream_name="Stress Browser",
            )

        self.assertEqual(events[:2], [("capture", "one", 48000), ("capture", "two", 48000)])
        self.assertEqual(events[2], ("playback", "wavelinux_browser", "StressBrowser"))
        self.assertEqual(captures["one"]["bytes"], 123)
        self.assertEqual(captures["two"]["peak"], 456)

    def test_capture_source_audio_uses_pw_record_and_reports_pcm_stats(self):
        class _FakeProc:
            def __init__(self):
                self.returncode = None

            def poll(self):
                return self.returncode

            def terminate(self):
                self.returncode = 1

            def communicate(self, timeout=None):
                self.returncode = 1
                return ("", "/tmp/test.raw")

            def kill(self):
                self.returncode = -9

        with tempfile.TemporaryDirectory() as temp_dir:
            capture_path = os.path.join(temp_dir, "capture.raw")
            sample_bytes = b"\x01\x00\xfe\xff\x10\x00\xf0\xff"

            def _fake_mkstemp(prefix="", suffix=""):
                fd = os.open(capture_path, os.O_CREAT | os.O_RDWR | os.O_TRUNC, 0o600)
                return fd, capture_path

            def _fake_popen(cmd, **kwargs):
                self.assertEqual(cmd[0], "pw-record")
                self.assertEqual(cmd[-1], capture_path)
                with open(capture_path, "wb") as handle:
                    handle.write(sample_bytes)
                return _FakeProc()

            with mock.patch.object(audio_probe.tempfile, "mkstemp", side_effect=_fake_mkstemp), \
                    mock.patch.object(audio_probe.subprocess, "Popen", side_effect=_fake_popen), \
                    mock.patch.object(audio_probe.time, "sleep"):
                stats = audio_probe.capture_source_audio("alsa_input.test", duration_s=1.0)

        self.assertEqual(stats["source_name"], "alsa_input.test")
        self.assertEqual(stats["bytes"], len(sample_bytes))
        self.assertEqual(stats["peak"], 16)
        self.assertGreater(stats["rms"], 0.0)
        self.assertEqual(stats["stderr"], "/tmp/test.raw")

    def test_capture_source_audio_falls_back_to_parec_when_pw_record_missing(self):
        fallback_proc = object()
        with mock.patch.object(audio_probe.subprocess, "Popen", side_effect=FileNotFoundError), \
                mock.patch.object(audio_probe, "_spawn_capture_process", return_value=fallback_proc) as spawn_capture, \
                mock.patch.object(audio_probe, "_collect_capture_process", return_value={"bytes": 7}) as collect_capture, \
                mock.patch.object(audio_probe.time, "sleep"):
            stats = audio_probe.capture_source_audio("alsa_input.test", duration_s=1.0)

        spawn_capture.assert_called_once_with("alsa_input.test", sample_rate=48000)
        collect_capture.assert_called_once_with(fallback_proc, source_name="alsa_input.test")
        self.assertEqual(stats["bytes"], 7)

    def test_bluetoothctl_script_sends_real_newlines_to_bluetoothctl(self):
        calls = {}

        def _fake_run(cmd, **kwargs):
            calls["cmd"] = cmd
            calls["input"] = kwargs.get("input")
            calls["capture_output"] = kwargs.get("capture_output")
            calls["text"] = kwargs.get("text")
            calls["timeout"] = kwargs.get("timeout")
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        with mock.patch.object(bluetooth_ops.subprocess, "run", side_effect=_fake_run):
            bluetooth_ops.bluetoothctl_script(
                "disconnect AC:80:0A:72:BD:10",
                "connect AC:80:0A:72:BD:10",
                timeout=9,
            )

        self.assertEqual(calls["cmd"], ["bluetoothctl"])
        self.assertEqual(
            calls["input"],
            "disconnect AC:80:0A:72:BD:10\nconnect AC:80:0A:72:BD:10\nquit\n",
        )
        self.assertTrue(calls["capture_output"])
        self.assertTrue(calls["text"])
        self.assertEqual(calls["timeout"], 9)

    def test_autoswitch_to_headset_enabled_parses_wpctl_settings(self):
        payload = """
Settings:

- Id: bluetooth.autoswitch-to-headset-profile
  Name: Auto-switch to headset profile
  Value: false
"""
        with mock.patch.object(bluetooth_ops, "run_text", return_value=SimpleNamespace(stdout=payload)):
            self.assertFalse(bluetooth_ops.autoswitch_to_headset_enabled())

        payload = """
Settings:

- Id: bluetooth.autoswitch-to-headset-profile
  Name: Auto-switch to headset profile
  Value: true
"""
        with mock.patch.object(bluetooth_ops, "run_text", return_value=SimpleNamespace(stdout=payload)):
            self.assertTrue(bluetooth_ops.autoswitch_to_headset_enabled())

    def test_autoswitch_to_headset_enabled_returns_none_on_timeout(self):
        with mock.patch.object(bluetooth_ops, "run_text", side_effect=TimeoutError("boom")):
            self.assertIsNone(bluetooth_ops.autoswitch_to_headset_enabled())

    def test_set_autoswitch_to_headset_uses_wpctl_settings(self):
        calls = []

        def _fake_run(cmd, **kwargs):
            calls.append((cmd, kwargs))
            return SimpleNamespace(returncode=0, stdout="", stderr="")

        with mock.patch.object(bluetooth_ops, "run_text", side_effect=_fake_run):
            bluetooth_ops.set_autoswitch_to_headset(False)
            bluetooth_ops.set_autoswitch_to_headset(True)

        self.assertEqual(
            calls[0][0],
            ["wpctl", "settings", "bluetooth.autoswitch-to-headset-profile", "false"],
        )
        self.assertEqual(
            calls[1][0],
            ["wpctl", "settings", "bluetooth.autoswitch-to-headset-profile", "true"],
        )

    def test_runner_ensure_a2dp_retries_reconnect_until_profile_is_available(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            runner = StressRunner(self._profile_payload(), run_root=temp_dir, phase_names=())
            profile_results = [
                {"ok": False, "available_profiles": [], "active_profile": ""},
                {"ok": False, "available_profiles": [], "active_profile": ""},
                {"ok": True, "available_profiles": ["a2dp-sink"], "active_profile": "a2dp-sink"},
            ]
            reconnect_calls = []
            connect_calls = []
            events = []

            with mock.patch("tools.stress.run_stress_suite.ensure_preferred_profile", side_effect=profile_results), \
                    mock.patch("tools.stress.run_stress_suite.reconnect_device", side_effect=lambda mac, settle_s=0: reconnect_calls.append((mac, settle_s))), \
                    mock.patch("tools.stress.run_stress_suite.connect_device", side_effect=lambda mac: connect_calls.append(mac)), \
                    mock.patch("tools.stress.run_stress_suite.time.sleep"), \
                    mock.patch.object(runner, "log_event", side_effect=lambda kind, **payload: events.append((kind, payload))):
                runner._ensure_a2dp()

        self.assertEqual(
            reconnect_calls,
            [
                ("00:11:22:33:44:55", 2.0),
                ("00:11:22:33:44:55", 3.0),
            ],
        )
        self.assertEqual(connect_calls, [])
        self.assertEqual(events[-1][0], "ensure_a2dp")
        self.assertTrue(events[-1][1]["result"]["ok"])

    def test_runner_resolve_runtime_app_ids_retries_until_expected_apps_appear(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            runner = StressRunner(self._profile_payload(), run_root=temp_dir, phase_names=())
            requests = []
            responses = iter(
                [
                    {"apps": []},
                    {"ok": True},
                    {"apps": [{"app_id": "app:stressmusic", "app_name": "StressMusic"}]},
                ]
            )

            class _Client:
                def request(self, command, args=None, timeout_s=None):
                    requests.append((command, args, timeout_s))
                    return next(responses)

            runner._client = _Client()
            with mock.patch("tools.stress.run_stress_suite.time.sleep"):
                mapping = runner._resolve_runtime_app_ids(["StressMusic"], timeout_s=1.0, interval_s=0.01)

        self.assertEqual(mapping, {"StressMusic": "app:stressmusic"})
        self.assertEqual(requests[0][0], "get_runtime_summary")
        self.assertEqual(requests[1][0], "refresh_now")
        self.assertEqual(requests[2][0], "get_runtime_summary")

    def test_phase_module_isolation_exercises_module_controls(self):
        with tempfile.TemporaryDirectory() as temp_dir:
            runner = StressRunner(self._profile_payload(), run_root=temp_dir, phase_names=())
            requests = []

            class _Client:
                def request(self, command, args=None, timeout_s=None):
                    args = dict(args or {})
                    requests.append((command, args, timeout_s))
                    if command == "disable_module":
                        return {"module_id": "metering", "state": "disabled"}
                    if command == "enable_module":
                        return {"module_id": "metering", "state": "running"}
                    if command == "open_settings_tab":
                        return {"active_tab": args.get("tab_name"), "visible": True}
                    if command == "restart_module":
                        return {"module_id": args.get("module_id"), "state": "running"}
                    if command == "get_runtime_summary":
                        tab_name = "Updates" if any(
                            req[0] == "restart_module" and req[1].get("module_id") == "updates"
                            for req in requests
                        ) else "Health"
                        return {"settings_visible": True, "settings_tab": tab_name}
                    if command == "close_settings":
                        return {"visible": False}
                    raise AssertionError(f"unexpected command {command}")

            runner._client = _Client()
            with mock.patch.object(runner, "launch_app", return_value={"active_default_sink": "alsa_output.speakers"}), \
                    mock.patch.object(runner, "_start_direct_streams"), \
                    mock.patch.object(runner, "_probe_monitor_path", return_value={"ok": True}), \
                    mock.patch.object(runner, "quit_app"), \
                    mock.patch.object(runner, "_stop_all_probe_streams"):
                checks = runner.phase_module_isolation(1)

        self.assertTrue(any(name == "baseline_monitor_probe" for name in [check["name"] for check in checks]))
        self.assertIn(("disable_module", {"module_id": "metering", "reason": "stress-module-isolation"}, 10.0), requests)
        self.assertIn(("restart_module", {"module_id": "settings_ui", "reason": "stress-module-isolation"}, 10.0), requests)
        self.assertIn(("restart_module", {"module_id": "updates", "reason": "stress-module-isolation"}, 10.0), requests)
        self.assertIn(("restart_module", {"module_id": "mixer_ui", "reason": "stress-module-isolation"}, 10.0), requests)
        self.assertIn(("restart_module", {"module_id": "app_routing", "reason": "stress-module-isolation"}, 10.0), requests)
        self.assertIn(("restart_module", {"module_id": "effects", "reason": "stress-module-isolation"}, 10.0), requests)
        self.assertIn(("restart_module", {"module_id": "device_policy", "reason": "stress-module-isolation"}, 10.0), requests)


if __name__ == "__main__":
    unittest.main()
