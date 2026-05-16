"""PipeWire audio routing and effects engine for WaveLinux."""

import subprocess
import json
import os
import shlex
import signal
import re
import socket
import time
import logging

import engine.app_identity as app_identity_engine
import engine.effects_pipeline as effects_pipeline_engine
import engine.effects_runtime as effects_runtime_engine
from engine.cleanup import (
    cleanup as cleanup_engine,
    full_audio_reset as full_audio_reset_engine,
    restore_physical_defaults_before_reset as restore_physical_defaults_before_reset_engine,
)
from engine.app_routing import (
    get_sink_inputs as get_sink_inputs_engine,
    parse_pactl_si_map as parse_pactl_si_map_engine,
)
from engine.cards import (
    list_cards as list_cards_engine,
    lock_bluetooth_to_a2dp as lock_bluetooth_to_a2dp_engine,
    set_card_profile as set_card_profile_engine,
    unlock_bluetooth_autoswitch as unlock_bluetooth_autoswitch_engine,
)
from engine.defaults import (
    get_default_sink as get_default_sink_engine,
    get_default_source as get_default_source_engine,
    set_default_sink as set_default_sink_engine,
    set_default_source as set_default_source_engine,
    source_name_aliases,
)
from engine.devices import (
    branding_label as branding_label_engine,
    friendly_name as friendly_name_engine,
    pretty_bt as pretty_bt_engine,
    sanitize_channel_name as sanitize_channel_name_engine,
)
from engine.fx_graph import (
    apply_channel_fx_transaction as apply_channel_fx_transaction_engine,
    clear_channel_fx_transaction as clear_channel_fx_transaction_engine,
    get_channel_fx_source as get_channel_fx_source_engine,
    reprime_channel_fx_capture as reprime_channel_fx_capture_engine,
    teardown_fx_plumbing as teardown_fx_plumbing_engine,
    unload_submix_replacements as unload_submix_replacements_engine,
)
from engine.ladspa import (
    EFFECT_REQUIREMENTS as LADSPA_EFFECT_REQUIREMENTS,
    bundled_ladspa_entries as bundled_ladspa_entries_engine,
    effect_available as effect_available_engine,
    env_flag_enabled as env_flag_enabled_engine,
    ladspa_env_entries as ladspa_env_entries_engine,
    ladspa_plugin_available as ladspa_plugin_available_engine,
    ladspa_plugin_path as ladspa_plugin_path_engine,
    ladspa_roots as ladspa_roots_engine,
    pipewire_spawn_env as pipewire_spawn_env_engine,
    probe_ladspa_plugins as probe_ladspa_plugins_engine,
)
from engine.levels import (
    MAX_VOLUME as LEVELS_MAX_VOLUME,
    clamp as clamp_engine,
    get_sink_input_volume as get_sink_input_volume_engine,
    get_sink_volume_by_name as get_sink_volume_by_name_engine,
    get_source_volume_by_name as get_source_volume_by_name_engine,
    get_volume as get_volume_engine,
    set_input_gain as set_input_gain_engine,
    set_mute as set_mute_engine,
    set_sink_input_volume as set_sink_input_volume_engine,
    set_sink_mute_by_name as set_sink_mute_by_name_engine,
    set_sink_volume_by_name as set_sink_volume_by_name_engine,
    set_source_volume_by_name as set_source_volume_by_name_engine,
    set_volume as set_volume_engine,
    snapshot_sink_inputs_by_owner as snapshot_sink_inputs_by_owner_engine,
    toggle_mute as toggle_mute_engine,
)
from engine.mixes import (
    create_output_mix as create_output_mix_engine,
    create_virtual_sink as create_virtual_sink_engine,
    get_live_mix_hardware_route as get_live_mix_hardware_route_engine,
    move_app_streams_off_managed_sinks as move_app_streams_off_managed_sinks_engine,
    preferred_hardware_sink_fallback as preferred_hardware_sink_fallback_engine,
    remove_output_mix as remove_output_mix_engine,
    remove_virtual_sink as remove_virtual_sink_engine,
    route_mix_to_hardware as route_mix_to_hardware_engine,
    unroute_mix_from_hardware as unroute_mix_from_hardware_engine,
)
from engine.models import AudioNode, EngineSnapshot, OutputMix
from engine.snapshots import (
    create_snapshot as create_snapshot_engine,
    display_name_for_sink as display_name_for_sink_engine,
    display_name_for_source as display_name_for_source_engine,
    get_all_nodes as get_all_nodes_engine,
    get_all_sinks as get_all_sinks_engine,
    get_app_streams as get_app_streams_engine,
    get_hardware_inputs as get_hardware_inputs_engine,
    get_hardware_outputs as get_hardware_outputs_engine,
    get_sink_description as get_sink_description_engine,
    get_virtual_sinks as get_virtual_sinks_engine,
    invalidate_snapshot as invalidate_snapshot_engine,
    is_internal_node_name as is_internal_node_name_engine,
    looks_like_stable_device_id as looks_like_stable_device_id_engine,
    node_by_name as node_by_name_engine,
    normalize_stable_component as normalize_stable_component_engine,
    normalized_bt_family as normalized_bt_family_engine,
    parse_nodes as parse_nodes_engine,
    parse_short_sinks as parse_short_sinks_engine,
    parse_sink_descriptions as parse_sink_descriptions_engine,
    parse_sinks_state as parse_sinks_state_engine,
    parse_sources_state as parse_sources_state_engine,
    resolve_hardware_sink_name as resolve_hardware_sink_name_engine,
    resolve_hardware_source_name as resolve_hardware_source_name_engine,
    stable_device_id_from_props as stable_device_id_from_props_engine,
    stable_sink_id as stable_sink_id_engine,
    stable_sink_inventory as stable_sink_inventory_engine,
    stable_source_id as stable_source_id_engine,
    stable_source_inventory as stable_source_inventory_engine,
)
from engine.source_routing import (
    list_source_outputs_on as list_source_outputs_on_engine,
    move_known_source_outputs as move_known_source_outputs_engine,
    move_source_output_with_retry as move_source_output_with_retry_engine,
    move_source_outputs as move_source_outputs_engine,
    resolve_source_name as resolve_source_name_engine,
    snapshot_external_source_outputs as snapshot_external_source_outputs_engine,
    snapshot_submix_bindings as snapshot_submix_bindings_engine,
    source_id_to_name as source_id_to_name_engine,
    source_output_locations as source_output_locations_engine,
    wait_source_visible as wait_source_visible_engine,
)
from engine.submix import (
    apply_loopback_state as apply_loopback_state_engine,
    build_loopback_index as build_loopback_index_engine,
    commit_submix_replacements as commit_submix_replacements_engine,
    create_submix_replacement as create_submix_replacement_engine,
    find_loopback_for as find_loopback_for_engine,
    get_submix_sink_input as get_submix_sink_input_engine,
    load_loopback_module as load_loopback_module_engine,
    reapply_submix_state_cache as reapply_submix_state_cache_engine,
    remove_node_routing as remove_node_routing_engine,
    route_input_to_submix as route_input_to_submix_engine,
    set_submix_mute as set_submix_mute_engine,
    set_submix_volume as set_submix_volume_engine,
    sink_input_for_module as sink_input_for_module_engine,
    wait_load_loopback as wait_load_loopback_engine,
    wait_sink_input_for_module as wait_sink_input_for_module_engine,
)
from engine.runtime_helpers import (
    find_module_by_arg as find_module_by_arg_engine,
    module_is_alive as module_is_alive_engine,
    preferred_hardware_source_fallback as preferred_hardware_source_fallback_engine,
    rename_virtual_sink as rename_virtual_sink_engine,
    sink_visible as sink_visible_engine,
)

_LOG_PATH = os.path.expanduser("~/.config/wavelinux/wavelinux.log")
os.makedirs(os.path.dirname(_LOG_PATH), exist_ok=True)
logging.basicConfig(
    filename=_LOG_PATH,
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)

class PipeWireEngine:
    """Full-featured PipeWire audio engine."""

    _LADSPA_PATHS = (
        "/usr/lib/ladspa",
        "/usr/lib64/ladspa",
        "/usr/local/lib/ladspa",
        "/usr/local/lib64/ladspa",
        "/usr/lib/x86_64-linux-gnu/ladspa",
        "/usr/lib/aarch64-linux-gnu/ladspa",
        os.path.expanduser("~/.ladspa"),
        os.path.expanduser("~/.local/lib/ladspa"),
    )
    _BT_SINK_FAMILY_RE = re.compile(
        r'^(bluez_output\.[0-9A-Fa-f]{2}(?:[_:-][0-9A-Fa-f]{2}){5})(?:\..+)?$',
        re.IGNORECASE,
    )
    _BT_SOURCE_FAMILY_RE = re.compile(
        r'^(bluez_input\.[0-9A-Fa-f]{2}(?:[_:-][0-9A-Fa-f]{2}){5})(?:\..+)?$',
        re.IGNORECASE,
    )

    def __init__(self):
        self.virtual_sink_modules = {}   # safe_name -> pactl module id
        self.output_mixes = {}           # mix_name -> OutputMix
        self.rnnoise_processes = {}      # channel_key -> subprocess
        self.loopback_modules = {}       # "mix_name->hw_name" -> module id
        self.submix_loopbacks = {}       # "node_id->mix_name" -> module id
        self.submix_sources = {}         # "node_id->mix_name" -> source_token
        self.channel_fx = {}             # node_name -> {effects, params, procs,
                                         #               source, capture_target,
                                         #               safe_key}
        self.submix_state_cache = {}    # "node_id->mix_name" -> {'vol', 'mute'}
        self._pending_submix_state_reapply = set()
        self._app_identity_overrides = {}
        self._app_identity_label_overrides = {}
        self.ladspa_plugins = self._probe_ladspa_plugins()
        self._reap_orphan_fx_processes()
        self._bt_autoswitch_overridden = False
        self.cleanup()
        self.lock_bluetooth_to_a2dp()

    @staticmethod
    def _reap_orphan_fx_processes():
        """Kill leftover `pipewire -c` filter-chain processes from a previous
        WaveLinux session. Pattern is anchored to `*.config/pipewire/wavelinux-`
        so we don't hit unrelated user clients."""
        try:
            subprocess.run(
                ['pkill', '-f', r'pipewire -c [^ ]*\.config/pipewire/wavelinux-'],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                timeout=2,
            )
        except Exception:
            pass

    def lock_bluetooth_to_a2dp(self):
        return lock_bluetooth_to_a2dp_engine(self)

    def unlock_bluetooth_autoswitch(self):
        return unlock_bluetooth_autoswitch_engine(self)

    @staticmethod
    def _env_flag_enabled(name, *, environ=None):
        return env_flag_enabled_engine(name, environ=environ)

    @classmethod
    def _bundled_ladspa_entries(cls, *, environ=None):
        _ = cls
        return bundled_ladspa_entries_engine(environ=environ)

    @classmethod
    def _ladspa_env_entries(cls, *, environ=None):
        _ = cls
        return ladspa_env_entries_engine(environ=environ)

    @classmethod
    def _ladspa_roots(cls, *, environ=None):
        return ladspa_roots_engine(cls._LADSPA_PATHS, environ=environ)

    @classmethod
    def _pipewire_spawn_env(cls, *, environ=None):
        _ = cls
        return pipewire_spawn_env_engine(environ=environ)

    @classmethod
    def _probe_ladspa_plugins(cls):
        return probe_ladspa_plugins_engine(cls._ladspa_roots())

    def ladspa_plugin_available(self, name):
        return ladspa_plugin_available_engine(name, self.ladspa_plugins)

    def ladspa_plugin_path(self, name):
        return ladspa_plugin_path_engine(name, self._ladspa_roots())

    _EFFECT_REQUIREMENTS = LADSPA_EFFECT_REQUIREMENTS

    def effect_available(self, effect_id):
        return effect_available_engine(
            effect_id,
            self.ladspa_plugins,
            self._EFFECT_REQUIREMENTS,
        )

    # ── Helpers ─────────────────────────────────────────────────────

    def _run(self, cmd, timeout=2):
        # Drop None entries and stringify everything — joining `None` into
        # an error log raises TypeError.
        cmd = [str(c) for c in cmd if c is not None]
        if not cmd:
            return None
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
            if r.returncode == 0:
                return r.stdout.strip()
            logging.error(f"Command failed: {' '.join(cmd)} - {r.stderr}")
        except subprocess.TimeoutExpired:
            logging.warning(f"Command timed out: {' '.join(cmd)}")
        except Exception as e:
            logging.error(f"Execution error: {' '.join(cmd)} - {e}")
        return None

    def get_default_sink(self):
        """Find the system's default audio output sink name."""
        return get_default_sink_engine(self)

    def get_default_source(self):
        """Find the system's default audio input source (mic) name. Used
        on first launch to pre-populate the master "Microphone Input"
        combo with the same mic the rest of the user's apps default to."""
        return get_default_source_engine(self)

    def set_default_sink(self, sink_name):
        """Set the system default playback sink. Returns True on success."""
        return set_default_sink_engine(self, sink_name)

    def set_default_source(self, source_name):
        """Set the system default capture source. Apps that follow the
        default mic (Discord, Zoom, browsers via getUserMedia) start
        recording from `source_name` after this call without changing
        their own settings. Returns True on success."""
        return set_default_source_engine(self, source_name)

    @staticmethod
    def _source_name_aliases(source_name):
        return source_name_aliases(source_name)

    def _source_names_match(self, left, right, snap=None):
        left_aliases = set(self._source_name_aliases(left))
        right_aliases = set(self._source_name_aliases(right))
        if left_aliases & right_aliases:
            return True
        resolved_left = self.resolve_source_name(left, snap=snap)
        resolved_right = self.resolve_source_name(right, snap=snap)
        if resolved_left:
            left_aliases.update(self._source_name_aliases(resolved_left))
        if resolved_right:
            right_aliases.update(self._source_name_aliases(resolved_right))
        return bool(left_aliases & right_aliases)

    def resolve_source_name(self, source_name, snap=None):
        return resolve_source_name_engine(self, source_name, snap=snap)

    def _source_id_to_name(self):
        return source_id_to_name_engine(self)

    def _list_source_outputs_on(self, source_name, exclude_modules=None):
        return list_source_outputs_on_engine(
            self,
            source_name,
            exclude_modules=exclude_modules,
        )

    def _move_source_outputs(self, from_source, to_source, exclude_modules=None):
        return move_source_outputs_engine(
            self,
            from_source,
            to_source,
            exclude_modules=exclude_modules,
        )

    def _source_output_locations(self):
        return source_output_locations_engine(self)

    def snapshot_external_source_outputs(self, source_name, exclude_modules=None):
        return snapshot_external_source_outputs_engine(
            self,
            source_name,
            exclude_modules=exclude_modules,
        )

    def _move_known_source_outputs(self, source_output_ids, from_source, to_source,
                                   attempts=20, delay=0.05):
        return move_known_source_outputs_engine(
            self,
            source_output_ids,
            from_source,
            to_source,
            attempts=attempts,
            delay=delay,
        )

    def _wait_source_visible(self, source_name, attempts=20, delay=0.05):
        return wait_source_visible_engine(
            self,
            source_name,
            attempts=attempts,
            delay=delay,
        )

    def _move_source_output_with_retry(self, source_output_id, from_source, to_source,
                                       attempts=20, delay=0.05):
        return move_source_output_with_retry_engine(
            self,
            source_output_id,
            from_source,
            to_source,
            attempts=attempts,
            delay=delay,
        )

    def _snapshot_submix_bindings(self, source_name):
        return snapshot_submix_bindings_engine(self, source_name)

    def _load_loopback_module(self, source_name, sink_name, latency_msec=20,
                              channels=None, channel_map=None,
                              source_dont_move=False, sink_dont_move=False):
        return load_loopback_module_engine(
            self,
            source_name,
            sink_name,
            latency_msec=latency_msec,
            channels=channels,
            channel_map=channel_map,
            source_dont_move=source_dont_move,
            sink_dont_move=sink_dont_move,
        )

    def _create_submix_replacement(self, source_name, mix_name, initial_state=None):
        return create_submix_replacement_engine(
            self,
            source_name,
            mix_name,
            initial_state=initial_state,
        )

    def _commit_submix_replacements(self, replacements, *, new_source):
        return commit_submix_replacements_engine(
            self,
            replacements,
            new_source=new_source,
        )

    # ── Per-refresh snapshot ───────────────────────────────────────

    # Minimum age (seconds) below which a fresh snapshot request just hands
    # back the cached one. Prevents pactl-subscribe storms (every internal
    # action we take generates a subscribe event we then react to) from
    # triggering 5-10 full snapshots per second.
    _SNAPSHOT_TTL = 0.25

    def reap_dead_processes(self):
        """Drop entries from `rnnoise_processes` whose subprocess has exited.
        Without this, every spawn-and-die FX stage stays parked in the dict
        forever, and `is_channel_fx_running` walks a growing list of dead
        Popen handles every time it's called. Cheap to run periodically."""
        for key in list(self.rnnoise_processes.keys()):
            proc = self.rnnoise_processes.get(key)
            if proc is None or proc.poll() is not None:
                self.rnnoise_processes.pop(key, None)

    def create_snapshot(self, force=False):
        return create_snapshot_engine(self, force=force)

    def invalidate_snapshot(self):
        return invalidate_snapshot_engine(self)

    @staticmethod
    def _parse_sink_descriptions(text):
        return parse_sink_descriptions_engine(text)

    @staticmethod
    def _parse_sinks_state(text):
        return parse_sinks_state_engine(text)

    @staticmethod
    def _parse_sources_state(text):
        return parse_sources_state_engine(text)

    def _parse_nodes(self):
        return parse_nodes_engine(self)

    def _parse_short_sinks(self):
        return parse_short_sinks_engine(self)

    @staticmethod
    def _pretty_bt(raw):
        return pretty_bt_engine(raw)

    @staticmethod
    def friendly_name(raw):
        return friendly_name_engine(raw)

    # ── Node Discovery ──────────────────────────────────────────────

    def get_all_nodes(self, snap=None):
        return get_all_nodes_engine(self, snap=snap)

    @staticmethod
    def _is_internal_node_name(name):
        return is_internal_node_name_engine(name)

    def get_hardware_outputs(self, snap=None):
        return get_hardware_outputs_engine(self, snap=snap)

    @classmethod
    def _normalize_stable_component(cls, value):
        _ = cls
        return normalize_stable_component_engine(value)

    @classmethod
    def _looks_like_stable_device_id(cls, value):
        _ = cls
        return looks_like_stable_device_id_engine(value)

    @classmethod
    def _normalized_bt_family(cls, name, *, source=False):
        return normalized_bt_family_engine(cls, name, source=source)

    @classmethod
    def _stable_device_id_from_props(cls, prefix, name, props=None, *, source=False):
        return stable_device_id_from_props_engine(
            cls,
            prefix,
            name,
            props=props,
            source=source,
        )

    @classmethod
    def stable_sink_id(cls, sink_name):
        return stable_sink_id_engine(cls, sink_name)

    def _node_by_name(self, node_name, snap=None):
        return node_by_name_engine(self, node_name, snap=snap)

    def stable_source_id(self, source_name_or_node, snap=None):
        return stable_source_id_engine(self, source_name_or_node, snap=snap)

    def stable_sink_inventory(self, snap=None):
        return stable_sink_inventory_engine(self, snap=snap)

    def stable_source_inventory(self, snap=None):
        return stable_source_inventory_engine(self, snap=snap)

    def resolve_hardware_sink_name(self, sink_name, snap=None):
        return resolve_hardware_sink_name_engine(self, sink_name, snap=snap)

    def resolve_hardware_source_name(self, source_or_stable_id, snap=None):
        return resolve_hardware_source_name_engine(self, source_or_stable_id, snap=snap)

    def get_hardware_inputs(self, snap=None):
        return get_hardware_inputs_engine(self, snap=snap)

    def display_name_for_source(self, source_name_or_node, snap=None):
        return display_name_for_source_engine(self, source_name_or_node, snap=snap)

    def get_virtual_sinks(self, snap=None):
        return get_virtual_sinks_engine(self, snap=snap)

    def get_app_streams(self, snap=None):
        return get_app_streams_engine(self, snap=snap)

    # ── Volume & Mute ──────────────────────────────────────────────

    def get_volume(self, node_id):
        return get_volume_engine(self, node_id)

    def set_volume(self, node_id, volume):
        set_volume_engine(self, node_id, volume)

    def set_mute(self, node_id, mute):
        set_mute_engine(self, node_id, mute)

    def toggle_mute(self, node_id):
        toggle_mute_engine(self, node_id)

    def set_sink_volume_by_name(self, sink_name, volume):
        set_sink_volume_by_name_engine(self, sink_name, volume)

    def set_source_volume_by_name(self, source_name, volume):
        set_source_volume_by_name_engine(self, source_name, volume)

    def get_source_volume_by_name(self, source_name, snap=None):
        return get_source_volume_by_name_engine(self, source_name, snap=snap)

    def get_sink_volume_by_name(self, sink_name, snap=None):
        return get_sink_volume_by_name_engine(self, sink_name, snap=snap)

    def set_sink_mute_by_name(self, sink_name, mute):
        set_sink_mute_by_name_engine(self, sink_name, mute)

    # ── Virtual Sink (Input Channel) Management ────────────────────

    def route_input_to_submix(self, node_id, node_name, media_class, mix_name,
                              snap=None, initial_state=None):
        return route_input_to_submix_engine(
            self,
            node_id,
            node_name,
            media_class,
            mix_name,
            snap=snap,
            initial_state=initial_state,
        )

    def _apply_loopback_state(self, module_id, state):
        return apply_loopback_state_engine(self, module_id, state)

    def _build_loopback_index(self, modules_text):
        return build_loopback_index_engine(modules_text)

    def _find_loopback_for(self, source_token, sink_token, snap=None):
        return find_loopback_for_engine(self, source_token, sink_token, snap=snap)

    def remove_node_routing(self, node_id):
        return remove_node_routing_engine(self, node_id)

    def get_submix_sink_input(self, node_id, mix_name, snap=None):
        return get_submix_sink_input_engine(self, node_id, mix_name, snap=snap)

    def set_submix_volume(self, node_id, mix_name, volume):
        return set_submix_volume_engine(self, node_id, mix_name, volume)

    def set_submix_mute(self, node_id, mix_name, mute):
        return set_submix_mute_engine(self, node_id, mix_name, mute)

    def _reapply_submix_state_cache(self):
        return reapply_submix_state_cache_engine(self)

    def snapshot_sink_inputs_by_owner(self, snap=None):
        return snapshot_sink_inputs_by_owner_engine(self, snap=snap)

    # ── Output Mix Management ──────────────────────────────────────

    @staticmethod
    def _sanitize_channel_name(display_name):
        return sanitize_channel_name_engine(display_name)

    @staticmethod
    def _branding_label(display_clean):
        return branding_label_engine(display_clean)

    def create_virtual_sink(self, display_name, custom_name=None):
        return create_virtual_sink_engine(
            self,
            display_name,
            custom_name=custom_name,
        )

    def remove_virtual_sink(self, sink_name):
        return remove_virtual_sink_engine(self, sink_name)

    def create_output_mix(self, name):
        return create_output_mix_engine(self, name)

    def remove_output_mix(self, mix_name):
        return remove_output_mix_engine(self, mix_name)

    def route_mix_to_hardware(self, mix_name, hw_sink_name):
        return route_mix_to_hardware_engine(self, mix_name, hw_sink_name)

    def _sink_visible(self, sink_name):
        return sink_visible_engine(self, sink_name)

    def _sink_input_for_module(self, module_id):
        return sink_input_for_module_engine(self, module_id)

    def _wait_sink_input_for_module(self, module_id, attempts=20, delay=0.05):
        return wait_sink_input_for_module_engine(
            self,
            module_id,
            attempts=attempts,
            delay=delay,
        )

    def get_live_mix_hardware_route(self, mix_name, snap=None):
        return get_live_mix_hardware_route_engine(self, mix_name, snap=snap)

    def _preferred_hardware_sink_fallback(self, snap=None):
        return preferred_hardware_sink_fallback_engine(self, snap=snap)

    def _preferred_hardware_source_fallback(self, snap=None):
        return preferred_hardware_source_fallback_engine(self, snap=snap)

    def _move_app_streams_off_managed_sinks(self, fallback_sink, snap=None):
        return move_app_streams_off_managed_sinks_engine(
            self,
            fallback_sink,
            snap=snap,
        )

    def _restore_physical_defaults_before_reset(self, snap=None):
        return restore_physical_defaults_before_reset_engine(self, snap=snap)

    def full_audio_reset(self):
        return full_audio_reset_engine(self)

    # ── App Routing ────────────────────────────────────────────────

    def _parse_pactl_si_map(self, text):
        return parse_pactl_si_map_engine(text)

    def get_sink_inputs(self, snap=None):
        return get_sink_inputs_engine(self, snap=snap)

    _GENERIC_APP_NAMES = app_identity_engine.GENERIC_APP_NAMES
    _KNOWN_APP_IDS = app_identity_engine.KNOWN_APP_IDS
    _EXEC_WRAPPERS = app_identity_engine.EXEC_WRAPPERS
    _PATH_TITLE_PATTERNS = app_identity_engine.PATH_TITLE_PATTERNS
    _BINARY_DISPLAY_NAMES = app_identity_engine.BINARY_DISPLAY_NAMES
    _BINARY_ICON_NAMES = app_identity_engine.BINARY_ICON_NAMES
    _MULTIPROCESS_CHILD_BINARIES = app_identity_engine.MULTIPROCESS_CHILD_BINARIES
    _WINDOW_IDENTITY_KEYS = app_identity_engine.WINDOW_IDENTITY_KEYS
    _TEXT_IDENTITY_KEYS = app_identity_engine.TEXT_IDENTITY_KEYS
    _WINDOW_TITLE_KEYS = app_identity_engine.WINDOW_TITLE_KEYS
    SYSTEM_SOUNDS_BUCKET = app_identity_engine.SYSTEM_SOUNDS_BUCKET

    @staticmethod
    def _normalize_app_name(s):
        return app_identity_engine.normalize_app_name(s)

    def _is_generic_name(self, s):
        return app_identity_engine.is_generic_name(self, s)

    @classmethod
    def _canonicalize_app_id(cls, app_id):
        return app_identity_engine.canonicalize_app_id(app_id)

    @staticmethod
    def _normalize_for_host_match(value):
        return app_identity_engine.normalize_for_host_match(value)

    @classmethod
    def _host_aliases(cls):
        return app_identity_engine.host_aliases(cls)

    @classmethod
    def name_matches_host(cls, value):
        return app_identity_engine.name_matches_host(cls, value)

    @staticmethod
    def _read_proc_cmdline(pid):
        return app_identity_engine.read_proc_cmdline(pid)

    @staticmethod
    def _read_proc_env(pid):
        return app_identity_engine.read_proc_env(pid)

    @staticmethod
    def _read_proc_cgroup(pid):
        return app_identity_engine.read_proc_cgroup(pid)

    def _identify_sandboxed_app(self, pid):
        return app_identity_engine.identify_sandboxed_app(self, pid)

    @classmethod
    def _desktop_app_index(cls):
        return app_identity_engine.desktop_app_index(cls)

    @staticmethod
    def _parse_desktop_file(path):
        return app_identity_engine.parse_desktop_file(path)

    @classmethod
    def _resolve_exec_binary(cls, exec_line):
        return app_identity_engine.resolve_exec_binary(cls, exec_line)

    def _infer_name_from_exe(self, pid, current_name=None):
        return app_identity_engine.infer_name_from_exe(self, pid, current_name=current_name)

    def _identify_via_desktop(self, pid):
        return app_identity_engine.identify_via_desktop(self, pid)

    @classmethod
    def _normalize_app_route_token(cls, value):
        return app_identity_engine.normalize_app_route_token(value)

    @classmethod
    def _append_icon_candidate(cls, out, seen, value):
        return app_identity_engine.append_icon_candidate(cls, out, seen, value)

    @classmethod
    def theme_icon_candidates_for_app_id(cls, app_id, fallback_name=None):
        return app_identity_engine.theme_icon_candidates_for_app_id(
            cls,
            app_id,
            fallback_name=fallback_name,
        )

    def _app_icon_candidates(self, current, *, app_id="", resolved_app_id="", app_name="", resolved_app_name=""):
        return app_identity_engine.app_icon_candidates(
            self,
            current,
            app_id=app_id,
            resolved_app_id=resolved_app_id,
            app_name=app_name,
            resolved_app_name=resolved_app_name,
        )

    @classmethod
    def _make_app_route_key(cls, prefix, value):
        return app_identity_engine.make_app_route_key(cls, prefix, value)

    @classmethod
    def _sanitize_app_label(cls, value):
        return app_identity_engine.sanitize_app_label(cls, value)

    @classmethod
    def display_name_for_app_id(cls, app_id):
        return app_identity_engine.display_name_for_app_id(cls, app_id)

    @classmethod
    def is_legacy_stream_label(cls, value):
        return app_identity_engine.is_legacy_stream_label(cls, value)

    @classmethod
    def is_persistent_app_id(cls, app_id):
        return app_identity_engine.is_persistent_app_id(cls, app_id)

    @classmethod
    def _normalize_identity_override_map(cls, raw):
        return app_identity_engine.normalize_identity_override_map(cls, raw)

    @classmethod
    def _normalize_label_override_map(cls, raw):
        return app_identity_engine.normalize_label_override_map(cls, raw)

    def set_app_identity_overrides(self, overrides, labels):
        return app_identity_engine.set_app_identity_overrides(self, overrides, labels)

    def _override_display_name_for_app_id(self, app_id, fallback=None):
        return app_identity_engine.override_display_name_for_app_id(self, app_id, fallback=fallback)

    @staticmethod
    def _proc_exe_basename(pid):
        return app_identity_engine.proc_exe_basename(pid)

    @staticmethod
    def _proc_comm(pid):
        return app_identity_engine.proc_comm(pid)

    @staticmethod
    def _parent_pid(pid):
        return app_identity_engine.parent_pid(pid)

    def _pid_lineage(self, pid, limit=10):
        return app_identity_engine.pid_lineage(self, pid, limit=limit)

    @staticmethod
    def _split_identity_tokens(raw):
        return app_identity_engine.split_identity_tokens(raw)

    @classmethod
    def _identity_candidate(cls, app_id, display_name, score, source):
        return app_identity_engine.identity_candidate(app_id, display_name, score, source)

    @classmethod
    def _candidate_from_raw(cls, prefix, raw_value, display_name, score, source):
        return app_identity_engine.candidate_from_raw(
            cls,
            prefix,
            raw_value,
            display_name,
            score,
            source,
        )

    def _stream_identity_candidate(self, current, display_name, score, source):
        return app_identity_engine.stream_identity_candidate(self, current, display_name, score, source)

    def _window_title_identity_label(self, raw):
        return app_identity_engine.window_title_identity_label(self, raw)

    def _generic_title_context(self, current):
        return app_identity_engine.generic_title_context(self, current)

    def _app_name_from_pid(self, pid):
        return app_identity_engine.app_name_from_pid(self, pid)

    @staticmethod
    def _is_system_sound_stream(current):
        return app_identity_engine.is_system_sound_stream(current)

    def _resolve_via_gio_env(self, pid):
        return app_identity_engine.resolve_via_gio_env(self, pid)

    def _gio_identity_candidate(self, pid):
        return app_identity_engine.gio_identity_candidate(self, pid)

    def _sandbox_identity_candidate(self, pid):
        return app_identity_engine.sandbox_identity_candidate(self, pid)

    def _window_identity_candidates(self, current):
        return app_identity_engine.window_identity_candidates(self, current)

    def _binary_identity_candidates(self, pid, current):
        return app_identity_engine.binary_identity_candidates(self, pid, current)

    def _cmdline_identity_candidates(self, pid):
        return app_identity_engine.cmdline_identity_candidates(self, pid)

    def _path_identity_candidate(self, pid):
        return app_identity_engine.path_identity_candidate(self, pid)

    def _text_identity_candidates(self, current):
        return app_identity_engine.text_identity_candidates(self, current)

    def _stream_fallback_identity(self, current):
        return app_identity_engine.stream_fallback_identity(self, current)

    def _prefer_specific_identity_candidate(self, candidates, best):
        return app_identity_engine.prefer_specific_identity_candidate(self, candidates, best)

    @staticmethod
    def _candidate_source_preference(candidate):
        return app_identity_engine.candidate_source_preference(candidate)

    def _prefer_wrapper_identity_candidate(self, candidates, best):
        return app_identity_engine.prefer_wrapper_identity_candidate(self, candidates, best)

    @staticmethod
    def _is_lineage_fallback_identity_source(source):
        return app_identity_engine.is_lineage_fallback_identity_source(source)

    def _prefer_explicit_stream_identity_candidate(self, current, candidates, best):
        return app_identity_engine.prefer_explicit_stream_identity_candidate(self, current, candidates, best)

    def _apply_identity_override(self, identity):
        return app_identity_engine.apply_identity_override(self, identity)

    def _resolve_app_identity(self, current):
        return app_identity_engine.resolve_app_identity(self, current)

    def _process_sink_input(self, current, entries, sink_id_to_name):
        return app_identity_engine.process_sink_input(self, current, entries, sink_id_to_name)

    # All volume writes clamp to this — PipeWire allows 1.5 (150%) but
    # that audibly clips, so we cap at unity everywhere.
    MAX_VOLUME = LEVELS_MAX_VOLUME

    def _clamp(self, volume):
        return clamp_engine(self, volume)

    def move_app_to_sink(self, sink_input_index, sink_name):
        """Move a sink-input to `sink_name`. None means System Default."""
        if sink_name is None:
            sink_name = self.get_default_sink()
        if not sink_name:
            return
        self._run(['pactl', 'move-sink-input', str(sink_input_index), sink_name])

    def set_sink_input_volume(self, sink_input_index, volume):
        set_sink_input_volume_engine(self, sink_input_index, volume)

    def get_sink_input_volume(self, sink_input_index):
        return get_sink_input_volume_engine(self, sink_input_index)

    def get_all_sinks(self, snap=None):
        return get_all_sinks_engine(self, snap=snap)

    def get_sink_description(self, sink_name, snap=None):
        return get_sink_description_engine(self, sink_name, snap=snap)

    def display_name_for_sink(self, sink_name, snap=None):
        return display_name_for_sink_engine(self, sink_name, snap=snap)

    # ── Wave Link-parity helpers ───────────────────────────────────

    def set_input_gain(self, node_id, volume):
        set_input_gain_engine(self, node_id, volume)

    def unroute_mix_from_hardware(self, mix_name):
        return unroute_mix_from_hardware_engine(self, mix_name)

    # ── Card / profile switching ───────────────────────────────────

    def list_cards(self):
        return list_cards_engine(self)

    def set_card_profile(self, card_name, profile_name):
        return set_card_profile_engine(self, card_name, profile_name)

    # ── Rename ─────────────────────────────────────────────────────

    def rename_virtual_sink(self, old_sink_name, new_display_name):
        return rename_virtual_sink_engine(self, old_sink_name, new_display_name)

    # ── Effects / RNNoise ──────────────────────────────────────────
    _FX_PREAMBLE = effects_runtime_engine.FX_PREAMBLE
    _AVAILABLE_EFFECTS = effects_runtime_engine.AVAILABLE_EFFECTS
    _EFFECT_PARAMS = effects_runtime_engine.EFFECT_PARAMS
    _EFFECT_HELP = effects_runtime_engine.EFFECT_HELP
    _EFFECT_PRESETS = effects_runtime_engine.EFFECT_PRESETS
    _CHAIN_ORDER = effects_runtime_engine.CHAIN_ORDER

    @staticmethod
    def _fx_client_config(client_id, filter_chain_args):
        return effects_runtime_engine.fx_client_config(client_id, filter_chain_args)

    @staticmethod
    def _fx_log_path(channel_key, effect_id):
        return effects_runtime_engine.fx_log_path(channel_key, effect_id)

    def _spawn_fx(self, config_path, log_path, key):
        return effects_runtime_engine.spawn_fx(self, config_path, log_path, key)

    def start_rnnoise(self, channel_key='default', params=None):
        return effects_runtime_engine.start_rnnoise(self, channel_key=channel_key, params=params)

    def stop_rnnoise(self, channel_key='default'):
        return effects_runtime_engine.stop_rnnoise(self, channel_key=channel_key)

    def is_rnnoise_active(self, channel_key='default'):
        return effects_runtime_engine.is_rnnoise_active(self, channel_key=channel_key)

    @property
    def rnnoise_active(self):
        return effects_runtime_engine.rnnoise_active(self)

    @classmethod
    def get_available_effects(cls):
        return effects_runtime_engine.get_available_effects(cls)

    @classmethod
    def get_effect_params(cls, effect_id):
        return effects_runtime_engine.get_effect_params(cls, effect_id)

    @classmethod
    def get_effect_help(cls, effect_id):
        return effects_runtime_engine.get_effect_help(cls, effect_id)

    @classmethod
    def get_effect_presets(cls, effect_id):
        return effects_runtime_engine.get_effect_presets(cls, effect_id)

    def _resolved_params(self, effect_id, overrides):
        return effects_runtime_engine.resolved_params(self, effect_id, overrides)

    @staticmethod
    def _render_control_block(params):
        return effects_runtime_engine.render_control_block(params)

    def _ladspa_node(self, name, plugin, label, values):
        return effects_runtime_engine.ladspa_node(self, name, plugin, label, values)

    def _build_filter_graph(self, effect_id, values):
        return effects_runtime_engine.build_filter_graph(self, effect_id, values)

    def apply_effect(self, channel_key, effect_id, params=None):
        return effects_runtime_engine.apply_effect(self, channel_key, effect_id, params=params)

    def remove_effect(self, channel_key, effect_id):
        return effects_runtime_engine.remove_effect(self, channel_key, effect_id)

    def is_effect_active(self, channel_key, effect_id):
        return effects_runtime_engine.is_effect_active(self, channel_key, effect_id)

    @classmethod
    def _ordered_chain(cls, effects):
        return effects_runtime_engine.ordered_chain(cls, effects)

    @staticmethod
    def _safe_channel_key(node_name):
        return effects_runtime_engine.safe_channel_key(node_name)

    def _effect_stage_blocks(self, effect_id, values, stage_idx):
        return effects_runtime_engine.effect_stage_blocks(self, effect_id, values, stage_idx)

    def _build_unified_filter_graph(self, ordered_effects, params_map):
        return effects_runtime_engine.build_unified_filter_graph(self, ordered_effects, params_map)

    def _build_unified_chain_config(self, safe_key, ordered_effects, params_map, stamp=None):
        return effects_runtime_engine.build_unified_chain_config(
            self,
            safe_key,
            ordered_effects,
            params_map,
            stamp=stamp,
        )

    @staticmethod
    def _is_inline_fx_info(info):
        return effects_runtime_engine.is_inline_fx_info(info)

    def _find_inline_fx_target(self, node_name, snap=None):
        return effects_runtime_engine.find_inline_fx_target(self, node_name, snap=snap)

    def _set_node_filter_graph(self, node_id, graph_text):
        return effects_runtime_engine.set_node_filter_graph(self, node_id, graph_text)

    def _apply_inline_channel_fx(self, node_name, capture_target, ordered, params_map):
        return effects_runtime_engine.apply_inline_channel_fx(
            self,
            node_name,
            capture_target,
            ordered,
            params_map,
        )

    def _clear_inline_channel_fx(self, node_name, info):
        return effects_runtime_engine.clear_inline_channel_fx(self, node_name, info)

    @staticmethod
    def _fx_proxy_names(safe_key):
        return effects_runtime_engine.fx_proxy_names(safe_key)

    def _ensure_fx_proxy(self, safe_key):
        return effects_runtime_engine.ensure_fx_proxy(self, safe_key)

    def _destroy_fx_proxy(self, info):
        return effects_runtime_engine.destroy_fx_proxy(self, info)

    def _build_fx_stage_config(self, safe_key, idx, effect_id, params):
        return effects_runtime_engine.build_fx_stage_config(self, safe_key, idx, effect_id, params)

    def _wait_load_loopback(self, source, sink, latency_msec=20, attempts=20,
                            delay=0.1, channels=None, channel_map=None,
                            source_dont_move=False, sink_dont_move=False):
        return wait_load_loopback_engine(
            self,
            source,
            sink,
            latency_msec=latency_msec,
            attempts=attempts,
            delay=delay,
            channels=channels,
            channel_map=channel_map,
            source_dont_move=source_dont_move,
            sink_dont_move=sink_dont_move,
        )

    def set_channel_fx(self, node_name, capture_target, effects, params_map=None):
        return effects_runtime_engine.set_channel_fx(
            self,
            node_name,
            capture_target,
            effects,
            params_map=params_map,
        )

    def _set_channel_fx_inner(self, node_name, capture_target, effects, params_map):
        return effects_runtime_engine.set_channel_fx_inner(
            self,
            node_name,
            capture_target,
            effects,
            params_map,
        )

    @staticmethod
    def _fx_result(success, *, active_source=None, kept_source=None,
                   rolled_back=False, failure_stage=None, message=""):
        return {
            'success': bool(success),
            'active_source': active_source,
            'kept_source': kept_source,
            'rolled_back': bool(rolled_back),
            'failure_stage': failure_stage,
            'message': message or "",
        }

    def _teardown_fx_plumbing(self, info):
        teardown_fx_plumbing_engine(self, info)

    def _unload_submix_replacements(self, replacements):
        unload_submix_replacements_engine(self, replacements)

    def apply_channel_fx_transaction(self, node_name, capture_target, effects, params_map=None):
        return apply_channel_fx_transaction_engine(
            self,
            node_name,
            capture_target,
            effects,
            params_map=params_map,
        )

    def clear_channel_fx(self, node_name):
        return effects_runtime_engine.clear_channel_fx(self, node_name)

    def _clear_channel_fx_inner(self, node_name):
        return effects_runtime_engine.clear_channel_fx_inner(self, node_name)

    def _clear_channel_fx_info(self, info, target_source=None):
        return effects_runtime_engine.clear_channel_fx_info(
            self,
            info,
            target_source=target_source,
        )

    def clear_channel_fx_transaction(self, node_name, target_source=None, keep_proxy=False):
        return clear_channel_fx_transaction_engine(
            self,
            node_name,
            target_source=target_source,
            keep_proxy=keep_proxy,
        )

    def reprime_channel_fx_capture(self, node_name, *, settle_s=1.0):
        return reprime_channel_fx_capture_engine(
            self,
            node_name,
            settle_s=settle_s,
        )

    def get_channel_fx_source(self, node_name, snap=None):
        return get_channel_fx_source_engine(self, node_name, snap=snap)

    def describe_channel_fx_runtime(self, node_name, snap=None, *, info=None, fx_status=None):
        return effects_pipeline_engine.describe_channel_fx_runtime(
            self,
            node_name,
            snap=snap,
            info=info,
            fx_status=fx_status,
        )

    def verify_channel_fx_runtime(
        self,
        node_name,
        *,
        expected_default=False,
        snap=None,
        info=None,
        fx_status=None,
        requested_effects=None,
    ):
        return effects_pipeline_engine.verify_channel_fx_runtime(
            self,
            node_name,
            expected_default=expected_default,
            snap=snap,
            info=info,
            fx_status=fx_status,
            requested_effects=requested_effects,
        )

    def list_channel_fx_artifacts(self, node_name, snap=None, *, info=None, fx_status=None):
        return effects_pipeline_engine.list_channel_fx_artifacts(
            self,
            node_name,
            snap=snap,
            info=info,
            fx_status=fx_status,
        )

    def is_channel_fx_running(self, node_name):
        return effects_runtime_engine.is_channel_fx_running(self, node_name)

    def get_channel_effects(self, node_name):
        return effects_runtime_engine.get_channel_effects(self, node_name)

    def _find_module_by_arg(self, pattern, modules_text=None):
        return find_module_by_arg_engine(self, pattern, modules_text=modules_text)

    def _module_is_alive(self, module_id, short_text=None):
        return module_is_alive_engine(self, module_id, short_text=short_text)

    # ── Cleanup ────────────────────────────────────────────────────

    def cleanup(self):
        return cleanup_engine(self)
