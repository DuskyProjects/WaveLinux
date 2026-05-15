"""Client for the WaveLinux stress control socket."""

from __future__ import annotations

import json
import socket
import time
import uuid


class StressControlClient:
    def __init__(self, socket_path, *, timeout_s=10.0):
        self.socket_path = socket_path
        self.timeout_s = float(timeout_s)

    def request(self, command, args=None, *, timeout_s=None):
        request_id = uuid.uuid4().hex
        payload = {
            "id": request_id,
            "command": str(command),
            "args": dict(args or {}),
        }
        timeout_s = self.timeout_s if timeout_s is None else float(timeout_s)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(timeout_s)
            sock.connect(self.socket_path)
            sock.sendall((json.dumps(payload, sort_keys=True) + "\n").encode("utf-8"))
            file_obj = sock.makefile("rb")
            line = file_obj.readline()
        if not line:
            raise RuntimeError(f"no response for command {command}")
        response = json.loads(line.decode("utf-8"))
        if not response.get("ok"):
            raise RuntimeError(
                f"stress-control {command} failed: {response.get('error')}\n"
                f"{response.get('traceback', '')}".strip()
            )
        return response.get("result")

    def wait_for_ready(self, *, timeout_s=30.0):
        return self.request("wait_for_ready", {"timeout_s": float(timeout_s)}, timeout_s=timeout_s + 5.0)


def wait_for_socket(socket_path, *, timeout_s=15.0):
    deadline = time.monotonic() + float(timeout_s)
    while time.monotonic() < deadline:
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                sock.settimeout(0.5)
                sock.connect(socket_path)
                return True
        except OSError:
            time.sleep(0.1)
    return False
