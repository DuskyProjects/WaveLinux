"""Effects runtime and filter-chain helpers for the PipeWire engine."""

from __future__ import annotations

import engine.effects_catalog as effects_catalog
import json
import logging
import os
import re
import signal
import subprocess


FX_PREAMBLE = effects_catalog.FX_PREAMBLE
AVAILABLE_EFFECTS = effects_catalog.AVAILABLE_EFFECTS
EFFECT_PARAMS = effects_catalog.EFFECT_PARAMS
EFFECT_HELP = effects_catalog.EFFECT_HELP
EFFECT_PRESETS = effects_catalog.EFFECT_PRESETS
CHAIN_ORDER = effects_catalog.CHAIN_ORDER


def fx_client_config(client_id, filter_chain_args):
    return effects_catalog.fx_client_config(client_id, filter_chain_args)


def fx_log_path(channel_key, effect_id):
    log_dir = os.path.expanduser("~/.config/wavelinux/fx-logs")
    os.makedirs(log_dir, exist_ok=True)
    return os.path.join(log_dir, f"{effect_id}-{channel_key}.log")


def spawn_fx(engine, config_path, log_path, key):
    try:
        with open(config_path, "r") as handle:
            rendered_config = handle.read()
    except OSError:
        rendered_config = "<read failed>"
    try:
        pw_ver = subprocess.run(
            ["pipewire", "--version"],
            capture_output=True,
            text=True,
            timeout=2,
        ).stdout.strip() or "unknown"
    except Exception:
        pw_ver = "unknown"
    spawn_env = engine._pipewire_spawn_env()
    header = (
        f"# WaveLinux FX spawn {key}\n"
        f"# pipewire --version: {pw_ver}\n"
        f"# config path:        {config_path}\n"
        f"# raw LADSPA_PATH:    {os.environ.get('LADSPA_PATH', '')}\n"
        f"# effective LADSPA_PATH: {spawn_env.get('LADSPA_PATH', '')}\n"
        f"# ──────── config ─────────\n"
        f"{rendered_config}"
        f"\n# ──────── pipewire stderr/stdout ────────\n"
    )

    try:
        log_file = open(log_path, "wb")
    except OSError as exc:
        logging.error("Could not open FX log file %s: %s", log_path, exc)
        return False

    proc = None
    try:
        log_file.write(header.encode("utf-8"))
        log_file.flush()
        proc = subprocess.Popen(
            ["pipewire", "-c", config_path],
            stdout=log_file,
            stderr=log_file,
            env=spawn_env,
        )
    except FileNotFoundError:
        logging.error("`pipewire` binary not found — cannot spawn filter chain")
        return False
    finally:
        try:
            log_file.close()
        except Exception:
            pass

    try:
        proc.wait(timeout=1.5)
    except subprocess.TimeoutExpired:
        engine.rnnoise_processes[key] = proc
        return True
    logging.error("FX process for %s exited immediately; see %s", key, log_path)
    return False


def start_rnnoise(engine, channel_key="default", params=None):
    if engine.rnnoise_processes.get(channel_key) is not None:
        engine.stop_rnnoise(channel_key)
    config_dir = os.path.expanduser("~/.config/pipewire")
    os.makedirs(config_dir, exist_ok=True)
    config_path = os.path.join(config_dir, f"wavelinux-rnnoise-{channel_key}.conf")
    values = engine._resolved_params("rnnoise", params)
    filter_graph = engine._build_filter_graph("rnnoise", values)
    if filter_graph is None:
        return False
    client_id = re.sub(r"[^A-Za-z0-9]+", "-", f"rnnoise-{channel_key}").strip("-") or "rnnoise"
    filter_chain_args = f"""{{
            node.description = "WaveLinux-Denoise ({channel_key})"
            media.name       = "WaveLinux-Denoise ({channel_key})"
            filter.graph = {{{filter_graph}
            }}
            capture.props = {{
                node.name    = "wavelinux.rnnoise.{channel_key}.capture"
                media.class  = Audio/Sink
                audio.rate   = 48000
                audio.channels = 1
                audio.position = [ MONO ]
            }}
            playback.props = {{
                node.name    = "wavelinux.rnnoise.{channel_key}.source"
                media.class  = Audio/Source
                audio.rate   = 48000
                audio.channels = 1
                audio.position = [ MONO ]
            }}
        }}"""
    config = engine._fx_client_config(client_id, filter_chain_args)
    with open(config_path, "w") as handle:
        handle.write(config)
    return engine._spawn_fx(config_path, engine._fx_log_path(channel_key, "rnnoise"), channel_key)


def stop_rnnoise(engine, channel_key="default"):
    proc = engine.rnnoise_processes.get(channel_key)
    if proc:
        try:
            proc.send_signal(signal.SIGTERM)
            proc.wait(timeout=3)
        except Exception:
            proc.kill()
            try:
                proc.wait(timeout=1)
            except Exception:
                pass
        del engine.rnnoise_processes[channel_key]
        return True
    return False


def is_rnnoise_active(engine, channel_key="default"):
    proc = engine.rnnoise_processes.get(channel_key)
    return proc is not None and proc.poll() is None


def rnnoise_active(engine):
    return any(proc.poll() is None for proc in engine.rnnoise_processes.values())


def get_available_effects(engine_cls):
    return effects_catalog.get_available_effects(engine_cls)


def get_effect_params(engine_cls, effect_id):
    return effects_catalog.get_effect_params(engine_cls, effect_id)


def get_effect_help(engine_cls, effect_id):
    return effects_catalog.get_effect_help(engine_cls, effect_id)


def get_effect_presets(engine_cls, effect_id):
    return effects_catalog.get_effect_presets(engine_cls, effect_id)


def resolved_params(engine, effect_id, overrides):
    return effects_catalog.resolved_params(engine, effect_id, overrides)


def render_control_block(params):
    return effects_catalog.render_control_block(params)


def ladspa_node(engine, name, plugin, label, values):
    return effects_catalog.ladspa_node(engine, name, plugin, label, values)


def build_filter_graph(engine, effect_id, values):
    return effects_catalog.build_filter_graph(engine, effect_id, values)


def apply_effect(engine, channel_key, effect_id, params=None):
    if effect_id == "rnnoise":
        return engine.start_rnnoise(channel_key, params=params)

    config_dir = os.path.expanduser("~/.config/pipewire")
    os.makedirs(config_dir, exist_ok=True)
    values = engine._resolved_params(effect_id, params)
    filter_graph = engine._build_filter_graph(effect_id, values)
    if filter_graph is None:
        return False

    config_path = os.path.join(config_dir, f"wavelinux-fx-{channel_key}-{effect_id}.conf")
    client_id = re.sub(r"[^A-Za-z0-9]+", "-", f"{effect_id}-{channel_key}").strip("-") or effect_id
    filter_chain_args = f"""{{
            node.description = "WaveLinux-{effect_id} ({channel_key})"
            media.name       = "WaveLinux-{effect_id} ({channel_key})"
            filter.graph = {{{filter_graph}
            }}
            capture.props = {{
                node.name    = "wavelinux.fx.{channel_key}.{effect_id}.capture"
                media.class  = Audio/Sink
                audio.rate   = 48000
            }}
            playback.props = {{
                node.name    = "wavelinux.fx.{channel_key}.{effect_id}.source"
                media.class  = Audio/Source
                audio.rate   = 48000
            }}
        }}"""
    config = engine._fx_client_config(client_id, filter_chain_args)
    with open(config_path, "w") as handle:
        handle.write(config)
    key = f"{channel_key}_{effect_id}"
    if engine.rnnoise_processes.get(key) is not None:
        engine.stop_rnnoise(key)
    return engine._spawn_fx(config_path, engine._fx_log_path(channel_key, effect_id), key)


def remove_effect(engine, channel_key, effect_id):
    if effect_id == "rnnoise":
        return engine.stop_rnnoise(channel_key)
    return engine.stop_rnnoise(f"{channel_key}_{effect_id}")


def is_effect_active(engine, channel_key, effect_id):
    if effect_id == "rnnoise":
        return engine.is_rnnoise_active(channel_key)
    proc = engine.rnnoise_processes.get(f"{channel_key}_{effect_id}")
    return proc is not None and proc.poll() is None


def ordered_chain(engine_cls, effects):
    return effects_catalog.ordered_chain(engine_cls, effects)


def safe_channel_key(node_name):
    return effects_catalog.safe_channel_key(node_name)


def effect_stage_blocks(engine, effect_id, values, stage_idx):
    return effects_catalog.effect_stage_blocks(engine, effect_id, values, stage_idx)


def build_unified_filter_graph(engine, ordered_effects, params_map):
    return effects_catalog.build_unified_filter_graph(engine, ordered_effects, params_map)


def build_unified_chain_config(engine, safe_key, ordered_effects, params_map, stamp=None):
    return effects_catalog.build_unified_chain_config(
        engine,
        safe_key,
        ordered_effects,
        params_map,
        stamp=stamp,
    )


def is_inline_fx_info(info):
    return bool(info and info.get("mode") == "inline")


def find_inline_fx_target(engine, node_name, snap=None):
    snap = snap or engine.create_snapshot(force=True)
    for node in engine.get_hardware_inputs(snap=snap):
        if node.name == node_name:
            return {
                "node_id": str(node.pw_id),
                "node_name": node.name,
                "media_class": node.media_class,
                "capture_target": node.name,
                "source_name": node.name,
            }
    try:
        virtual_nodes = engine.get_virtual_sinks(snap=snap)
    except AttributeError:
        virtual_nodes = []
    for node in virtual_nodes:
        if node.name == node_name:
            return {
                "node_id": str(node.pw_id),
                "node_name": node.name,
                "media_class": node.media_class,
                "capture_target": f"{node.name}.monitor",
                "source_name": f"{node.name}.monitor",
            }
    return None


def set_node_filter_graph(engine, node_id, graph_text):
    node_id = str(node_id or "").strip()
    if not node_id:
        return False
    param_text = (
        '{ params = [ '
        '"audioconvert.filter-graph.disable" false '
        '"audioconvert.filter-graph" '
        f'{json.dumps(graph_text or "")}'
        ' ] }'
    )
    out = engine._run(["pw-cli", "s", node_id, "Props", param_text], timeout=3)
    return out is not None


def apply_inline_channel_fx(engine, node_name, capture_target, ordered, params_map):
    old_info = engine.channel_fx.get(node_name) or {}
    if old_info and not engine._is_inline_fx_info(old_info):
        return None
    target = engine._find_inline_fx_target(node_name)
    if not target:
        return None
    graph_text, used_effects = engine._build_unified_filter_graph(ordered, params_map)
    if graph_text is None or not used_effects:
        return engine._fx_result(
            False,
            kept_source=target["source_name"],
            failure_stage="config_build",
            message="No renderable effects were available for this chain",
        )
    if not engine._set_node_filter_graph(target["node_id"], graph_text):
        return engine._fx_result(
            False,
            kept_source=target["source_name"],
            failure_stage="inline_set_param",
            message="PipeWire rejected the live filter-graph update",
        )
    engine.channel_fx[node_name] = {
        "mode": "inline",
        "effects": list(used_effects),
        "params": {effect_id: dict(params_map.get(effect_id, {})) for effect_id in used_effects},
        "procs": [],
        "loopbacks": [],
        "source": target["source_name"],
        "capture_target": capture_target or target["capture_target"],
        "safe_key": engine._safe_channel_key(node_name),
        "node_id": target["node_id"],
    }
    engine.invalidate_snapshot()
    return engine._fx_result(
        True,
        active_source=target["source_name"],
        kept_source=target["source_name"],
        message="FX chain active",
    )


def clear_inline_channel_fx(engine, node_name, info):
    target = engine._find_inline_fx_target(node_name)
    node_id = (target or {}).get("node_id") or info.get("node_id")
    source_name = (target or {}).get("source_name") or info.get("source") or info.get("capture_target")
    if not engine._set_node_filter_graph(node_id, ""):
        return engine._fx_result(
            False,
            kept_source=source_name,
            active_source=source_name,
            rolled_back=True,
            failure_stage="inline_clear",
            message="PipeWire rejected the live filter-graph clear",
        )
    engine.channel_fx.pop(node_name, None)
    engine.invalidate_snapshot()
    return engine._fx_result(True, kept_source=source_name, message="FX chain cleared")


def fx_proxy_names(safe_key):
    return (
        f"wavelinux.fx.{safe_key}.sink",
        f"wavelinux.fx.{safe_key}.source",
    )


def ensure_fx_proxy(engine, safe_key):
    sink_name, requested_source_name = engine._fx_proxy_names(safe_key)
    sink_module_id = engine._find_module_by_arg(f"sink_name={sink_name}")
    if sink_module_id is None:
        sink_module_id = engine._run(
            [
                "pactl",
                "load-module",
                "module-null-sink",
                f"sink_name={sink_name}",
                "channels=1",
                "channel_map=mono",
                (
                    "sink_properties="
                    "device.description=_WaveLinux-FX-Sink "
                    "node.description=_WaveLinux-FX-Sink "
                    "node.nick=_WaveLinux-FX-Sink "
                    "media.name=_WaveLinux-FX-Sink "
                    "application.name=_WaveLinux-FX-Sink "
                    "media.class=Audio/Sink"
                ),
            ]
        )
        if sink_module_id:
            engine._run(["pactl", "set-sink-mute", sink_name, "0"])
            engine._run(["pactl", "set-sink-volume", sink_name, "100%"])
            engine.invalidate_snapshot()
    if sink_module_id is None:
        return None

    source_module_id = engine._find_module_by_arg(f"source_name={requested_source_name}")
    if source_module_id is None:
        source_module_id = engine._run(
            [
                "pactl",
                "load-module",
                "module-virtual-source",
                f"source_name={requested_source_name}",
                f"master={sink_name}.monitor",
                "channels=1",
                "channel_map=mono",
                (
                    "source_properties="
                    "device.description=_WaveLinux-FX-Source "
                    "node.description=_WaveLinux-FX-Source "
                    "node.nick=_WaveLinux-FX-Source "
                    "media.name=_WaveLinux-FX-Source "
                    "application.name=_WaveLinux-FX-Source "
                    "media.class=Audio/Source "
                    "device.class=sound"
                ),
            ]
        )
        if source_module_id:
            engine.invalidate_snapshot()
    if source_module_id is None:
        if sink_module_id is not None:
            engine._run(["pactl", "unload-module", str(sink_module_id)])
        return None

    source_name = engine._wait_source_visible(requested_source_name, attempts=20, delay=0.05) or engine.resolve_source_name(
        requested_source_name
    )
    if not source_name:
        if source_module_id is not None:
            engine._run(["pactl", "unload-module", str(source_module_id)])
        if sink_module_id is not None:
            engine._run(["pactl", "unload-module", str(sink_module_id)])
        engine.invalidate_snapshot()
        return None
    return {
        "sink_name": sink_name,
        "sink_module_id": str(sink_module_id),
        "source_name": source_name,
        "source_request_name": requested_source_name,
        "source_module_id": str(source_module_id),
    }


def destroy_fx_proxy(engine, info):
    source_module_id = info.get("proxy_source_module_id")
    sink_module_id = info.get("proxy_sink_module_id")
    source_name = info.get("proxy_source_name")
    source_request_name = info.get("proxy_source_request_name")
    sink_name = info.get("proxy_sink_name")

    if source_module_id is None:
        for candidate in (
            source_request_name,
            source_name,
            str(source_name or "").removeprefix("output."),
        ):
            if not candidate:
                continue
            source_module_id = engine._find_module_by_arg(f"source_name={candidate}")
            if source_module_id is not None:
                break
    if sink_module_id is None and sink_name:
        sink_module_id = engine._find_module_by_arg(f"sink_name={sink_name}")

    if source_module_id is not None:
        engine._run(["pactl", "unload-module", str(source_module_id)])
    if sink_module_id is not None:
        engine._run(["pactl", "unload-module", str(sink_module_id)])
    if source_module_id is not None or sink_module_id is not None:
        engine.invalidate_snapshot()


def build_fx_stage_config(engine, safe_key, idx, effect_id, params):
    return effects_catalog.build_fx_stage_config(engine, safe_key, idx, effect_id, params)


def set_channel_fx(engine, node_name, capture_target, effects, params_map=None):
    return engine._set_channel_fx_inner(node_name, capture_target, effects, params_map)


def set_channel_fx_inner(engine, node_name, capture_target, effects, params_map):
    result = engine.apply_channel_fx_transaction(
        node_name,
        capture_target,
        effects,
        params_map=params_map,
    )
    if not result.get("success"):
        return None
    return result.get("active_source")


def clear_channel_fx(engine, node_name):
    return engine._clear_channel_fx_inner(node_name)


def clear_channel_fx_inner(engine, node_name):
    result = engine.clear_channel_fx_transaction(node_name)
    return bool(result.get("success"))


def clear_channel_fx_info(engine, info, target_source=None):
    if not info:
        return False
    for node_name, current in list(engine.channel_fx.items()):
        if current is info:
            result = engine.clear_channel_fx_transaction(
                node_name,
                target_source=target_source,
            )
            return bool(result.get("success"))
    fx_source = info.get("source")
    for submix_key in list(engine.submix_sources.keys()):
        if engine.submix_sources.get(submix_key) != fx_source:
            continue
        mod_id = engine.submix_loopbacks.pop(submix_key, None)
        engine.submix_sources.pop(submix_key, None)
        if mod_id is not None:
            engine._run(["pactl", "unload-module", str(mod_id)])
    engine._teardown_fx_plumbing(info)
    engine.invalidate_snapshot()
    return True


def is_channel_fx_running(engine, node_name):
    return engine.get_channel_fx_source(node_name) is not None


def get_channel_effects(engine, node_name):
    info = engine.channel_fx.get(node_name)
    return list(info.get("effects", [])) if info else []
