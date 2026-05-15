"""Test-only local control socket for automated WaveLinux stress runs."""

from __future__ import annotations

import json
import os
import queue
import socket
import threading
import time
import traceback
import uuid

from PyQt6.QtCore import QObject, QTimer, pyqtSignal


class StressControlServer(QObject):
    command_requested = pyqtSignal(str, str, object)

    def __init__(self, window, *, socket_path=None):
        super().__init__(window)
        self.window = window
        self.socket_path = socket_path or f"/tmp/wavelinux-stress-control-{os.getpid()}.sock"
        self._pending = {}
        self._pending_lock = threading.Lock()
        self._server_thread = None
        self._stop_event = threading.Event()
        self._server_sock = None
        self.command_requested.connect(self._handle_command_main_thread)

    def start(self):
        self.stop()
        os.makedirs(os.path.dirname(self.socket_path), exist_ok=True)
        try:
            if os.path.exists(self.socket_path):
                os.remove(self.socket_path)
        except OSError:
            pass
        self._stop_event.clear()
        server_sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server_sock.bind(self.socket_path)
        server_sock.listen(8)
        server_sock.settimeout(0.5)
        self._server_sock = server_sock
        self._server_thread = threading.Thread(
            target=self._serve_forever,
            name="wavelinux-stress-control",
            daemon=True,
        )
        self._server_thread.start()
        return self.socket_path

    def stop(self):
        self._stop_event.set()
        sock = self._server_sock
        self._server_sock = None
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        thread = self._server_thread
        self._server_thread = None
        if thread is not None and thread.is_alive():
            thread.join(timeout=1.0)
        try:
            if os.path.exists(self.socket_path):
                os.remove(self.socket_path)
        except OSError:
            pass

    def _serve_forever(self):
        while not self._stop_event.is_set():
            try:
                conn, _ = self._server_sock.accept()
            except socket.timeout:
                continue
            except OSError:
                if self._stop_event.is_set():
                    break
                continue
            thread = threading.Thread(
                target=self._serve_connection,
                args=(conn,),
                name="wavelinux-stress-client",
                daemon=True,
            )
            thread.start()

    def _serve_connection(self, conn):
        with conn:
            try:
                file_obj = conn.makefile("rwb")
            except OSError:
                return
            with file_obj:
                while not self._stop_event.is_set():
                    line = file_obj.readline()
                    if not line:
                        break
                    try:
                        request = json.loads(line.decode("utf-8"))
                    except Exception as exc:
                        response = {
                            "id": "",
                            "ok": False,
                            "error": f"invalid_json:{exc}",
                        }
                    else:
                        response = self._handle_request(request)
                    file_obj.write((json.dumps(response, sort_keys=True) + "\n").encode("utf-8"))
                    file_obj.flush()

    def _handle_request(self, request):
        req_id = str(request.get("id") or uuid.uuid4().hex)
        command = str(request.get("command") or "").strip()
        args = request.get("args") or {}
        if not command:
            return {"id": req_id, "ok": False, "error": "missing_command"}
        if command == "wait_for_ready":
            timeout_s = max(0.1, min(float(args.get("timeout_s", 20.0) or 20.0), 300.0))
            deadline = time.monotonic() + timeout_s
            last_summary = {}
            while time.monotonic() < deadline and not self._stop_event.is_set():
                remaining = max(2.0, min(15.0, deadline - time.monotonic()))
                response = self._request_main_thread("get_runtime_summary", {}, timeout_s=remaining)
                if not response.get("ok"):
                    return {"id": req_id, "ok": False, "error": response.get("error", "summary_failed")}
                last_summary = dict(response.get("result") or {})
                if last_summary.get("ready"):
                    return {"id": req_id, "ok": True, "result": last_summary}
                time.sleep(0.1)
            return {
                "id": req_id,
                "ok": False,
                "error": "timeout",
                "result": last_summary,
            }
        response = self._request_main_thread(command, args, timeout_s=10.0)
        response["id"] = req_id
        return response

    def _request_main_thread(self, command, args, *, timeout_s):
        token = uuid.uuid4().hex
        result_queue = queue.Queue(maxsize=1)
        with self._pending_lock:
            self._pending[token] = result_queue
        self.command_requested.emit(token, command, dict(args or {}))
        try:
            response = result_queue.get(timeout=timeout_s)
        except queue.Empty:
            with self._pending_lock:
                self._pending.pop(token, None)
            return {"ok": False, "error": f"timeout_waiting_for_{command}"}
        return response

    def _handle_command_main_thread(self, token, command, args):
        try:
            result = self._dispatch_command(command, dict(args or {}))
            response = {"ok": True, "result": result}
        except Exception as exc:
            response = {
                "ok": False,
                "error": str(exc),
                "traceback": traceback.format_exc(limit=8),
            }
        with self._pending_lock:
            result_queue = self._pending.pop(token, None)
        if result_queue is not None:
            result_queue.put(response)

    def _dispatch_command(self, command, args):
        window = self.window
        if command == "ping":
            return {"pong": True, "socket_path": self.socket_path}
        if command == "get_runtime_summary":
            return window._stress_runtime_summary()
        if command == "get_health_summary":
            return window._stress_health_summary()
        if command == "list_modules":
            return window._stress_list_modules()
        if command == "get_module_health":
            return window._stress_get_module_health(args.get("module_id"))
        if command == "disable_module":
            return window._stress_disable_module(
                args.get("module_id"),
                reason=str(args.get("reason") or "stress-disable"),
            )
        if command == "enable_module":
            return window._stress_enable_module(args.get("module_id"))
        if command == "restart_module":
            return window._stress_restart_module(
                args.get("module_id"),
                reason=str(args.get("reason") or "stress-restart"),
            )
        if command == "export_diagnostics":
            reason = str(args.get("reason") or "stress-control").strip() or "stress-control"
            return {"path": window.runtime.export_diagnostics(reason=reason)}
        if command == "set_monitor_output":
            return window._stress_set_monitor_output(
                args.get("sink_name"),
                persist=bool(args.get("persist", True)),
                include_summary=bool(args.get("include_summary", False)),
            )
        if command == "set_stream_output":
            return window._stress_set_stream_output(
                args.get("sink_name"),
                persist=bool(args.get("persist", True)),
                include_summary=bool(args.get("include_summary", False)),
            )
        if command == "set_selected_mic":
            return window._stress_set_selected_mic(
                args.get("source_name") or args.get("mic_name"),
                persist=bool(args.get("persist", True)),
                include_summary=bool(args.get("include_summary", False)),
            )
        if command == "set_app_route":
            app_id = str(args.get("app_id") or "").strip()
            sink_name = args.get("sink_name")
            if not app_id:
                raise ValueError("set_app_route requires app_id")
            window.runtime.set_app_route(app_id, sink_name)
            if bool(args.get("refresh", True)):
                window._request_runtime_refresh("stress-set-app-route")
            return window._stress_runtime_summary()
        if command == "open_settings_tab":
            return window._stress_open_settings_tab(args.get("tab_name"))
        if command == "close_settings":
            return window._stress_close_settings()
        if command == "refresh_now":
            reason = str(args.get("reason") or "stress-refresh").strip() or "stress-refresh"
            window._request_runtime_refresh(reason)
            return {"requested": True, "reason": reason}
        if command == "quit_cleanly":
            QTimer.singleShot(0, window._request_quit_app)
            return {"accepted": True}
        if command == "list_known_sinks":
            return window._stress_list_known_sinks()
        if command == "list_known_sources":
            return window._stress_list_known_sources()
        raise ValueError(f"unknown_command:{command}")
