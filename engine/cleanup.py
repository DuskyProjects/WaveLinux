"""Runtime cleanup and reset helpers."""

from __future__ import annotations

import logging


def restore_physical_defaults_before_reset(engine, snap=None):
    fallback_sink = engine._preferred_hardware_sink_fallback(snap=snap)
    moved = engine._move_app_streams_off_managed_sinks(fallback_sink, snap=snap)
    if moved:
        logging.info(
            "Moved %d app streams off managed WaveLinux sinks before reset",
            len(moved),
        )

    current_default_sink = str(engine.get_default_sink() or "").strip()
    if current_default_sink and engine._is_internal_node_name(current_default_sink) and fallback_sink:
        engine.set_default_sink(fallback_sink)

    current_default_source = str(engine.get_default_source() or "").strip()
    fallback_source = engine._preferred_hardware_source_fallback(snap=snap)
    if current_default_source and engine._is_internal_node_name(current_default_source) and fallback_source:
        engine.set_default_source(fallback_source)


def full_audio_reset(engine):
    """Emergency cleanup of all WaveLinux modules and FX chains."""
    logging.info("Performing full audio reset...")
    try:
        engine.unlock_bluetooth_autoswitch()
    except AttributeError:
        pass
    snap = None
    try:
        snap = engine.create_snapshot(force=True)
    except Exception:
        snap = None
    try:
        restore_physical_defaults_before_reset(engine, snap=snap)
    except Exception as exc:
        logging.warning(f"Pre-reset endpoint restore failed: {exc}")
    for node_name in list(getattr(engine, "channel_fx", {}).keys()):
        try:
            engine.clear_channel_fx(node_name)
        except Exception as exc:
            logging.warning(f"FX teardown failed during reset for {node_name}: {exc}")
    for key in list(getattr(engine, "rnnoise_processes", {}).keys()):
        try:
            engine.stop_rnnoise(key)
        except Exception as exc:
            logging.warning(f"FX process teardown failed during reset for {key}: {exc}")

    out = engine._run(["pactl", "list", "short", "modules"], timeout=5)
    if out:
        lines = out.splitlines()
        for line in reversed(lines):
            if "wavelinux" in line and "module-loopback" in line:
                mod_id = line.split()[0]
                logging.info(f"Unloading loopback: {mod_id}")
                engine._run(["pactl", "unload-module", mod_id], timeout=3)
        for line in reversed(lines):
            if "wavelinux" in line and "module-virtual-source" in line:
                mod_id = line.split()[0]
                logging.info(f"Unloading source: {mod_id}")
                engine._run(["pactl", "unload-module", mod_id], timeout=3)
        for line in reversed(lines):
            if "wavelinux" in line and "module-null-sink" in line:
                mod_id = line.split()[0]
                logging.info(f"Unloading sink: {mod_id}")
                engine._run(["pactl", "unload-module", mod_id], timeout=3)

    full_modules = engine._run(["pactl", "list", "modules"], timeout=5)
    if full_modules:
        curr_id = None
        to_unload = []
        for raw_line in full_modules.splitlines():
            line = raw_line.strip()
            if line.startswith("Module #"):
                curr_id = line.split("#", 1)[1].strip()
                continue
            if curr_id and ("wavelinux" in line or "WaveLinux" in line):
                if curr_id not in to_unload:
                    to_unload.append(curr_id)
        for mod_id in reversed(to_unload):
            logging.info(f"Hard-sweep unloading module: {mod_id}")
            engine._run(["pactl", "unload-module", mod_id], timeout=3)

    engine.loopback_modules.clear()
    engine.submix_loopbacks.clear()
    engine.submix_sources.clear()
    engine.virtual_sink_modules.clear()
    engine.output_mixes.clear()
    engine.channel_fx.clear()
    engine.submix_state_cache.clear()
    engine._pending_submix_state_reapply.clear()
    try:
        engine._reap_orphan_fx_processes()
    except Exception as exc:
        logging.warning(f"Orphan FX process reap failed during reset: {exc}")
    engine.invalidate_snapshot()


def cleanup(engine):
    """Hard cleanup of all WaveLinux PipeWire modules."""
    try:
        engine.unlock_bluetooth_autoswitch()
    except AttributeError:
        pass
    for node_name in list(engine.channel_fx.keys()):
        engine.clear_channel_fx(node_name)
    for key in list(engine.rnnoise_processes.keys()):
        engine.stop_rnnoise(key)

    engine.virtual_sink_modules.clear()
    engine.output_mixes.clear()
    engine.loopback_modules.clear()
    engine.submix_loopbacks.clear()
    engine.submix_sources.clear()
    engine.submix_state_cache.clear()
    engine._pending_submix_state_reapply.clear()

    out = engine._run(["pactl", "list", "modules"])
    if out:
        curr_id = None
        to_unload = []
        for line in out.splitlines():
            line = line.strip()
            if line.startswith("Module #"):
                curr_id = line.split("#")[1].strip()
            if ("wavelinux" in line or "WaveLinux" in line) and curr_id:
                if curr_id not in to_unload:
                    to_unload.append(curr_id)

        for mid in to_unload:
            engine._run(["pactl", "unload-module", mid])
