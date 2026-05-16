"""Channel FX graph transaction helpers."""

from __future__ import annotations

import engine.effects_pipeline as effects_pipeline
import logging
import time


def fx_result(success, *, active_source=None, kept_source=None,
              rolled_back=False, failure_stage=None, message=""):
    return {
        "success": bool(success),
        "active_source": active_source,
        "kept_source": kept_source,
        "rolled_back": bool(rolled_back),
        "failure_stage": failure_stage,
        "message": message or "",
    }


def teardown_fx_plumbing(engine, info):
    for mod_id in info.get("loopbacks", []):
        engine._run(["pactl", "unload-module", str(mod_id)])
    for pk in info.get("procs", []):
        engine.stop_rnnoise(pk)


def unload_submix_replacements(engine, replacements):
    for binding in replacements.values():
        module_id = binding.get("module_id")
        if module_id is not None:
            engine._run(["pactl", "unload-module", str(module_id)])


def _stored_proxy_info(info):
    return {
        "sink_name": info.get("proxy_sink_name"),
        "sink_module_id": info.get("proxy_sink_module_id"),
        "source_name": info.get("proxy_source_name") or info.get("source"),
        "source_request_name": info.get("proxy_source_request_name"),
        "source_module_id": info.get("proxy_source_module_id"),
    }


def _proxy_is_usable(engine, proxy):
    sink_name = str(proxy.get("sink_name") or "").strip()
    source_name = str(proxy.get("source_name") or "").strip()
    if not sink_name or not source_name:
        return False
    sink_module_id = proxy.get("sink_module_id")
    if sink_module_id is not None and not engine._module_is_alive(sink_module_id):
        return False
    source_module_id = proxy.get("source_module_id")
    if source_module_id is not None and not engine._module_is_alive(source_module_id):
        return False
    resolve_source_name = getattr(engine, "resolve_source_name", None)
    if callable(resolve_source_name):
        requested = str(proxy.get("source_request_name") or source_name).strip()
        resolved = (
            resolve_source_name(requested)
            or resolve_source_name(source_name)
        )
        if not resolved:
            return False
        proxy["source_name"] = resolved
    return True


def _prepare_proxy(engine, info, node_name):
    proxy = _stored_proxy_info(info)
    if _proxy_is_usable(engine, proxy):
        return proxy
    safe_key = str(info.get("safe_key") or engine._safe_channel_key(node_name) or "").strip()
    if not safe_key:
        return None
    return engine._ensure_fx_proxy(safe_key)


def _source_visible(engine, source_name, snap=None):
    source_name = str(source_name or "").strip()
    if not source_name:
        return False
    resolver = getattr(engine, "resolve_source_name", None)
    if callable(resolver):
        try:
            if resolver(source_name, snap=snap):
                return True
        except TypeError:
            if resolver(source_name):
                return True
    return False


def _wait_sink_visible(engine, sink_name, *, attempts=20, delay=0.05):
    sink_name = str(sink_name or "").strip()
    if not sink_name:
        return False
    checker = getattr(engine, "_sink_visible", None)
    if not callable(checker):
        return True
    for _ in range(max(1, int(attempts))):
        try:
            if checker(sink_name):
                return True
        except Exception:
            pass
        time.sleep(max(0.0, float(delay)))
    return False


def _fx_info_is_live(engine, info, snap=None):
    for proc_key in info.get("procs", []):
        proc = engine.rnnoise_processes.get(proc_key)
        if proc is None or proc.poll() is not None:
            return False

    mode = str(info.get("mode") or "").strip()
    if mode not in {"proxy", "proxy_passthrough"}:
        return bool(info.get("source"))

    for module_key in ("proxy_sink_module_id", "proxy_source_module_id"):
        module_id = info.get(module_key)
        if module_id is not None and not engine._module_is_alive(module_id):
            return False

    for loopback_id in info.get("loopbacks", []):
        if loopback_id is not None and not engine._module_is_alive(loopback_id):
            return False

    if not _source_visible(engine, info.get("source"), snap=snap):
        return False

    if mode == "proxy" and not _source_visible(engine, info.get("active_chain_source"), snap=snap):
        return False

    return True


def reprime_channel_fx_capture(engine, node_name, *, settle_s=1.0):
    info = engine.channel_fx.get(str(node_name or "").strip()) or {}
    if str(info.get("mode") or "").strip() != "proxy":
        return False
    capture_target = str(info.get("capture_target") or "").strip()
    active_chain_sink = str(info.get("active_chain_sink") or "").strip()
    loopbacks = list(info.get("loopbacks") or [])
    modules_text = engine._run(["pactl", "list", "modules"]) or ""
    upstream_loopback = ""
    for module_id in loopbacks:
        module_id = str(module_id or "").strip()
        if not module_id:
            continue
        marker = f"Module #{module_id}"
        start = modules_text.find(marker)
        if start < 0:
            continue
        end = modules_text.find("Module #", start + len(marker))
        section = modules_text[start:end if end >= 0 else None]
        if f"source={capture_target}" not in section:
            continue
        if f"sink={active_chain_sink}" not in section:
            continue
        upstream_loopback = module_id
        break
    if not capture_target or not active_chain_sink or not upstream_loopback:
        return False
    loader = getattr(engine, "_load_loopback_module", None)
    if callable(loader):
        new_loopback = loader(
            capture_target,
            active_chain_sink,
            channels=1,
            channel_map="mono",
            source_dont_move=True,
            sink_dont_move=True,
        )
    else:
        new_loopback = engine._wait_load_loopback(
            capture_target,
            active_chain_sink,
            channels=1,
            channel_map="mono",
            source_dont_move=True,
            sink_dont_move=True,
        )
    if new_loopback is None:
        return False
    time.sleep(max(0.0, float(settle_s)))
    engine._run(["pactl", "unload-module", upstream_loopback])
    info["loopbacks"] = [
        new_loopback if str(module_id or "").strip() == upstream_loopback else str(module_id)
        for module_id in loopbacks
        if str(module_id or "").strip()
    ]
    engine.invalidate_snapshot()
    return True


def apply_channel_fx_transaction(engine, node_name, capture_target, effects, params_map=None):
    """Transactionally replace a channel FX chain without dropping audio."""
    if not node_name:
        return fx_result(False, failure_stage="precondition", message="Missing channel name")

    params_map = params_map or {}
    ordered = [fid for fid in engine._ordered_chain(effects)
               if engine.effect_available(fid)]
    if not ordered:
        return clear_channel_fx_transaction(
            engine,
            node_name,
            target_source=capture_target,
        )

    old_info = dict(engine.channel_fx.get(node_name) or {})
    if engine._is_inline_fx_info(old_info):
        try:
            engine._clear_inline_channel_fx(node_name, old_info)
        finally:
            engine.channel_fx.pop(node_name, None)
        old_info = {}

    capture_target = str(capture_target or old_info.get("capture_target") or "").strip() or None
    proxy = None

    if old_info.get("mode") in {"proxy", "proxy_passthrough"}:
        proxy = _prepare_proxy(engine, old_info, node_name)
    reuse_proxy = proxy is not None
    effective_source = old_info.get("source") or capture_target
    if not reuse_proxy and old_info.get("mode") in {"proxy", "proxy_passthrough"} and capture_target:
        effective_source = capture_target
    if not effective_source:
        return fx_result(
            False,
            kept_source=None,
            failure_stage="precondition",
            message="Missing capture target for FX chain",
        )

    safe_key = engine._safe_channel_key(node_name)
    stamp = int(time.time() * 1000)
    config_path, sink_name, source_name, used_effects = (
        engine._build_unified_chain_config(safe_key, ordered, params_map, stamp)
    )
    if config_path is None or not used_effects:
        return fx_result(
            False,
            kept_source=effective_source,
            failure_stage="config_build",
            message="No renderable effects were available for this chain",
        )

    log_path = engine._fx_log_path(safe_key, f"chain_{stamp}")
    proc_key = f"chain_{safe_key}_{stamp}"
    if not engine._spawn_fx(config_path, log_path, proc_key):
        logging.warning(
            f"Unified FX chain failed to spawn for {node_name}; "
            f"see {log_path} for the pipewire stderr."
        )
        return fx_result(
            False,
            kept_source=effective_source,
            failure_stage="spawn",
            message=f"FX chain failed to spawn; see {log_path}",
        )

    mic_cutover = bool(capture_target and not capture_target.endswith(".monitor"))
    default_before = engine.get_default_source() if mic_cutover else None
    prev_default = old_info.get("prev_default")
    if prev_default is None and mic_cutover and default_before:
        prev_default = default_before

    binding_snapshot = {}
    external_source_outputs = []
    if not reuse_proxy:
        binding_snapshot = engine._snapshot_submix_bindings(effective_source)
        exclude_modules = list(old_info.get("loopbacks", []))
        for binding in binding_snapshot.values():
            old_module_id = binding.get("module_id")
            if old_module_id is not None:
                exclude_modules.append(old_module_id)
        external_source_outputs = engine.snapshot_external_source_outputs(
            effective_source,
            exclude_modules=exclude_modules,
        )

    lb = None
    proxy_feed = None
    replacements = {}
    default_changed = False
    failure_stage = None

    try:
        if not engine._wait_source_visible(source_name):
            failure_stage = "candidate_source"
            raise RuntimeError("candidate source did not appear")

        if not _wait_sink_visible(engine, sink_name):
            failure_stage = "candidate_sink"
            raise RuntimeError("candidate sink did not appear")

        if proxy is None:
            proxy = engine._ensure_fx_proxy(safe_key)
            if proxy is None:
                failure_stage = "proxy_create"
                raise RuntimeError("stable FX source could not be created")

        if not _wait_sink_visible(engine, proxy["sink_name"]):
            failure_stage = "proxy_sink"
            raise RuntimeError("stable FX sink did not appear")

        proxy_feed = engine._wait_load_loopback(
            source_name,
            proxy["sink_name"],
            channels=1,
            channel_map="mono",
            source_dont_move=True,
            sink_dont_move=True,
        )
        if proxy_feed is None or not engine._module_is_alive(proxy_feed):
            failure_stage = "proxy_feed"
            raise RuntimeError("processed signal could not be attached to the stable FX source")

        # Prime downstream demand before attaching the live mic/source input.
        time.sleep(0.1)

        lb = engine._wait_load_loopback(
            capture_target,
            sink_name,
            channels=1,
            channel_map="mono",
            source_dont_move=True,
            sink_dont_move=True,
        )
        if lb is None:
            failure_stage = "upstream_loopback"
            raise RuntimeError("candidate upstream loopback failed")

        if not reuse_proxy:
            for key, binding in binding_snapshot.items():
                module_id = engine._create_submix_replacement(
                    proxy["source_name"],
                    binding["mix_name"],
                    initial_state=binding.get("state") or {},
                )
                if module_id is None:
                    failure_stage = f"submix_{binding['mix_name'].lower()}"
                    raise RuntimeError(
                        f"replacement submix loopback failed for {binding['mix_name']}"
                    )
                replacements[key] = {
                    "mix_name": binding["mix_name"],
                    "module_id": module_id,
                    "old_module_id": binding.get("module_id"),
                    "state": dict(binding.get("state", {}) or {}),
                }

        if mic_cutover:
            proxy_source = (
                engine._wait_source_visible(proxy["source_name"], attempts=20, delay=0.05)
                or engine.resolve_source_name(proxy["source_name"])
                or proxy["source_name"]
            )
            proxy["source_name"] = proxy_source
            if not engine._source_names_match(default_before, proxy_source):
                if not engine.set_default_source(proxy_source):
                    failure_stage = "default_source"
                    raise RuntimeError("default source could not switch to the stable FX source")
                default_changed = True
            if not reuse_proxy and not engine._move_known_source_outputs(
                    external_source_outputs,
                    effective_source,
                    proxy_source):
                failure_stage = "source_output_move"
                raise RuntimeError("source-output move to stable FX source failed")
            if not engine._source_names_match(engine.get_default_source(), proxy_source):
                failure_stage = "default_source"
                raise RuntimeError("default source did not switch to the stable FX source")

        candidate_info = {
            "mode": "proxy",
            "effects": list(used_effects),
            "params": {
                fid: dict(params_map.get(fid, {}))
                for fid in used_effects
            },
            "procs": [proc_key],
            "loopbacks": [proxy_feed, lb],
            "source": proxy["source_name"],
            "active_chain_source": source_name,
            "active_chain_sink": sink_name,
            "capture_target": capture_target,
            "safe_key": safe_key,
            "prev_default": prev_default,
            "proxy_sink_name": proxy["sink_name"],
            "proxy_sink_module_id": proxy["sink_module_id"],
            "proxy_source_name": proxy["source_name"],
            "proxy_source_request_name": proxy.get("source_request_name"),
            "proxy_source_module_id": proxy["source_module_id"],
        }
        verification = effects_pipeline.verify_channel_fx_runtime(
            engine,
            node_name,
            expected_default=mic_cutover,
            info=candidate_info,
            fx_status={
                "state": "active",
                "generation": 0,
                "message": "FX chain active",
                "error": "",
            },
            requested_effects=used_effects,
        )
        if not verification.ready:
            failure_stage = "verification"
            reason_text = "; ".join(
                str(reason.detail or reason.code).strip()
                for reason in verification.reasons
            ).strip()
            raise RuntimeError(reason_text or "FX runtime verification failed")
        engine.channel_fx[node_name] = candidate_info
        if replacements:
            engine._commit_submix_replacements(
                replacements,
                new_source=proxy["source_name"],
            )
        if old_info:
            engine._teardown_fx_plumbing(old_info)
        engine.invalidate_snapshot()
        return fx_result(
            True,
            active_source=proxy["source_name"],
            kept_source=proxy["source_name"],
            message="FX chain active",
        )
    except Exception as exc:
        if default_changed and default_before:
            engine.set_default_source(default_before)
        if not reuse_proxy and proxy and effective_source:
            engine._move_known_source_outputs(
                external_source_outputs,
                proxy["source_name"],
                effective_source,
            )
        engine._unload_submix_replacements(replacements)
        if proxy_feed is not None:
            engine._run(["pactl", "unload-module", str(proxy_feed)])
        if lb is not None:
            engine._run(["pactl", "unload-module", str(lb)])
        engine.stop_rnnoise(proc_key)
        if not reuse_proxy and proxy:
            engine._destroy_fx_proxy({
                "proxy_sink_name": proxy.get("sink_name"),
                "proxy_sink_module_id": proxy.get("sink_module_id"),
                "proxy_source_name": proxy.get("source_name"),
                "proxy_source_request_name": proxy.get("source_request_name"),
                "proxy_source_module_id": proxy.get("source_module_id"),
            })
        engine.invalidate_snapshot()
        return fx_result(
            False,
            kept_source=proxy["source_name"] if reuse_proxy and proxy else effective_source,
            rolled_back=True,
            failure_stage=failure_stage or "cutover",
            message=str(exc),
        )


def clear_channel_fx_transaction(engine, node_name, target_source=None, keep_proxy=False):
    """Transactionally clear a channel FX chain without dropping audio."""
    info = engine.channel_fx.get(node_name)
    if not info:
        return fx_result(True, kept_source=target_source, message="FX chain already cleared")

    if engine._is_inline_fx_info(info):
        return engine._clear_inline_channel_fx(node_name, info)

    if info.get("mode") in {"proxy", "proxy_passthrough"} and info.get("proxy_sink_name"):
        proxy = _prepare_proxy(engine, info, node_name)
        proxy_source = (proxy or {}).get("source_name") or info.get("proxy_source_name") or info.get("source")
        proxy_sink = (proxy or {}).get("sink_name") or info.get("proxy_sink_name")
        capture_target = info.get("capture_target") or ""
        dest_source = target_source or capture_target
        if not proxy_source or not proxy_sink or not dest_source:
            engine._teardown_fx_plumbing(info)
            engine._destroy_fx_proxy(info)
            engine.channel_fx.pop(node_name, None)
            engine.invalidate_snapshot()
            return fx_result(
                True,
                kept_source=dest_source,
                message="FX chain state was incomplete",
            )

        binding_snapshot = engine._snapshot_submix_bindings(proxy_source)
        external_source_outputs = engine.snapshot_external_source_outputs(
            proxy_source,
        )
        mic_cutover = bool(capture_target and not capture_target.endswith(".monitor"))
        default_before = engine.get_default_source() if mic_cutover else None
        replacement_default = target_source or info.get("prev_default") or capture_target
        replacements = {}
        replacement_feed = None
        default_changed = False
        failure_stage = None

        try:
            replacement_feed = engine._wait_load_loopback(
                dest_source,
                proxy_sink,
                channels=1,
                channel_map="mono",
                source_dont_move=True,
                sink_dont_move=True,
            )
            if replacement_feed is None or not engine._module_is_alive(replacement_feed):
                failure_stage = "proxy_feed"
                raise RuntimeError("raw source could not be rebound to the stable FX source")

            if keep_proxy:
                engine.channel_fx[node_name] = {
                    "mode": "proxy_passthrough",
                    "effects": [],
                    "params": {},
                    "procs": [],
                    "loopbacks": [replacement_feed],
                    "source": proxy_source,
                    "active_chain_source": "",
                    "active_chain_sink": "",
                    "capture_target": capture_target,
                    "safe_key": info.get("safe_key") or engine._safe_channel_key(node_name),
                    "prev_default": replacement_default,
                    "proxy_sink_name": info.get("proxy_sink_name"),
                    "proxy_sink_module_id": info.get("proxy_sink_module_id"),
                    "proxy_source_name": proxy_source,
                    "proxy_source_request_name": info.get("proxy_source_request_name"),
                    "proxy_source_module_id": info.get("proxy_source_module_id"),
                }
                old_loopbacks = [
                    mod_id for mod_id in info.get("loopbacks", [])
                    if str(mod_id) != str(replacement_feed)
                ]
                engine._teardown_fx_plumbing({
                    "loopbacks": old_loopbacks,
                    "procs": info.get("procs", []),
                })
                engine.invalidate_snapshot()
                return fx_result(
                    True,
                    active_source=proxy_source,
                    kept_source=proxy_source,
                    message="FX chain bypassed through stable proxy",
                )

            for key, binding in binding_snapshot.items():
                module_id = engine._create_submix_replacement(
                    dest_source,
                    binding["mix_name"],
                    initial_state=binding.get("state") or {},
                )
                if module_id is None:
                    failure_stage = f"submix_{binding['mix_name'].lower()}"
                    raise RuntimeError(
                        f"replacement submix loopback failed for {binding['mix_name']}"
                    )
                replacements[key] = {
                    "mix_name": binding["mix_name"],
                    "module_id": module_id,
                    "old_module_id": binding.get("module_id"),
                    "state": dict(binding.get("state", {}) or {}),
                }

            if mic_cutover:
                if not engine._move_known_source_outputs(
                        external_source_outputs,
                        proxy_source,
                        dest_source):
                    failure_stage = "source_output_move"
                    raise RuntimeError("source-output move off stable FX source failed")
                if engine._source_names_match(default_before, proxy_source) and replacement_default:
                    if not engine.set_default_source(replacement_default):
                        failure_stage = "default_source"
                        raise RuntimeError("default source could not restore correctly")
                    default_changed = True
                    if not engine._source_names_match(
                            engine.get_default_source(),
                            replacement_default):
                        failure_stage = "default_source"
                        raise RuntimeError("default source did not restore correctly")

            engine.channel_fx.pop(node_name, None)
            if replacements:
                engine._commit_submix_replacements(replacements, new_source=dest_source)
            else:
                for skey in list(engine.submix_sources.keys()):
                    if engine.submix_sources.get(skey) != proxy_source:
                        continue
                    mod_id = engine.submix_loopbacks.pop(skey, None)
                    engine.submix_sources.pop(skey, None)
                    if mod_id is not None:
                        engine._run(["pactl", "unload-module", str(mod_id)])
            engine._teardown_fx_plumbing(info)
            if replacement_feed is not None:
                engine._run(["pactl", "unload-module", str(replacement_feed)])
            engine._destroy_fx_proxy(info)
            engine.invalidate_snapshot()
            return fx_result(
                True,
                kept_source=dest_source,
                message="FX chain cleared",
            )
        except Exception as exc:
            if default_changed and default_before:
                engine.set_default_source(default_before)
            engine._move_known_source_outputs(
                external_source_outputs,
                dest_source,
                proxy_source,
            )
            engine._unload_submix_replacements(replacements)
            if replacement_feed is not None:
                engine._run(["pactl", "unload-module", str(replacement_feed)])
            engine.invalidate_snapshot()
            return fx_result(
                False,
                active_source=proxy_source,
                kept_source=proxy_source,
                rolled_back=True,
                failure_stage=failure_stage or "cutover",
                message=str(exc),
            )

    fx_source = info.get("source")
    capture_target = info.get("capture_target") or ""
    dest_source = target_source or capture_target
    if not fx_source:
        engine.channel_fx.pop(node_name, None)
        return fx_result(True, kept_source=dest_source, message="FX chain state was incomplete")

    binding_snapshot = engine._snapshot_submix_bindings(fx_source)
    exclude_modules = list(info.get("loopbacks", []))
    for binding in binding_snapshot.values():
        old_module_id = binding.get("module_id")
        if old_module_id is not None:
            exclude_modules.append(old_module_id)
    external_source_outputs = engine.snapshot_external_source_outputs(
        fx_source,
        exclude_modules=exclude_modules,
    )

    mic_cutover = bool(capture_target and not capture_target.endswith(".monitor"))
    default_before = engine.get_default_source() if mic_cutover else None
    replacement_default = target_source or info.get("prev_default") or capture_target
    replacements = {}
    default_changed = False
    failure_stage = None

    try:
        if dest_source:
            for key, binding in binding_snapshot.items():
                module_id = engine._create_submix_replacement(
                    dest_source,
                    binding["mix_name"],
                    initial_state=binding.get("state") or {},
                )
                if module_id is None:
                    failure_stage = f"submix_{binding['mix_name'].lower()}"
                    raise RuntimeError(
                        f"replacement submix loopback failed for {binding['mix_name']}"
                    )
                replacements[key] = {
                    "mix_name": binding["mix_name"],
                    "module_id": module_id,
                    "old_module_id": binding.get("module_id"),
                    "state": dict(binding.get("state", {}) or {}),
                }

        if mic_cutover and dest_source:
            if not engine._move_known_source_outputs(
                    external_source_outputs,
                    fx_source,
                    dest_source):
                failure_stage = "source_output_move"
                raise RuntimeError("source-output move off FX source failed")
            if engine._source_names_match(default_before, fx_source) and replacement_default:
                if not engine.set_default_source(replacement_default):
                    failure_stage = "default_source"
                    raise RuntimeError("default source could not restore correctly")
                default_changed = True
                if not engine._source_names_match(
                        engine.get_default_source(),
                        replacement_default):
                    failure_stage = "default_source"
                    raise RuntimeError("default source did not restore correctly")

        engine.channel_fx.pop(node_name, None)
        if replacements:
            engine._commit_submix_replacements(replacements, new_source=dest_source)
        else:
            for skey in list(engine.submix_sources.keys()):
                if engine.submix_sources.get(skey) != fx_source:
                    continue
                mod_id = engine.submix_loopbacks.pop(skey, None)
                engine.submix_sources.pop(skey, None)
                if mod_id is not None:
                    engine._run(["pactl", "unload-module", str(mod_id)])
        engine._teardown_fx_plumbing(info)
        engine.invalidate_snapshot()
        return fx_result(
            True,
            kept_source=dest_source,
            message="FX chain cleared",
        )
    except Exception as exc:
        if default_changed and default_before:
            engine.set_default_source(default_before)
        if dest_source:
            engine._move_known_source_outputs(
                external_source_outputs,
                dest_source,
                fx_source,
            )
        engine._unload_submix_replacements(replacements)
        engine.invalidate_snapshot()
        return fx_result(
            False,
            active_source=fx_source,
            kept_source=fx_source,
            rolled_back=True,
            failure_stage=failure_stage or "cutover",
            message=str(exc),
        )


def get_channel_fx_source(engine, node_name, snap=None):
    """Return the effective source carrying a channel's FX output, or None."""
    info = engine.channel_fx.get(node_name)
    if not info:
        return None
    if engine._is_inline_fx_info(info):
        target = engine._find_inline_fx_target(node_name, snap=snap)
        if target is None:
            engine.channel_fx.pop(node_name, None)
            return None
        info["node_id"] = target["node_id"]
        info["source"] = target["source_name"]
        info["capture_target"] = target["capture_target"]
        return info.get("source")
    if not _fx_info_is_live(engine, info, snap=snap):
        engine.clear_channel_fx(node_name)
        return None
    return info.get("source")
