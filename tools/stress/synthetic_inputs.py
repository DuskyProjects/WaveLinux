"""Synthetic input fixtures for deterministic FX stress validation."""

from __future__ import annotations

import subprocess
import time


def _run(cmd, *, timeout=5.0):
    result = subprocess.run(
        list(cmd),
        capture_output=True,
        text=True,
        timeout=float(timeout),
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"Command failed: {' '.join(cmd)}\n"
            f"{(result.stderr or result.stdout or '').strip()}"
        )
    return (result.stdout or "").strip()


def _list_short_sources():
    return _run(["pactl", "list", "short", "sources"], timeout=5.0)


def _resolve_source_name(requested_name):
    requested_name = str(requested_name or "").strip()
    if not requested_name:
        return ""
    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline:
        text = _list_short_sources()
        exact = ""
        fallback = ""
        bare_requested = requested_name.removeprefix("output.")
        for line in text.splitlines():
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            name = str(parts[1] or "").strip()
            if not name:
                continue
            if name == requested_name:
                exact = name
                break
            if name == bare_requested or name == f"output.{bare_requested}":
                fallback = name
        if exact:
            return exact
        if fallback:
            return fallback
        time.sleep(0.1)
    return ""


class SyntheticInputFixture:
    def __init__(self, base_name):
        base_name = str(base_name or "").strip()
        if not base_name:
            raise ValueError("SyntheticInputFixture requires a non-empty base_name")
        if not base_name.startswith("wavelinux_stress_fx_"):
            base_name = f"wavelinux_stress_fx_{base_name}"
        self.base_name = base_name
        self.sink_name = f"{base_name}.sink"
        self.requested_source_name = f"{base_name}.source"
        self.source_name = ""
        self.sink_module_id = ""
        self.source_module_id = ""

    def provision(self):
        if self.sink_module_id:
            return self
        self.sink_module_id = _run(
            [
                "pactl",
                "load-module",
                "module-null-sink",
                f"sink_name={self.sink_name}",
                "channels=1",
                "channel_map=mono",
                (
                    "sink_properties="
                    "device.description=_WaveLinux-Stress-Synthetic-Input "
                    "node.description=_WaveLinux-Stress-Synthetic-Input "
                    "media.name=_WaveLinux-Stress-Synthetic-Input "
                    "application.name=_WaveLinux-Stress-Synthetic-Input "
                    "media.class=Audio/Sink"
                ),
            ],
            timeout=5.0,
        )
        try:
            _run(["pactl", "set-sink-mute", self.sink_name, "0"], timeout=3.0)
            _run(["pactl", "set-sink-volume", self.sink_name, "100%"], timeout=3.0)
            self.source_module_id = _run(
                [
                    "pactl",
                    "load-module",
                    "module-virtual-source",
                    f"source_name={self.requested_source_name}",
                    f"master={self.sink_name}.monitor",
                    "channels=1",
                    "channel_map=mono",
                    (
                        "source_properties="
                        "device.description=_WaveLinux-Stress-Synthetic-Source "
                        "node.description=_WaveLinux-Stress-Synthetic-Source "
                        "media.name=_WaveLinux-Stress-Synthetic-Source "
                        "application.name=_WaveLinux-Stress-Synthetic-Source "
                        "media.class=Audio/Source "
                        "device.class=sound"
                    ),
                ],
                timeout=5.0,
            )
            self.source_name = _resolve_source_name(self.requested_source_name) or self.requested_source_name
        except Exception:
            self.unload()
            raise
        return self

    def unload(self):
        for module_id in (self.source_module_id, self.sink_module_id):
            module_id = str(module_id or "").strip()
            if not module_id:
                continue
            try:
                subprocess.run(
                    ["pactl", "unload-module", module_id],
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=3.0,
                )
            except Exception:
                pass
        self.source_module_id = ""
        self.sink_module_id = ""
        self.source_name = ""

    def to_dict(self):
        return {
            "base_name": self.base_name,
            "sink_name": self.sink_name,
            "requested_source_name": self.requested_source_name,
            "source_name": self.source_name,
            "sink_module_id": self.sink_module_id,
            "source_module_id": self.source_module_id,
        }
