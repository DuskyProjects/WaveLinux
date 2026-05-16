#!/usr/bin/env python3
"""Deep selected-mic FX path diagnostic for RNNoise/device-swap failures."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import time
import traceback

if __package__ in (None, ""):
    PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    if PROJECT_ROOT not in sys.path:
        sys.path.insert(0, PROJECT_ROOT)

from tools.stress.audio_probe import capture_source_audio
from tools.stress.run_stress_suite import StressRunner, load_profile
from tools.stress.system_snapshot import ensure_dir, write_json


DEFAULT_SEQUENCE = ("a", "b", "a")
DEFAULT_CHECKPOINT_DELAYS = (0.0, 2.0, 8.5, 12.0)
FX_LIVE_PEAK = 16
FX_LIVE_RMS = 1.0


def run_cmd(cmd, *, timeout=8.0):
    result = subprocess.run(
        list(cmd),
        capture_output=True,
        text=True,
        timeout=float(timeout),
    )
    return {
        "cmd": list(cmd),
        "returncode": int(result.returncode),
        "stdout": (result.stdout or "").strip(),
        "stderr": (result.stderr or "").strip(),
    }


def split_pactl_sections(text, header):
    sections = []
    current = []
    for line in str(text or "").splitlines():
        if line.startswith(header):
            if current:
                sections.append("\n".join(current))
            current = [line]
        elif current:
            current.append(line)
    if current:
        sections.append("\n".join(current))
    return sections


def section_id(section):
    first = str(section or "").splitlines()[0] if section else ""
    match = re.search(r"#(\d+)", first)
    return match.group(1) if match else ""


def section_property(section, key):
    pattern = re.compile(rf"\b{re.escape(key)}\s*=\s*\"([^\"]+)\"")
    match = pattern.search(str(section or ""))
    return match.group(1) if match else ""


def section_for_owner(text, header, module_id):
    module_id = str(module_id or "").strip()
    if not module_id:
        return {}
    for section in split_pactl_sections(text, header):
        if f"Owner Module: {module_id}" not in section and f'module.id = "{module_id}"' not in section:
            continue
        return {
            "id": section_id(section),
            "node_name": section_property(section, "node.name"),
            "section": section,
        }
    return {}


def module_section(modules_text, module_id):
    module_id = str(module_id or "").strip()
    if not module_id:
        return ""
    marker = f"Module #{module_id}"
    start = str(modules_text or "").find(marker)
    if start < 0:
        return ""
    end = str(modules_text or "").find("Module #", start + len(marker))
    return str(modules_text or "")[start:end if end >= 0 else None]


def identify_loopbacks(runtime_state, modules_text):
    runtime_state = dict(runtime_state or {})
    capture_target = str(runtime_state.get("capture_target") or "").strip()
    chain_sink = str(runtime_state.get("active_chain_sink") or "").strip()
    chain_source = str(runtime_state.get("active_chain_source") or "").strip()
    proxy_sink = str(runtime_state.get("proxy_sink_name") or "").strip()
    result = {
        "upstream": "",
        "downstream": "",
        "sections": {},
    }
    for module_id in list(runtime_state.get("loopbacks") or []):
        module_id = str(module_id or "").strip()
        section = module_section(modules_text, module_id)
        result["sections"][module_id] = section
        if capture_target and chain_sink and f"source={capture_target}" in section and f"sink={chain_sink}" in section:
            result["upstream"] = module_id
        if chain_source and proxy_sink and f"source={chain_source}" in section and f"sink={proxy_sink}" in section:
            result["downstream"] = module_id
    return result


def capture_named_sources(sources, *, duration_s):
    captures = {}
    for key, source_name in dict(sources or {}).items():
        source_name = str(source_name or "").strip()
        if not source_name:
            captures[key] = {
                "source_name": "",
                "bytes": 0,
                "peak": 0,
                "rms": 0.0,
                "stderr": "missing source",
            }
            continue
        try:
            captures[key] = capture_source_audio(source_name, duration_s=float(duration_s))
        except Exception as exc:
            captures[key] = {
                "source_name": source_name,
                "bytes": 0,
                "peak": 0,
                "rms": 0.0,
                "stderr": repr(exc),
            }
    return captures


def capture_is_live(capture, *, peak_threshold=FX_LIVE_PEAK, rms_threshold=FX_LIVE_RMS):
    capture = dict(capture or {})
    return (
        int(capture.get("bytes") or 0) > 0
        and int(capture.get("peak") or 0) >= int(peak_threshold)
        and float(capture.get("rms") or 0.0) >= float(rms_threshold)
    )


def classify_fx_path(captures):
    captures = dict(captures or {})
    live = {key: capture_is_live(value) for key, value in captures.items()}
    return {
        "raw_live": bool(live.get("raw_source")),
        "upstream_source_output_live": bool(live.get("upstream_source_output_port")),
        "upstream_sink_input_live": bool(live.get("upstream_sink_input_port")),
        "chain_sink_monitor_live": bool(live.get("chain_sink_monitor")),
        "chain_source_live": bool(live.get("chain_source")),
        "proxy_source_live": bool(live.get("proxy_source")),
        "fx_output_live": bool(live.get("chain_source") or live.get("proxy_source")),
        "all_fx_points_live": all(
            bool(live.get(key))
            for key in (
                "upstream_source_output_port",
                "upstream_sink_input_port",
                "chain_sink_monitor",
                "chain_source",
                "proxy_source",
            )
        ),
    }


class FxPathDiagnostic:
    def __init__(self, profile, *, run_root, capture_duration_s=0.5):
        self.runner = StressRunner(
            profile,
            phase_names=("effects_cycle",),
            loop_counts={"effects_cycles": 1},
            run_root=run_root,
        )
        self.run_root = run_root
        self.capture_duration_s = float(capture_duration_s)
        self.records = []

    def _runtime_summary(self):
        return self.runner._client.request("get_runtime_summary", timeout_s=10.0)

    def _record(self, label, fixture):
        runtime = self._runtime_summary()
        fx_runtime = dict(runtime.get("selected_mic_fx_runtime") or {})
        modules = run_cmd(["pactl", "list", "modules"], timeout=8.0)
        source_outputs = run_cmd(["pactl", "list", "source-outputs"], timeout=8.0)
        sink_inputs = run_cmd(["pactl", "list", "sink-inputs"], timeout=8.0)
        loopbacks = identify_loopbacks(fx_runtime, modules.get("stdout", ""))
        upstream = loopbacks.get("upstream")
        upstream_source_output = section_for_owner(
            source_outputs.get("stdout", ""),
            "Source Output #",
            upstream,
        )
        upstream_sink_input = section_for_owner(
            sink_inputs.get("stdout", ""),
            "Sink Input #",
            upstream,
        )
        sources = {
            "raw_source": getattr(fixture, "source_name", ""),
            "upstream_source_output_port": (
                f"{upstream_source_output.get('node_name')}:capture_MONO"
                if upstream_source_output.get("node_name") else ""
            ),
            "upstream_sink_input_port": (
                f"{upstream_sink_input.get('node_name')}:output_MONO"
                if upstream_sink_input.get("node_name") else ""
            ),
            "chain_sink_monitor": (
                f"{fx_runtime.get('active_chain_sink')}.monitor"
                if fx_runtime.get("active_chain_sink") else ""
            ),
            "chain_source": fx_runtime.get("active_chain_source") or "",
            "proxy_source": runtime.get("selected_mic_fx_source") or "",
        }
        captures = capture_named_sources(
            sources,
            duration_s=self.capture_duration_s,
        )
        record = {
            "label": label,
            "at": time.time(),
            "selected_mic": runtime.get("selected_mic"),
            "fx_status": runtime.get("selected_mic_fx_status"),
            "fx_ready": bool(runtime.get("selected_mic_fx_ready")),
            "fx_verification": runtime.get("selected_mic_fx_verification"),
            "fx_runtime": fx_runtime,
            "loopbacks": loopbacks,
            "upstream_source_output": upstream_source_output,
            "upstream_sink_input": upstream_sink_input,
            "sources": sources,
            "captures": captures,
            "classification": classify_fx_path(captures),
        }
        self.records.append(record)
        write_json(os.path.join(self.run_root, "fx-path-diagnostic.latest.json"), self.records)
        return record

    def _external_reload_upstream(self, label, fixture):
        runtime = self._runtime_summary()
        fx_runtime = dict(runtime.get("selected_mic_fx_runtime") or {})
        modules = run_cmd(["pactl", "list", "modules"], timeout=8.0)
        loopbacks = identify_loopbacks(fx_runtime, modules.get("stdout", ""))
        upstream = loopbacks.get("upstream")
        capture_target = str(fx_runtime.get("capture_target") or "").strip()
        chain_sink = str(fx_runtime.get("active_chain_sink") or "").strip()
        commands = []
        if upstream and capture_target and chain_sink:
            commands.append(run_cmd(["pactl", "unload-module", upstream], timeout=8.0))
            time.sleep(1.0)
            commands.append(
                run_cmd(
                    [
                        "pactl",
                        "load-module",
                        "module-loopback",
                        f"source={capture_target}",
                        f"sink={chain_sink}",
                        "latency_msec=20",
                        "adjust_time=0",
                        "channels=1",
                        "channel_map=mono",
                        "source_dont_move=true",
                        "sink_dont_move=true",
                    ],
                    timeout=8.0,
                )
            )
            time.sleep(1.0)
        self.records.append({
            "label": f"{label}:external_reload",
            "at": time.time(),
            "upstream": upstream,
            "commands": commands,
        })
        return self._record(f"{label}:after_external_reload", fixture)

    def _activate_fixture(self, key):
        fixture = self.runner._ensure_synthetic_fixture(key)
        self.runner._start_synthetic_input_signal(
            fixture,
            stream_key=f"FxPath{key.upper()}",
        )
        self.runner._client.request(
            "set_selected_mic",
            {
                "source_name": fixture.source_name,
                "include_summary": True,
            },
            timeout_s=20.0,
        )
        self.runner._set_channel_fx(
            fixture.source_name,
            include_summary=True,
        )
        return fixture

    def run(self, sequence, checkpoint_delays, *, external_reload_mode="on-silent"):
        external_reload_mode = str(external_reload_mode or "on-silent").strip().lower()
        if external_reload_mode not in {"always", "on-silent", "never"}:
            raise ValueError(f"Unsupported external reload mode: {external_reload_mode}")
        ensure_dir(self.run_root)
        report = {
            "schema": 1,
            "started_at": time.time(),
            "sequence": list(sequence),
            "checkpoint_delays": list(checkpoint_delays),
            "external_reload_mode": external_reload_mode,
            "run_root": self.run_root,
            "records": self.records,
            "error": "",
            "traceback": "",
        }
        try:
            self.runner.launch_app()
            for index, key in enumerate(sequence):
                label = f"{index}:{key}"
                fixture = self._activate_fixture(key)
                self._record(f"{label}:after_request", fixture)
                try:
                    self.runner._wait_for_selected_mic_fx_ready(
                        fixture.source_name,
                        timeout_s=max(8.0, max(checkpoint_delays or [0.0]) + 4.0),
                        require_signal=False,
                    )
                except Exception as exc:
                    self.records.append({
                        "label": f"{label}:structural_wait_failed",
                        "at": time.time(),
                        "error": repr(exc),
                    })
                last_record = None
                for delay_s in checkpoint_delays:
                    time.sleep(max(0.0, float(delay_s)))
                    last_record = self._record(f"{label}:delay_{float(delay_s):.1f}s", fixture)
                should_external_reload = external_reload_mode == "always"
                if external_reload_mode == "on-silent":
                    classification = dict((last_record or {}).get("classification") or {})
                    should_external_reload = not bool(classification.get("all_fx_points_live"))
                if should_external_reload:
                    self._external_reload_upstream(label, fixture)
            self.runner.quit_app()
        except Exception as exc:
            report["error"] = str(exc)
            report["traceback"] = traceback.format_exc()
        finally:
            try:
                self.runner._stop_all_probe_streams()
            except Exception:
                pass
            try:
                self.runner._release_synthetic_fixtures()
            except Exception:
                pass
            try:
                self.runner.ensure_wave_stopped()
            except Exception:
                pass
            report["completed_at"] = time.time()
            report["records"] = self.records
            write_json(os.path.join(self.run_root, "fx-path-diagnostic.json"), report)
        return report


def parse_sequence(value):
    items = [item.strip().lower() for item in str(value or "").split(",") if item.strip()]
    return tuple(items or DEFAULT_SEQUENCE)


def parse_delays(value):
    if not value:
        return DEFAULT_CHECKPOINT_DELAYS
    return tuple(float(item.strip()) for item in str(value).split(",") if item.strip())


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", default="tools/stress/profile.current-machine.json")
    parser.add_argument("--appimage", default="dist/WaveLinux-3.0-x86_64.AppImage")
    parser.add_argument("--run-root", default="")
    parser.add_argument("--sequence", default=",".join(DEFAULT_SEQUENCE))
    parser.add_argument("--checkpoint-delays", default=",".join(str(v) for v in DEFAULT_CHECKPOINT_DELAYS))
    parser.add_argument("--capture-duration", type=float, default=0.5)
    parser.add_argument(
        "--external-reload-mode",
        choices=("always", "on-silent", "never"),
        default="on-silent",
        help="Whether to reload the upstream loopback after checkpoints. Default reloads only after a silent FX path.",
    )
    parser.add_argument("--skip-external-reload", action="store_true")
    args = parser.parse_args(argv)

    profile = load_profile(args.profile)
    appimage = os.path.abspath(os.path.expanduser(args.appimage))
    if appimage and os.path.exists(appimage):
        profile["appimage_path"] = appimage
    run_root = os.path.expanduser(args.run_root or f"/tmp/wavelinux-fx-path-diagnostic-{time.strftime('%Y%m%dT%H%M%S')}")
    diagnostic = FxPathDiagnostic(
        profile,
        run_root=run_root,
        capture_duration_s=args.capture_duration,
    )
    report = diagnostic.run(
        parse_sequence(args.sequence),
        parse_delays(args.checkpoint_delays),
        external_reload_mode="never" if args.skip_external_reload else args.external_reload_mode,
    )
    print(json.dumps({
        "ok": not bool(report.get("error")),
        "run_root": run_root,
        "report": os.path.join(run_root, "fx-path-diagnostic.json"),
        "records": len(report.get("records") or []),
        "error": report.get("error", ""),
    }, indent=2, sort_keys=True))
    return 0 if not report.get("error") else 1


if __name__ == "__main__":
    raise SystemExit(main())
