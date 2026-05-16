"""FX runtime description and verification helpers."""

from __future__ import annotations

from engine.effects_models import FxReadiness, FxRuntimeState, FxVerificationResult


def _status_attr(status, name, default=""):
    if status is None:
        return default
    if isinstance(status, dict):
        return status.get(name, default)
    return getattr(status, name, default)


def _safe_default_source(engine):
    getter = getattr(engine, "get_default_source", None)
    if getter is None:
        return ""
    try:
        return str(getter() or "").strip()
    except Exception:
        return ""


def _safe_source_visible(engine, source_name, *, snap=None):
    source_name = str(source_name or "").strip()
    if not source_name:
        return False
    resolver = getattr(engine, "resolve_source_name", None)
    if callable(resolver):
        try:
            return bool(resolver(source_name, snap=snap))
        except TypeError:
            return bool(resolver(source_name))
        except Exception:
            return False
    return False


def _safe_module_alive(engine, module_id, *, pattern=""):
    module_id = str(module_id or "").strip()
    if module_id:
        checker = getattr(engine, "_module_is_alive", None)
        if callable(checker):
            try:
                return bool(checker(module_id))
            except TypeError:
                return bool(checker(module_id, None))
            except Exception:
                return False
    if not pattern:
        return False
    finder = getattr(engine, "_find_module_by_arg", None)
    if callable(finder):
        try:
            found = finder(pattern)
        except Exception:
            return False
        if found is None:
            return False
        checker = getattr(engine, "_module_is_alive", None)
        if callable(checker):
            try:
                return bool(checker(found))
            except TypeError:
                return bool(checker(found, None))
            except Exception:
                return False
        return True
    return False


def describe_channel_fx_runtime(engine, node_name, *, snap=None, info=None, fx_status=None):
    node_name = str(node_name or "").strip()
    info = dict(info or engine.channel_fx.get(node_name) or {})
    if not info:
        return None

    mode = str(info.get("mode") or "").strip()
    requested_effects = [str(effect_id) for effect_id in list(info.get("effects") or []) if effect_id]
    params_map = {
        str(effect_id): dict(values or {})
        for effect_id, values in (info.get("params") or {}).items()
        if effect_id
    }
    source = str(info.get("source") or "").strip()
    active_chain_source = str(info.get("active_chain_source") or "").strip()
    active_chain_sink = str(info.get("active_chain_sink") or "").strip()
    capture_target = str(info.get("capture_target") or "").strip()
    proxy_sink_name = str(info.get("proxy_sink_name") or "").strip()
    proxy_source_name = str(
        info.get("proxy_source_name") or info.get("proxy_source_request_name") or source
    ).strip()
    proxy_sink_module_id = str(info.get("proxy_sink_module_id") or "").strip()
    proxy_source_module_id = str(info.get("proxy_source_module_id") or "").strip()
    loopbacks = [str(module_id) for module_id in list(info.get("loopbacks") or []) if str(module_id).strip()]
    processes = [str(proc_key) for proc_key in list(info.get("procs") or []) if str(proc_key).strip()]
    live_loopbacks = {
        module_id: _safe_module_alive(engine, module_id)
        for module_id in loopbacks
    }
    live_processes = {}
    for proc_key in processes:
        proc = (getattr(engine, "rnnoise_processes", {}) or {}).get(proc_key)
        live_processes[proc_key] = bool(proc is not None and proc.poll() is None)

    return FxRuntimeState(
        node_name=node_name,
        mode=mode,
        requested_effects=requested_effects,
        params_map=params_map,
        capture_target=capture_target,
        source=source,
        active_chain_source=active_chain_source,
        active_chain_sink=active_chain_sink,
        default_source=_safe_default_source(engine),
        proxy_sink_name=proxy_sink_name,
        proxy_source_name=proxy_source_name,
        proxy_sink_module_id=proxy_sink_module_id,
        proxy_source_module_id=proxy_source_module_id,
        proxy_sink_alive=_safe_module_alive(
            engine,
            proxy_sink_module_id,
            pattern=f"sink_name={proxy_sink_name}" if proxy_sink_name else "",
        ),
        proxy_source_alive=_safe_module_alive(
            engine,
            proxy_source_module_id,
            pattern=f"source_name={proxy_source_name}" if proxy_source_name else "",
        ),
        source_visible=_safe_source_visible(engine, source, snap=snap),
        active_chain_visible=_safe_source_visible(engine, active_chain_source, snap=snap),
        loopbacks=loopbacks,
        live_loopbacks=live_loopbacks,
        processes=processes,
        live_processes=live_processes,
        status_state=str(_status_attr(fx_status, "state", "") or "").strip(),
        status_message=str(_status_attr(fx_status, "message", "") or "").strip(),
        status_error=str(_status_attr(fx_status, "error", "") or "").strip(),
        status_generation=int(_status_attr(fx_status, "generation", 0) or 0),
    )


def verify_channel_fx_runtime(
    engine,
    node_name,
    *,
    expected_default=False,
    snap=None,
    info=None,
    fx_status=None,
    requested_effects=None,
):
    runtime = describe_channel_fx_runtime(
        engine,
        node_name,
        snap=snap,
        info=info,
        fx_status=fx_status,
    )
    requested = list(requested_effects) if requested_effects is not None else list(
        (runtime.requested_effects if runtime is not None else [])
    )
    if runtime is None:
        ready = not bool(requested)
        return FxVerificationResult(
            ready=ready,
            requested=bool(requested),
            state=str(_status_attr(fx_status, "state", "") or "").strip(),
            runtime=None,
            reasons=[
                FxReadiness(
                    "desired_fx_missing",
                    "Effects are requested, but no FX runtime state is present.",
                    {"node_name": str(node_name or "").strip()},
                )
            ] if requested else [],
        )

    reasons = []
    if not requested:
        return FxVerificationResult(
            ready=False,
            requested=False,
            state=runtime.status_state,
            runtime=runtime,
            reasons=[],
        )

    if runtime.status_state != "active":
        reasons.append(
            FxReadiness(
                "fx_status_not_active",
                (
                    "FX status is not active."
                    if not runtime.status_state
                    else f"FX status is {runtime.status_state}."
                ),
                {
                    "state": runtime.status_state,
                    "message": runtime.status_message,
                    "error": runtime.status_error,
                },
            )
        )
    if not runtime.source:
        reasons.append(
            FxReadiness(
                "desired_fx_missing",
                "The requested FX source is missing.",
                {"node_name": runtime.node_name},
            )
        )
    elif not runtime.source_visible:
        reasons.append(
            FxReadiness(
                "fx_proxy_source_missing" if runtime.mode in {"proxy", "proxy_passthrough"} else "fx_source_not_present",
                "The FX output source is not visible in PipeWire.",
                {"source": runtime.source},
            )
        )
    if expected_default and runtime.source and runtime.default_source:
        matcher = getattr(engine, "_source_names_match", None)
        matches = False
        if callable(matcher):
            try:
                matches = bool(matcher(runtime.default_source, runtime.source))
            except Exception:
                matches = runtime.default_source == runtime.source
        else:
            matches = runtime.default_source == runtime.source
        if not matches:
            reasons.append(
                FxReadiness(
                    "default_source_mismatch",
                    "The selected mic FX source is not the current default source.",
                    {
                        "expected": runtime.source,
                        "actual": runtime.default_source,
                    },
                )
            )

    if runtime.mode in {"proxy", "proxy_passthrough"}:
        if not runtime.proxy_sink_alive:
            reasons.append(
                FxReadiness(
                    "fx_proxy_sink_dead",
                    "The stable FX proxy sink module is missing or dead.",
                    {"sink_name": runtime.proxy_sink_name, "module_id": runtime.proxy_sink_module_id},
                )
            )
        if not runtime.proxy_source_alive:
            reasons.append(
                FxReadiness(
                    "fx_proxy_source_dead",
                    "The stable FX proxy source module is missing or dead.",
                    {"source_name": runtime.proxy_source_name, "module_id": runtime.proxy_source_module_id},
                )
            )
        if runtime.mode == "proxy" and not runtime.active_chain_visible:
            reasons.append(
                FxReadiness(
                    "fx_active_chain_source_missing",
                    "The active FX chain source is not visible.",
                    {"source": runtime.active_chain_source},
                )
            )
        if runtime.mode == "proxy_passthrough":
            if not runtime.loopbacks or not all(runtime.live_loopbacks.values()):
                reasons.append(
                    FxReadiness(
                        "fx_passthrough_feed_dead",
                        "The proxy passthrough feed loopback is missing or dead.",
                        {"loopbacks": dict(runtime.live_loopbacks)},
                    )
                )

    if runtime.processes:
        dead = [key for key, alive in runtime.live_processes.items() if not alive]
        if dead:
            reasons.append(
                FxReadiness(
                    "fx_process_dead",
                    "One or more FX processes are no longer running.",
                    {"dead_processes": dead},
                )
            )
    elif runtime.mode == "proxy" and requested:
        reasons.append(
            FxReadiness(
                "fx_process_dead",
                "No live FX process was recorded for the requested chain.",
                {"mode": runtime.mode},
            )
        )

    if runtime.mode == "proxy":
        if not runtime.loopbacks:
            reasons.append(
                FxReadiness(
                    "fx_loopback_dead",
                    "The FX chain loopbacks are missing.",
                    {"mode": runtime.mode},
                )
            )
        else:
            dead_loopbacks = [module_id for module_id, alive in runtime.live_loopbacks.items() if not alive]
            if dead_loopbacks:
                reasons.append(
                    FxReadiness(
                        "fx_loopback_dead",
                        "One or more FX loopbacks are missing or dead.",
                        {"dead_loopbacks": dead_loopbacks},
                    )
                )

    return FxVerificationResult(
        ready=not reasons,
        requested=True,
        state=runtime.status_state,
        runtime=runtime,
        reasons=reasons,
    )


def list_channel_fx_artifacts(engine, node_name, *, snap=None, info=None, fx_status=None):
    runtime = describe_channel_fx_runtime(
        engine,
        node_name,
        snap=snap,
        info=info,
        fx_status=fx_status,
    )
    if runtime is None:
        return {}
    return {
        "node_name": runtime.node_name,
        "mode": runtime.mode,
        "capture_target": runtime.capture_target,
        "source": runtime.source,
        "active_chain_source": runtime.active_chain_source,
        "active_chain_sink": runtime.active_chain_sink,
        "proxy_sink_name": runtime.proxy_sink_name,
        "proxy_source_name": runtime.proxy_source_name,
        "proxy_sink_module_id": runtime.proxy_sink_module_id,
        "proxy_source_module_id": runtime.proxy_source_module_id,
        "loopbacks": list(runtime.loopbacks),
        "processes": list(runtime.processes),
    }
