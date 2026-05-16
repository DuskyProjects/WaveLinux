"""Stress-control setup and runtime inspection helpers."""

from __future__ import annotations

import logging
import os
import time


class StressControlController:
    def __init__(self, window, *, startup_preflight_reporter, install_state_loader, enabled_fn):
        self.window = window
        self.startup_preflight_reporter = startup_preflight_reporter
        self.install_state_loader = install_state_loader
        self.enabled_fn = enabled_fn

    def _attrs(self):
        return self.window.__dict__

    def setup_stress_control(self):
        if not self.enabled_fn():
            return
        try:
            from stress_control import StressControlServer

            socket_path = str(os.environ.get("WAVELINUX_STRESS_SOCKET_PATH", "") or "").strip() or None
            server = StressControlServer(self.window, socket_path=socket_path)
            server.start()
            self.window._stress_control_server = server
            logging.info("Stress control enabled on %s", server.socket_path)
        except Exception as exc:
            logging.exception("Could not start stress control server: %s", exc)
            self.window._stress_control_server = None

    def stop_stress_control(self):
        server = self._attrs().get("_stress_control_server")
        if server is None:
            return
        try:
            server.stop()
        except Exception as exc:
            logging.warning("Could not stop stress control server cleanly: %s", exc)
        self.window._stress_control_server = None

    def stress_runtime_summary(self):
        default_sink = None
        default_source = None
        desired_mix_hw = self._attrs().get("_desired_mix_hw", {}) or {}
        monitor_output = desired_mix_hw.get("Monitor")
        stream_output = desired_mix_hw.get("Stream")
        live_monitor_output = None
        live_stream_output = None
        selected_fx_source = None
        selected_fx_status = {}
        selected_fx_runtime = {}
        selected_fx_verification = {}
        selected_fx_ready = False
        selected_fx_requested = {}
        selected_fx_generation = 0
        graph_nodes = []
        sink_inventory = []
        source_inventory = []
        runtime = self._attrs().get("runtime")
        selected_mic = str(self._attrs().get("selected_mic") or "").strip()
        status = None
        if runtime is not None and selected_mic:
            status = runtime.fx_status_for(selected_mic)
            selected_fx_status = {
                "state": str(getattr(status, "state", "") or "").strip(),
                "generation": int(getattr(status, "generation", 0) or 0),
                "message": str(getattr(status, "message", "") or "").strip(),
                "error": str(getattr(status, "error", "") or "").strip(),
            }
            if hasattr(runtime, "fx_request_for"):
                selected_fx_requested = runtime.fx_request_for(selected_mic)
            if hasattr(runtime, "fx_generation_for"):
                selected_fx_generation = runtime.fx_generation_for(selected_mic)
            else:
                selected_fx_generation = int(getattr(status, "generation", 0) or 0)
        try:
            with self.window.engine.session() as engine:
                snap = engine.create_snapshot(force=True)
                default_sink = engine.get_default_sink()
                default_source = engine.get_default_source()
                live_monitor_output = engine.get_live_mix_hardware_route("Monitor", snap=snap)
                live_stream_output = engine.get_live_mix_hardware_route("Stream", snap=snap)
                if selected_mic:
                    selected_fx_source = engine.get_channel_fx_source(selected_mic, snap=snap)
                    if hasattr(engine, "describe_channel_fx_runtime"):
                        runtime_state = engine.describe_channel_fx_runtime(
                            selected_mic,
                            snap=snap,
                            fx_status=status,
                        )
                        if runtime_state is not None:
                            selected_fx_runtime = runtime_state.to_dict()
                    if hasattr(engine, "verify_channel_fx_runtime"):
                        verification = engine.verify_channel_fx_runtime(
                            selected_mic,
                            expected_default=bool((selected_fx_requested or {}).get("effects")),
                            snap=snap,
                            fx_status=status,
                            requested_effects=(selected_fx_requested or {}).get("effects"),
                        )
                        if verification is not None:
                            selected_fx_verification = verification.to_dict()
                            selected_fx_ready = bool(getattr(verification, "ready", False))
                if hasattr(engine, "stable_sink_inventory"):
                    sink_inventory = list(engine.stable_sink_inventory(snap=snap) or [])
                if hasattr(engine, "stable_source_inventory"):
                    source_inventory = list(engine.stable_source_inventory(snap=snap) or [])
                for node in getattr(snap, "nodes", []) or []:
                    node_name = str(getattr(node, "name", "") or "").strip()
                    if not node_name:
                        continue
                    if (
                        node_name.startswith("wavelinux")
                        or node_name.startswith("output.wavelinux")
                        or node_name.startswith("input.wavelinux")
                    ):
                        graph_nodes.append(node_name)
        except Exception:
            logging.exception("Could not build stress runtime summary")
        view = self._attrs().get("_runtime_view_state")
        app_routes = {}
        app_summaries = []
        degraded_channels = []
        current_default_sink = default_sink
        current_default_source = default_source
        graph_blockers = {}
        if view is not None:
            current_default_sink = getattr(view, "default_sink", None) or current_default_sink
            current_default_source = getattr(view, "default_source", None) or current_default_source
            for app_view in getattr(view, "app_views", []) or []:
                app_summary = {
                    "app_id": str(getattr(app_view, "app_id", "") or ""),
                    "app_name": str(getattr(app_view, "app_name", "") or ""),
                    "resolved_app_id": str(getattr(app_view, "resolved_app_id", "") or ""),
                    "resolved_app_name": str(getattr(app_view, "resolved_app_name", "") or ""),
                    "identity_source": str(getattr(app_view, "identity_source", "") or ""),
                    "current_sink": getattr(app_view, "current_sink", None),
                    "current_volume": getattr(app_view, "current_volume", None),
                    "active_indices": list(getattr(app_view, "active_indices", []) or []),
                }
                app_summaries.append(app_summary)
                if app_summary["app_id"]:
                    app_routes[app_summary["app_id"]] = app_summary["current_sink"]
            degraded_channels = list(self.window._runtime_degraded_channels())
            graph_blockers = self.window._startup_graph_health_blockers(view=view)
        expected_default_source = selected_fx_source or self._attrs().get("selected_mic")
        audio_ready = self.window._startup_audio_ready(view=view)
        audio_ready_settled = self.window._startup_audio_ready_settled(view=view)
        fx_requested = bool((selected_fx_requested or {}).get("effects"))
        ready = bool(
            not self._attrs().get("_runtime_stopped", False)
            and audio_ready_settled
            and graph_nodes
            and monitor_output
            and live_monitor_output
            and current_default_sink
            and not graph_blockers
            and (
                not expected_default_source
                or current_default_source == expected_default_source
            )
            and (
                not fx_requested
                or selected_fx_ready
            )
        )
        return {
            "running": not bool(self._attrs().get("_runtime_stopped", False)),
            "ready": ready,
            "startup_audio_ready": bool(audio_ready),
            "startup_audio_ready_settled": bool(audio_ready_settled),
            "selected_mic": self._attrs().get("selected_mic"),
            "selected_mic_fx_generation": int(selected_fx_generation or 0),
            "selected_mic_fx_requested": {
                "capture_target": str((selected_fx_requested or {}).get("capture_target") or "").strip(),
                "effects": list((selected_fx_requested or {}).get("effects") or []),
                "params_map": {
                    effect_id: dict(values or {})
                    for effect_id, values in ((selected_fx_requested or {}).get("params_map") or {}).items()
                },
            },
            "selected_mic_fx_ready": bool(selected_fx_ready),
            "selected_mic_fx_runtime": dict(selected_fx_runtime or {}),
            "selected_mic_fx_verification": dict(selected_fx_verification or {}),
            "selected_mic_fx_source": selected_fx_source,
            "selected_mic_fx_status": selected_fx_status,
            "expected_default_source": expected_default_source,
            "active_default_sink": current_default_sink,
            "active_default_source": current_default_source,
            "monitor_output": monitor_output,
            "stream_output": stream_output,
            "live_monitor_output": live_monitor_output,
            "live_stream_output": live_stream_output,
            "wave_modules_loaded": len(set(graph_nodes)),
            "graph_present": bool(graph_nodes),
            "graph_nodes": sorted(set(graph_nodes)),
            "degraded_channels": degraded_channels,
            "graph_blockers": dict(graph_blockers),
            "app_routes": app_routes,
            "apps": app_summaries,
            "modules": self.window._stress_list_modules(),
            "known_sinks": sink_inventory,
            "known_sources": source_inventory,
            "settings_tab": self.window._active_settings_tab_name(),
            "settings_visible": self.window._settings_dialog_visible(),
        }

    def stress_health_summary(self):
        issues = self.window._collect_health_issues(
            preflight=self._attrs().get("_startup_preflight") or self.startup_preflight_reporter(),
            state=self.install_state_loader(),
        )
        return [
            {
                "code": issue.code,
                "severity": issue.severity,
                "title": issue.title,
                "detail": issue.detail,
                "primary_action": issue.primary_action,
                "secondary_action": issue.secondary_action,
                "context": dict(issue.context or {}),
            }
            for issue in issues
        ]

    def stress_list_modules(self):
        manager = self._attrs().get("module_manager")
        if manager is None:
            return []
        return [
            {
                "module_id": health.module_id,
                "state": health.state,
                "summary": health.summary,
                "issues": list(health.issues),
                "restartable": bool(health.restartable),
            }
            for health in manager.list_modules()
        ]

    def stress_get_module_health(self, module_id):
        manager = self._attrs().get("module_manager")
        if manager is None:
            return {}
        health = manager.module_health(str(module_id or ""))
        return {
            "module_id": health.module_id,
            "state": health.state,
            "summary": health.summary,
            "issues": list(health.issues),
            "restartable": bool(health.restartable),
        }

    def stress_disable_module(self, module_id, *, reason="stress-disable"):
        manager = self._attrs().get("module_manager")
        if manager is None:
            return {"accepted": False}
        manager.disable_module(str(module_id or ""), reason)
        return self.window._stress_get_module_health(module_id)

    def stress_enable_module(self, module_id):
        manager = self._attrs().get("module_manager")
        if manager is None:
            return {"accepted": False}
        manager.enable_module(str(module_id or ""))
        return self.window._stress_get_module_health(module_id)

    def stress_restart_module(self, module_id, *, reason="stress-restart"):
        manager = self._attrs().get("module_manager")
        if manager is None:
            return {"accepted": False}
        manager.restart_module(str(module_id or ""), reason)
        return self.window._stress_get_module_health(module_id)

    def stress_list_known_sinks(self):
        summary = self.window._stress_runtime_summary()
        return list(summary.get("known_sinks") or [])

    def stress_list_known_sources(self):
        summary = self.window._stress_runtime_summary()
        return list(summary.get("known_sources") or [])

    def stress_set_monitor_output(self, sink_name, *, persist=True, include_summary=False):
        sink_name = str(sink_name or "").strip() or None
        self.window._set_mix_output_target(
            "Monitor",
            sink_name,
            persist=bool(persist),
            update_combo=True,
            sync_runtime=True,
            sync_runtime_refresh=False,
        )
        if sink_name:
            self.window._record_preferred_monitor(sink_name, view=self._attrs().get("_runtime_view_state"))
            self.window._schedule_monitor_route_followups(sink_name)
        if include_summary:
            return self.window._stress_runtime_summary()
        return {
            "monitor_output": sink_name,
            "requested": True,
        }

    def stress_set_stream_output(self, sink_name, *, persist=True, include_summary=False):
        sink_name = str(sink_name or "").strip() or None
        self.window._set_mix_output_target(
            "Stream",
            sink_name,
            persist=bool(persist),
            update_combo=True,
            sync_runtime=False,
            sync_runtime_refresh=False,
        )
        if include_summary:
            return self.window._stress_runtime_summary()
        return {
            "stream_output": sink_name,
            "requested": True,
        }

    def stress_set_selected_mic(self, mic_name, *, persist=True, include_summary=False):
        self.window._set_selected_mic_target(
            str(mic_name or "").strip() or None,
            record_preference=True,
            persist=bool(persist),
            request_refresh=True,
            view=self._attrs().get("_runtime_view_state"),
        )
        if include_summary:
            return self.window._stress_runtime_summary()
        return {
            "selected_mic": self._attrs().get("selected_mic"),
            "requested": True,
        }

    def stress_set_channel_fx(self, node_name, effects=None, params_map=None, *, persist=True, include_summary=False):
        node_name = str(node_name or "").strip() or str(self._attrs().get("selected_mic") or "").strip()
        if not node_name:
            raise ValueError("set_channel_fx requires node_name")
        wanted, normalized = self.window._normalize_effect_request_for_node(
            node_name,
            effects or [],
            params_map or {},
        )
        if wanted:
            self.window.active_effects[node_name] = list(wanted)
        else:
            self.window.active_effects.pop(node_name, None)
        if wanted and normalized:
            self.window.effect_params[node_name] = {
                effect_id: dict(values)
                for effect_id, values in normalized.items()
                if values
            }
            if not self.window.effect_params[node_name]:
                self.window.effect_params.pop(node_name, None)
        else:
            self.window.effect_params.pop(node_name, None)
        self.window._sync_runtime_persistent_state(immediate=True)
        if bool(persist):
            self.window.schedule_save()
        runtime = self._attrs().get("runtime")
        status = runtime.fx_status_for(node_name) if runtime is not None else None
        status_payload = {
            "state": str(getattr(status, "state", "") or "").strip(),
            "generation": int(getattr(status, "generation", 0) or 0),
            "message": str(getattr(status, "message", "") or "").strip(),
            "error": str(getattr(status, "error", "") or "").strip(),
        }
        if include_summary:
            summary = self.window._stress_runtime_summary()
            summary["fx_status"] = status_payload
            summary["fx_node_name"] = node_name
            summary["requested_effects"] = list(wanted)
            return summary
        return {
            "node_name": node_name,
            "requested": True,
            "effects": list(wanted),
            "fx_status": status_payload,
        }

    def stress_open_settings_tab(self, tab_name):
        started = time.monotonic()
        if not self.window._settings_dialog_visible():
            open_started = time.monotonic()
            self.window._open_settings()
            open_elapsed_ms = (time.monotonic() - open_started) * 1000.0
        else:
            open_elapsed_ms = 0.0
        tabs = self._attrs().get("_settings_tabs")
        names = tuple(self._attrs().get("_settings_tab_names", ()) or ())
        tab_name = str(tab_name or "").strip()
        switch_elapsed_ms = 0.0
        if tabs is not None and tab_name:
            switch_started = time.monotonic()
            for index, name in enumerate(names):
                if str(name or "").strip().lower() == tab_name.lower():
                    tabs.setCurrentIndex(index)
                    break
            switch_elapsed_ms = (time.monotonic() - switch_started) * 1000.0
        refresh_started = time.monotonic()
        self.window._schedule_active_settings_tab_refresh(force=False)
        refresh_elapsed_ms = (time.monotonic() - refresh_started) * 1000.0
        total_elapsed_ms = (time.monotonic() - started) * 1000.0
        if total_elapsed_ms >= 100.0:
            logging.info(
                "Slow stress open_settings_tab(%s): %.1f ms total (open=%.1f, switch=%.1f, refresh=%.1f)",
                tab_name,
                total_elapsed_ms,
                open_elapsed_ms,
                switch_elapsed_ms,
                refresh_elapsed_ms,
            )
        return {
            "active_tab": self.window._active_settings_tab_name(),
            "visible": self.window._settings_dialog_visible(),
        }

    def stress_close_settings(self):
        dialog = self._attrs().get("settings_dialog")
        if dialog is not None:
            dialog.hide()
        return {"visible": self.window._settings_dialog_visible()}
