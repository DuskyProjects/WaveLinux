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

_LOG_PATH = os.path.expanduser("~/.config/wavelinux/wavelinux.log")
os.makedirs(os.path.dirname(_LOG_PATH), exist_ok=True)
logging.basicConfig(
    filename=_LOG_PATH,
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)


class AudioNode:
    """Represents a PipeWire audio node."""
    def __init__(self, pw_id, name, description, media_class, app_name=None, props=None):
        self.pw_id = pw_id
        self.name = name
        self.description = description
        self.media_class = media_class
        self.app_name = app_name
        self.props = props or {}
        self.volume = 1.0
        self.muted = False


class OutputMix:
    """Output mix model (Monitor, Stream, etc.)."""
    def __init__(self, name, sink_module_id=None, sink_name=None):
        self.name = name
        self.sink_name = sink_name  # PipeWire sink name
        self.sink_module_id = sink_module_id
        self.source_name = None
        self.source_module_id = None
        self.channel_volumes = {}  # channel_key -> float (0.0 - 1.5)
        self.channel_mutes = {}    # channel_key -> bool
        self.hardware_output = None  # which hardware output to route to
        self.master_volume = 1.0
        self.master_muted = False


class EngineSnapshot:
    """Per-refresh cache for expensive pactl/pw-dump reads."""

    __slots__ = ("modules_text", "short_modules_text", "sink_inputs_text",
                 "sinks_text", "sources_text", "nodes", "sinks", "_loopback_index",
                 "_sink_state_by_name", "_sink_descriptions", "_source_state_by_name")

    def __init__(self, modules_text="", short_modules_text="",
                 sink_inputs_text="", sinks_text="", sources_text="",
                 nodes=None, sinks=None):
        self.modules_text = modules_text or ""
        self.short_modules_text = short_modules_text or ""
        self.sink_inputs_text = sink_inputs_text or ""
        self.sinks_text = sinks_text or ""
        self.sources_text = sources_text or ""
        self.nodes = nodes or []
        self.sinks = sinks or []
        self._loopback_index = None
        self._sink_state_by_name = None
        self._sink_descriptions = None
        self._source_state_by_name = None


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
        """Disable WirePlumber's A2DP↔HSP autoswitch for this session.
        Volatile — restored when wireplumber restarts or `unlock_bluetooth_autoswitch`
        is called. Returns False if wpctl is too old (< 0.5) or the call failed;
        non-fatal in that case."""
        try:
            res = subprocess.run(
                ['wpctl', 'settings', 'bluetooth.autoswitch-to-headset-profile', 'false'],
                capture_output=True, text=True, timeout=3,
            )
        except (FileNotFoundError, subprocess.TimeoutExpired) as e:
            logging.warning(f"Could not lock bluetooth profile (wpctl unavailable): {e}")
            return False
        if res.returncode != 0:
            logging.warning(
                f"wpctl rejected bluetooth.autoswitch override "
                f"(rc={res.returncode}): {res.stderr.strip()}"
            )
            return False
        self._bt_autoswitch_overridden = True
        logging.info("Locked BT profile to A2DP for this session")
        return True

    def unlock_bluetooth_autoswitch(self):
        """Restore the BT autoswitch default. No-op if the lock never landed."""
        if not self._bt_autoswitch_overridden:
            return False
        try:
            res = subprocess.run(
                ['wpctl', 'settings', 'bluetooth.autoswitch-to-headset-profile', 'true'],
                capture_output=True, text=True, timeout=3,
            )
        except (FileNotFoundError, subprocess.TimeoutExpired):
            return False
        if res.returncode == 0:
            self._bt_autoswitch_overridden = False
            return True
        return False

    @staticmethod
    def _env_flag_enabled(name, *, environ=None):
        env = os.environ if environ is None else environ
        return env.get(name, "").strip().lower() in {
            "1",
            "true",
            "yes",
            "on",
        }

    @classmethod
    def _bundled_ladspa_entries(cls, *, environ=None):
        env = os.environ if environ is None else environ
        return [
            p for p in env.get("WAVELINUX_BUNDLED_LADSPA_PATH", "").split(":") if p
        ]

    @classmethod
    def _ladspa_env_entries(cls, *, environ=None):
        """Return sanitized non-bundled LADSPA_PATH entries.

        AppImage runtimes may inject their own `usr/lib/ladspa` into
        `LADSPA_PATH`. That must stay opt-in, otherwise the packaged app
        silently shadows host plugins and FX chains fail to spawn.
        """
        env = os.environ if environ is None else environ
        env_entries = [p for p in env.get("LADSPA_PATH", "").split(":") if p]
        bundled_entries = cls._bundled_ladspa_entries(environ=env)
        bundled_keys = {os.path.normpath(path) for path in bundled_entries}
        env_entries = [
            path for path in env_entries
            if os.path.normpath(path) not in bundled_keys
        ]

        deduped = []
        seen = set()
        for path in env_entries:
            key = os.path.normpath(path)
            if key in seen:
                continue
            deduped.append(path)
            seen.add(key)
        return deduped

    @classmethod
    def _ladspa_roots(cls, *, environ=None):
        """Return LADSPA search roots with host paths first.

        AppImage-bundled LADSPA paths are opt-in because the plugins are
        loaded by the host PipeWire daemon, not by the WaveLinux process.
        """
        roots = []
        roots.extend(cls._ladspa_env_entries(environ=environ))
        roots.extend(cls._LADSPA_PATHS)
        env = os.environ if environ is None else environ
        if cls._env_flag_enabled("WAVELINUX_ENABLE_BUNDLED_LADSPA", environ=env):
            roots.extend(cls._bundled_ladspa_entries(environ=env))
        deduped = []
        seen = set()
        for root in roots:
            if root in seen:
                continue
            deduped.append(root)
            seen.add(root)
        return deduped

    @classmethod
    def _pipewire_spawn_env(cls, *, environ=None):
        env = dict(os.environ if environ is None else environ)
        ladspa_entries = cls._ladspa_env_entries(environ=env)
        if cls._env_flag_enabled("WAVELINUX_ENABLE_BUNDLED_LADSPA", environ=env):
            ladspa_entries.extend(cls._bundled_ladspa_entries(environ=env))
        deduped = []
        seen = set()
        for path in ladspa_entries:
            key = os.path.normpath(path)
            if key in seen:
                continue
            deduped.append(path)
            seen.add(key)
        if ladspa_entries:
            env["LADSPA_PATH"] = ":".join(deduped)
        else:
            env.pop("LADSPA_PATH", None)
        return env

    @classmethod
    def _probe_ladspa_plugins(cls):
        """Return a set of LADSPA plugin names (sans .so) found on disk."""
        roots = cls._ladspa_roots()
        found = set()
        for root in roots:
            try:
                for entry in os.listdir(root):
                    if entry.endswith(".so"):
                        found.add(entry[:-3])
            except OSError:
                continue
        return found

    def ladspa_plugin_available(self, name):
        """Case-insensitive exact or prefix match — distros sometimes drop
        the `_1913` version suffix or use different capitalisation."""
        if name in self.ladspa_plugins:
            return True
        low = name.lower()
        for plugin in self.ladspa_plugins:
            if plugin.lower() == low:
                return True
            if plugin.lower().startswith(low + "_"):
                return True
        return False

    def ladspa_plugin_path(self, name):
        """Find the absolute path to a LADSPA plugin .so. Returns None if
        not found. Filter-chain accepts a bare plugin name but using the
        absolute path eliminates a class of "plugin not found" failures on
        systems where the .so lives in a non-standard directory."""
        roots = self._ladspa_roots()
        target_lower = name.lower()
        for root in roots:
            try:
                entries = os.listdir(root)
            except OSError:
                continue
            for entry in entries:
                if not entry.endswith(".so"):
                    continue
                stem = entry[:-3]
                if (stem == name
                        or stem.lower() == target_lower
                        or stem.lower().startswith(target_lower + "_")):
                    full = os.path.join(root, entry)
                    if os.path.isfile(full):
                        return full
        return None

    _EFFECT_REQUIREMENTS = {
        'rnnoise': ('librnnoise_ladspa',),
        'compressor': ('sc4m_1916',),
        'gate': ('gate_1410',),
        # highpass, eq, and limiter use PipeWire's builtin nodes
        # (biquad / linear / clamp) — always available.
        'highpass': (),
        'eq': (),
        'limiter': (),
    }

    def effect_available(self, effect_id):
        """Return True if the filter-chain backend for this effect has
        everything it needs on disk. Keeps the FX UI from offering things
        that will silently fail at spawn time."""
        needed = self._EFFECT_REQUIREMENTS.get(effect_id, ())
        return all(self.ladspa_plugin_available(n) for n in needed)

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
        return self._run(['pactl', 'get-default-sink'])

    def get_default_source(self):
        """Find the system's default audio input source (mic) name. Used
        on first launch to pre-populate the master "Microphone Input"
        combo with the same mic the rest of the user's apps default to."""
        return self._run(['pactl', 'get-default-source'])

    def set_default_sink(self, sink_name):
        """Set the system default playback sink. Returns True on success."""
        if not sink_name:
            return False
        resolved = self.resolve_hardware_sink_name(sink_name) or sink_name
        return self._run(['pactl', 'set-default-sink', resolved]) is not None

    def set_default_source(self, source_name):
        """Set the system default capture source. Apps that follow the
        default mic (Discord, Zoom, browsers via getUserMedia) start
        recording from `source_name` after this call without changing
        their own settings. Returns True on success."""
        if not source_name:
            return False
        resolved = self.resolve_source_name(source_name) or source_name
        return self._run(['pactl', 'set-default-source', resolved]) is not None

    @staticmethod
    def _source_name_aliases(source_name):
        source_name = str(source_name or "").strip()
        if not source_name:
            return []
        aliases = [source_name]
        if source_name.startswith("output."):
            aliases.append(source_name[len("output."):])
        else:
            aliases.append(f"output.{source_name}")
        return list(dict.fromkeys(alias for alias in aliases if alias))

    def resolve_source_name(self, source_name, snap=None):
        """Resolve a persisted source token to the currently visible source name.

        Pulse virtual sources often surface as `output.<requested_name>`
        even when the module argument was `source_name=<requested_name>`.
        """
        wanted = self._source_name_aliases(source_name)
        if not wanted:
            return None
        visible = set()
        if snap is not None:
            visible.update(
                node.name for node in getattr(snap, "nodes", [])
                if getattr(node, "media_class", "") == "Audio/Source"
            )
        visible.update(self._source_id_to_name().values())
        for candidate in wanted:
            if candidate in visible:
                return candidate
        return None

    def _source_id_to_name(self):
        """Build {source_id: source_name} from `pactl list short sources`."""
        out = self._run(['pactl', 'list', 'short', 'sources'])
        mapping = {}
        if not out:
            return mapping
        for line in out.splitlines():
            parts = line.split('\t')
            if len(parts) >= 2 and parts[0].strip().isdigit():
                mapping[parts[0].strip()] = parts[1].strip()
        return mapping

    def _list_source_outputs_on(self, source_name, exclude_modules=None):
        """Return [source_output_id, ...] for streams currently capturing
        from `source_name`.

        Use the live bound source from `pactl list short source-outputs`
        where column 2 is the current source id on this PipeWire stack.
        Full `pactl list source-outputs` is still consulted for owner
        module ids (for exclusions) and as a fallback `target.object`
        hint when the short source id cannot be resolved back to a name.

        If `exclude_modules` is provided, source-outputs whose owner
        module is in that set are skipped — this lets callers protect
        WaveLinux's own loopbacks from being swept up in a bulk move."""
        short = self._run(['pactl', 'list', 'short', 'source-outputs'])
        full = self._run(['pactl', 'list', 'source-outputs'])
        if not short or not full:
            return []

        id_to_name = self._source_id_to_name()
        current_by_so = {}
        for line in short.splitlines():
            parts = line.split('\t')
            if len(parts) < 2:
                continue
            so_id = parts[0].strip()
            src_id = parts[1].strip()
            current_by_so[so_id] = id_to_name.get(src_id)

        excluded = {str(m) for m in (exclude_modules or ()) if m is not None}
        wanted_names = set(self._source_name_aliases(source_name))
        resolved_name = self.resolve_source_name(source_name)
        if resolved_name:
            wanted_names.add(resolved_name)
        ids = []
        current_so = None
        current_owner = None
        current_target = None

        def flush():
            if not current_so:
                return
            if current_owner in excluded:
                return
            live_source = current_by_so.get(current_so)
            if live_source in wanted_names or current_target in wanted_names:
                ids.append(current_so)

        for line in full.splitlines():
            stripped = line.strip()
            if stripped.startswith('Source Output #'):
                flush()
                current_so = stripped.split('#', 1)[1].strip()
                current_owner = None
                current_target = None
                continue
            if current_so is None:
                continue
            if stripped.startswith('Owner Module:'):
                current_owner = stripped.split(':', 1)[1].strip()
                continue
            if 'target.object =' in stripped:
                current_target = stripped.split('=', 1)[1].strip().strip('"')
                continue

        flush()
        return ids

    def _move_source_outputs(self, from_source, to_source, exclude_modules=None):
        """Move every source-output currently capturing from `from_source`
        onto `to_source`. No-op if either is missing or the lookup
        returns nothing.

        `exclude_modules` is a collection of pactl module ids whose
        source-outputs must NOT be moved. Critically, when a channel's
        FX chain is brought up, the chain's own upstream loopback
        (raw_mic → chain.input) is reading from `from_source` — moving
        it onto `to_source` (= chain.source) closes a feedback loop
        that silences the chain entirely."""
        if not from_source or not to_source or from_source == to_source:
            return
        resolved_to_source = self._wait_source_visible(to_source)
        if not resolved_to_source:
            logging.warning(
                f"Destination source {to_source} never became visible; "
                f"skipping move-source-output from {from_source}"
            )
            return
        for sid in self._list_source_outputs_on(
                from_source, exclude_modules=exclude_modules):
            self._move_source_output_with_retry(sid, from_source, resolved_to_source)

    def _source_output_locations(self):
        """Return {source_output_id: source_name} from short pactl state."""
        short = self._run(['pactl', 'list', 'short', 'source-outputs'])
        if not short:
            return {}
        id_to_name = self._source_id_to_name()
        locations = {}
        for line in short.splitlines():
            parts = line.split('\t')
            if len(parts) < 2:
                continue
            so_id = parts[0].strip()
            src_id = parts[1].strip()
            if not so_id:
                continue
            locations[so_id] = id_to_name.get(src_id)
        return locations

    def snapshot_external_source_outputs(self, source_name, exclude_modules=None):
        """Capture the current external source-outputs on `source_name`."""
        return list(self._list_source_outputs_on(
            source_name,
            exclude_modules=exclude_modules,
        ))

    def _move_known_source_outputs(self, source_output_ids, from_source, to_source,
                                   attempts=20, delay=0.05):
        """Move a known set of source-output ids and verify they rebind."""
        if not source_output_ids:
            return True
        if not from_source or not to_source or from_source == to_source:
            return True
        resolved_to_source = self._wait_source_visible(
            to_source,
            attempts=attempts,
            delay=delay,
        )
        if not resolved_to_source:
            logging.warning(
                f"Destination source {to_source} never became visible; "
                f"skipping targeted move-source-output from {from_source}"
            )
            return False
        wanted = {str(so_id).strip() for so_id in source_output_ids if str(so_id).strip()}
        for so_id in wanted:
            moved = False
            for _ in range(max(1, int(attempts))):
                self._run(['pactl', 'move-source-output', so_id, resolved_to_source])
                current = self._source_output_locations().get(so_id)
                if current is None or current == resolved_to_source:
                    moved = True
                    break
                time.sleep(max(0.0, float(delay)))
            if not moved:
                return False
        return True

    def _wait_source_visible(self, source_name, attempts=20, delay=0.05):
        if not source_name:
            return False
        for _ in range(max(1, int(attempts))):
            resolved = self.resolve_source_name(source_name)
            if resolved:
                return resolved
            time.sleep(max(0.0, float(delay)))
        return False

    def _move_source_output_with_retry(self, source_output_id, from_source, to_source,
                                       attempts=20, delay=0.05):
        source_output_id = str(source_output_id).strip()
        if not source_output_id:
            return False
        for _ in range(max(1, int(attempts))):
            self._run(['pactl', 'move-source-output', source_output_id, to_source])
            if source_output_id not in self._list_source_outputs_on(from_source):
                return True
            time.sleep(max(0.0, float(delay)))
        logging.warning(
            f"Source output {source_output_id} stayed on {from_source} "
            f"after move attempt to {to_source}"
        )
        return False

    def _snapshot_submix_bindings(self, source_name):
        """Capture current submix loopbacks reading from `source_name`."""
        bindings = {}
        for key, current_source in list(self.submix_sources.items()):
            if current_source != source_name:
                continue
            _, _, mix_name = key.partition('->')
            bindings[key] = {
                'mix_name': mix_name,
                'module_id': self.submix_loopbacks.get(key),
                'state': dict(self.submix_state_cache.get(key, {}) or {}),
            }
        return bindings

    def _load_loopback_module(self, source_name, sink_name, latency_msec=20):
        out = self._run([
            'pactl', 'load-module', 'module-loopback',
            f'source={source_name}',
            f'sink={sink_name}',
            f'latency_msec={int(latency_msec)}',
            'adjust_time=0',
        ])
        if not out:
            return None
        stripped = out.strip().splitlines()[-1].strip()
        if not stripped.isdigit():
            return None
        return stripped

    def _create_submix_replacement(self, source_name, mix_name, initial_state=None):
        mix = self.output_mixes.get(mix_name)
        if not mix or not mix.sink_name:
            return None
        module_id = self._find_loopback_for(source_name, mix.sink_name)
        if module_id is None:
            module_id = self._load_loopback_module(source_name, mix.sink_name)
            if module_id is None:
                return None
            self.invalidate_snapshot()
        module_id = str(module_id)
        if not self._module_is_alive(module_id):
            return None
        state = dict(initial_state or {})
        if state:
            key = next(
                (
                    existing_key
                    for existing_key, existing_module in self.submix_loopbacks.items()
                    if str(existing_module) == str(module_id)
                ),
                None,
            )
            if key:
                self.submix_state_cache[key] = {
                    'vol': self._clamp(state.get('vol', 1.0)),
                    'mute': bool(state.get('mute', False)),
                }
                if self._apply_loopback_state(module_id, state):
                    self._pending_submix_state_reapply.discard(key)
                else:
                    self._pending_submix_state_reapply.add(key)
        return module_id

    def _commit_submix_replacements(self, replacements, *, new_source):
        """Swap submix bookkeeping to replacement loopbacks."""
        for key, binding in replacements.items():
            self.submix_loopbacks[key] = binding['module_id']
            self.submix_sources[key] = new_source
            state = dict(binding.get('state', {}) or {})
            if state:
                self.submix_state_cache[key] = {
                    'vol': self._clamp(state.get('vol', 1.0)),
                    'mute': bool(state.get('mute', False)),
                }
                self._pending_submix_state_reapply.add(key)
        for binding in replacements.values():
            old_module = binding.get('old_module_id')
            new_module = binding.get('module_id')
            if old_module is None or str(old_module) == str(new_module):
                continue
            self._run(['pactl', 'unload-module', str(old_module)])
        if replacements:
            self.invalidate_snapshot()

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
        """Fetch every expensive state dump once so a whole refresh tick can
        share them. Safe to call with PipeWire misbehaving — missing outputs
        degrade to empty strings/lists.

        Cached for `_SNAPSHOT_TTL` seconds so back-to-back refresh requests
        (e.g. one from `pactl subscribe`, one from a slider release, one
        from the poll timer all within 100 ms) don't each spawn six
        subprocesses. Pass `force=True` after a known structural change
        (sink added/removed) to bypass the cache.

        NOTE on atomicity: the six pactl/pw-dump invocations below run
        sequentially, so PipeWire CAN mutate between them — a sink that
        existed in the modules dump may not appear in the sinks dump
        100 ms later. Helpers that look up a sink_id from
        sink_inputs_text in a sink_id_to_name dict built from sinks
        dumps tolerate missing keys and fall back to the raw id. This
        is an unavoidable consequence of driving a daemon over CLI;
        the next tick reconciles. Don't hold structural decisions for
        more than one tick on snapshot data alone — re-query when you
        actually act."""
        now = time.monotonic()
        if getattr(self, "_pending_submix_state_reapply", None):
            self._reapply_submix_state_cache()
            force = True
        cached = getattr(self, '_snapshot_cache', None)
        cached_at = getattr(self, '_snapshot_cache_at', 0.0)
        if cached is not None and not force and (now - cached_at) < self._SNAPSHOT_TTL:
            return cached

        snap = EngineSnapshot(
            modules_text=self._run(['pactl', 'list', 'modules']) or "",
            short_modules_text=self._run(['pactl', 'list', 'short', 'modules']) or "",
            sink_inputs_text=self._run(['pactl', 'list', 'sink-inputs']) or "",
            sinks_text=self._run(['pactl', 'list', 'sinks']) or "",
            sources_text=self._run(['pactl', 'list', 'sources']) or "",
            nodes=self._parse_nodes(),
            sinks=self._parse_short_sinks(),
        )
        self._snapshot_cache = snap
        self._snapshot_cache_at = now
        # Piggyback periodic GC on the snapshot rebuild. Only runs when we
        # actually fetch new data (i.e. at most once per _SNAPSHOT_TTL).
        self.reap_dead_processes()
        return snap

    def invalidate_snapshot(self):
        """Drop the cached snapshot so the next `create_snapshot` re-fetches.
        Call after a structural change we made ourselves (sink/loopback
        load/unload, FX chain rebuild) so the next refresh sees the new
        state immediately rather than waiting for the cache to expire."""
        self._snapshot_cache = None
        self._snapshot_cache_at = 0.0

    @staticmethod
    def _parse_sink_descriptions(text):
        """Return {sink_name: description} from `pactl list sinks`. UI uses
        descriptions because they hold model info (e.g. 'Sony WH-1000XM4'),
        while node.name is typically 'bluez_output.8C_1D_…'."""
        out = {}
        curr_name = None
        curr_desc = None
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Sink #'):
                if curr_name is not None and curr_desc:
                    out[curr_name] = curr_desc
                curr_name = None
                curr_desc = None
            elif stripped.startswith('Name:'):
                curr_name = stripped.split(':', 1)[1].strip()
            elif stripped.startswith('Description:'):
                curr_desc = stripped.split(':', 1)[1].strip()
        if curr_name is not None and curr_desc:
            out[curr_name] = curr_desc
        return out

    @staticmethod
    def _parse_sinks_state(text):
        """Parse `pactl list sinks` into {sink_name: (volume 0..1.5, muted)}."""
        state = {}
        curr_name = None
        curr_vol = None
        curr_mute = False
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Sink #'):
                if curr_name is not None and curr_vol is not None:
                    state[curr_name] = (curr_vol, curr_mute)
                curr_name = None
                curr_vol = None
                curr_mute = False
            elif stripped.startswith('Name:'):
                curr_name = stripped.split(':', 1)[1].strip()
            elif stripped.startswith('Mute:'):
                curr_mute = stripped.split(':', 1)[1].strip().lower() == 'yes'
            elif stripped.startswith('Volume:') and curr_vol is None:
                m = re.search(r'/\s*(\d+)%', stripped)
                if m:
                    try:
                        curr_vol = int(m.group(1)) / 100.0
                    except ValueError:
                        pass
        if curr_name is not None and curr_vol is not None:
            state[curr_name] = (curr_vol, curr_mute)
        return state

    @staticmethod
    def _parse_sources_state(text):
        """Parse `pactl list sources` into {source_name: (volume 0..1.0, muted)}."""
        state = {}
        curr_name = None
        curr_vol = None
        curr_mute = False
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Source #'):
                if curr_name is not None and curr_vol is not None:
                    state[curr_name] = (curr_vol, curr_mute)
                curr_name = None
                curr_vol = None
                curr_mute = False
            elif stripped.startswith('Name:'):
                curr_name = stripped.split(':', 1)[1].strip()
            elif stripped.startswith('Mute:'):
                curr_mute = stripped.split(':', 1)[1].strip().lower() == 'yes'
            elif stripped.startswith('Volume:') and curr_vol is None:
                m = re.search(r'/\s*(\d+)%', stripped)
                if m:
                    try:
                        curr_vol = int(m.group(1)) / 100.0
                    except ValueError:
                        pass
        if curr_name is not None and curr_vol is not None:
            state[curr_name] = (curr_vol, curr_mute)
        return state

    def _parse_nodes(self):
        raw = self._run(['pw-dump'], timeout=4)
        if not raw:
            # Empty pw-dump collapses the node graph for one tick and the
            # UI flickers. Log so a chronically slow daemon is visible.
            logging.warning("pw-dump returned no output; node graph empty for this tick")
            return []
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
            logging.warning("pw-dump output was not valid JSON; node graph empty for this tick")
            return []
        client_props_by_id = {}
        for obj in data:
            if obj.get('type') != 'PipeWire:Interface:Client':
                continue
            props = obj.get('info', {}).get('props', {}) or {}
            client_id = obj.get('id')
            if client_id is None:
                continue
            client_props_by_id[str(client_id)] = dict(props)
        nodes = []
        for obj in data:
            if obj.get('type') != 'PipeWire:Interface:Node':
                continue
            props = dict(obj.get('info', {}).get('props', {}) or {})
            client_id = props.get('client.id')
            client_props = client_props_by_id.get(str(client_id or ""))
            if client_props:
                for key, value in client_props.items():
                    if value and not props.get(key):
                        props[key] = value
            mc = props.get('media.class', '')
            if not mc.startswith(('Audio/', 'Stream/')):
                continue
            nodes.append(AudioNode(
                pw_id=obj['id'],
                name=props.get('node.name', ''),
                description=props.get('node.description', props.get('node.name', 'Unknown')),
                media_class=mc,
                app_name=props.get('application.name'),
                props=props,
            ))
        return nodes

    def _parse_short_sinks(self):
        out = self._run(['pactl', 'list', 'short', 'sinks'])
        if not out:
            return []
        sinks = []
        for line in out.splitlines():
            parts = line.split('\t')
            if len(parts) >= 2:
                sinks.append({'index': parts[0], 'name': parts[1]})
        return sinks

    # Anything matching this is junk that usually comes from short ALSA
    # descriptions ("Bd 10 1" is the tail of a Bluetooth MAC after we've
    # collapsed separators). Trigger a fallback up the lookup chain.
    _JUNK_NAME_RE = re.compile(r'^(?:[A-Za-z]{1,3}\s?\d+\s?\d+)$|^Unknown', re.IGNORECASE)

    @staticmethod
    def _pretty_bt(raw):
        """Extract a MAC address from a PipeWire bluetooth node.name and
        format it as 'Bluetooth XX:XX:…'. For UI labels callers should
        prefer the description (which has the model name) — this is just
        the fallback for when description is missing."""
        m = re.search(r'([0-9A-Fa-f]{2}(?:[:_-][0-9A-Fa-f]{2}){5})', raw)
        if m:
            return "Bluetooth " + m.group(1).replace('_', ':').upper()
        return None

    @staticmethod
    def friendly_name(raw):
        if not raw:
            return "Unknown"
        original = raw
        name = raw.strip()

        # Strip common ALSA prefixes.
        for prefix in ['Alsa Output.', 'Alsa Input.', 'alsa_output.',
                       'alsa_input.', 'bluez_output.', 'bluez_input.']:
            if name.lower().startswith(prefix.lower()):
                name = name[len(prefix):]

        # Before we mangle it, recognise Bluetooth device node.names so
        # they stop being rendered as "Bd 10 1".
        if raw.lower().startswith(('bluez_output.', 'bluez_input.')):
            bt = PipeWireEngine._pretty_bt(raw)
            if bt:
                return bt

        # Drop PCI / USB addresses.
        name = re.sub(r'pci-[0-9a-fA-F._-]+\.', '', name, flags=re.IGNORECASE)
        name = re.sub(r'Pci-[0-9a-fA-F. -]+Platform-\w+\s*', '', name, flags=re.IGNORECASE)
        name = re.sub(r'usb-[A-Za-z0-9_]+_[A-Za-z0-9_]+-\d+\.', '', name, flags=re.IGNORECASE)

        # Strip verbose boilerplate.
        verbose_terms = [
            'High Definition Audio Controller',
            'HD Audio Controller',
            'Raptor Lake', 'Alder Lake', 'Comet Lake', 'Tiger Lake',
            'Meteor Lake', 'Cannon Lake', 'Coffee Lake', 'Sunrise Point',
            'Cezanne', 'Renoir', 'Rembrandt', 'Phoenix',
            'Starship/Matisse', 'Matisse', 'Family 17h', 'Family 19h',
            'PCH', 'USB Audio', 'Generic', 'Built-in',
        ]
        for term in verbose_terms:
            name = re.sub(r'\b' + re.escape(term) + r'\b', '', name, flags=re.IGNORECASE)

        # Onboard Intel HDA: "CX8200 Analog" / "ALC256 Analog" etc. Users
        # know these as 'onboard'. Replace with something sensible.
        if re.search(r'\bALC\d+\b', name, re.IGNORECASE):
            # Keep the "Analog Stereo" / "Digital Microphone" suffix for context.
            suffix = re.search(r'(Analog|Digital|HDMI)\b.*', name, re.IGNORECASE)
            name = "Onboard"
            if suffix:
                name = f"Onboard {suffix.group(0).strip().title()}"

        # Clean up separators.
        name = name.replace('_', ' ').replace('.', ' ').replace('-', ' ')
        name = re.sub(r'\s+', ' ', name).strip()

        # If we've sanitised it into nothing or 'Bd 10 1'-style junk,
        # fall back to the raw string.
        if not name or PipeWireEngine._JUNK_NAME_RE.match(name):
            return original

        name = name.title()

        # Truncate if still too long.
        if len(name) > 28:
            parts = name.split()
            if len(parts) > 3:
                name = " ".join(parts[-3:])
            if len(name) > 28:
                name = name[:26] + '…'

        return name or original

    # ── Node Discovery ──────────────────────────────────────────────

    def get_all_nodes(self, snap=None):
        if snap is not None and hasattr(snap, "nodes"):
            return snap.nodes
        return self._parse_nodes()

    @staticmethod
    def _is_internal_node_name(name):
        # Internal plumbing uses both `wavelinux_` (underscore, for sinks
        # we create via pactl) and `wavelinux.` (dot, for filter-chain
        # virtual nodes spawned by `pipewire -c`). Both must be hidden
        # from device pickers, otherwise the FX chain's own input/output
        # surfaces in the mic / output dropdowns.
        raw = str(name or "").strip().lower()
        return raw.startswith((
            "wavelinux_",
            "wavelinux.",
            "output.wavelinux_",
            "output.wavelinux.",
            "input.wavelinux_",
            "input.wavelinux.",
        ))

    def get_hardware_outputs(self, snap=None):
        return [n for n in self.get_all_nodes(snap)
                if n.media_class == 'Audio/Sink'
                and not self._is_internal_node_name(n.name)]

    @classmethod
    def _normalize_stable_component(cls, value):
        return re.sub(r'[^a-z0-9]+', '_', str(value or "").strip().lower()).strip('_')

    @classmethod
    def _looks_like_stable_device_id(cls, value):
        value = str(value or "").strip().lower()
        return value.startswith(("bt:", "usb:", "hw:", "name:"))

    @classmethod
    def _normalized_bt_family(cls, name, *, source=False):
        name = str(name or "").strip()
        if not name:
            return ""
        matcher = cls._BT_SOURCE_FAMILY_RE if source else cls._BT_SINK_FAMILY_RE
        match = matcher.match(name)
        if not match:
            return ""
        return cls._normalize_stable_component(match.group(1))

    @classmethod
    def _stable_device_id_from_props(cls, prefix, name, props=None, *, source=False):
        name = str(name or "").strip()
        props = dict(props or {})
        bt_family = cls._normalized_bt_family(name, source=source)
        if bt_family:
            return f"bt:{bt_family}"

        for key in (
            "device.string",
            "device.api",
            "device.description",
            "node.description",
            "device.name",
        ):
            bt_family = cls._normalized_bt_family(props.get(key), source=source)
            if bt_family:
                return f"bt:{bt_family}"

        for key in (
            "device.serial",
            "device.bus-id",
            "device.bus_id",
            "device.product.id",
            "device.vendor.id",
        ):
            token = cls._normalize_stable_component(props.get(key))
            if token:
                return f"usb:{token}"

        for key in (
            "device.bus-path",
            "device.bus_path",
            "device.path",
            "api.alsa.path",
            "object.path",
            "alsa.card",
            "api.alsa.card",
            "api.alsa.card.longname",
            "device.name",
        ):
            token = cls._normalize_stable_component(props.get(key))
            if token:
                return f"hw:{token}"

        stem = name
        stem = re.sub(r'^(?:output|input)\.', '', stem, flags=re.IGNORECASE)
        stem = re.sub(r'\.monitor$', '', stem, flags=re.IGNORECASE)
        token = cls._normalize_stable_component(stem)
        if token:
            return f"name:{token}"
        return ""

    @classmethod
    def stable_sink_id(cls, sink_name):
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            return ""
        if cls._looks_like_stable_device_id(sink_name):
            return sink_name.lower()
        return cls._stable_device_id_from_props("sink", sink_name, {})

    def _node_by_name(self, node_name, snap=None):
        node_name = str(node_name or "").strip()
        if not node_name:
            return None
        for node in self.get_all_nodes(snap):
            if str(getattr(node, "name", "") or "").strip() == node_name:
                return node
        return None

    def stable_source_id(self, source_name_or_node, snap=None):
        if isinstance(source_name_or_node, AudioNode):
            node = source_name_or_node
            return self._stable_device_id_from_props(
                "source",
                getattr(node, "name", ""),
                getattr(node, "props", {}) or {},
                source=True,
            )
        source_name = str(source_name_or_node or "").strip()
        if not source_name:
            return ""
        if self._looks_like_stable_device_id(source_name):
            return source_name.lower()
        node = self._node_by_name(source_name, snap=snap)
        props = getattr(node, "props", {}) or {}
        return self._stable_device_id_from_props(
            "source",
            source_name,
            props,
            source=True,
        )

    def stable_sink_inventory(self, snap=None):
        inventory = []
        for sink in self.get_all_sinks(snap=snap):
            sink_name = str(sink.get("name") or "").strip()
            if not sink_name or self._is_internal_node_name(sink_name):
                continue
            node = self._node_by_name(sink_name, snap=snap)
            stable_id = self._stable_device_id_from_props(
                "sink",
                sink_name,
                getattr(node, "props", {}) or {},
            )
            inventory.append({
                "name": sink_name,
                "display_name": self.display_name_for_sink(sink_name, snap=snap),
                "stable_id": stable_id or self.stable_sink_id(sink_name),
            })
        return inventory

    def stable_source_inventory(self, snap=None):
        inventory = []
        for node in self.get_hardware_inputs(snap=snap):
            source_name = str(getattr(node, "name", "") or "").strip()
            if not source_name:
                continue
            inventory.append({
                "name": source_name,
                "display_name": self.friendly_name(getattr(node, "description", None)) or source_name,
                "stable_id": self.stable_source_id(node, snap=snap),
            })
        return inventory

    def resolve_hardware_sink_name(self, sink_name, snap=None):
        """Resolve a persisted hardware sink token to a currently visible sink.

        This keeps Bluetooth routes alive across profile/name churn like
        `bluez_output.<mac>.1` ↔ `.2`.
        """
        sink_name = str(sink_name or "").strip()
        if not sink_name:
            return None
        inventory = self.stable_sink_inventory(snap=snap)
        names = [sink["name"] for sink in inventory if sink.get("name")]
        if sink_name in names:
            return sink_name
        wanted = sink_name.lower() if self._looks_like_stable_device_id(sink_name) else self.stable_sink_id(sink_name)
        if not wanted:
            return None
        for sink in inventory:
            if sink.get("stable_id") == wanted:
                return sink.get("name")
        return None

    def resolve_hardware_source_name(self, source_or_stable_id, snap=None):
        source_or_stable_id = str(source_or_stable_id or "").strip()
        if not source_or_stable_id:
            return None
        inventory = self.stable_source_inventory(snap=snap)
        names = [source["name"] for source in inventory if source.get("name")]
        if source_or_stable_id in names:
            return source_or_stable_id
        wanted = (
            source_or_stable_id.lower()
            if self._looks_like_stable_device_id(source_or_stable_id)
            else self.stable_source_id(source_or_stable_id, snap=snap)
        )
        for source in inventory:
            if source.get("stable_id") == wanted:
                return source.get("name")
        return None

    def get_hardware_inputs(self, snap=None):
        nodes = []
        for node in self.get_all_nodes(snap):
            if node.media_class != 'Audio/Source':
                continue
            if 'rnnoise' in node.name.lower():
                continue
            if self._is_internal_node_name(node.name):
                continue
            node.volume, node.muted = self.get_source_volume_by_name(node.name, snap=snap)
            nodes.append(node)
        return nodes

    def get_virtual_sinks(self, snap=None):
        """User-created WaveLinux channels only (no internal mix/source sinks)."""
        return [n for n in self.get_all_nodes(snap)
                if n.media_class == 'Audio/Sink'
                and n.name in self.virtual_sink_modules
                and not n.name.startswith('wavelinux_mix_')
                and not n.name.startswith('wavelinux_src_')]

    def get_app_streams(self, snap=None):
        # Accept any media.class that starts with Stream/Output/Audio to
        # catch variants like Stream/Output/Audio:Playback used by some
        # PipeWire builds and Chromium-family apps (Brave, Ferdium, etc.).
        return [n for n in self.get_all_nodes(snap)
                if n.media_class.startswith('Stream/Output/Audio')]

    # ── Volume & Mute ──────────────────────────────────────────────

    def get_volume(self, node_id):
        out = self._run(['wpctl', 'get-volume', str(node_id)])
        if out:
            muted = '[MUTED]' in out
            try:
                vol = float(out.split(':')[1].strip().split()[0])
                return vol, muted
            except (IndexError, ValueError):
                pass
        return 1.0, False

    def set_volume(self, node_id, volume):
        self._run(['wpctl', 'set-volume', str(node_id), f'{volume:.2f}'])

    def set_mute(self, node_id, mute):
        self._run(['wpctl', 'set-mute', str(node_id), '1' if mute else '0'])

    def toggle_mute(self, node_id):
        self._run(['wpctl', 'set-mute', str(node_id), 'toggle'])

    def set_sink_volume_by_name(self, sink_name, volume):
        """wpctl expects numeric IDs; pactl addresses sinks by name."""
        pct = max(0, min(int(round(self._clamp(volume) * 100)), 100))
        self._run(['pactl', 'set-sink-volume', sink_name, f'{pct}%'])

    def set_source_volume_by_name(self, source_name, volume):
        resolved = self.resolve_source_name(source_name) or source_name
        pct = max(0, min(int(round(self._clamp(volume) * 100)), 100))
        self._run(['pactl', 'set-source-volume', resolved, f'{pct}%'])

    def get_source_volume_by_name(self, source_name, snap=None):
        wanted = self._source_name_aliases(source_name)
        if not wanted:
            return 1.0, False
        if snap is not None:
            if snap._source_state_by_name is None:
                snap._source_state_by_name = self._parse_sources_state(snap.sources_text)
            for candidate in wanted:
                hit = snap._source_state_by_name.get(candidate)
                if hit is not None:
                    return self._clamp(hit[0]), bool(hit[1])
            return 1.0, False

        out = self._run(['pactl', 'list', 'sources'])
        if not out:
            return 1.0, False
        state = self._parse_sources_state(out)
        for candidate in wanted:
            hit = state.get(candidate)
            if hit is not None:
                return self._clamp(hit[0]), bool(hit[1])
        return 1.0, False

    def get_sink_volume_by_name(self, sink_name, snap=None):
        if snap is not None:
            if snap._sink_state_by_name is None:
                snap._sink_state_by_name = self._parse_sinks_state(snap.sinks_text)
            hit = snap._sink_state_by_name.get(sink_name)
            if hit is not None:
                return hit
            return 1.0, False

        out = self._run(['pactl', 'get-sink-volume', sink_name])
        if not out:
            return 1.0, False
        muted = False
        mute_out = self._run(['pactl', 'get-sink-mute', sink_name])
        if mute_out and 'yes' in mute_out.lower():
            muted = True
        m = re.search(r'/\s*(\d+)%', out)
        if m:
            try:
                return int(m.group(1)) / 100.0, muted
            except ValueError:
                pass
        return 1.0, muted

    def set_sink_mute_by_name(self, sink_name, mute):
        self._run(['pactl', 'set-sink-mute', sink_name, '1' if mute else '0'])

    # ── Virtual Sink (Input Channel) Management ────────────────────

    def route_input_to_submix(self, node_id, node_name, media_class, mix_name,
                              snap=None, initial_state=None):
        """Loopback an input source (or sink monitor) into a submix.
        Idempotent on every refresh tick. When the channel's FX chain
        toggles, the source token changes (raw mic ↔ FX virtual-source) and
        the loopback gets rebuilt pointing at the new source.

        Returns True if `submix_loopbacks[key]` is up to date afterwards.

        `initial_state={'vol': float, 'mute': bool}` is applied
        synchronously to a freshly-loaded sink-input — without it,
        pulse-bridge's default for a new module-loopback is unmuted,
        and any audio leaks for the ~150ms gap before the caller's
        first sync push silences it. (Most visible at startup with a
        muted-by-default mic Monitor.) Reused loopbacks keep their
        existing state."""
        key = f'{node_id}->{mix_name}'

        mix = self.output_mixes.get(mix_name)
        if not mix:
            return False

        short = snap.short_modules_text if snap else None

        # The "true" source token: FX chain output if the channel has any
        # effects running, otherwise the raw mic / sink-monitor.
        fx_source = self.get_channel_fx_source(node_name, snap=snap)
        raw_source_id = str(node_name)
        if fx_source:
            source_id = fx_source
        elif media_class == 'Audio/Sink':
            source_id = f"{node_name}.monitor"
        else:
            # Use the stable Pulse/PipeWire source token (`node.name`) for
            # input devices instead of the transient numeric node id.
            #
            # FX toggles and source-output migration paths track capture
            # targets by name (e.g. `alsa_input.*`). If we build Monitor/
            # Stream loopbacks from the numeric id, a refresh can mismatch
            # routing state after an FX apply/clear and leave a submix
            # loopback pinned to a stale source token. Using the same
            # name-based token here keeps submix routing coherent across FX
            # rebuilds.
            source_id = raw_source_id

        # If a loopback we created earlier is still live AND its source
        # matches the current routing (FX state hasn't changed), keep it.
        known = self.submix_loopbacks.get(key)
        known_source = self.submix_sources.get(key)
        known_alive = bool(
            known and self._module_is_alive(known, short_text=short)
        )
        if known_alive and known_source == source_id:
            return True

        existing = self._find_loopback_for(source_id, mix.sink_name, snap=snap)
        target_module = str(existing) if existing else None
        if not target_module:
            target_module = self._run([
                'pactl', 'load-module', 'module-loopback',
                f'source={source_id}',
                f'sink={mix.sink_name}',
                'latency_msec=20',
                'adjust_time=0'
            ])
            if not target_module:
                # Keep the last known-good route instead of tearing it down
                # and leaving the mix silent when PipeWire is mid-churn.
                if known_alive:
                    return True
                return False
            self.invalidate_snapshot()

        self.submix_loopbacks[key] = target_module
        self.submix_sources[key] = source_id
        if fx_source and media_class != 'Audio/Sink':
            stale_raw_module = self._find_loopback_for(raw_source_id, mix.sink_name, snap=snap)
            if stale_raw_module is not None and str(stale_raw_module) != str(target_module):
                self._run(['pactl', 'unload-module', str(stale_raw_module)])
                self.invalidate_snapshot()
        if target_module != str(known or ""):
            state = dict(initial_state or self.submix_state_cache.get(key, {}) or {})
            if state:
                self.submix_state_cache[key] = {
                    'vol': self._clamp(state.get('vol', 1.0)),
                    'mute': bool(state.get('mute', False)),
                }
                # Fresh loopbacks often get a late stream-restore pass from
                # Pulse/PipeWire that can resurrect 100% / unmuted defaults
                # after our first write. Always schedule one more cached-state
                # reapply on the next snapshot so startup restore and route
                # rebuilds converge back to the saved submix values.
                self._apply_loopback_state(target_module, state)
                self._pending_submix_state_reapply.add(key)
            if known and str(known) != str(target_module):
                logging.warning(
                    f"[FX-DEBUG] route_input_to_submix({key}): source changed "
                    f"'{known_source}' -> '{source_id}', replacing module {known} with {target_module}"
                )
                self._run(['pactl', 'unload-module', str(known)])
                self.invalidate_snapshot()
        return True

    def _apply_loopback_state(self, module_id, state):
        """Push volume/mute onto a loopback sink-input once it becomes visible."""
        if not module_id or not state:
            return False
        si = self._sink_input_for_module(module_id)
        if si is None:
            for _ in range(20):
                time.sleep(0.005)
                si = self._sink_input_for_module(module_id)
                if si is not None:
                    break
        if si is None:
            return False
        vol = self._clamp(state.get('vol', 1.0))
        mute = bool(state.get('mute', False))
        pct = max(0, min(int(round(vol * 100)), 100))
        self._run(['pactl', 'set-sink-input-volume', si, f'{pct}%'])
        self._run(['pactl', 'set-sink-input-mute', si, '1' if mute else '0'])
        return True

    def _build_loopback_index(self, modules_text):
        """Parse a pactl-modules dump once into (source,sink) -> module_id."""
        index = {}
        curr_id = None
        curr_name = ''
        curr_args = []

        def flush():
            if curr_id and curr_name == 'module-loopback':
                src = next((a.split('=', 1)[1] for a in curr_args
                            if a.startswith('source=')), None)
                snk = next((a.split('=', 1)[1] for a in curr_args
                            if a.startswith('sink=')), None)
                if src and snk:
                    index.setdefault((src, snk), curr_id)

        for line in modules_text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Module #'):
                flush()
                curr_id = stripped.split('#', 1)[1].strip()
                curr_name = ''
                curr_args = []
            elif stripped.startswith('Name:'):
                curr_name = stripped.split(':', 1)[1].strip()
            elif stripped.startswith('Argument:'):
                curr_args = stripped.split('Argument:', 1)[1].strip().split()
        flush()
        return index

    def _find_loopback_for(self, source_token, sink_token, snap=None):
        if snap is not None:
            if snap._loopback_index is None:
                snap._loopback_index = self._build_loopback_index(snap.modules_text)
            return snap._loopback_index.get((source_token, sink_token))
        modules_text = self._run(['pactl', 'list', 'modules']) or ''
        return self._build_loopback_index(modules_text).get((source_token, sink_token))

    def remove_node_routing(self, node_id):
        """Clean up all loopbacks associated with a removed node."""
        node_id = str(node_id)
        for key in list(self.submix_loopbacks.keys()):
            if key.startswith(f"{node_id}->"):
                mod_id = self.submix_loopbacks.pop(key)
                self.submix_sources.pop(key, None)
                self.submix_state_cache.pop(key, None)
                self._pending_submix_state_reapply.discard(key)
                self._run(['pactl', 'unload-module', str(mod_id)])

    def get_submix_sink_input(self, node_id, mix_name, snap=None):
        module_id = self.submix_loopbacks.get(f'{node_id}->{mix_name}')
        if module_id is None:
            return None
        module_id = str(module_id)

        text = snap.sink_inputs_text if snap else self._run(['pactl', 'list', 'sink-inputs'])
        if not text:
            return None
        current_si = None
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Sink Input #'):
                current_si = stripped.split('#', 1)[1].strip()
            elif 'module.id =' in stripped and f'"{module_id}"' in stripped:
                return current_si
            elif stripped.startswith('Owner Module:'):
                owner = stripped.split(':', 1)[1].strip()
                if owner == module_id:
                    return current_si
        return None

    def set_submix_volume(self, node_id, mix_name, volume):
        """Set the submix volume. Returns False if the sink-input isn't
        found yet (loopback hasn't loaded or was unloaded mid-tick); UI
        uses the bool to retry on the next refresh."""
        key = f'{node_id}->{mix_name}'
        cache = self.submix_state_cache.setdefault(key, {})
        cache['vol'] = self._clamp(volume)
        si = self.get_submix_sink_input(node_id, mix_name)
        if not si:
            logging.warning(f"Could not find sink-input for {node_id}->{mix_name}")
            self._pending_submix_state_reapply.add(key)
            return False
        clamped = cache['vol']
        pct = max(0, min(int(round(clamped * 100)), 100))
        self._run(['pactl', 'set-sink-input-volume', si, f'{pct}%'])
        self._pending_submix_state_reapply.discard(key)
        return True

    def set_submix_mute(self, node_id, mix_name, mute):
        """Set the submix mute. Same retry semantics as set_submix_volume."""
        key = f'{node_id}->{mix_name}'
        cache = self.submix_state_cache.setdefault(key, {})
        cache['mute'] = bool(mute)
        si = self.get_submix_sink_input(node_id, mix_name)
        if not si:
            logging.warning(f"Could not find sink-input to mute for {node_id}->{mix_name}")
            self._pending_submix_state_reapply.add(key)
            return False
        bmute = cache['mute']
        self._run(['pactl', 'set-sink-input-mute', si, '1' if bmute else '0'])
        self._pending_submix_state_reapply.discard(key)
        return True

    def _reapply_submix_state_cache(self):
        """Re-push every cached submix sink-input state. Calling this
        after `_move_source_outputs` closes the brief unmute window
        where pulse-bridge can flip a moved sink-input back to its
        stale per-app default — the audio leak users hear as 'mic
        plays for a second every time effects rebuild'."""
        pending = set(getattr(self, "_pending_submix_state_reapply", set()) or set())
        for key in pending:
            cache = self.submix_state_cache.get(key)
            if not cache:
                self._pending_submix_state_reapply.discard(key)
                continue
            mod_id = self.submix_loopbacks.get(key)
            if mod_id is None:
                continue
            si = self._sink_input_for_module(mod_id)
            if si is None:
                continue
            if 'vol' in cache:
                pct = max(0, min(int(round(cache['vol'] * 100)), 100))
                self._run(['pactl', 'set-sink-input-volume', si, f'{pct}%'])
            if 'mute' in cache:
                self._run(['pactl', 'set-sink-input-mute', si,
                           '1' if cache['mute'] else '0'])
            self._pending_submix_state_reapply.discard(key)

    def snapshot_sink_inputs_by_owner(self, snap=None):
        """Map `owner_module_id -> (volume 0..1.5, muted)` in a single
        `pactl list sink-inputs` pass. Used by the UI to reflect external
        changes (pavucontrol, KMix, media keys) without per-channel calls."""
        text = snap.sink_inputs_text if snap else self._run(['pactl', 'list', 'sink-inputs'])
        if not text:
            return {}
        by_owner = {}
        curr_owner = None
        curr_vol = None
        curr_mute = False
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Sink Input #'):
                if curr_owner is not None and curr_vol is not None:
                    by_owner[curr_owner] = (curr_vol, curr_mute)
                curr_owner = None
                curr_vol = None
                curr_mute = False
            elif stripped.startswith('Owner Module:'):
                curr_owner = stripped.split(':', 1)[1].strip()
            elif stripped.startswith('Mute:'):
                curr_mute = stripped.split(':', 1)[1].strip().lower() == 'yes'
            elif stripped.startswith('Volume:') and curr_vol is None:
                m = re.search(r'/\s*(\d+)%', stripped)
                if m:
                    try:
                        curr_vol = int(m.group(1)) / 100.0
                    except ValueError:
                        pass
        if curr_owner is not None and curr_vol is not None:
            by_owner[curr_owner] = (curr_vol, curr_mute)
        return by_owner

    # ── Output Mix Management ──────────────────────────────────────

    @staticmethod
    def _sanitize_channel_name(display_name):
        """Turn 'Game  ' into 'game', '  My  Mic ' into 'my_mic'."""
        cleaned = re.sub(r'\s+', ' ', (display_name or '').strip())
        safe = re.sub(r'[^A-Za-z0-9_]+', '_', cleaned.lower()).strip('_')
        return cleaned, safe or 'channel'

    @staticmethod
    def _branding_label(display_clean):
        """Build the visible 'WaveLinux-<name>' device label. Whitespace
        is collapsed to '-' because pactl's `sink_properties=` parser
        splits on spaces and quoting behaviour differs between PulseAudio
        and pipewire-pulse — single-token values render identically
        everywhere."""
        if not display_clean:
            return "WaveLinux"
        compact = re.sub(r'\s+', '-', display_clean.strip())
        return f"WaveLinux-{compact}"

    def create_virtual_sink(self, display_name, custom_name=None):
        """Create a virtual null-sink. Returns the sink name on success."""
        display_clean, safe_tail = self._sanitize_channel_name(display_name)
        safe_name = custom_name or f"wavelinux_{safe_tail}"
        description = self._branding_label(display_clean)

        existing = self._find_module_by_arg(f"sink_name={safe_name}")
        if existing:
            logging.info(f"Using existing sink {safe_name} (ID: {existing})")
            if not safe_name.startswith('wavelinux_mix_'):
                self.virtual_sink_modules[safe_name] = existing
            return safe_name

        # No quotes — the description is guaranteed whitespace-free now.
        cmd = [
            "pactl", "load-module", "module-null-sink",
            f"sink_name={safe_name}",
            (
                f"sink_properties="
                f"device.description={description} "
                f"node.description={description} "
                f"node.nick={description} "
                f"media.name={description} "
                f"application.name={description} "
                f"media.class=Audio/Sink"
            ),
        ]
        out = self._run(cmd)
        if out:
            self._run(['pactl', 'set-sink-mute', safe_name, '0'])
            self._run(['pactl', 'set-sink-volume', safe_name, '100%'])
            if not safe_name.startswith('wavelinux_mix_'):
                self.virtual_sink_modules[safe_name] = out
            return safe_name
        return None

    def remove_virtual_sink(self, sink_name):
        """Unload a user-created virtual sink and drop its loopbacks."""
        module_id = self.virtual_sink_modules.pop(sink_name, None)
        if module_id is None:
            module_id = self._find_module_by_arg(f"sink_name={sink_name}")
        if module_id is None:
            return False

        # Drop any loopbacks that target this sink as their destination.
        full = self._run(['pactl', 'list', 'modules']) or ''
        curr_id = None
        to_drop = []
        for line in full.splitlines():
            line = line.strip()
            if line.startswith('Module #'):
                curr_id = line.split('#', 1)[1].strip()
            elif 'Argument:' in line and f'sink={sink_name}' in line and curr_id:
                to_drop.append(curr_id)
        for mid in to_drop:
            self._run(['pactl', 'unload-module', mid])

        # Drop submix loopbacks that READ from this sink's monitor.
        # The submix_loopbacks key format is `node_id->mix_name`, so
        # the old `key.endswith(...)` check never matched anything;
        # filter on the tracked source token instead.
        monitor_token = f"{sink_name}.monitor"
        for skey in list(self.submix_sources.keys()):
            if self.submix_sources.get(skey) != monitor_token:
                continue
            mod = self.submix_loopbacks.pop(skey, None)
            self.submix_sources.pop(skey, None)
            self.submix_state_cache.pop(skey, None)
            if mod is not None:
                self._run(['pactl', 'unload-module', str(mod)])

        self._run(['pactl', 'unload-module', str(module_id)])
        return True

    def create_output_mix(self, name):
        """Create a mix bus: a null-sink plus a virtual source so apps like OBS
        can pick it up as a dedicated recording device (e.g. 'WaveLinux-Stream')."""
        _, safe_name = self._sanitize_channel_name(name)
        sink_name = f"wavelinux_mix_{safe_name}"
        requested_source_name = f"wavelinux_src_{safe_name}"
        description = self._branding_label(name)

        # 1. The thing apps play *to*.
        if self.create_virtual_sink(name, custom_name=sink_name) is None:
            return None
        sink_module_id = (self.virtual_sink_modules.get(sink_name)
                          or self._find_module_by_arg(f"sink_name={sink_name}"))

        # 2. Dedicated recording source so OBS / browsers see a named device
        # instead of a generic "Monitor of null sink". Whitespace-free
        # description values so pactl's sink_properties parser can't fumble.
        src_module_id = self._find_module_by_arg(f"source_name={requested_source_name}")
        if not src_module_id:
            src_module_id = self._run([
                'pactl', 'load-module', 'module-virtual-source',
                f'source_name={requested_source_name}',
                f'master={sink_name}.monitor',
                (
                    f"source_properties="
                    f"device.description={description} "
                    f"node.description={description} "
                    f"node.nick={description} "
                    f"media.name={description} "
                    f"application.name={description} "
                    f"media.class=Audio/Source "
                    f"device.class=sound"
                ),
            ])
        source_name = (
            self._wait_source_visible(requested_source_name, attempts=20, delay=0.05)
            or self.resolve_source_name(requested_source_name)
            or requested_source_name
        )

        mix = OutputMix(name, sink_module_id=sink_module_id, sink_name=sink_name)
        mix.source_name = source_name
        mix.source_module_id = src_module_id
        self.output_mixes[name] = mix
        return mix

    def remove_output_mix(self, mix_name):
        mix = self.output_mixes.get(mix_name)
        if not mix:
            return False
        for mid in (getattr(mix, 'source_module_id', None), mix.sink_module_id):
            if mid:
                self._run(['pactl', 'unload-module', str(mid)])
        for key in list(self.loopback_modules.keys()):
            if key.startswith(mix_name + '->'):
                self._run(['pactl', 'unload-module', str(self.loopback_modules[key])])
                del self.loopback_modules[key]
        # Drop submix loopbacks for this mix. Submix-loopback keys are
        # `node_id->mix_name`, so match the suffix. Without this,
        # set_submix_volume / set_submix_mute keep targeting a dead
        # module id forever — there's no later code path that rebuilds
        # them, because the mix is gone.
        for skey in list(self.submix_loopbacks.keys()):
            if skey.endswith(f'->{mix_name}'):
                mod = self.submix_loopbacks.pop(skey, None)
                self.submix_sources.pop(skey, None)
                if mod is not None:
                    self._run(['pactl', 'unload-module', str(mod)])
        del self.output_mixes[mix_name]
        return True

    def route_mix_to_hardware(self, mix_name, hw_sink_name):
        """Route a mix bus to a hardware output via module-loopback.
        Bluetooth-aware: doesn't pin the loopback's sink (BT sinks rotate
        names on profile change, e.g. `bluez_output.MAC.1` ↔ `.2`). After
        load, force the new sink-input to 100% / unmuted so a freshly
        routed BT device isn't silently created at 0%."""
        mix = self.output_mixes.get(mix_name)
        if not mix:
            return False
        requested_sink = str(hw_sink_name or "").strip()
        resolved_sink = self.resolve_hardware_sink_name(requested_sink)
        allow_fallback = mix_name == "Monitor"
        if not resolved_sink and allow_fallback:
            resolved_sink = self._preferred_hardware_sink_fallback(snap=None)
            if resolved_sink and requested_sink:
                logging.warning(
                    "route_mix_to_hardware: falling back from missing sink %s to %s for %s",
                    requested_sink,
                    resolved_sink,
                    mix_name,
                )
        current_key = f'{mix_name}->{resolved_sink or requested_sink}'
        short = self._run(['pactl', 'list', 'short', 'modules']) or ''
        existing_mod = self.loopback_modules.get(current_key)
        if (existing_mod
                and self._module_is_alive(existing_mod, short_text=short)
                and self._wait_sink_input_for_module(existing_mod, attempts=2, delay=0.01)):
            mix.hardware_output = resolved_sink or requested_sink
            return True

        # Retry briefly — picked sink may not be visible yet (BT mid-profile
        # negotiation, USB still enumerating).
        out = None
        adopted = None
        candidate_sink = resolved_sink
        if candidate_sink:
            adopted = self._find_loopback_for(f'{mix.sink_name}.monitor', candidate_sink)
            if adopted and self._wait_sink_input_for_module(adopted, attempts=2, delay=0.01):
                out = str(adopted)
        for attempt in range(12):
            if out:
                break
            candidate_sink = self.resolve_hardware_sink_name(requested_sink)
            if not candidate_sink and allow_fallback:
                candidate_sink = self._preferred_hardware_sink_fallback(snap=None)
            if candidate_sink:
                adopted = self._find_loopback_for(f'{mix.sink_name}.monitor', candidate_sink)
                if adopted and self._wait_sink_input_for_module(adopted, attempts=2, delay=0.01):
                    out = str(adopted)
                    break
                out = self._run([
                    'pactl', 'load-module', 'module-loopback',
                    f'source={mix.sink_name}.monitor',
                    f'sink={candidate_sink}',
                    'latency_msec=20',
                    'adjust_time=0',
                ])
                if out:
                    break
            time.sleep(0.1)

        if not out:
            logging.warning(
                f"route_mix_to_hardware: could not load loopback "
                f"{mix.sink_name}.monitor → {requested_sink or '<none>'}"
            )
            return False

        candidate_mod = str(out)
        if not self._wait_sink_input_for_module(candidate_mod):
            if candidate_mod != str(existing_mod or ""):
                self._run(['pactl', 'unload-module', candidate_mod])
            logging.warning(
                f"route_mix_to_hardware: loopback module {candidate_mod} "
                f"never produced a sink-input for {mix_name}"
            )
            return False

        for key in list(self.loopback_modules.keys()):
            if not key.startswith(mix_name + '->'):
                continue
            if str(self.loopback_modules[key]) == candidate_mod:
                continue
            self._run(['pactl', 'unload-module', str(self.loopback_modules[key])])
            del self.loopback_modules[key]

        resolved_sink = candidate_sink or resolved_sink or requested_sink
        current_key = f'{mix_name}->{resolved_sink}'
        self.loopback_modules[current_key] = candidate_mod
        mix.hardware_output = resolved_sink
        # Force the new sink-input to 100%/unmuted — pulse-bridge can
        # apply a stale per-app-per-sink rule that defaults it to 0%.
        si = self._sink_input_for_module(candidate_mod)
        if si is not None:
            self._run(['pactl', 'set-sink-input-volume', si, '100%'])
            self._run(['pactl', 'set-sink-input-mute',   si, '0'])
        self.invalidate_snapshot()
        return True

    def _sink_visible(self, sink_name):
        """Cheap check: is `sink_name` in `pactl list short sinks` right now?"""
        if not sink_name:
            return False
        out = self._run(['pactl', 'list', 'short', 'sinks'])
        if not out:
            return False
        for line in out.splitlines():
            parts = line.split('\t')
            if len(parts) >= 2 and parts[1].strip() == sink_name:
                return True
        return False

    def _sink_input_for_module(self, module_id):
        """Find the sink-input index owned by `module_id` (a pactl module
        we just loaded). Returns the sink-input index as a string, or None."""
        if not module_id:
            return None
        text = self._run(['pactl', 'list', 'sink-inputs'])
        if not text:
            return None
        mid = str(module_id)
        current_si = None
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Sink Input #'):
                current_si = stripped.split('#', 1)[1].strip()
            elif stripped.startswith('Owner Module:') and stripped.split(':', 1)[1].strip() == mid:
                return current_si
            elif 'module.id =' in stripped and f'"{mid}"' in stripped:
                return current_si
        return None

    def _wait_sink_input_for_module(self, module_id, attempts=20, delay=0.05):
        if not module_id:
            return None
        for _ in range(max(1, int(attempts))):
            si = self._sink_input_for_module(module_id)
            if si is not None:
                return si
            time.sleep(max(0.0, float(delay)))
        return None

    def get_live_mix_hardware_route(self, mix_name, snap=None):
        mix = self.output_mixes.get(mix_name)
        if not mix or not getattr(mix, "hardware_output", None):
            return None
        resolved_sink = self.resolve_hardware_sink_name(mix.hardware_output, snap=snap)
        if not resolved_sink:
            return None
        current_key = f'{mix_name}->{resolved_sink}'
        short = snap.short_modules_text if snap is not None else None
        existing_mod = self.loopback_modules.get(current_key)
        if (existing_mod
                and self._module_is_alive(existing_mod, short_text=short)
                and self._wait_sink_input_for_module(existing_mod, attempts=2, delay=0.01)):
            return resolved_sink
        adopted = self._find_loopback_for(f'{mix.sink_name}.monitor', resolved_sink, snap=snap)
        if (adopted
                and self._module_is_alive(adopted, short_text=short)
                and self._wait_sink_input_for_module(adopted, attempts=2, delay=0.01)):
            self.loopback_modules[current_key] = str(adopted)
            return resolved_sink
        return None

    def _preferred_hardware_sink_fallback(self, snap=None):
        current_default = str(self.get_default_sink() or "").strip()
        resolved_default = self.resolve_hardware_sink_name(current_default, snap=snap)
        if resolved_default and not self._is_internal_node_name(resolved_default):
            return resolved_default
        for mix_name in ("Monitor", "Stream"):
            mix = self.output_mixes.get(mix_name)
            candidate = getattr(mix, "hardware_output", None)
            resolved = self.resolve_hardware_sink_name(candidate, snap=snap)
            if resolved and not self._is_internal_node_name(resolved):
                return resolved
        for node in self.get_hardware_outputs(snap=snap):
            name = str(getattr(node, "name", "") or "").strip()
            if name:
                return name
        return None

    def _preferred_hardware_source_fallback(self, snap=None):
        def _resolve_visible_source(candidate):
            try:
                return self.resolve_source_name(candidate, snap=snap)
            except TypeError:
                return self.resolve_source_name(candidate)

        current_default = str(self.get_default_source() or "").strip()
        resolved_default = (
            self.resolve_hardware_source_name(current_default, snap=snap)
            or _resolve_visible_source(current_default)
            or str(current_default or "").strip()
        )
        if resolved_default and not self._is_internal_node_name(resolved_default):
            return resolved_default
        for info in getattr(self, "channel_fx", {}).values():
            for candidate in (info.get("prev_default"), info.get("capture_target")):
                resolved = (
                    self.resolve_hardware_source_name(candidate, snap=snap)
                    or _resolve_visible_source(candidate)
                    or str(candidate or "").strip()
                )
                if resolved and not self._is_internal_node_name(resolved):
                    return resolved
        for node in self.get_hardware_inputs(snap=snap):
            name = str(getattr(node, "name", "") or "").strip()
            if name:
                return name
        return None

    def _move_app_streams_off_managed_sinks(self, fallback_sink, snap=None):
        fallback_sink = self.resolve_hardware_sink_name(fallback_sink, snap=snap) or fallback_sink
        fallback_sink = str(fallback_sink or "").strip()
        if not fallback_sink or self._is_internal_node_name(fallback_sink):
            return []
        moved = []
        for app in self.get_sink_inputs(snap=snap):
            sink_name = str(app.get("sink") or "").strip()
            sink_input_index = str(app.get("index") or "").strip()
            if not sink_input_index or not sink_name.startswith("wavelinux_"):
                continue
            self.move_app_to_sink(sink_input_index, fallback_sink)
            moved.append((sink_input_index, sink_name, fallback_sink))
        return moved

    def _restore_physical_defaults_before_reset(self, snap=None):
        fallback_sink = self._preferred_hardware_sink_fallback(snap=snap)
        moved = self._move_app_streams_off_managed_sinks(fallback_sink, snap=snap)
        if moved:
            logging.info(
                "Moved %d app streams off managed WaveLinux sinks before reset",
                len(moved),
            )

        current_default_sink = str(self.get_default_sink() or "").strip()
        if current_default_sink and self._is_internal_node_name(current_default_sink) and fallback_sink:
            self.set_default_sink(fallback_sink)

        current_default_source = str(self.get_default_source() or "").strip()
        fallback_source = self._preferred_hardware_source_fallback(snap=snap)
        if current_default_source and self._is_internal_node_name(current_default_source) and fallback_source:
            self.set_default_source(fallback_source)

    def full_audio_reset(self):
        """Emergency cleanup of ALL wavelinux modules."""
        logging.info("Performing full audio reset...")
        snap = None
        try:
            snap = self.create_snapshot(force=True)
        except Exception:
            snap = None
        try:
            self._restore_physical_defaults_before_reset(snap=snap)
        except Exception as exc:
            logging.warning(f"Pre-reset endpoint restore failed: {exc}")
        out = self._run(['pactl', 'list', 'short', 'modules'], timeout=5)
        if out:
            # First unload loopbacks to avoid dependency issues
            lines = out.splitlines()
            for line in reversed(lines):
                if 'wavelinux' in line and 'module-loopback' in line:
                    mod_id = line.split()[0]
                    logging.info(f"Unloading loopback: {mod_id}")
                    self._run(['pactl', 'unload-module', mod_id], timeout=3)

            # Then unload virtual sources that depend on the mix monitors.
            for line in reversed(lines):
                if 'wavelinux' in line and 'module-virtual-source' in line:
                    mod_id = line.split()[0]
                    logging.info(f"Unloading source: {mod_id}")
                    self._run(['pactl', 'unload-module', mod_id], timeout=3)

            # Then unload sinks
            for line in reversed(lines):
                if 'wavelinux' in line and 'module-null-sink' in line:
                    mod_id = line.split()[0]
                    logging.info(f"Unloading sink: {mod_id}")
                    self._run(['pactl', 'unload-module', mod_id], timeout=3)

        self.loopback_modules.clear()
        self.submix_loopbacks.clear()
        self.virtual_sink_modules.clear()
        self.output_mixes.clear()

    # ── App Routing ────────────────────────────────────────────────

    def _parse_pactl_si_map(self, text):
        """Parse `pactl list sink-inputs` text into two lookup dicts.

        Returns (by_node_id, by_index) where each value is a dict of all
        raw k=v properties plus special keys '_index' (pactl sink-input
        index string) and '_sink_id' (Sink: line value).

        by_node_id is keyed by any sink-input reference pactl exposes
        (`node.id`, `object.serial`, or the sink-input index itself) so
        it can be cross-referenced against pw-dump AudioNode values even
        when pactl omits `node.id` for native PipeWire clients.
        by_index is keyed by the pactl sink-input index string.
        """
        by_node_id = {}
        by_index = {}
        current = {}
        current_index = None

        def _flush():
            if current_index is None:
                return
            entry = dict(current)
            entry['_index'] = current_index
            by_index[current_index] = entry
            for ref in (
                current.get('node.id'),
                current.get('object.serial'),
                current_index,
            ):
                if ref is not None and str(ref).strip():
                    by_node_id[str(ref).strip()] = entry

        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Sink Input #'):
                _flush()
                current_index = stripped.split('#', 1)[1].strip()
                current = {}
            elif stripped.startswith('Sink:') and current_index is not None:
                current['_sink_id'] = stripped.split(':', 1)[1].strip()
            elif stripped.startswith('Volume:') and current_index is not None:
                match = re.search(r'/\s*(\d+)%', stripped)
                if match:
                    try:
                        current['volume'] = int(match.group(1)) / 100.0
                    except ValueError:
                        pass
            elif '=' in stripped and current_index is not None:
                parts = stripped.split('=', 1)
                if len(parts) == 2:
                    key = parts[0].strip()
                    val = parts[1].strip().strip('"')
                    current[key] = val
                    if key in ('pipewire.sec.pid', 'application.process.id'):
                        current['pid'] = val
                    elif key == 'application.process.binary':
                        current['binary'] = val
        _flush()
        return by_node_id, by_index

    def get_sink_inputs(self, snap=None):
        sinks = self.get_all_sinks(snap=snap)
        sink_id_to_name = {s['index']: s['name'] for s in sinks}

        si_text = snap.sink_inputs_text if snap else (
            self._run(['pactl', 'list', 'sink-inputs']) or '')
        by_node_id, by_index = self._parse_pactl_si_map(si_text)

        entries = []
        seen_pactl_refs = set()

        # Primary path: pw-dump Stream/Output/Audio nodes give reliable
        # discovery even for native PipeWire clients that may not surface
        # cleanly through the PulseAudio compat text output. Each node is
        # enriched with the matching pactl properties (pid, binary, app IDs)
        # via a node.id cross-reference so _process_sink_input gets the full
        # property set it needs for name resolution.
        stream_nodes = self.get_app_streams(snap=snap)
        for node in stream_nodes:
            # Merge: pw-dump JSON is authoritative for identity props
            # (application.name, node.description, pid, …). pactl text adds
            # pactl-only fields (sink index, sink id) and fills in any prop
            # that pw-dump didn't carry — but must NOT overwrite a good
            # pw-dump value with a blank/wrong one from the text parser.
            pactl = {}
            for ref in (
                node.props.get('node.id'),
                node.props.get('object.id'),
                node.props.get('object.serial'),
                node.pw_id,
            ):
                if ref is None:
                    continue
                pactl = by_node_id.get(str(ref), {})
                if pactl:
                    seen_pactl_refs.add(str(ref))
                    break
            current = dict(node.props)          # pw-dump JSON props (authoritative)
            for k, v in pactl.items():
                if k in ('_index', '_sink_id'):
                    continue
                # Only let pactl fill gaps; don't clobber non-empty pw-dump values.
                if v and not current.get(k):
                    current[k] = v

            # Ensure the convenience aliases used by _process_sink_input are set.
            current.setdefault('node.name', node.name)
            current.setdefault('node.description', node.description)
            if node.app_name:
                current.setdefault('application.name', node.app_name)
            # Seed node.id from the pw-dump object id so _process_sink_input's
            # last-resort fallback name has a stable non-None id even for native
            # PipeWire clients that never appear in pactl list sink-inputs.
            current.setdefault('node.id', str(node.pw_id))

            # Derive the 'pid' shorthand from whichever PW property is present.
            if 'pid' not in current:
                current['pid'] = (current.get('pipewire.sec.pid')
                                  or current.get('application.process.id'))

            # pactl-specific fields (index is None when pactl has no entry yet;
            # the app will still show in the list but can't be moved yet).
            current['index'] = pactl.get('_index')
            sink_id = pactl.get('_sink_id')
            current['sink_id'] = sink_id
            current['sink'] = sink_id_to_name.get(sink_id, sink_id) if sink_id else None

            self._process_sink_input(current, entries, sink_id_to_name)

        # Fallback path: pactl entries that have no matching pw-dump node
        # (e.g. PulseAudio-compat streams on some PipeWire builds, or
        # entries that appeared between pw-dump and pactl calls).
        for node_id_str, pactl in by_node_id.items():
            if node_id_str in seen_pactl_refs:
                continue
            current = {k: v for k, v in pactl.items()
                       if k not in ('_index', '_sink_id')}
            current['index'] = pactl['_index']
            sink_id = pactl.get('_sink_id')
            current['sink_id'] = sink_id
            current['sink'] = sink_id_to_name.get(sink_id, sink_id) if sink_id else None
            if 'pid' not in current:
                current['pid'] = (current.get('pipewire.sec.pid')
                                  or current.get('application.process.id'))
            self._process_sink_input(current, entries, sink_id_to_name)

        return entries

    # Known-generic names that should trigger a deeper lookup instead of being displayed.
    # Compared via _is_generic_name(), which normalizes hyphens/underscores/dots
    # to spaces and collapses whitespace — so "audio-src", "Audio Src", and
    # "AudioSrc" all match the single entry "audio src" below. Keep entries in
    # the canonical normalized form (lowercase, spaces only).
    _GENERIC_APP_NAMES = {
        "audio src", "audio sink", "audio stream", "audio output",
        "audiostream", "audio playback", "playback stream", "output",
        "speech dispatcher", "unknown", "libcanberra", "playback",
        "pipewire", "pipewire pulse", "pulseaudio", "alsa plugins",
        "alsa plug in", "alsa plug ins", "audiostreamforandroid",
        "audio stream for android", "application", "pw loopback", "loopback",
        # Chromium/Electron Flatpak apps often default to these:
        "chromium", "electron", "chrome", "chrome browser",
        # Generic Qt / GStreamer / SDL stream names:
        "qt", "qtmultimedia", "gstreamer", "sdl", "sdl audio", "media stream",
    }

    @staticmethod
    def _normalize_app_name(s):
        """Canonical form for generic-name comparison: lowercase, hyphens/
        underscores/dots → space, collapse whitespace. So "Audio-Src",
        "audio src", and "AUDIO_SRC" all normalize to "audio src"."""
        if not s:
            return ''
        s = str(s).strip().lower()
        for ch in '-_.':
            s = s.replace(ch, ' ')
        return ' '.join(s.split())

    def _is_generic_name(self, s):
        """True if `s` (after normalization) is in _GENERIC_APP_NAMES, OR is
        empty / a bare numeric stream id. Single source of truth for every
        name-resolution fallback in _process_sink_input."""
        norm = self._normalize_app_name(s)
        if not norm:
            return True
        if norm in self._GENERIC_APP_NAMES:
            return True
        # Numeric-only or single-letter tokens are useless as display names.
        if norm.isdigit() or len(norm) <= 1:
            return True
        return False

    # Reverse-DNS → friendly-name fallback for Flatpak / .desktop app IDs
    # we've seen in the wild. Wins when heuristics can't pick a good name.
    _KNOWN_APP_IDS = {
        "com.spotify.client": "Spotify",
        "com.spotify.spotify": "Spotify",
        "spotify": "Spotify",
        "com.discordapp.discord": "Discord",
        "com.discordapp.discordcanary": "Discord Canary",
        "com.discordapp.discordptb": "Discord PTB",
        "com.obsproject.studio": "OBS Studio",
        "com.valvesoftware.steam": "Steam",
        "org.mozilla.firefox": "Firefox",
        "org.mozilla.thunderbird": "Thunderbird",
        "com.google.chrome": "Chrome",
        "com.brave.browser": "Brave",
        "com.brave.browser.beta": "Brave Beta",
        "com.brave.browser.nightly": "Brave Nightly",
        "com.brave.browser.origin": "Brave Origin Beta",
        "io.ferdium.ferdium": "Ferdium",
        "org.ferdium.ferdium": "Ferdium",
        "org.telegram.desktop": "Telegram",
        "com.slack.slack": "Slack",
        "us.zoom.zoom": "Zoom",
        "com.microsoft.teams": "Microsoft Teams",
        "org.videolan.vlc": "VLC",
        "io.mpv.mpv": "mpv",
        "com.github.iwalton3.jellyfin-media-player": "Jellyfin",
        "tv.plex.plexmediaplayer": "Plex",
    }

    @classmethod
    def _canonicalize_app_id(cls, app_id):
        if not app_id:
            return None
        mapped = cls._KNOWN_APP_IDS.get(app_id.lower())
        if mapped:
            return mapped
        # Strip the reverse-DNS prefix: com.spotify.Client → Client
        # That's better than the generic fallback but worse than the
        # curated mapping above, hence checked second.
        tail = app_id.rsplit('.', 1)[-1]
        return tail.replace('-', ' ').replace('_', ' ').strip() or app_id

    @staticmethod
    def _normalize_for_host_match(value):
        """Lower-case and strip non-alphanumerics so 'DuskyPC',
        'dusky_pc', 'Dusky PC', 'dusky-pc.local' all collapse to one
        comparable token. Used by the host filter for the App Routing tab."""
        if not value:
            return ''
        return re.sub(r'[^a-z0-9]', '', value.lower())

    @classmethod
    def _host_aliases(cls):
        """Cached set of normalised tokens identifying this machine.
        Used to drop sink-inputs that PipeWire hangs off the local host
        instead of a real app from the App Routing tab."""
        cached = getattr(cls, '_host_alias_cache', None)
        if cached is not None:
            return cached
        raw = set()
        try:
            h = socket.gethostname()
            if h:
                raw.add(h)
                short = h.split('.', 1)[0]
                if short:
                    raw.add(short)
        except Exception:
            pass
        try:
            with open('/etc/hostname', 'r') as f:
                h = f.read().strip()
                if h:
                    raw.add(h)
                    raw.add(h.split('.', 1)[0])
        except OSError:
            pass
        names = {cls._normalize_for_host_match(h) for h in raw}
        names.discard('')
        if not names:
            # gethostname() and /etc/hostname both failed — host filter
            # is now a silent passthrough, log once.
            logging.warning(
                "Could not determine hostname; host-name filter for "
                "system streams in App Routing will be inactive."
            )
        cls._host_alias_cache = names
        return names

    @classmethod
    def name_matches_host(cls, value):
        """True if `value` is one of this machine's hostnames after
        normalisation."""
        token = cls._normalize_for_host_match(value)
        if not token:
            return False
        return token in cls._host_aliases()

    @staticmethod
    def _read_proc_cmdline(pid):
        try:
            with open(f"/proc/{pid}/cmdline", "rb") as f:
                raw = f.read()
        except OSError:
            return []
        return [p.decode('utf-8', 'replace') for p in raw.split(b'\x00') if p]

    @staticmethod
    def _read_proc_env(pid):
        """Return /proc/<pid>/environ as a dict, or {} if unreadable."""
        try:
            with open(f"/proc/{pid}/environ", "rb") as f:
                raw = f.read()
        except OSError:
            return {}
        env = {}
        for entry in raw.split(b'\x00'):
            if b'=' in entry:
                k, v = entry.split(b'=', 1)
                try:
                    env[k.decode('utf-8', 'replace')] = v.decode('utf-8', 'replace')
                except Exception:
                    continue
        return env

    @staticmethod
    def _read_proc_cgroup(pid):
        try:
            with open(f"/proc/{pid}/cgroup", "r") as f:
                return f.read()
        except OSError:
            return ""

    def _identify_sandboxed_app(self, pid):
        """Resolve a friendly name for Flatpak/Snap/AppImage wrappers.
        Honours the curated _KNOWN_APP_IDS table so common apps like
        Spotify stop being rendered as 'audio-src'."""
        if not pid:
            return None
        env = self._read_proc_env(pid)

        # 1. Flatpak: FLATPAK_ID env var, or /.flatpak-info's `name=` line.
        flatpak_id = env.get("FLATPAK_ID")
        if not flatpak_id:
            try:
                with open(f"/proc/{pid}/root/.flatpak-info", "r") as f:
                    for line in f:
                        line = line.strip()
                        if line.startswith("name=") or line.startswith("application="):
                            flatpak_id = line.split("=", 1)[1].strip()
                            break
            except OSError:
                pass
        if flatpak_id:
            return self._canonicalize_app_id(flatpak_id)

        # 2. Snap: SNAP_INSTANCE_NAME / SNAP_NAME env vars.
        snap_name = env.get("SNAP_INSTANCE_NAME") or env.get("SNAP_NAME")
        if snap_name:
            return snap_name.replace('-', ' ').replace('_', ' ').title()

        # 3. cgroup scopes for flatpak + snap + systemd-run bundles.
        cgroup = self._read_proc_cgroup(pid)
        m = re.search(r'app-flatpak-([A-Za-z0-9_.+-]+?)-\d+\.scope', cgroup)
        if m:
            return self._canonicalize_app_id(m.group(1))
        m = re.search(r'snap\.([A-Za-z0-9_-]+)', cgroup)
        if m:
            return m.group(1).replace('-', ' ').replace('_', ' ').title()
        m = re.search(r'app-([A-Za-z0-9_.+-]+?)\.slice', cgroup)
        if m:
            return self._canonicalize_app_id(m.group(1))

        # 4. Desktop-file id from GTK_APPLICATION_ID / etc.
        for env_key in ("GTK_APPLICATION_ID", "APP_ID", "XDG_CURRENT_DESKTOP_APP"):
            val = env.get(env_key)
            if val:
                return self._canonicalize_app_id(val)

        # 5. AppImage mounts show up under /tmp/.mount_… — use the basename
        # of the first cmdline arg (which is usually the mount path).
        cmdline = self._read_proc_cmdline(pid)
        if cmdline:
            first = cmdline[0]
            m = re.search(r'/tmp/\.mount_([^/]+)', first)
            if m:
                # .mount_SpotifyXXXX → Spotify
                stripped = re.sub(r'[A-Za-z0-9]{4,8}$', '', m.group(1)).rstrip('_-.')
                if stripped:
                    return stripped.replace('_', ' ').replace('-', ' ').title()

        return None

    # Common wrapper / launcher binaries that should be peeled off of an
    # `Exec=` line when looking up the *real* binary a .desktop entry runs.
    _EXEC_WRAPPERS = {
        'env', 'gtk-launch', 'flatpak', 'flatpak-spawn',
        'snap', 'snap-confine', 'sh', 'bash', 'zsh',
        'pkexec', 'sudo', 'gamemoderun', 'mangohud', 'optirun',
        'primusrun', 'prime-run', 'nice', 'taskset', 'systemd-run',
        'wine', 'wine64', 'wineserver',
    }

    @classmethod
    def _desktop_app_index(cls):
        """Index of every .desktop file's binary-basename → display Name.
        Lets us resolve native apps (AUR Spotify → 'Spotify') without a
        curated alias table. Cached for 60s."""
        now = time.time()
        cache = getattr(cls, '_desktop_cache', None)
        cache_at = getattr(cls, '_desktop_cache_at', 0)
        if cache is not None and (now - cache_at) < 60:
            return cache

        roots = [
            "/usr/share/applications",
            "/usr/local/share/applications",
            os.path.expanduser("~/.local/share/applications"),
            "/var/lib/flatpak/exports/share/applications",
            os.path.expanduser("~/.local/share/flatpak/exports/share/applications"),
        ]
        index = {}
        for root in roots:
            try:
                entries = os.listdir(root)
            except OSError:
                continue
            for entry in entries:
                if not entry.endswith('.desktop'):
                    continue
                path = os.path.join(root, entry)
                name, exec_line, no_display = cls._parse_desktop_file(path)
                if no_display or not name or not exec_line:
                    continue
                bin_name = cls._resolve_exec_binary(exec_line)
                if bin_name and bin_name.lower() not in index:
                    index[bin_name.lower()] = name
        cls._desktop_cache = index
        cls._desktop_cache_at = now
        return index

    @staticmethod
    def _parse_desktop_file(path):
        """Return (Name, Exec, NoDisplay) from the [Desktop Entry] section
        of a .desktop file.  Anything outside the main section is ignored."""
        name = None
        exec_line = None
        no_display = False
        try:
            with open(path, 'r', encoding='utf-8', errors='replace') as f:
                in_main = False
                for line in f:
                    s = line.strip()
                    if s.startswith('[') and s.endswith(']'):
                        in_main = (s == '[Desktop Entry]')
                        continue
                    if not in_main:
                        continue
                    if s.startswith('Name=') and name is None:
                        name = s.split('=', 1)[1].strip() or None
                    elif s.startswith('Exec=') and exec_line is None:
                        exec_line = s.split('=', 1)[1].strip() or None
                    elif s.startswith('NoDisplay='):
                        no_display = s.split('=', 1)[1].strip().lower() == 'true'
        except OSError:
            return None, None, False
        return name, exec_line, no_display

    @classmethod
    def _resolve_exec_binary(cls, exec_line):
        """Pick the actual program out of a freedesktop Exec= field.  Strips
        leading wrappers (`env VAR=val …`, `flatpak run --branch=… app`,
        `gamemoderun mangohud bin`) and returns the real binary's basename."""
        # Drop `%U` / `%F` style format placeholders before splitting.
        cleaned = re.sub(r'%[a-zA-Z]', '', exec_line).strip()
        if not cleaned:
            return None
        try:
            tokens = shlex.split(cleaned, posix=True)
        except ValueError:
            tokens = cleaned.split()
        i = 0
        while i < len(tokens):
            t = tokens[i]
            base = os.path.basename(t).lower()
            # Wrapper itself — skip past, then past any flags/env-vars.
            if base in cls._EXEC_WRAPPERS:
                i += 1
                while i < len(tokens) and (
                    tokens[i].startswith('-')
                    or '=' in tokens[i]  # env-style "VAR=value"
                ):
                    i += 1
                continue
            # `flatpak run com.spotify.Client` style — already past wrappers.
            return os.path.basename(t)
        return None

    # Directory patterns that map a binary's install location to a game /
    # app title. Each tuple is (compiled-regex, "human-readable shape"). The
    # first capture group is the title to surface. Order matters — Steam
    # patterns win over the generic "/Games/<title>" fallback.
    _PATH_TITLE_PATTERNS = (
        # Steam — both stock layout and SteamLibrary-on-secondary-disk.
        re.compile(r'/[Ss]team(?:[Ll]ibrary)?/steamapps/common/([^/]+)/'),
        re.compile(r'/\.steam/[^/]+/steamapps/common/([^/]+)/'),
        re.compile(r'/SteamApps/common/([^/]+)/', re.IGNORECASE),
        # Heroic / Lutris / Bottles — Wine prefixes have a `drive_c` root.
        re.compile(r'/drive_c/(?:Program Files(?: \(x86\))?|Games|GOG Games)/([^/]+)/', re.IGNORECASE),
        # Itch / GOG / Lutris generic install dirs.
        re.compile(r'/(?:Games|GOG Games|gog-games|itch|Lutris/games)/([^/]+)/', re.IGNORECASE),
        # /opt/<App>/<bin> — many vendor packages install this way.
        re.compile(r'^/opt/([^/]+)/'),
    )

    def _infer_name_from_exe(self, pid, current_name=None):
        """Use the install-path directory name when comm is a generic
        launcher (e.g. 'aces' for War Thunder, 'launcher' for many games)
        but the exe path contains the real title."""
        if not pid:
            return None
        try:
            exe = os.readlink(f"/proc/{pid}/exe")
        except OSError:
            exe = ""
        # Wine / Proton games run as /usr/bin/wine64-preloader → check the
        # cmdline for the actual .exe path too.
        cmdline = self._read_proc_cmdline(pid)
        haystacks = [exe]
        if cmdline:
            haystacks.extend(cmdline)
        for hs in haystacks:
            if not hs:
                continue
            for pattern in self._PATH_TITLE_PATTERNS:
                m = pattern.search(hs)
                if m:
                    title = m.group(1).strip()
                    # Skip obvious noise — empty, dotted (e.g. ".cache"), or
                    # the same as the current name (no upgrade).
                    if not title or title.startswith('.'):
                        continue
                    if current_name and title.lower() == current_name.lower():
                        continue
                    return title
        return None

    def _identify_via_desktop(self, pid):
        """Best-effort: resolve the running PID to a .desktop entry's Name=
        by looking at /proc/<pid>/exe and /proc/<pid>/comm."""
        if not pid:
            return None
        index = self._desktop_app_index() or {}
        # Try the resolved binary path first (handles `/usr/bin/spotify` →
        # 'spotify' even when comm reports something different).
        candidates = []
        try:
            exe = os.readlink(f"/proc/{pid}/exe")
            if exe:
                candidates.append(os.path.basename(exe).lower())
        except OSError:
            pass
        try:
            with open(f"/proc/{pid}/comm", "r") as f:
                comm = f.read().strip().lower()
                if comm:
                    candidates.append(comm)
        except OSError:
            pass
        cmdline = self._read_proc_cmdline(pid)
        if cmdline:
            candidates.append(os.path.basename(cmdline[0]).lower())
        for c in candidates:
            # Binary display table wins over .desktop lookup for well-known
            # browser/Electron apps whose renderer comm matches the main binary.
            # Run this BEFORE the empty-index short-circuit so a missing
            # .desktop index doesn't skip the curated mappings.
            if c in self._BINARY_DISPLAY_NAMES:
                return self._BINARY_DISPLAY_NAMES[c]
            if c in index:
                return index[c]
        return None

    # Chromium-renderer and Electron-renderer processes report the parent
    # binary name as comm. Map the binary stem directly to a display name
    # so we don't surface raw comm strings like "brave-browser" in the UI.
    _BINARY_DISPLAY_NAMES = {
        # Brave channels (binary name, comm, and icon_name variants)
        "brave": "Brave",
        "brave-browser": "Brave",
        "brave-browser-stable": "Brave",
        "brave-browser-beta": "Brave Beta",
        "brave-browser-nightly": "Brave Nightly",
        "brave-browser-origin": "Brave Origin Beta",
        "brave-origin": "Brave Origin Beta",
        # Chrome / Chromium
        "google-chrome": "Chrome",
        "google-chrome-stable": "Chrome",
        "google-chrome-beta": "Chrome Beta",
        "google-chrome-unstable": "Chrome Dev",
        "chromium-browser": "Chromium",
        # Firefox variants
        "firefox": "Firefox",
        "firefox-esr": "Firefox ESR",
        "firefox-developer-edition": "Firefox Dev",
        "librewolf": "LibreWolf",
        # Multi-app wrappers
        "ferdium": "Ferdium",
        "ferdi": "Ferdi",
        "hamsket": "Hamsket",
        # Spotify
        "spotify": "Spotify",
        # Common media players
        "vlc": "VLC",
        "mpv": "mpv",
        "rhythmbox": "Rhythmbox",
        "clementine": "Clementine",
        "strawberry": "Strawberry",
        "elisa": "Elisa",
        # Communication
        "discord": "Discord",
        "vesktop": "Vesktop",
        "webcord": "WebCord",
        "signal-desktop": "Signal",
        "telegram-desktop": "Telegram",
        "slack": "Slack",
        # Streaming / video
        "obs": "OBS Studio",
        "obs-studio": "OBS Studio",
        "zoom": "Zoom",
    }
    _BINARY_ICON_NAMES = {
        # Browsers / wrappers
        "brave": "brave-desktop",
        "brave-browser": "brave-desktop",
        "brave-browser-stable": "brave-desktop",
        "brave-browser-beta": "brave-browser-beta",
        "brave-browser-nightly": "brave-browser-nightly",
        "brave-browser-origin": "brave-browser-origin",
        "brave-origin": "brave-browser-origin",
        "google-chrome": "google-chrome",
        "google-chrome-stable": "google-chrome",
        "google-chrome-beta": "google-chrome-beta",
        "google-chrome-unstable": "google-chrome-unstable",
        "chromium": "chromium",
        "chromium-browser": "chromium-browser",
        "firefox": "firefox",
        "firefox-esr": "firefox-esr",
        "firefox-developer-edition": "firefox-developer-edition",
        "librewolf": "librewolf",
        "ferdium": "ferdium",
        "ferdi": "ferdi",
        "hamsket": "hamsket",
        # Media
        "spotify": "spotify",
        "vlc": "vlc",
        "mpv": "mpv",
        "rhythmbox": "rhythmbox",
        "clementine": "clementine",
        "strawberry": "strawberry",
        "elisa": "elisa",
        # Communication
        "discord": "discord",
        "vesktop": "vesktop",
        "webcord": "webcord",
        "signal-desktop": "signal-desktop",
        "telegram-desktop": "telegram-desktop",
        "slack": "slack",
        # Streaming / video
        "obs": "obs",
        "obs-studio": "obs-studio",
        "zoom": "Zoom",
    }

    _MULTIPROCESS_CHILD_BINARIES = {
        "chrome",
        "chromium",
        "chromium-browser",
        "firefox",
        "firefox-bin",
        "renderer",
        "zygote",
        "utility",
        "gpu-process",
        "plugin-host",
        "webkitwebprocess",
    }
    _WINDOW_IDENTITY_KEYS = (
        "application.id",
        "pipewire.access.portal.app_id",
        "xdg.portal.app_id",
        "window.app_id",
        "window.x11.wm_class",
        "window.x11.instance",
        "window.class",
        "application.icon_name",
    )
    _TEXT_IDENTITY_KEYS = (
        "application.display_name",
        "application.name",
        "node.description",
        "node.nick",
        "node.name",
        "media.name",
    )
    _WINDOW_TITLE_KEYS = (
        "window.title",
        "window.name",
        "media.title",
    )

    @classmethod
    def _normalize_app_route_token(cls, value):
        if value is None:
            return ""
        return re.sub(r'[^a-z0-9._:+-]+', '-', str(value).strip().lower()).strip('-')

    @classmethod
    def _append_icon_candidate(cls, out, seen, value):
        token = str(value or "").strip()
        if not token:
            return

        def add(candidate):
            candidate = str(candidate or "").strip()
            if not candidate or candidate in seen:
                return
            seen.add(candidate)
            out.append(candidate)

        add(token)
        low = token.lower()
        add(low)
        mapped = cls._BINARY_ICON_NAMES.get(low)
        if mapped:
            add(mapped)
        normalized = re.sub(r'[^a-z0-9.+-]+', '-', low).strip('-')
        if normalized:
            add(normalized)
            dotted = normalized.replace('.', '-')
            if dotted != normalized:
                add(dotted)
            parts = [part for part in re.split(r'[._-]+', normalized) if part]
            if parts:
                add(parts[-1])
            if len(parts) >= 2:
                add(f"{parts[-2]}-{parts[-1]}")
            if len(parts) >= 3:
                add(f"{parts[-3]}-{parts[-2]}-{parts[-1]}")

    @classmethod
    def theme_icon_candidates_for_app_id(cls, app_id, fallback_name=None):
        candidates = []
        seen = set()
        raw = str(app_id or "").strip()
        if raw:
            cls._append_icon_candidate(candidates, seen, raw)
            if ":" in raw:
                _, raw_token = raw.split(":", 1)
                cls._append_icon_candidate(candidates, seen, raw_token)
        if fallback_name:
            cls._append_icon_candidate(candidates, seen, fallback_name)
        return candidates

    def _app_icon_candidates(self, current, *, app_id="", resolved_app_id="",
                             app_name="", resolved_app_name=""):
        candidates = []
        seen = set()
        for extra in (
            app_id,
            resolved_app_id,
            app_name,
            resolved_app_name,
        ):
            for candidate in self.theme_icon_candidates_for_app_id(extra):
                if candidate not in seen:
                    seen.add(candidate)
                    candidates.append(candidate)
        for key in (
            "application.icon_name",
            "application.id",
            "pipewire.access.portal.app_id",
            "xdg.portal.app_id",
            "window.app_id",
            "window.x11.wm_class",
            "window.x11.instance",
            "window.class",
            "application.process.binary",
            "binary",
            "application.name",
        ):
            self._append_icon_candidate(candidates, seen, current.get(key))
        return candidates

    @classmethod
    def _make_app_route_key(cls, prefix, value):
        token = cls._normalize_app_route_token(value)
        if not token:
            return None
        return f"{prefix}:{token}"

    @classmethod
    def _sanitize_app_label(cls, value):
        if value is None:
            return None
        label = str(value).strip()
        if not label:
            return None
        mapped = cls._BINARY_DISPLAY_NAMES.get(label.lower())
        if mapped:
            return mapped
        if '.' in label and ' ' not in label and len(label.split('.')) >= 2:
            known = cls._KNOWN_APP_IDS.get(label.lower())
            if known:
                return known
        label = label.replace('_', ' ').replace('-', ' ').strip()
        if label and label.islower():
            return label.title()
        return label

    @classmethod
    def display_name_for_app_id(cls, app_id):
        if not app_id:
            return "Unknown App"
        if app_id == cls.SYSTEM_SOUNDS_BUCKET:
            return cls.SYSTEM_SOUNDS_BUCKET
        if ':' not in app_id:
            return cls._sanitize_app_label(app_id) or app_id
        kind, raw = app_id.split(':', 1)
        if kind == "app":
            return cls._canonicalize_app_id(raw) or cls._sanitize_app_label(raw) or raw
        if kind == "snap":
            return raw.replace('-', ' ').replace('_', ' ').title()
        if kind == "stream":
            return f"Audio Stream #{raw}"
        return cls._sanitize_app_label(raw) or raw.replace('.', ' ').strip() or app_id

    @classmethod
    def is_legacy_stream_label(cls, value):
        return isinstance(value, str) and value.startswith(("Media Stream #", "Audio Stream #"))

    @classmethod
    def is_persistent_app_id(cls, app_id):
        if not app_id:
            return False
        if app_id == cls.SYSTEM_SOUNDS_BUCKET:
            return True
        if cls.is_legacy_stream_label(app_id):
            return False
        return not str(app_id).startswith("stream:")

    @classmethod
    def _normalize_identity_override_map(cls, raw):
        if not isinstance(raw, dict):
            return {}
        cleaned = {}
        for source_app_id, target_app_id in raw.items():
            if not isinstance(source_app_id, str) or not isinstance(target_app_id, str):
                continue
            source_app_id = source_app_id.strip()
            target_app_id = target_app_id.strip()
            if not source_app_id or not target_app_id:
                continue
            if source_app_id == cls.SYSTEM_SOUNDS_BUCKET or target_app_id == cls.SYSTEM_SOUNDS_BUCKET:
                continue
            if not cls.is_persistent_app_id(source_app_id):
                continue
            if not cls.is_persistent_app_id(target_app_id):
                continue
            cleaned[source_app_id] = target_app_id
        return cleaned

    @classmethod
    def _normalize_label_override_map(cls, raw):
        if not isinstance(raw, dict):
            return {}
        cleaned = {}
        for app_id, label in raw.items():
            if not isinstance(app_id, str):
                continue
            app_id = app_id.strip()
            if not app_id or not cls.is_persistent_app_id(app_id):
                continue
            if app_id == cls.SYSTEM_SOUNDS_BUCKET:
                continue
            normalized = cls._sanitize_app_label(label) if label is not None else None
            if not normalized:
                continue
            cleaned[app_id] = normalized
        return cleaned

    def set_app_identity_overrides(self, overrides, labels):
        self._app_identity_overrides = self._normalize_identity_override_map(overrides)
        self._app_identity_label_overrides = self._normalize_label_override_map(labels)

    def _override_display_name_for_app_id(self, app_id, fallback=None):
        label = getattr(self, "_app_identity_label_overrides", {}).get(app_id)
        if label:
            return label
        if fallback:
            return fallback
        return self.display_name_for_app_id(app_id)

    @staticmethod
    def _proc_exe_basename(pid):
        """Resolve /proc/<pid>/exe to its basename. Unlike /proc/<pid>/comm
        this is NOT truncated to 15 chars, so it preserves long binary
        names like 'brave-browser-origin' which comm truncates to
        'brave-browser-o'."""
        if not pid:
            return ''
        try:
            exe = os.readlink(f"/proc/{pid}/exe")
        except OSError:
            return ''
        return os.path.basename(exe) if exe else ''

    @staticmethod
    def _proc_comm(pid):
        if not pid:
            return ''
        try:
            with open(f"/proc/{pid}/comm", "r") as f:
                return f.read().strip()
        except OSError:
            return ''

    @staticmethod
    def _parent_pid(pid):
        try:
            with open(f"/proc/{pid}/status", "r") as f:
                for line in f:
                    if line.startswith("PPid:"):
                        return line.split()[1]
        except OSError:
            return None
        return None

    def _pid_lineage(self, pid, limit=10):
        if not pid:
            return []
        out = []
        seen = set()
        cur = str(pid)
        for _ in range(limit):
            if not cur or cur in seen or cur == "0":
                break
            seen.add(cur)
            out.append(cur)
            cur = self._parent_pid(cur)
        return out

    @staticmethod
    def _split_identity_tokens(raw):
        if raw is None:
            return []
        text = str(raw).strip()
        if not text:
            return []
        parts = [text]
        parts.extend(p.strip() for p in re.split(r'[;,]', text) if p.strip())
        out = []
        seen = set()
        for part in parts:
            low = part.lower()
            if low in seen:
                continue
            seen.add(low)
            out.append(part)
        return out

    @classmethod
    def _identity_candidate(cls, app_id, display_name, score, source):
        if not app_id or not display_name:
            return None
        return {
            "app_id": app_id,
            "app_name": display_name,
            "score": int(score),
            "source": source,
        }

    @classmethod
    def _candidate_from_raw(cls, prefix, raw_value, display_name, score, source):
        app_id = cls._make_app_route_key(prefix, raw_value)
        label = display_name or cls.display_name_for_app_id(app_id)
        return cls._identity_candidate(app_id, label, score, source)

    def _stream_identity_candidate(self, current, display_name, score, source):
        stream_id = (
            current.get("node.id")
            or current.get("index")
            or current.get("pid")
            or current.get("application.process.id")
        )
        if not stream_id:
            return None
        return self._identity_candidate(
            self._make_app_route_key("stream", stream_id),
            display_name,
            score,
            source,
        )

    def _window_title_identity_label(self, raw):
        title = str(raw).strip()
        if not title:
            return None
        lowered = title.lower()
        if re.search(r'https?://|www\.|[a-z0-9-]+\.(?:com|org|net|io|gg|tv)\b', lowered):
            return None
        if any(sep in title for sep in (" - ", " | ", " — ", " :: ", " • ")):
            return None
        label = self._sanitize_app_label(title)
        if not label or self._is_generic_name(label) or self.name_matches_host(label):
            return None
        if len(label) > 80:
            return None
        return label

    def _generic_title_context(self, current):
        for key in ("application.id", "window.app_id", "application.display_name", "application.name"):
            raw = (current.get(key) or "").strip()
            if raw and not self._is_generic_name(raw) and not self.name_matches_host(raw):
                return False
        for key in ("application.process.binary", "node.name", "media.name"):
            raw = (current.get(key) or "").strip().lower()
            if not raw:
                continue
            norm = self._normalize_app_name(raw)
            if (
                self._is_generic_name(raw)
                or raw in self._MULTIPROCESS_CHILD_BINARIES
                or raw in {"chrome", "chromium", "chromium-browser", "electron", "wine", "wine64", "launcher", "helper"}
                or norm in {"chrome", "chromium", "electron", "renderer", "utility", "plugin host", "audio stream", "unknown"}
            ):
                return True
        return False

    def _app_name_from_pid(self, pid):
        """Best-effort process-name lookup, skipping wrapper binaries.

        Tries the exe symlink first (full binary name, no comm truncation),
        then falls back to comm and the ppid chain. Each candidate is run
        through `_BINARY_DISPLAY_NAMES` so e.g. 'brave-browser-origin' →
        'Brave Origin Beta' instead of leaking the raw binary name."""
        if not pid:
            return None
        exe_base = self._proc_exe_basename(pid)
        try:
            with open(f"/proc/{pid}/comm", "r") as f:
                comm = f.read().strip()
        except OSError:
            comm = ""

        wrapper_set = {"bwrap", "flatpak", "snap", "snap-confine", "bash", "sh",
                       "python", "python3", "wine", "wine64", "wineserver"}

        # Try exe basename first since it's not truncated.
        for cand in (exe_base, comm):
            cl = cand.lower() if cand else ''
            if cl and cl in self._BINARY_DISPLAY_NAMES:
                return self._BINARY_DISPLAY_NAMES[cl]

        # Prefer the exe basename over comm when both are non-wrapper —
        # exe is the authoritative binary name.
        if exe_base and exe_base.lower() not in wrapper_set:
            return exe_base
        if comm and comm.lower() not in wrapper_set:
            return comm

        # Walk up the ppid chain for a non-wrapper parent.
        seen = set()
        cur = pid
        for _ in range(6):
            try:
                with open(f"/proc/{cur}/status", "r") as f:
                    ppid = None
                    for line in f:
                        if line.startswith("PPid:"):
                            ppid = line.split()[1]
                            break
            except OSError:
                return comm or exe_base or None
            if not ppid or ppid in seen or ppid == "0":
                return comm or exe_base or None
            seen.add(ppid)
            parent_exe = self._proc_exe_basename(ppid)
            try:
                with open(f"/proc/{ppid}/comm", "r") as f:
                    parent_comm = f.read().strip()
            except OSError:
                parent_comm = ''
            for cand in (parent_exe, parent_comm):
                cl = cand.lower() if cand else ''
                if cl and cl in self._BINARY_DISPLAY_NAMES:
                    return self._BINARY_DISPLAY_NAMES[cl]
            if parent_exe and parent_exe.lower() not in wrapper_set:
                return parent_exe
            if parent_comm and parent_comm.lower() not in wrapper_set:
                return parent_comm
            cur = ppid
        return comm or exe_base or None

    SYSTEM_SOUNDS_BUCKET = "System Sounds"

    @staticmethod
    def _is_system_sound_stream(current):
        """Detect notification / event / alert streams. These get bucketed
        under a single 'System Sounds' entry so the user can route every
        ding-and-bong to one channel."""
        media_role = (current.get('media.role') or '').lower()
        if media_role in ('event', 'notification', 'phone-notification', 'phone',
                          'alert', 'production'):
            return True
        binary = (current.get('application.process.binary') or '').lower()
        if binary in {'canberra-gtk-play', 'canberra-gtk-module', 'paplay',
                      'aplay', 'speaker-test', 'notify-send', 'kdialog',
                      'kdedialog', 'plasma-pa'}:
            return True
        app_name = (current.get('application.name') or '').lower()
        if app_name in {'libcanberra', 'canberra', 'plasma-pa',
                        'speech-dispatcher', 'org.freedesktop.notifications',
                        'plasma-pulseaudio', 'plasmashell', 'kded', 'kded5',
                        'kded6', 'org.kde.plasmashell', 'org.kde.kded'}:
            return True
        node_name = (current.get('node.name') or '').lower()
        if 'canberra' in node_name or 'notification' in node_name:
            return True
        return False

    def _resolve_via_gio_env(self, pid):
        """The KDE/GNOME-blessed way to identify an app from a process:
        walk the ppid chain looking for the GIO_LAUNCHED_DESKTOP_FILE env
        var. When set, it points at the .desktop file the user launched,
        and its Name= field is exactly what KDE shows in the taskbar.
        Returns the friendly name or None."""
        if not pid:
            return None
        seen = set()
        cur = str(pid)
        for _ in range(10):
            if cur in seen or cur in ('0', ''):
                break
            seen.add(cur)
            env = self._read_proc_env(cur)
            desktop_path = env.get("GIO_LAUNCHED_DESKTOP_FILE")
            if desktop_path and os.path.isfile(desktop_path):
                name, _e, _h = self._parse_desktop_file(desktop_path)
                if name:
                    return name
            try:
                with open(f"/proc/{cur}/status", "r") as f:
                    ppid = None
                    for line in f:
                        if line.startswith("PPid:"):
                            ppid = line.split()[1]
                            break
            except OSError:
                return None
            if not ppid or ppid == "0":
                break
            cur = ppid
        return None

    def _gio_identity_candidate(self, pid):
        for depth, cur in enumerate(self._pid_lineage(pid)):
            env = self._read_proc_env(cur)
            desktop_path = env.get("GIO_LAUNCHED_DESKTOP_FILE")
            if not desktop_path or not os.path.isfile(desktop_path):
                continue
            name, _exec, _hidden = self._parse_desktop_file(desktop_path)
            if not name:
                continue
            desktop_id = os.path.splitext(os.path.basename(desktop_path))[0]
            return self._candidate_from_raw(
                "desktop",
                desktop_id,
                name,
                130 - depth,
                "gio-desktop",
            )
        return None

    def _sandbox_identity_candidate(self, pid):
        if not pid:
            return None
        for depth, cur in enumerate(self._pid_lineage(pid)):
            env = self._read_proc_env(cur)
            flatpak_id = env.get("FLATPAK_ID")
            if not flatpak_id:
                try:
                    with open(f"/proc/{cur}/root/.flatpak-info", "r") as f:
                        for line in f:
                            line = line.strip()
                            if line.startswith("name=") or line.startswith("application="):
                                flatpak_id = line.split("=", 1)[1].strip()
                                break
                except OSError:
                    pass
            if flatpak_id:
                return self._candidate_from_raw(
                    "app",
                    flatpak_id,
                    self._canonicalize_app_id(flatpak_id),
                    126 - depth,
                    "flatpak",
                )

            snap_name = env.get("SNAP_INSTANCE_NAME") or env.get("SNAP_NAME")
            if snap_name:
                return self._candidate_from_raw(
                    "snap",
                    snap_name,
                    snap_name.replace('-', ' ').replace('_', ' ').title(),
                    122 - depth,
                    "snap-env",
                )

            cgroup = self._read_proc_cgroup(cur)
            matchers = (
                (r'app-flatpak-([A-Za-z0-9_.+-]+?)-\d+\.scope', "app", self._canonicalize_app_id, 124, "flatpak-cgroup"),
                (r'app-([A-Za-z0-9_.+-]+?)\.slice', "app", self._canonicalize_app_id, 116, "app-slice"),
                (r'snap\.([A-Za-z0-9_-]+)', "snap", lambda v: v.replace('-', ' ').replace('_', ' ').title(), 114, "snap-cgroup"),
            )
            for pattern, prefix, display_fn, score, source in matchers:
                match = re.search(pattern, cgroup)
                if not match:
                    continue
                raw = match.group(1)
                return self._candidate_from_raw(
                    prefix,
                    raw,
                    display_fn(raw),
                    score - depth,
                    source,
                )

            for env_key in ("GTK_APPLICATION_ID", "APP_ID", "XDG_CURRENT_DESKTOP_APP"):
                raw = env.get(env_key)
                if raw:
                    return self._candidate_from_raw(
                        "app",
                        raw,
                        self._canonicalize_app_id(raw),
                        118 - depth,
                        env_key.lower(),
                    )

            cmdline = self._read_proc_cmdline(cur)
            if cmdline:
                for idx, token in enumerate(cmdline[:-1]):
                    if os.path.basename(token).lower() == "flatpak":
                        candidate = cmdline[idx + 1]
                        if candidate == "run" and idx + 2 < len(cmdline):
                            candidate = cmdline[idx + 2]
                        if '.' in candidate and not candidate.startswith('-'):
                            return self._candidate_from_raw(
                                "app",
                                candidate,
                                self._canonicalize_app_id(candidate),
                                112 - depth,
                                "flatpak-cmdline",
                            )
                mount_match = re.search(r'/tmp/\.mount_([^/]+)', cmdline[0])
                if mount_match:
                    stripped = re.sub(r'[A-Za-z0-9]{4,8}$', '', mount_match.group(1)).rstrip('_-.')
                    if stripped:
                        return self._candidate_from_raw(
                            "path",
                            stripped,
                            stripped.replace('_', ' ').replace('-', ' ').title(),
                            92 - depth,
                            "appimage",
                        )
        return None

    def _window_identity_candidates(self, current):
        candidates = []
        for key in self._WINDOW_IDENTITY_KEYS:
            raw = current.get(key)
            if not raw:
                continue
            if key in {"application.id", "pipewire.access.portal.app_id", "xdg.portal.app_id"}:
                base_score = 116
            elif key == "window.app_id":
                base_score = 108
            else:
                base_score = 96
            for token in self._split_identity_tokens(raw):
                low = token.lower()
                if '.' in token:
                    candidates.append(self._candidate_from_raw(
                        "app",
                        token,
                        self._canonicalize_app_id(token),
                        base_score,
                        key,
                    ))
                mapped = self._BINARY_DISPLAY_NAMES.get(low)
                if mapped:
                    candidates.append(self._candidate_from_raw(
                        "binary",
                        token,
                        mapped,
                        base_score - 2,
                        key,
                    ))
                elif not self._is_generic_name(token) and not self.name_matches_host(token):
                    candidates.append(self._candidate_from_raw(
                        "wmclass",
                        token,
                        self._sanitize_app_label(token),
                        base_score - 10,
                        key,
                    ))
        return [candidate for candidate in candidates if candidate]

    def _binary_identity_candidates(self, pid, current):
        candidates = []
        current_binary = (current.get('application.process.binary') or '').strip()
        if current_binary:
            mapped = self._BINARY_DISPLAY_NAMES.get(current_binary.lower())
            if mapped:
                candidates.append(self._candidate_from_raw(
                    "binary",
                    current_binary,
                    mapped,
                    88,
                    "application.process.binary",
                ))
            elif not self._is_generic_name(current_binary):
                candidates.append(self._candidate_from_raw(
                    "binary",
                    current_binary,
                    current_binary,
                    70,
                    "application.process.binary",
                ))

        index = self._desktop_app_index() or {}
        wrapper_set = self._EXEC_WRAPPERS | {
            "bwrap", "python", "python3", "flatpak", "snap", "snap-confine",
        }
        for depth, cur in enumerate(self._pid_lineage(pid)):
            raw_candidates = [
                self._proc_exe_basename(cur),
                self._proc_comm(cur),
            ]
            cmdline = self._read_proc_cmdline(cur)
            if cmdline:
                raw_candidates.append(os.path.basename(cmdline[0]))
            seen = set()
            for raw in raw_candidates:
                if not raw:
                    continue
                low = raw.lower()
                if low in seen or low in wrapper_set:
                    continue
                seen.add(low)
                mapped = self._BINARY_DISPLAY_NAMES.get(low)
                score_penalty = depth * 2
                if low in self._MULTIPROCESS_CHILD_BINARIES:
                    score_penalty += 22
                if low in index:
                    candidates.append(self._candidate_from_raw(
                        "binary",
                        low,
                        index[low],
                        104 - score_penalty,
                        f"desktop-index:{depth}",
                    ))
                if mapped:
                    candidates.append(self._candidate_from_raw(
                        "binary",
                        low,
                        mapped,
                        100 - score_penalty,
                        f"binary-map:{depth}",
                    ))
                elif not self._is_generic_name(raw):
                    candidates.append(self._candidate_from_raw(
                        "binary",
                        low,
                        raw,
                        74 - score_penalty,
                        f"binary:{depth}",
                    ))
        return [candidate for candidate in candidates if candidate]

    def _cmdline_identity_candidates(self, pid):
        candidates = []
        if not pid:
            return candidates
        index = self._desktop_app_index() or {}
        wrapper_set = self._EXEC_WRAPPERS | {
            "bwrap", "python", "python3", "flatpak", "snap", "snap-confine",
        }
        for depth, cur in enumerate(self._pid_lineage(pid)):
            cmdline = self._read_proc_cmdline(cur)
            if not cmdline:
                continue
            seen = set()
            score_penalty = depth * 2
            for token in cmdline:
                if not token:
                    continue
                if token.startswith("--") and "=" in token:
                    flag, raw_value = token.split("=", 1)
                    value = raw_value.strip()
                    if not value:
                        continue
                    low_flag = flag.lower()
                    if low_flag in {"--class", "--name"} and not self._is_generic_name(value):
                        candidates.append(self._candidate_from_raw(
                            "wmclass" if low_flag == "--class" else "name",
                            value,
                            self._sanitize_app_label(value),
                            106 - score_penalty,
                            f"cmdline:{low_flag}",
                        ))
                        continue
                    if low_flag in {"--app", "--app-id"} and (
                        "." in value or value.lower() in self._KNOWN_APP_IDS
                    ):
                        candidates.append(self._candidate_from_raw(
                            "app",
                            value,
                            self._canonicalize_app_id(value),
                            112 - score_penalty,
                            f"cmdline:{low_flag}",
                        ))
                        continue
                if token.startswith("-"):
                    continue
                if "." in token and token.lower() in self._KNOWN_APP_IDS:
                    candidates.append(self._candidate_from_raw(
                        "app",
                        token,
                        self._canonicalize_app_id(token),
                        112 - score_penalty,
                        "cmdline-app-id",
                    ))
                base = os.path.basename(token).strip().lower()
                if not base or base in seen or base in wrapper_set:
                    continue
                seen.add(base)
                if base in index:
                    candidates.append(self._candidate_from_raw(
                        "binary",
                        base,
                        index[base],
                        108 - score_penalty,
                        f"cmdline-index:{depth}",
                    ))
                mapped = self._BINARY_DISPLAY_NAMES.get(base)
                if mapped:
                    child_penalty = 18 if base in self._MULTIPROCESS_CHILD_BINARIES else 0
                    candidates.append(self._candidate_from_raw(
                        "binary",
                        base,
                        mapped,
                        106 - score_penalty - child_penalty,
                        f"cmdline-binary:{depth}",
                    ))
                elif not self._is_generic_name(base):
                    candidates.append(self._candidate_from_raw(
                        "binary",
                        base,
                        base,
                        78 - score_penalty,
                        f"cmdline:{depth}",
                    ))
        return [candidate for candidate in candidates if candidate]

    def _path_identity_candidate(self, pid):
        for depth, cur in enumerate(self._pid_lineage(pid)):
            current_name = self._proc_exe_basename(cur) or self._proc_comm(cur)
            title = self._infer_name_from_exe(cur, current_name=current_name)
            if title and not self._is_generic_name(title) and not self.name_matches_host(title):
                return self._candidate_from_raw(
                    "path",
                    title,
                    title,
                    94 - depth,
                    "exe-path",
                )
        return None

    def _text_identity_candidates(self, current):
        candidates = []
        for key in self._TEXT_IDENTITY_KEYS:
            raw = (current.get(key) or '').strip()
            if not raw:
                continue
            if self.name_matches_host(raw):
                continue
            if key == "application.name":
                mapped = self._BINARY_DISPLAY_NAMES.get(raw.lower())
                if mapped:
                    candidates.append(self._candidate_from_raw(
                        "name",
                        raw,
                        mapped,
                        89 if not self._is_generic_name(raw) else 68,
                        key,
                    ))
            if self._is_generic_name(raw):
                continue
            if key == "application.display_name":
                score = 92
            elif key == "application.name":
                score = 89
            elif key.startswith("application."):
                score = 72
            else:
                score = 56
            candidates.append(self._candidate_from_raw(
                "name",
                raw,
                self._sanitize_app_label(raw),
                score,
                key,
            ))
        title_score = 90 if self._generic_title_context(current) else 72
        for key in self._WINDOW_TITLE_KEYS:
            raw = (current.get(key) or '').strip()
            if not raw:
                continue
            label = self._window_title_identity_label(raw)
            if not label:
                continue
            candidate = self._stream_identity_candidate(
                current,
                label,
                title_score - (6 if key == "media.title" else 0),
                key,
            )
            if candidate:
                candidates.append(candidate)
        return [candidate for candidate in candidates if candidate]

    def _stream_fallback_identity(self, current):
        stream_id = str(current.get('node.id') or current.get('index') or current.get('pid') or '?')
        display = None
        for key in ("application.name", "node.description", "node.name", "media.name"):
            raw = (current.get(key) or '').strip()
            if raw and not self.name_matches_host(raw):
                display = self._sanitize_app_label(raw)
                break
        if not display:
            display = f"Audio Stream #{stream_id}"
        return {
            "app_id": self._make_app_route_key("stream", stream_id),
            "app_name": display,
            "resolved_app_id": self._make_app_route_key("stream", stream_id),
            "resolved_app_name": display,
            "source": "fallback",
            "override_applied": False,
        }

    def _prefer_specific_identity_candidate(self, candidates, best):
        if not best or not self._is_generic_name(best.get("app_name")):
            return best
        alternatives = [
            candidate for candidate in candidates
            if candidate
            and not self._is_generic_name(candidate.get("app_name"))
            and not self.name_matches_host(candidate.get("app_name"))
        ]
        if not alternatives:
            return best
        alternative = max(alternatives, key=lambda item: (item["score"], len(item["app_name"])))
        if alternative["score"] >= best["score"] - 14:
            return alternative
        return best

    @staticmethod
    def _candidate_source_preference(candidate):
        source = str((candidate or {}).get("source") or "")
        if source in {"gio-desktop", "flatpak", "snap-env", "flatpak-cgroup", "snap-cgroup"}:
            return 5
        if source.startswith(("cmdline:--app", "cmdline:--app-id")):
            return 5
        if source.startswith(("desktop-index:", "cmdline-index:", "cmdline-binary:", "appimage")):
            return 4
        if source in {"exe-path"} or source.startswith(("flatpak-cmdline", "gtk_application_id", "app_id", "xdg_current_desktop_app")):
            return 4
        if source.startswith(("application.id", "pipewire.access.portal.app_id", "xdg.portal.app_id", "window.app_id")):
            return 4
        if source.startswith(("window.x11.", "window.class", "wmclass")):
            return 3
        if source.startswith(("application.display_name", "application.name", "node.description", "node.nick")):
            return 2
        if source.startswith(("application.process.binary", "binary-map:", "binary:", "name")):
            return 1
        return 0

    def _prefer_wrapper_identity_candidate(self, candidates, best):
        if not best:
            return best
        best_source_pref = self._candidate_source_preference(best)
        best_is_generic = self._is_generic_name(best.get("app_name"))
        alternatives = [
            candidate for candidate in candidates
            if candidate
            and not self._is_generic_name(candidate.get("app_name"))
            and not self.name_matches_host(candidate.get("app_name"))
            and self._candidate_source_preference(candidate) >= 3
        ]
        if not alternatives:
            return best
        alternative = max(
            alternatives,
            key=lambda item: (
                self._candidate_source_preference(item),
                item["score"],
                len(item["app_name"]),
            ),
        )
        alt_source_pref = self._candidate_source_preference(alternative)
        if alt_source_pref > best_source_pref and alternative["score"] >= best["score"] - 18:
            return alternative
        if best_is_generic and alt_source_pref >= best_source_pref and alternative["score"] >= best["score"] - 18:
            return alternative
        return best

    def _apply_identity_override(self, identity):
        if not isinstance(identity, dict):
            return identity
        resolved_app_id = str(identity.get("app_id") or "").strip()
        resolved_app_name = str(identity.get("app_name") or "").strip()
        source = str(identity.get("source") or "").strip()
        if not resolved_app_id:
            return identity
        target_app_id = getattr(self, "_app_identity_overrides", {}).get(
            resolved_app_id,
            resolved_app_id,
        )
        override_applied = target_app_id != resolved_app_id
        if override_applied:
            display_name = self._override_display_name_for_app_id(target_app_id)
        else:
            display_name = self._override_display_name_for_app_id(
                target_app_id,
                fallback=resolved_app_name,
            )
        return {
            "app_id": target_app_id,
            "app_name": display_name or resolved_app_name or self.display_name_for_app_id(target_app_id),
            "resolved_app_id": resolved_app_id,
            "resolved_app_name": resolved_app_name or self.display_name_for_app_id(resolved_app_id),
            "source": source,
            "override_applied": override_applied,
        }

    def _resolve_app_identity(self, current):
        if self._is_system_sound_stream(current):
            return {
                "app_id": self.SYSTEM_SOUNDS_BUCKET,
                "app_name": self.SYSTEM_SOUNDS_BUCKET,
                "resolved_app_id": self.SYSTEM_SOUNDS_BUCKET,
                "resolved_app_name": self.SYSTEM_SOUNDS_BUCKET,
                "source": "system-sounds",
                "override_applied": False,
            }

        pid = current.get('pid') or current.get('application.process.id')
        candidates = []
        for candidate in (
            self._gio_identity_candidate(pid),
            self._sandbox_identity_candidate(pid),
            self._path_identity_candidate(pid),
        ):
            if candidate:
                candidates.append(candidate)
        candidates.extend(self._cmdline_identity_candidates(pid))
        candidates.extend(self._window_identity_candidates(current))
        candidates.extend(self._binary_identity_candidates(pid, current))
        candidates.extend(self._text_identity_candidates(current))

        best_by_id = {}
        for candidate in candidates:
            if not candidate:
                continue
            key = candidate["app_id"]
            existing = best_by_id.get(key)
            rank = (candidate["score"], len(candidate["app_name"]))
            if existing is None or rank > (existing["score"], len(existing["app_name"])):
                best_by_id[key] = candidate

        if not best_by_id:
            return self._stream_fallback_identity(current)

        best = max(best_by_id.values(), key=lambda item: (item["score"], len(item["app_name"])))
        best = self._prefer_wrapper_identity_candidate(candidates, best)
        best = self._prefer_specific_identity_candidate(candidates, best)
        return self._apply_identity_override({
            "app_id": best["app_id"],
            "app_name": best["app_name"],
            "source": best["source"],
        })

    def _process_sink_input(self, current, entries, sink_id_to_name):
        # Resolve sink name
        sink_id = current.get('sink_id')
        current['sink'] = sink_id_to_name.get(sink_id, sink_id)

        # Drop our own internals — but never drop the apps playing to them.
        node_name = (current.get('node.name') or '').lower()
        media_name = (current.get('media.name') or '').lower()
        if any(t in node_name for t in
               ('wavelinux_mix', 'wavelinux_src', 'wavelinux.fx',
                'rnnoise', 'loopback')):
            return
        if 'wavelinux_mix' in media_name:
            return

        identity = self._resolve_app_identity(current)
        current['app_id'] = identity["app_id"]
        current['app_name'] = identity["app_name"] or "Unknown App"
        current['resolved_app_id'] = identity.get("resolved_app_id") or current['app_id']
        current['resolved_app_name'] = identity.get("resolved_app_name") or current['app_name']
        current['app_identity_source'] = identity.get("source", "")
        current['app_identity_override_applied'] = bool(identity.get("override_applied"))
        current['app_icon_candidates'] = self._app_icon_candidates(
            current,
            app_id=current['app_id'],
            resolved_app_id=current['resolved_app_id'],
            app_name=current['app_name'],
            resolved_app_name=current['resolved_app_name'],
        )
        current['_is_system_sound'] = current['app_id'] == self.SYSTEM_SOUNDS_BUCKET
        entries.append(current)

    # All volume writes clamp to this — PipeWire allows 1.5 (150%) but
    # that audibly clips, so we cap at unity everywhere.
    MAX_VOLUME = 1.0

    def _clamp(self, volume):
        try:
            return max(0.0, min(float(volume), self.MAX_VOLUME))
        except (TypeError, ValueError):
            return 1.0

    def move_app_to_sink(self, sink_input_index, sink_name):
        """Move a sink-input to `sink_name`. None means System Default."""
        if sink_name is None:
            sink_name = self.get_default_sink()
        if not sink_name:
            return
        self._run(['pactl', 'move-sink-input', str(sink_input_index), sink_name])

    def set_sink_input_volume(self, sink_input_index, volume):
        """App-stream volume. Uses pactl (sink-input indices); wpctl wants
        numeric PipeWire node IDs which don't match here."""
        pct = max(0, min(int(round(self._clamp(volume) * 100)), 100))
        self._run(['pactl', 'set-sink-input-volume', str(sink_input_index), f'{pct}%'])

    def get_sink_input_volume(self, sink_input_index):
        """Return 0..1.0 for the given sink-input."""
        out = self._run(['pactl', 'list', 'sink-inputs'])
        if not out:
            return 1.0
        target = f'Sink Input #{sink_input_index}'
        seen = False
        for line in out.splitlines():
            stripped = line.strip()
            if stripped.startswith('Sink Input #'):
                seen = stripped == target
            elif seen and stripped.startswith('Volume:'):
                m = re.search(r'/\s*(\d+)%', stripped)
                if m:
                    try:
                        return int(m.group(1)) / 100.0
                    except ValueError:
                        pass
                return 1.0
        return 1.0

    def get_all_sinks(self, snap=None):
        if snap is not None and hasattr(snap, "sinks"):
            return snap.sinks
        return self._parse_short_sinks()

    def get_sink_description(self, sink_name, snap=None):
        """The user-facing Description field from `pactl list sinks`
        ('Sony WH-1000XM4' for a paired BT headset, for example).
        Returns None when we have to fall back to name-based naming."""
        if snap is None:
            text = self._run(['pactl', 'list', 'sinks']) or ''
            return self._parse_sink_descriptions(text).get(sink_name)
        if snap._sink_descriptions is None:
            snap._sink_descriptions = self._parse_sink_descriptions(snap.sinks_text)
        return snap._sink_descriptions.get(sink_name)

    def display_name_for_sink(self, sink_name, snap=None):
        """Best human-readable label for a sink: prefer the PipeWire
        Description field (has model names like 'Sony WH-1000XM4'); fall
        back to the cleaned-up node.name when there's no description."""
        desc = self.get_sink_description(sink_name, snap=snap)
        if desc:
            cleaned = self.friendly_name(desc)
            if cleaned and cleaned != "Unknown":
                return cleaned
        return self.friendly_name(sink_name)

    # ── Wave Link-parity helpers ───────────────────────────────────

    def set_input_gain(self, node_id, volume):
        """Pre-fader channel gain for mics / virtual sinks (0.0..1.0)."""
        self._run(['wpctl', 'set-volume', str(node_id), f'{self._clamp(volume):.2f}'])

    def unroute_mix_from_hardware(self, mix_name):
        """Remove any hardware loopback for the named mix, so 'None' in the
        combo actually disconnects the bus."""
        changed = False
        for key in list(self.loopback_modules.keys()):
            if key.startswith(mix_name + '->'):
                self._run(['pactl', 'unload-module', str(self.loopback_modules[key])])
                del self.loopback_modules[key]
                changed = True
        mix = self.output_mixes.get(mix_name)
        if mix:
            mix.hardware_output = None
        return changed

    # ── Card / profile switching ───────────────────────────────────

    def list_cards(self):
        """Return [{name, description, active_profile, profiles:[{name,description,available}]}]
        for each ALSA card PipeWire knows about."""
        out = self._run(['pactl', 'list', 'cards'])
        if not out:
            return []
        cards = []
        curr = None
        section = None  # 'profiles' | None
        for raw in out.splitlines():
            line = raw.rstrip()
            stripped = line.strip()
            if stripped.startswith('Card #'):
                if curr is not None:
                    cards.append(curr)
                curr = {
                    'name': '', 'description': '', 'active_profile': '',
                    'profiles': [],
                }
                section = None
                continue
            if curr is None:
                continue
            if stripped.startswith('Name:'):
                curr['name'] = stripped.split(':', 1)[1].strip()
            elif stripped.startswith('Active Profile:'):
                curr['active_profile'] = stripped.split(':', 1)[1].strip()
            elif stripped.startswith('device.description ='):
                curr['description'] = stripped.split('=', 1)[1].strip().strip('"')
            elif stripped.startswith('Profiles:'):
                section = 'profiles'
            elif section == 'profiles' and line.startswith('\t\t'):
                # "\t\tprofile_name: Friendly Description (sinks: 1, sources: 1, priority: 7538, available: yes)"
                entry = stripped
                if ':' in entry:
                    pname, rest = entry.split(':', 1)
                    avail = 'available: yes' in rest or 'available: unknown' in rest
                    # Everything before the final "(...)" block is the description.
                    desc = rest.strip()
                    lparen = desc.rfind('(')
                    if lparen >= 0:
                        desc = desc[:lparen].strip()
                    curr['profiles'].append({
                        'name': pname.strip(),
                        'description': desc or pname.strip(),
                        'available': avail,
                    })
            elif stripped.startswith(('Ports:', 'Sinks:', 'Sources:', 'Properties:')):
                section = None
        if curr is not None:
            cards.append(curr)
        return cards

    def set_card_profile(self, card_name, profile_name):
        # The trailing `or True` was unconditionally turning every result
        # into success — masking real `pactl set-card-profile` failures
        # (e.g. unavailable profile). Return what `_run` actually says.
        return self._run(['pactl', 'set-card-profile', card_name, profile_name]) is not None

    # ── Rename ─────────────────────────────────────────────────────

    def rename_virtual_sink(self, old_sink_name, new_display_name):
        """Destroy the user virtual sink and re-create it under a new name.
        Returns the new sink_name (e.g. 'wavelinux_voice_chat') or None."""
        if not old_sink_name.startswith('wavelinux_'):
            return None
        display_clean, safe_tail = self._sanitize_channel_name(new_display_name)
        new_sink_name = f"wavelinux_{safe_tail}"
        if new_sink_name == old_sink_name:
            return old_sink_name
        # Unload the old sink (drops its loopbacks too).
        self.remove_virtual_sink(old_sink_name)
        if self.create_virtual_sink(new_display_name) is None:
            return None
        return new_sink_name

    # ── Effects / RNNoise ──────────────────────────────────────────
    #
    # `pipewire -c <conf>` runs a fresh pipewire instance with `conf` as
    # its COMPLETE config (no merge with the system default). For its
    # filter-chain nodes to register with the running daemon, the conf
    # must (1) set `core.daemon = false`, (2) load the audioconvert/
    # support SPA libs, (3) load rt/protocol-native/client-node/adapter
    # modules, and (4) load `libpipewire-module-filter-chain` last with
    # our graph. See `_fx_client_config`.

    @staticmethod
    def _fx_client_config(client_id, filter_chain_args):
        """Build a `pipewire -c` config that runs as a client of the
        running daemon and loads one `libpipewire-module-filter-chain`.

        Module list and SPA libs match upstream's canonical
        `filter-chain.conf`. Do NOT add `libpipewire-module-metadata` —
        on some setups it ENOENTs and aborts the whole client."""
        return f"""\
context.properties = {{
    core.daemon = false
    core.name   = wavelinux-fx-{client_id}
    log.level   = 2
}}

context.spa-libs = {{
    audio.convert.* = audioconvert/libspa-audioconvert
    support.*       = support/libspa-support
}}

context.modules = [
    {{ name = libpipewire-module-rt
        args = {{ nice.level = -11 }}
        flags = [ ifexists nofail ]
    }}
    {{ name = libpipewire-module-protocol-native }}
    {{ name = libpipewire-module-client-node }}
    {{ name = libpipewire-module-adapter }}
    {{ name = libpipewire-module-filter-chain
        flags = [ nofail ]
        args = {filter_chain_args}
    }}
]
"""

    # Stub kept so an unupgraded checkout doesn't NameError on import.
    _FX_PREAMBLE = ""

    @staticmethod
    def _fx_log_path(channel_key, effect_id):
        log_dir = os.path.expanduser('~/.config/wavelinux/fx-logs')
        os.makedirs(log_dir, exist_ok=True)
        return os.path.join(log_dir, f'{effect_id}-{channel_key}.log')

    def _spawn_fx(self, config_path, log_path, key):
        """Spawn `pipewire -c <config>`. Returns True if the process is
        still alive after a 1.5s settle window (parked in
        `rnnoise_processes[key]` for later teardown), False otherwise.

        1.5s is empirical — typical filter-chain spawn is ~250ms; the
        headroom covers BT-audio wakeup and NUMA-pinned RT scheduling.
        Logs a debug header (config, env, pipewire version) so post-
        mortem on a failed spawn just needs `cat <log>`."""
        # Read config back from disk so the header is the FILE we ran.
        try:
            with open(config_path, 'r') as cf:
                rendered_config = cf.read()
        except OSError:
            rendered_config = '<read failed>'
        try:
            pw_ver = subprocess.run(
                ['pipewire', '--version'], capture_output=True,
                text=True, timeout=2,
            ).stdout.strip() or 'unknown'
        except Exception:
            pw_ver = 'unknown'
        spawn_env = self._pipewire_spawn_env()
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
            log_file = open(log_path, 'wb')
        except OSError as e:
            logging.error(f"Could not open FX log file {log_path}: {e}")
            return False

        # Close log_file in the parent once Popen has dup'd the FD into
        # the child — otherwise long FX-edit sessions leak FDs to EMFILE.
        proc = None
        try:
            log_file.write(header.encode('utf-8'))
            log_file.flush()
            proc = subprocess.Popen(
                ['pipewire', '-c', config_path],
                stdout=log_file, stderr=log_file,
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
            # Still running = success.
            self.rnnoise_processes[key] = proc
            return True
        logging.error(f"FX process for {key} exited immediately; see {log_path}")
        return False

    def start_rnnoise(self, channel_key='default', params=None):
        """Single-effect rnnoise spawn. Reachable via `apply_effect`;
        new code should go through `set_channel_fx` for proper chain routing."""
        # If a previous spawn for this key is still alive, terminate it
        # first — `_spawn_fx` blindly overwrites `rnnoise_processes[key]`
        # otherwise, orphaning the old `pipewire -c` process and leaking
        # its filter-chain virtual nodes into the audio graph.
        if self.rnnoise_processes.get(channel_key) is not None:
            self.stop_rnnoise(channel_key)
        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)
        config_path = os.path.join(config_dir, f'wavelinux-rnnoise-{channel_key}.conf')
        values = self._resolved_params('rnnoise', params)
        filter_graph = self._build_filter_graph('rnnoise', values)
        if filter_graph is None:
            return False
        client_id = re.sub(r'[^A-Za-z0-9]+', '-', f'rnnoise-{channel_key}').strip('-') or 'rnnoise'
        filter_chain_args = f"""{{
            node.description = "WaveLinux-Denoise ({channel_key})"
            media.name       = "WaveLinux-Denoise ({channel_key})"
            filter.graph = {{{filter_graph}
            }}
            capture.props = {{
                node.name    = "wavelinux.rnnoise.{channel_key}.capture"
                media.class  = Audio/Sink
                audio.rate   = 48000
            }}
            playback.props = {{
                node.name    = "wavelinux.rnnoise.{channel_key}.source"
                media.class  = Audio/Source
                audio.rate   = 48000
            }}
        }}"""
        config = self._fx_client_config(client_id, filter_chain_args)
        with open(config_path, 'w') as f:
            f.write(config)
        return self._spawn_fx(config_path, self._fx_log_path(channel_key, 'rnnoise'), channel_key)

    def stop_rnnoise(self, channel_key='default'):
        proc = self.rnnoise_processes.get(channel_key)
        if proc:
            try:
                proc.send_signal(signal.SIGTERM)
                proc.wait(timeout=3)
            except Exception:
                # SIGTERM didn't take or wait timed out — force-kill and
                # reap so we don't leak a zombie. Without this final
                # wait(), the child sits in the kernel proc table until
                # Popen's __del__ happens to run.
                proc.kill()
                try:
                    proc.wait(timeout=1)
                except Exception:
                    pass
            del self.rnnoise_processes[channel_key]
            return True
        return False

    def is_rnnoise_active(self, channel_key='default'):
        proc = self.rnnoise_processes.get(channel_key)
        return proc is not None and proc.poll() is None

    @property
    def rnnoise_active(self):
        return any(p.poll() is None for p in self.rnnoise_processes.values())

    # ── Built-in Effects via PipeWire filter-chain ─────────────────

    _AVAILABLE_EFFECTS = (
        {'id': 'rnnoise', 'name': 'Noise Suppression', 'icon': '🎙️',
         'desc': 'AI-powered background noise removal'},
        {'id': 'highpass', 'name': 'High-Pass Filter', 'icon': '🎵',
         'desc': 'Roll off low rumble (fans, handling noise)'},
        {'id': 'eq', 'name': '3-Band EQ', 'icon': '🎚️',
         'desc': 'Shape tone with low shelf / mid peak / high shelf'},
        {'id': 'compressor', 'name': 'Compressor', 'icon': '📉',
         'desc': 'Smooth out loud/quiet differences'},
        {'id': 'gate', 'name': 'Noise Gate', 'icon': '🚪',
         'desc': 'Cut audio below a threshold'},
        {'id': 'limiter', 'name': 'Limiter', 'icon': '🛡️',
         'desc': 'Prevent audio clipping'},
    )

    @classmethod
    def get_available_effects(cls):
        return cls._AVAILABLE_EFFECTS

    # Parameter descriptors for the FX UI. Each entry is
    # (pactl_control_key, display_label, min, max, default, suffix).
    _EFFECT_PARAMS = {
        'rnnoise': [
            ('VAD Threshold (%)', 'VAD Threshold', 0.0, 100.0, 50.0, '%'),
        ],
        'highpass': [
            ('Freq', 'Cutoff', 20.0, 500.0, 80.0, ' Hz'),
        ],
        'eq': [
            ('Low Freq', 'Low Freq', 40.0, 400.0, 120.0, ' Hz'),
            ('Low Gain', 'Low Gain', -12.0, 12.0, 0.0, ' dB'),
            ('Mid Freq', 'Mid Freq', 300.0, 4000.0, 1000.0, ' Hz'),
            ('Mid Gain', 'Mid Gain', -12.0, 12.0, 0.0, ' dB'),
            ('High Freq', 'High Freq', 2000.0, 12000.0, 6000.0, ' Hz'),
            ('High Gain', 'High Gain', -12.0, 12.0, 0.0, ' dB'),
        ],
        'compressor': [
            # Keys must match sc4m_1916's LADSPA control-port names
            # verbatim — PipeWire's filter-graph does exact-string matching.
            # load_config in main.py migrates pre-rewrite snake_case keys.
            ('Threshold level (dB)', 'Threshold', -60.0, 0.0, -20.0, ' dB'),
            ('Ratio (1:n)',          'Ratio',     1.0,   20.0,  4.0,  ':1'),
            ('Attack time (ms)',     'Attack',    0.1,   200.0, 5.0,  ' ms'),
            ('Release time (ms)',    'Release',   5.0,   1000.0,100.0,' ms'),
            ('Makeup gain (dB)',     'Makeup',    0.0,   24.0,  0.0,  ' dB'),
        ],
        'gate': [
            ('Threshold (dB)', 'Threshold', -80.0, 0.0, -40.0, ' dB'),
            ('Attack (ms)', 'Attack', 0.1, 100.0, 2.5, ' ms'),
            ('Hold (ms)', 'Hold', 0.0, 500.0, 10.0, ' ms'),
            ('Decay (ms)', 'Release', 10.0, 2000.0, 200.0, ' ms'),
            ('Range (dB)', 'Range', -80.0, 0.0, -40.0, ' dB'),
        ],
        'limiter': [
            ('Input gain (dB)', 'Input Gain', -20.0, 20.0, 0.0, ' dB'),
            ('Limit (dB)', 'Ceiling', -20.0, 0.0, -1.0, ' dB'),
        ],
    }

    @classmethod
    def get_effect_params(cls, effect_id):
        return list(cls._EFFECT_PARAMS.get(effect_id, []))

    # Plain-English description of what each effect does, shown in the
    # FX dialog so the user isn't guessing what 'VAD' or 'Makeup' means.
    _EFFECT_HELP = {
        'rnnoise':
            "AI-powered noise suppression. Removes steady background noise "
            "(fans, keyboard, street). VAD threshold controls how aggressive "
            "it is — higher numbers cut more but risk chopping quiet speech.",
        'highpass':
            "Rolls off low-frequency rumble below the cutoff. 80 Hz is a "
            "safe default for voice; push to 100–120 Hz for very rumbly "
            "rooms, drop to 40–60 Hz for music or deep voices.",
        'eq':
            "Three-band tone shaping. Low shelf warms or thins the bass, "
            "mid peak carves out muddiness or adds presence around 1–3 kHz, "
            "high shelf brightens or tames sibilance.",
        'compressor':
            "Evens out loud vs. quiet moments. Threshold is where it starts "
            "working, ratio is how hard it clamps (4:1 is a solid broadcast "
            "setting), makeup brings the level back up afterwards.",
        'gate':
            "Silences the channel when it's below the threshold. Useful on "
            "mics to kill room tone between words. Range is how much to "
            "attenuate when closed; too strong makes breaths choppy.",
        'limiter':
            "A brick-wall ceiling on the signal so nothing clips. Leave "
            "'Ceiling' at -1 dB for broadcast. Bump 'Input Gain' if your "
            "mic is quiet and you want it to ride harder against the ceiling.",
    }

    # Short, labeled preset bundles for each effect. These are all safe
    # starting points, not magic values — users are expected to tweak.
    _EFFECT_PRESETS = {
        'rnnoise': [
            ("Gentle",     {"VAD Threshold (%)": 25.0}),
            ("Broadcast",  {"VAD Threshold (%)": 50.0}),
            ("Aggressive", {"VAD Threshold (%)": 75.0}),
        ],
        'highpass': [
            ("Voice 80 Hz",  {"Freq":  80.0}),
            ("Rumble 120 Hz", {"Freq": 120.0}),
            ("Music 40 Hz",  {"Freq":  40.0}),
        ],
        'eq': [
            ("Flat",
             {"Low Freq": 120.0, "Low Gain": 0.0,
              "Mid Freq": 1000.0, "Mid Gain": 0.0,
              "High Freq": 6000.0, "High Gain": 0.0}),
            ("Broadcast Voice",
             {"Low Freq": 120.0, "Low Gain": -2.0,
              "Mid Freq": 2500.0, "Mid Gain": 2.0,
              "High Freq": 8000.0, "High Gain": 1.5}),
            ("Warm Music",
             {"Low Freq": 100.0, "Low Gain": 2.0,
              "Mid Freq": 800.0, "Mid Gain": -1.0,
              "High Freq": 10000.0, "High Gain": 2.0}),
        ],
        'compressor': [
            ("Gentle 2:1",
             {"Threshold level (dB)": -20.0, "Ratio (1:n)": 2.0,
              "Attack time (ms)": 10.0, "Release time (ms)": 120.0,
              "Makeup gain (dB)": 2.0}),
            ("Broadcast 4:1",
             {"Threshold level (dB)": -18.0, "Ratio (1:n)": 4.0,
              "Attack time (ms)": 5.0, "Release time (ms)": 100.0,
              "Makeup gain (dB)": 3.0}),
            ("Streaming 6:1",
             {"Threshold level (dB)": -16.0, "Ratio (1:n)": 6.0,
              "Attack time (ms)": 3.0, "Release time (ms)": 80.0,
              "Makeup gain (dB)": 4.0}),
        ],
        'gate': [
            ("Soft -60 dB",
             {"Threshold (dB)": -60.0, "Range (dB)": -20.0,
              "Attack (ms)": 5.0, "Hold (ms)": 20.0, "Decay (ms)": 200.0}),
            ("Room mic -40 dB",
             {"Threshold (dB)": -40.0, "Range (dB)": -40.0,
              "Attack (ms)": 2.5, "Hold (ms)": 10.0, "Decay (ms)": 120.0}),
            ("Noisy mic -30 dB",
             {"Threshold (dB)": -30.0, "Range (dB)": -50.0,
              "Attack (ms)": 1.0, "Hold (ms)": 10.0, "Decay (ms)": 80.0}),
        ],
        'limiter': [
            ("Gentle -3 dB",
             {"Input gain (dB)": 0.0, "Limit (dB)": -3.0}),
            ("Broadcast -1 dB",
             {"Input gain (dB)": 0.0, "Limit (dB)": -1.0}),
            ("Loud -0.5 dB",
             {"Input gain (dB)": 3.0, "Limit (dB)": -0.5}),
        ],
    }

    @classmethod
    def get_effect_help(cls, effect_id):
        return cls._EFFECT_HELP.get(effect_id, "")

    @classmethod
    def get_effect_presets(cls, effect_id):
        return list(cls._EFFECT_PRESETS.get(effect_id, []))

    def _resolved_params(self, effect_id, overrides):
        """Merge overrides on top of `_EFFECT_PARAMS` defaults, clamped
        to each entry's declared min/max range. UI sliders already clamp,
        but a hand-edited config.json could otherwise smuggle in values
        the LADSPA plugin would reject."""
        ranges = {key: (mn, mx) for (key, _l, mn, mx, _d, _u)
                  in self._EFFECT_PARAMS.get(effect_id, [])}
        out = {key: default for (key, _l, _mn, _mx, default, _u)
               in self._EFFECT_PARAMS.get(effect_id, [])}
        if overrides:
            for k, v in overrides.items():
                if k in out:
                    try:
                        v = float(v)
                    except (TypeError, ValueError):
                        continue
                    mn, mx = ranges[k]
                    out[k] = max(mn, min(mx, v))
        return out

    @staticmethod
    def _render_control_block(params):
        """Emit filter-chain control block: `control = { "Key" = val ... }`."""
        lines = []
        for k, v in params.items():
            lines.append(f'                            "{k}" = {float(v):.3f}')
        body = "\n".join(lines)
        return f"                        control = {{\n{body}\n                        }}"

    def _ladspa_node(self, name, plugin, label, values):
        """Render a single LADSPA node block for filter.graph. Uses the
        absolute .so path when we can find one — falls back to the bare
        plugin name (pipewire resolves it via $LADSPA_PATH)."""
        path = self.ladspa_plugin_path(plugin) or plugin
        return f"""
                nodes = [
                    {{
                        type   = ladspa
                        name   = {name}
                        plugin = "{path}"
                        label  = {label}
{self._render_control_block(values)}
                    }}
                ]
                inputs  = [ "{name}:Input" ]
                outputs = [ "{name}:Output" ]
"""

    def _build_filter_graph(self, effect_id, values):
        """Render the `filter.graph` body for a single effect. Returns
        None for unknown ids. Used by `apply_effect` (single-effect spawn);
        the unified per-channel chain uses `_effect_stage_blocks` instead."""
        if effect_id == 'rnnoise':
            return self._ladspa_node('rnnoise', 'librnnoise_ladspa',
                                     'noise_suppressor_mono', values)
        if effect_id == 'highpass':
            # PipeWire's native biquad — no LADSPA needed.
            return f"""
                nodes = [
                    {{
                        type  = builtin
                        name  = highpass
                        label = bq_highpass
{self._render_control_block(values)}
                    }}
                ]
                inputs  = [ "highpass:In" ]
                outputs = [ "highpass:Out" ]
"""
        if effect_id == 'eq':
            # Three-stage biquad chain: low shelf → mid peaking → high shelf.
            return f"""
                nodes = [
                    {{
                        type  = builtin
                        name  = eq_low
                        label = bq_lowshelf
                        control = {{
                            "Freq" = {float(values.get('Low Freq', 120.0)):.2f}
                            "Q"    = 0.707
                            "Gain" = {float(values.get('Low Gain', 0.0)):.2f}
                        }}
                    }}
                    {{
                        type  = builtin
                        name  = eq_mid
                        label = bq_peaking
                        control = {{
                            "Freq" = {float(values.get('Mid Freq', 1000.0)):.2f}
                            "Q"    = 1.0
                            "Gain" = {float(values.get('Mid Gain', 0.0)):.2f}
                        }}
                    }}
                    {{
                        type  = builtin
                        name  = eq_high
                        label = bq_highshelf
                        control = {{
                            "Freq" = {float(values.get('High Freq', 6000.0)):.2f}
                            "Q"    = 0.707
                            "Gain" = {float(values.get('High Gain', 0.0)):.2f}
                        }}
                    }}
                ]
                links = [
                    {{ output = "eq_low:Out"  input = "eq_mid:In"  }}
                    {{ output = "eq_mid:Out"  input = "eq_high:In" }}
                ]
                inputs  = [ "eq_low:In" ]
                outputs = [ "eq_high:Out" ]
"""
        if effect_id == 'gate':
            return self._ladspa_node('gate', 'gate_1410', 'gate', values)
        if effect_id == 'compressor':
            return self._ladspa_node('compressor', 'sc4m_1916', 'sc4m', values)
        if effect_id == 'limiter':
            # `linear` applies input gain, `clamp` brick-walls at the ceiling.
            # Mono-native builtins — chosen over `fast_lookahead_limiter_1913`
            # which is a stereo LADSPA plugin and surfaced a phantom
            # self-monitor via WirePlumber auto-routing in this graph.
            ceiling_db = float(values.get('Limit (dB)', -1.0))
            input_db   = float(values.get('Input gain (dB)', 0.0))
            ceiling = max(0.0001, min(1.0, 10 ** (ceiling_db / 20.0)))
            in_gain = 10 ** (input_db / 20.0)
            return f"""
                nodes = [
                    {{
                        type  = builtin
                        name  = lim_in
                        label = linear
                        control = {{ "Mult" = {in_gain:.4f} "Add" = 0.0 }}
                    }}
                    {{
                        type  = builtin
                        name  = lim_out
                        label = clamp
                        control = {{ "Min" = {-ceiling:.4f} "Max" = {ceiling:.4f} }}
                    }}
                ]
                links = [
                    {{ output = "lim_in:Out" input = "lim_out:In" }}
                ]
                inputs  = [ "lim_in:In" ]
                outputs = [ "lim_out:Out" ]
"""
        return None

    def apply_effect(self, channel_key, effect_id, params=None):
        """Spawn a single-effect filter-chain in isolation. Per-channel
        mic effects should go through `set_channel_fx` instead — that
        actually routes the mic through the chain. This is just a
        free-floating filter-chain."""
        if effect_id == 'rnnoise':
            return self.start_rnnoise(channel_key, params=params)

        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)
        values = self._resolved_params(effect_id, params)
        filter_graph = self._build_filter_graph(effect_id, values)
        if filter_graph is None:
            return False

        config_path = os.path.join(config_dir, f'wavelinux-fx-{channel_key}-{effect_id}.conf')
        client_id = re.sub(r'[^A-Za-z0-9]+', '-',
                           f'{effect_id}-{channel_key}').strip('-') or effect_id
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
        config = self._fx_client_config(client_id, filter_chain_args)
        with open(config_path, 'w') as f:
            f.write(config)
        key = f'{channel_key}_{effect_id}'
        # Same orphan-process guard as start_rnnoise — `_spawn_fx`
        # overwrites `rnnoise_processes[key]` blindly.
        if self.rnnoise_processes.get(key) is not None:
            self.stop_rnnoise(key)
        return self._spawn_fx(config_path, self._fx_log_path(channel_key, effect_id), key)

    def remove_effect(self, channel_key, effect_id):
        if effect_id == 'rnnoise':
            return self.stop_rnnoise(channel_key)
        key = f'{channel_key}_{effect_id}'
        return self.stop_rnnoise(key)

    def is_effect_active(self, channel_key, effect_id):
        if effect_id == 'rnnoise':
            return self.is_rnnoise_active(channel_key)
        key = f'{channel_key}_{effect_id}'
        proc = self.rnnoise_processes.get(key)
        return proc is not None and proc.poll() is None

    # ── Chain API (per-channel FX bus) ─────────────────────────────
    #
    # `set_channel_fx` builds ONE `pipewire -c` filter-chain process per
    # channel, with every enabled effect as a node in a single
    # `filter.graph` block. The chain's input sink takes audio from the
    # mic via a `module-loopback`; the submix loopbacks pull from the
    # chain's output source.

    # Canonical signal-flow order. Effects toggled on are sorted into
    # this order regardless of how the user enabled them.
    _CHAIN_ORDER = ('rnnoise', 'highpass', 'eq', 'compressor', 'gate', 'limiter')

    @classmethod
    def _ordered_chain(cls, effects):
        """Sort an effect list by the canonical signal-flow order so that
        e.g. denoise always runs before EQ regardless of toggle order."""
        rank = {fid: i for i, fid in enumerate(cls._CHAIN_ORDER)}
        return sorted(effects, key=lambda fid: (rank.get(fid, len(rank)), fid))

    @staticmethod
    def _safe_channel_key(node_name):
        """node.name → safe identifier usable in pipewire node names and
        on-disk paths. 'alsa_input.usb-Foo_Bar.analog-stereo' → 'alsa_input_usb_foo_bar_analog_stereo'."""
        cleaned = re.sub(r'[^A-Za-z0-9]+', '_', (node_name or '').lower()).strip('_')
        return cleaned or 'chan'

    # ── Unified per-channel filter-chain ──────────────────────────────
    #
    # Layout inspired by EasyEffects (GPL-3.0,
    # https://github.com/wwmm/easyeffects). PipeWire reference:
    # https://docs.pipewire.org/page_module_filter_chain.html

    def _effect_stage_blocks(self, effect_id, values, stage_idx):
        """Building blocks for one effect inside the unified chain.

        Returns (nodes_text, internal_links, exit_port, entry_port) or
        (None, None, None, None) for unknown / unavailable effects.
        Multi-node effects (e.g. the 3-band EQ) namespace internal node
        names with an `s<idx>_` prefix to avoid collisions across stages."""
        prefix = f's{stage_idx}_'

        if effect_id == 'rnnoise':
            # LADSPA port names are matched exactly by filter-graph;
            # noise_suppressor_mono exposes "Input" / "Output".
            path = self.ladspa_plugin_path('librnnoise_ladspa') or 'librnnoise_ladspa'
            name = f'{prefix}rnnoise'
            nodes = f"""
                {{
                    type   = ladspa
                    name   = {name}
                    plugin = "{path}"
                    label  = noise_suppressor_mono
{self._render_control_block(values)}
                }}"""
            return nodes, [], f'{name}:Output', f'{name}:Input'

        if effect_id == 'highpass':
            name = f'{prefix}highpass'
            nodes = f"""
                {{
                    type  = builtin
                    name  = {name}
                    label = bq_highpass
{self._render_control_block(values)}
                }}"""
            return nodes, [], f'{name}:Out', f'{name}:In'

        if effect_id == 'eq':
            low, mid, high = f'{prefix}eq_low', f'{prefix}eq_mid', f'{prefix}eq_high'
            nodes = f"""
                {{
                    type  = builtin
                    name  = {low}
                    label = bq_lowshelf
                    control = {{
                        "Freq" = {float(values.get('Low Freq', 120.0)):.2f}
                        "Q"    = 0.707
                        "Gain" = {float(values.get('Low Gain', 0.0)):.2f}
                    }}
                }}
                {{
                    type  = builtin
                    name  = {mid}
                    label = bq_peaking
                    control = {{
                        "Freq" = {float(values.get('Mid Freq', 1000.0)):.2f}
                        "Q"    = 1.0
                        "Gain" = {float(values.get('Mid Gain', 0.0)):.2f}
                    }}
                }}
                {{
                    type  = builtin
                    name  = {high}
                    label = bq_highshelf
                    control = {{
                        "Freq" = {float(values.get('High Freq', 6000.0)):.2f}
                        "Q"    = 0.707
                        "Gain" = {float(values.get('High Gain', 0.0)):.2f}
                    }}
                }}"""
            internal = [
                f'{{ output = "{low}:Out"  input = "{mid}:In"  }}',
                f'{{ output = "{mid}:Out"  input = "{high}:In" }}',
            ]
            return nodes, internal, f'{high}:Out', f'{low}:In'

        if effect_id == 'compressor':
            # Mono sc4m (id 1916). The stereo sc4_1882 trips the same
            # auto-route-to-headphones bug the limiter used to have.
            # Audio ports on sc4m: "Input" / "Output".
            path = self.ladspa_plugin_path('sc4m_1916') or 'sc4m_1916'
            name = f'{prefix}compressor'
            nodes = f"""
                {{
                    type   = ladspa
                    name   = {name}
                    plugin = "{path}"
                    label  = sc4m
{self._render_control_block(values)}
                }}"""
            return nodes, [], f'{name}:Output', f'{name}:Input'

        if effect_id == 'gate':
            # gate_1410 is mono — audio ports are literally "Input" and
            # "Output" per the LADSPA descriptor.
            path = self.ladspa_plugin_path('gate_1410') or 'gate_1410'
            name = f'{prefix}gate'
            nodes = f"""
                {{
                    type   = ladspa
                    name   = {name}
                    plugin = "{path}"
                    label  = gate
{self._render_control_block(values)}
                }}"""
            return nodes, [], f'{name}:Output', f'{name}:Input'

        if effect_id == 'limiter':
            # Mono builtin pair: `linear` (input gain) → `clamp` (ceiling).
            # NOT `fast_lookahead_limiter_1913` — that's stereo and its
            # dangling outputs got auto-routed onto the default sink as a
            # phantom self-monitor. Trade-off: brick-wall clamp instead of
            # lookahead+release.
            ceiling_db = float(values.get('Limit (dB)', -1.0))
            input_db = float(values.get('Input gain (dB)', 0.0))
            ceiling = max(0.0001, min(1.0, 10 ** (ceiling_db / 20.0)))
            in_gain = 10 ** (input_db / 20.0)
            lin = f'{prefix}lim_in'
            clp = f'{prefix}lim_out'
            nodes = f"""
                {{
                    type  = builtin
                    name  = {lin}
                    label = linear
                    control = {{ "Mult" = {in_gain:.4f} "Add" = 0.0 }}
                }}
                {{
                    type  = builtin
                    name  = {clp}
                    label = clamp
                    control = {{ "Min" = {-ceiling:.4f} "Max" = {ceiling:.4f} }}
                }}"""
            internal = [f'{{ output = "{lin}:Out" input = "{clp}:In" }}']
            return nodes, internal, f'{clp}:Out', f'{lin}:In'

        return None, None, None, None

    def _build_unified_filter_graph(self, ordered_effects, params_map):
        """Render a filter.graph body suitable for runtime live updates."""
        all_nodes = []
        all_links = []
        first_entry = None
        prev_exit = None
        used_effects = []

        for stage_idx, effect_id in enumerate(ordered_effects):
            values = self._resolved_params(effect_id, params_map.get(effect_id))
            nodes_text, internal_links, exit_port, entry_port = \
                self._effect_stage_blocks(effect_id, values, stage_idx)
            if nodes_text is None:
                logging.warning(
                    f"Skipping unknown / unavailable effect {effect_id} "
                    "in live filter graph"
                )
                continue
            if first_entry is None:
                first_entry = entry_port
            all_nodes.append(nodes_text)
            all_links.extend(internal_links)
            if prev_exit is not None:
                all_links.append(
                    f'{{ output = "{prev_exit}" input = "{entry_port}" }}'
                )
            prev_exit = exit_port
            used_effects.append(effect_id)

        if not used_effects or first_entry is None or prev_exit is None:
            return None, []

        nodes_block = '\n'.join(all_nodes)
        links_block = '\n                    '.join(all_links) if all_links else ''
        graph = (
            "{\n"
            "    nodes = ["
            f"{nodes_block}\n"
            "    ]\n"
            "    links = [\n"
            f"        {links_block}\n"
            "    ]\n"
            f"    inputs = [ \"{first_entry}\" ]\n"
            f"    outputs = [ \"{prev_exit}\" ]\n"
            "}"
        )
        return graph, used_effects

    def _build_unified_chain_config(self, safe_key, ordered_effects, params_map, stamp=None):
        """Write the filter-chain config combining all enabled effects in
        one `filter.graph` with explicit inter-stage `links`. Returns
        (config_path, sink_name, source_name, used_effects) or
        (None, None, None, []) if nothing was renderable."""
        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)

        graph_text, used_effects = self._build_unified_filter_graph(
            ordered_effects,
            params_map,
        )
        if graph_text is None or not used_effects:
            return None, None, None, []

        stamp_str = f'.{stamp}' if stamp else ''
        sink_name = f'wavelinux.fx.{safe_key}{stamp_str}.input'
        source_name = f'wavelinux.fx.{safe_key}{stamp_str}.source'

        # `node.always-process` prevents the chain from suspending mid-
        # construction (which would race the upstream loopback's bind).
        # `audio.position = [ MONO ]` keeps the chain mono so stereo
        # plugins don't end up half-connected.
        # `node.virtual` + `priority.session = -1000` demote these nodes
        # in the system audio panels so they don't clutter device pickers.
        client_id = f'{safe_key}-chain-{stamp}' if stamp else f'{safe_key}-chain'
        filter_chain_args = f"""{{
            node.description = "_WaveLinux internal: chain ({safe_key})"
            node.nick        = "_WaveLinux-chain"
            media.name       = "_WaveLinux-chain ({safe_key})"
            node.virtual     = true
            priority.session = -1000
            priority.driver  = -1000
            filter.graph = {graph_text}
            capture.props = {{
                node.name           = "{sink_name}"
                node.description    = "_WaveLinux internal: chain input"
                node.nick           = "_WaveLinux-chain-in"
                media.class         = Audio/Sink
                node.virtual        = true
                priority.session    = -1000
                priority.driver     = -1000
                audio.rate          = 48000
                audio.position      = [ MONO ]
                node.always-process = true
            }}
            playback.props = {{
                node.name           = "{source_name}"
                node.description    = "_WaveLinux internal: chain output"
                node.nick           = "_WaveLinux-chain-out"
                media.class         = Audio/Source
                node.virtual        = true
                priority.session    = -1000
                priority.driver     = -1000
                audio.rate          = 48000
                audio.position      = [ MONO ]
                node.always-process = true
            }}
        }}"""
        config = self._fx_client_config(client_id, filter_chain_args)
        config_path = os.path.join(config_dir, f'wavelinux-chain-{safe_key}.conf')
        # Atomic write — a partial write would leave a broken config that
        # blocks the next spawn.
        tmp_path = config_path + '.tmp'
        with open(tmp_path, 'w') as f:
            f.write(config)
        os.replace(tmp_path, config_path)
        return config_path, sink_name, source_name, used_effects

    @staticmethod
    def _is_inline_fx_info(info):
        return bool(info and info.get('mode') == 'inline')

    def _find_inline_fx_target(self, node_name, snap=None):
        snap = snap or self.create_snapshot(force=True)
        for node in self.get_hardware_inputs(snap=snap):
            if node.name == node_name:
                return {
                    'node_id': str(node.pw_id),
                    'node_name': node.name,
                    'media_class': node.media_class,
                    'capture_target': node.name,
                    'source_name': node.name,
                }
        try:
            virtual_nodes = self.get_virtual_sinks(snap=snap)
        except AttributeError:
            virtual_nodes = []
        for node in virtual_nodes:
            if node.name == node_name:
                return {
                    'node_id': str(node.pw_id),
                    'node_name': node.name,
                    'media_class': node.media_class,
                    'capture_target': f"{node.name}.monitor",
                    'source_name': f"{node.name}.monitor",
                }
        return None

    def _set_node_filter_graph(self, node_id, graph_text):
        node_id = str(node_id or "").strip()
        if not node_id:
            return False
        graph_value = graph_text or ""
        param_text = (
            '{ params = [ '
            '"audioconvert.filter-graph.disable" false '
            '"audioconvert.filter-graph" '
            f'{json.dumps(graph_value)}'
            ' ] }'
        )
        out = self._run(['pw-cli', 's', node_id, 'Props', param_text], timeout=3)
        return out is not None

    def _apply_inline_channel_fx(self, node_name, capture_target, ordered, params_map):
        old_info = self.channel_fx.get(node_name) or {}
        if old_info and not self._is_inline_fx_info(old_info):
            return None
        target = self._find_inline_fx_target(node_name)
        if not target:
            return None
        graph_text, used_effects = self._build_unified_filter_graph(ordered, params_map)
        if graph_text is None or not used_effects:
            return self._fx_result(
                False,
                kept_source=target['source_name'],
                failure_stage='config_build',
                message='No renderable effects were available for this chain',
            )
        if not self._set_node_filter_graph(target['node_id'], graph_text):
            return self._fx_result(
                False,
                kept_source=target['source_name'],
                failure_stage='inline_set_param',
                message='PipeWire rejected the live filter-graph update',
            )
        self.channel_fx[node_name] = {
            'mode': 'inline',
            'effects': list(used_effects),
            'params': {
                fid: dict(params_map.get(fid, {}))
                for fid in used_effects
            },
            'procs': [],
            'loopbacks': [],
            'source': target['source_name'],
            'capture_target': capture_target or target['capture_target'],
            'safe_key': self._safe_channel_key(node_name),
            'node_id': target['node_id'],
        }
        self.invalidate_snapshot()
        return self._fx_result(
            True,
            active_source=target['source_name'],
            kept_source=target['source_name'],
            message='FX chain active',
        )

    def _clear_inline_channel_fx(self, node_name, info):
        target = self._find_inline_fx_target(node_name)
        node_id = (target or {}).get('node_id') or info.get('node_id')
        source_name = (target or {}).get('source_name') or info.get('source') or info.get('capture_target')
        if not self._set_node_filter_graph(node_id, ""):
            return self._fx_result(
                False,
                kept_source=source_name,
                active_source=source_name,
                rolled_back=True,
                failure_stage='inline_clear',
                message='PipeWire rejected the live filter-graph clear',
            )
        self.channel_fx.pop(node_name, None)
        self.invalidate_snapshot()
        return self._fx_result(
            True,
            kept_source=source_name,
            message='FX chain cleared',
        )

    def _fx_proxy_names(self, safe_key):
        return (
            f"wavelinux.fx.{safe_key}.sink",
            f"wavelinux.fx.{safe_key}.source",
        )

    def _ensure_fx_proxy(self, safe_key):
        """Create or reuse the stable internal source that external apps read."""
        sink_name, requested_source_name = self._fx_proxy_names(safe_key)
        sink_module_id = self._find_module_by_arg(f"sink_name={sink_name}")
        if sink_module_id is None:
            sink_module_id = self._run([
                'pactl', 'load-module', 'module-null-sink',
                f'sink_name={sink_name}',
                (
                    "sink_properties="
                    "device.description=_WaveLinux-FX-Sink "
                    "node.description=_WaveLinux-FX-Sink "
                    "node.nick=_WaveLinux-FX-Sink "
                    "media.name=_WaveLinux-FX-Sink "
                    "application.name=_WaveLinux-FX-Sink "
                    "media.class=Audio/Sink"
                ),
            ])
            if sink_module_id:
                self._run(['pactl', 'set-sink-mute', sink_name, '0'])
                self._run(['pactl', 'set-sink-volume', sink_name, '100%'])
                self.invalidate_snapshot()
        if sink_module_id is None:
            return None

        source_module_id = self._find_module_by_arg(f"source_name={requested_source_name}")
        if source_module_id is None:
            source_module_id = self._run([
                'pactl', 'load-module', 'module-virtual-source',
                f'source_name={requested_source_name}',
                f'master={sink_name}.monitor',
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
            ])
            if source_module_id:
                self.invalidate_snapshot()
        if source_module_id is None:
            if sink_module_id is not None:
                self._run(['pactl', 'unload-module', str(sink_module_id)])
            return None
        source_name = (
            self._wait_source_visible(requested_source_name, attempts=20, delay=0.05)
            or self.resolve_source_name(requested_source_name)
        )
        if not source_name:
            if source_module_id is not None:
                self._run(['pactl', 'unload-module', str(source_module_id)])
            if sink_module_id is not None:
                self._run(['pactl', 'unload-module', str(sink_module_id)])
            self.invalidate_snapshot()
            return None
        return {
            'sink_name': sink_name,
            'sink_module_id': str(sink_module_id),
            'source_name': source_name,
            'source_request_name': requested_source_name,
            'source_module_id': str(source_module_id),
        }

    def _destroy_fx_proxy(self, info):
        source_module_id = info.get('proxy_source_module_id')
        sink_module_id = info.get('proxy_sink_module_id')
        source_name = info.get('proxy_source_name')
        source_request_name = info.get('proxy_source_request_name')
        sink_name = info.get('proxy_sink_name')

        if source_module_id is None:
            for candidate in (
                    source_request_name,
                    source_name,
                    str(source_name or "").removeprefix("output."),
            ):
                if not candidate:
                    continue
                source_module_id = self._find_module_by_arg(f"source_name={candidate}")
                if source_module_id is not None:
                    break
        if sink_module_id is None and sink_name:
            sink_module_id = self._find_module_by_arg(f"sink_name={sink_name}")

        if source_module_id is not None:
            self._run(['pactl', 'unload-module', str(source_module_id)])
        if sink_module_id is not None:
            self._run(['pactl', 'unload-module', str(sink_module_id)])
        if source_module_id is not None or sink_module_id is not None:
            self.invalidate_snapshot()

    def _build_fx_stage_config(self, safe_key, idx, effect_id, params):
        """Per-stage filter-chain builder. Superseded by
        `_build_unified_chain_config` — kept so an out-of-tree import
        doesn't NameError."""
        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)

        values = self._resolved_params(effect_id, params)
        filter_graph = self._build_filter_graph(effect_id, values)
        if filter_graph is None:
            return None, None, None

        sink_name   = f'wavelinux.fx.{safe_key}.{idx}.{effect_id}.input'
        source_name = f'wavelinux.fx.{safe_key}.{idx}.{effect_id}.source'

        client_id = f'{safe_key}-{idx}-{effect_id}'
        # See `_build_unified_chain_config` for the rationale behind
        # always-process / mono position / virtual+priority props.
        filter_chain_args = f"""{{
            node.description = "_WaveLinux internal: {effect_id} ({safe_key}#{idx})"
            node.nick        = "_WaveLinux-{effect_id}"
            media.name       = "_WaveLinux-{effect_id} ({safe_key}#{idx})"
            node.virtual     = true
            priority.session = -1000
            priority.driver  = -1000
            filter.graph = {{{filter_graph}
            }}
            capture.props = {{
                node.name           = "{sink_name}"
                node.description    = "_WaveLinux internal: {effect_id} input"
                node.nick           = "_WaveLinux-{effect_id}-in"
                media.class         = Audio/Sink
                node.virtual        = true
                priority.session    = -1000
                priority.driver     = -1000
                audio.rate          = 48000
                audio.position      = [ MONO ]
                node.always-process = true
            }}
            playback.props = {{
                node.name           = "{source_name}"
                node.description    = "_WaveLinux internal: {effect_id} output"
                node.nick           = "_WaveLinux-{effect_id}-out"
                media.class         = Audio/Source
                node.virtual        = true
                priority.session    = -1000
                priority.driver     = -1000
                audio.rate          = 48000
                audio.position      = [ MONO ]
                node.always-process = true
            }}
        }}"""
        config = self._fx_client_config(client_id, filter_chain_args)
        config_path = os.path.join(
            config_dir, f'wavelinux-chain-{safe_key}-{idx}-{effect_id}.conf'
        )
        with open(config_path, 'w') as f:
            f.write(config)
        return config_path, sink_name, source_name

    def _wait_load_loopback(self, source, sink, latency_msec=20, attempts=20, delay=0.1):
        """Load a `module-loopback` from `source` to `sink`, retrying for
        up to ~2s while pipewire-pulse registers a freshly-spawned node.
        Returns the loopback module id, or None on failure.

        20ms latency matches the rest of the codebase. Lower (5ms) was
        unstable on some rigs — filter-chain couldn't keep up at that
        quantum and audio stopped flowing despite the module loading."""
        for _ in range(attempts):
            out = self._run([
                'pactl', 'load-module', 'module-loopback',
                f'source={source}',
                f'sink={sink}',
                f'latency_msec={int(latency_msec)}',
                'adjust_time=0',
            ])
            if out:
                stripped = out.strip().splitlines()[-1].strip()
                if stripped.isdigit():
                    # Pulse stream-restore can resurrect a stale muted state
                    # for loopback sink-inputs keyed by media name
                    # (`loopback-... input`). If this upstream FX feed starts
                    # muted, the chain appears "running" but outputs silence.
                    for _ in range(20):
                        si = self._sink_input_for_module(stripped)
                        if si is None:
                            time.sleep(0.01)
                            continue
                        self._run(['pactl', 'set-sink-input-volume', si, '100%'])
                        self._run(['pactl', 'set-sink-input-mute', si, '0'])
                        break
                    return stripped
            time.sleep(delay)
        return None

    def set_channel_fx(self, node_name, capture_target, effects, params_map=None):
        """Apply a channel FX chain.

        Effects are exposed through a stable WaveLinux-owned source while
        active, so app capture, default-mic routing, and WaveLinux's own
        monitor path all hear the same processed signal."""
        return self._set_channel_fx_inner(
            node_name, capture_target, effects, params_map,
        )

    def _set_channel_fx_inner(self, node_name, capture_target, effects, params_map):
        result = self.apply_channel_fx_transaction(
            node_name,
            capture_target,
            effects,
            params_map=params_map,
        )
        if not result.get('success'):
            return None
        return result.get('active_source')

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
        for mod_id in info.get('loopbacks', []):
            self._run(['pactl', 'unload-module', str(mod_id)])
        for pk in info.get('procs', []):
            self.stop_rnnoise(pk)

    def _unload_submix_replacements(self, replacements):
        for binding in replacements.values():
            module_id = binding.get('module_id')
            if module_id is not None:
                self._run(['pactl', 'unload-module', str(module_id)])

    def apply_channel_fx_transaction(self, node_name, capture_target, effects, params_map=None):
        """Transactionally replace a channel FX chain without dropping audio."""
        if not node_name:
            return self._fx_result(False, failure_stage='precondition', message='Missing channel name')

        params_map = params_map or {}
        ordered = [fid for fid in self._ordered_chain(effects)
                   if self.effect_available(fid)]
        if not ordered:
            return self.clear_channel_fx_transaction(
                node_name,
                target_source=capture_target,
            )

        old_info = dict(self.channel_fx.get(node_name) or {})
        if self._is_inline_fx_info(old_info):
            try:
                self._clear_inline_channel_fx(node_name, old_info)
            finally:
                self.channel_fx.pop(node_name, None)
            old_info = {}

        reuse_proxy = bool(
            old_info.get('mode') == 'proxy'
            and old_info.get('proxy_sink_name')
            and old_info.get('proxy_source_name')
        )
        effective_source = old_info.get('source') or capture_target
        if not effective_source:
            return self._fx_result(
                False,
                kept_source=None,
                failure_stage='precondition',
                message='Missing capture target for FX chain',
            )

        safe_key = self._safe_channel_key(node_name)
        proxy = {
            'sink_name': old_info.get('proxy_sink_name'),
            'sink_module_id': old_info.get('proxy_sink_module_id'),
            'source_name': old_info.get('proxy_source_name') or old_info.get('source'),
            'source_module_id': old_info.get('proxy_source_module_id'),
        } if reuse_proxy else None
        stamp = int(time.time() * 1000)
        config_path, sink_name, source_name, used_effects = \
            self._build_unified_chain_config(safe_key, ordered, params_map, stamp)
        if config_path is None or not used_effects:
            return self._fx_result(
                False,
                kept_source=effective_source,
                failure_stage='config_build',
                message='No renderable effects were available for this chain',
            )

        log_path = self._fx_log_path(safe_key, f'chain_{stamp}')
        proc_key = f'chain_{safe_key}_{stamp}'
        if not self._spawn_fx(config_path, log_path, proc_key):
            logging.warning(
                f"Unified FX chain failed to spawn for {node_name}; "
                f"see {log_path} for the pipewire stderr."
            )
            return self._fx_result(
                False,
                kept_source=effective_source,
                failure_stage='spawn',
                message=f'FX chain failed to spawn; see {log_path}',
            )

        mic_cutover = bool(capture_target and not capture_target.endswith('.monitor'))
        default_before = self.get_default_source() if mic_cutover else None
        prev_default = old_info.get('prev_default')
        if prev_default is None and mic_cutover and default_before:
            prev_default = default_before

        binding_snapshot = {}
        external_source_outputs = []
        if not reuse_proxy:
            binding_snapshot = self._snapshot_submix_bindings(effective_source)
            exclude_modules = list(old_info.get('loopbacks', []))
            for binding in binding_snapshot.values():
                old_module_id = binding.get('module_id')
                if old_module_id is not None:
                    exclude_modules.append(old_module_id)
            external_source_outputs = self.snapshot_external_source_outputs(
                effective_source,
                exclude_modules=exclude_modules,
            )

        lb = None
        proxy_feed = None
        replacements = {}
        default_changed = False
        failure_stage = None

        try:
            if not self._wait_source_visible(source_name):
                failure_stage = 'candidate_source'
                raise RuntimeError('candidate source did not appear')

            lb = self._wait_load_loopback(capture_target, sink_name)
            if lb is None:
                failure_stage = 'upstream_loopback'
                raise RuntimeError('candidate upstream loopback failed')

            if proxy is None:
                proxy = self._ensure_fx_proxy(safe_key)
                if proxy is None:
                    failure_stage = 'proxy_create'
                    raise RuntimeError('stable FX source could not be created')

            proxy_feed = self._load_loopback_module(
                source_name,
                proxy['sink_name'],
            )
            if proxy_feed is None or not self._module_is_alive(proxy_feed):
                failure_stage = 'proxy_feed'
                raise RuntimeError('processed signal could not be attached to the stable FX source')

            if not reuse_proxy:
                for key, binding in binding_snapshot.items():
                    module_id = self._create_submix_replacement(
                        proxy['source_name'],
                        binding['mix_name'],
                        initial_state=binding.get('state') or {},
                    )
                    if module_id is None:
                        failure_stage = f"submix_{binding['mix_name'].lower()}"
                        raise RuntimeError(f"replacement submix loopback failed for {binding['mix_name']}")
                    replacements[key] = {
                        'mix_name': binding['mix_name'],
                        'module_id': module_id,
                        'old_module_id': binding.get('module_id'),
                        'state': dict(binding.get('state', {}) or {}),
                    }

            if mic_cutover:
                if default_before != proxy['source_name']:
                    self.set_default_source(proxy['source_name'])
                    default_changed = True
                if not reuse_proxy and not self._move_known_source_outputs(
                        external_source_outputs,
                        effective_source,
                        proxy['source_name']):
                    failure_stage = 'source_output_move'
                    raise RuntimeError('source-output move to stable FX source failed')
                if self.get_default_source() != proxy['source_name']:
                    failure_stage = 'default_source'
                    raise RuntimeError('default source did not switch to the stable FX source')

            self.channel_fx[node_name] = {
                'mode': 'proxy',
                'effects': list(used_effects),
                'params': {
                    fid: dict(params_map.get(fid, {}))
                    for fid in used_effects
                },
                'procs': [proc_key],
                'loopbacks': [lb, proxy_feed],
                'source': proxy['source_name'],
                'active_chain_source': source_name,
                'active_chain_sink': sink_name,
                'capture_target': capture_target,
                'safe_key': safe_key,
                'prev_default': prev_default,
                'proxy_sink_name': proxy['sink_name'],
                'proxy_sink_module_id': proxy['sink_module_id'],
                'proxy_source_name': proxy['source_name'],
                'proxy_source_request_name': proxy.get('source_request_name'),
                'proxy_source_module_id': proxy['source_module_id'],
            }
            if replacements:
                self._commit_submix_replacements(
                    replacements,
                    new_source=proxy['source_name'],
                )
            if old_info:
                self._teardown_fx_plumbing(old_info)
            self.invalidate_snapshot()
            return self._fx_result(
                True,
                active_source=proxy['source_name'],
                kept_source=proxy['source_name'],
                message='FX chain active',
            )
        except Exception as exc:
            if default_changed and default_before:
                self.set_default_source(default_before)
            if not reuse_proxy and proxy and effective_source:
                self._move_known_source_outputs(
                    external_source_outputs,
                    proxy['source_name'],
                    effective_source,
                )
            self._unload_submix_replacements(replacements)
            if proxy_feed is not None:
                self._run(['pactl', 'unload-module', str(proxy_feed)])
            if lb is not None:
                self._run(['pactl', 'unload-module', str(lb)])
            self.stop_rnnoise(proc_key)
            if not reuse_proxy and proxy:
                self._destroy_fx_proxy({
                    'proxy_sink_name': proxy.get('sink_name'),
                    'proxy_sink_module_id': proxy.get('sink_module_id'),
                    'proxy_source_name': proxy.get('source_name'),
                    'proxy_source_request_name': proxy.get('source_request_name'),
                    'proxy_source_module_id': proxy.get('source_module_id'),
                })
            self.invalidate_snapshot()
            return self._fx_result(
                False,
                kept_source=proxy['source_name'] if reuse_proxy and proxy else effective_source,
                rolled_back=True,
                failure_stage=failure_stage or 'cutover',
                message=str(exc),
            )

    def clear_channel_fx(self, node_name):
        """Tear down the FX chain on a channel. Idempotent. Order matters:
        unload the inter-stage loopbacks AND any submix loopback that was
        consuming this channel's FX output BEFORE stopping the
        filter-chain stages, so we don't leave PipeWire briefly routing
        audio into a sink that's about to disappear (which can wedge
        pipewire-pulse for a beat)."""
        return self._clear_channel_fx_inner(node_name)

    def _clear_channel_fx_inner(self, node_name):
        result = self.clear_channel_fx_transaction(node_name)
        return bool(result.get('success'))

    def _clear_channel_fx_info(self, info, target_source=None):
        if not info:
            return False
        for node_name, current in list(self.channel_fx.items()):
            if current is info:
                result = self.clear_channel_fx_transaction(
                    node_name,
                    target_source=target_source,
                )
                return bool(result.get('success'))
        fx_source = info.get('source')
        for skey in list(self.submix_sources.keys()):
            if self.submix_sources.get(skey) != fx_source:
                continue
            mod_id = self.submix_loopbacks.pop(skey, None)
            self.submix_sources.pop(skey, None)
            if mod_id is not None:
                self._run(['pactl', 'unload-module', str(mod_id)])
        self._teardown_fx_plumbing(info)
        self.invalidate_snapshot()
        return True

    def clear_channel_fx_transaction(self, node_name, target_source=None):
        """Transactionally clear a channel FX chain without dropping audio."""
        info = self.channel_fx.get(node_name)
        if not info:
            return self._fx_result(True, kept_source=target_source, message='FX chain already cleared')

        if self._is_inline_fx_info(info):
            return self._clear_inline_channel_fx(node_name, info)

        if info.get('mode') == 'proxy' and info.get('proxy_sink_name'):
            proxy_source = info.get('proxy_source_name') or info.get('source')
            proxy_sink = info.get('proxy_sink_name')
            capture_target = info.get('capture_target') or ''
            dest_source = target_source or capture_target
            if not proxy_source or not proxy_sink or not dest_source:
                self.channel_fx.pop(node_name, None)
                return self._fx_result(
                    True,
                    kept_source=dest_source,
                    message='FX chain state was incomplete',
                )

            binding_snapshot = self._snapshot_submix_bindings(proxy_source)
            external_source_outputs = self.snapshot_external_source_outputs(
                proxy_source,
            )
            mic_cutover = bool(capture_target and not capture_target.endswith('.monitor'))
            default_before = self.get_default_source() if mic_cutover else None
            replacement_default = target_source or info.get('prev_default') or capture_target
            replacements = {}
            replacement_feed = None
            default_changed = False
            failure_stage = None

            try:
                replacement_feed = self._load_loopback_module(dest_source, proxy_sink)
                if replacement_feed is None or not self._module_is_alive(replacement_feed):
                    failure_stage = 'proxy_feed'
                    raise RuntimeError('raw source could not be rebound to the stable FX source')

                for key, binding in binding_snapshot.items():
                    module_id = self._create_submix_replacement(
                        dest_source,
                        binding['mix_name'],
                        initial_state=binding.get('state') or {},
                    )
                    if module_id is None:
                        failure_stage = f"submix_{binding['mix_name'].lower()}"
                        raise RuntimeError(f"replacement submix loopback failed for {binding['mix_name']}")
                    replacements[key] = {
                        'mix_name': binding['mix_name'],
                        'module_id': module_id,
                        'old_module_id': binding.get('module_id'),
                        'state': dict(binding.get('state', {}) or {}),
                    }

                if mic_cutover:
                    if not self._move_known_source_outputs(
                            external_source_outputs,
                            proxy_source,
                            dest_source):
                        failure_stage = 'source_output_move'
                        raise RuntimeError('source-output move off stable FX source failed')
                    if default_before == proxy_source and replacement_default:
                        self.set_default_source(replacement_default)
                        default_changed = True
                        if self.get_default_source() != replacement_default:
                            failure_stage = 'default_source'
                            raise RuntimeError('default source did not restore correctly')

                self.channel_fx.pop(node_name, None)
                if replacements:
                    self._commit_submix_replacements(replacements, new_source=dest_source)
                else:
                    for skey in list(self.submix_sources.keys()):
                        if self.submix_sources.get(skey) != proxy_source:
                            continue
                        mod_id = self.submix_loopbacks.pop(skey, None)
                        self.submix_sources.pop(skey, None)
                        if mod_id is not None:
                            self._run(['pactl', 'unload-module', str(mod_id)])
                self._teardown_fx_plumbing(info)
                if replacement_feed is not None:
                    self._run(['pactl', 'unload-module', str(replacement_feed)])
                self._destroy_fx_proxy(info)
                self.invalidate_snapshot()
                return self._fx_result(
                    True,
                    kept_source=dest_source,
                    message='FX chain cleared',
                )
            except Exception as exc:
                if default_changed and default_before:
                    self.set_default_source(default_before)
                self._move_known_source_outputs(
                    external_source_outputs,
                    dest_source,
                    proxy_source,
                )
                self._unload_submix_replacements(replacements)
                if replacement_feed is not None:
                    self._run(['pactl', 'unload-module', str(replacement_feed)])
                self.invalidate_snapshot()
                return self._fx_result(
                    False,
                    active_source=proxy_source,
                    kept_source=proxy_source,
                    rolled_back=True,
                    failure_stage=failure_stage or 'cutover',
                    message=str(exc),
                )

        fx_source = info.get('source')
        capture_target = info.get('capture_target') or ''
        dest_source = target_source or capture_target
        if not fx_source:
            self.channel_fx.pop(node_name, None)
            return self._fx_result(True, kept_source=dest_source, message='FX chain state was incomplete')

        binding_snapshot = self._snapshot_submix_bindings(fx_source)
        exclude_modules = list(info.get('loopbacks', []))
        for binding in binding_snapshot.values():
            old_module_id = binding.get('module_id')
            if old_module_id is not None:
                exclude_modules.append(old_module_id)
        external_source_outputs = self.snapshot_external_source_outputs(
            fx_source,
            exclude_modules=exclude_modules,
        )

        mic_cutover = bool(capture_target and not capture_target.endswith('.monitor'))
        default_before = self.get_default_source() if mic_cutover else None
        replacement_default = target_source or info.get('prev_default') or capture_target
        replacements = {}
        default_changed = False
        failure_stage = None

        try:
            if dest_source:
                for key, binding in binding_snapshot.items():
                    module_id = self._create_submix_replacement(
                        dest_source,
                        binding['mix_name'],
                        initial_state=binding.get('state') or {},
                    )
                    if module_id is None:
                        failure_stage = f"submix_{binding['mix_name'].lower()}"
                        raise RuntimeError(f"replacement submix loopback failed for {binding['mix_name']}")
                    replacements[key] = {
                        'mix_name': binding['mix_name'],
                        'module_id': module_id,
                        'old_module_id': binding.get('module_id'),
                        'state': dict(binding.get('state', {}) or {}),
                    }

            if mic_cutover and dest_source:
                if not self._move_known_source_outputs(
                        external_source_outputs,
                        fx_source,
                        dest_source):
                    failure_stage = 'source_output_move'
                    raise RuntimeError('source-output move off FX source failed')
                if default_before == fx_source and replacement_default:
                    self.set_default_source(replacement_default)
                    default_changed = True
                    if self.get_default_source() != replacement_default:
                        failure_stage = 'default_source'
                        raise RuntimeError('default source did not restore correctly')

            self.channel_fx.pop(node_name, None)
            if replacements:
                self._commit_submix_replacements(replacements, new_source=dest_source)
            else:
                for skey in list(self.submix_sources.keys()):
                    if self.submix_sources.get(skey) != fx_source:
                        continue
                    mod_id = self.submix_loopbacks.pop(skey, None)
                    self.submix_sources.pop(skey, None)
                    if mod_id is not None:
                        self._run(['pactl', 'unload-module', str(mod_id)])
            self._teardown_fx_plumbing(info)
            self.invalidate_snapshot()
            return self._fx_result(
                True,
                kept_source=dest_source,
                message='FX chain cleared',
            )
        except Exception as exc:
            if default_changed and default_before:
                self.set_default_source(default_before)
            if dest_source:
                self._move_known_source_outputs(
                    external_source_outputs,
                    dest_source,
                    fx_source,
                )
            self._unload_submix_replacements(replacements)
            self.invalidate_snapshot()
            return self._fx_result(
                False,
                active_source=fx_source,
                kept_source=fx_source,
                rolled_back=True,
                failure_stage=failure_stage or 'cutover',
                message=str(exc),
            )

    def get_channel_fx_source(self, node_name, snap=None):
        """Return the effective source carrying a channel's FX output, or None."""
        info = self.channel_fx.get(node_name)
        if not info:
            return None
        if self._is_inline_fx_info(info):
            target = self._find_inline_fx_target(node_name, snap=snap)
            if target is None:
                self.channel_fx.pop(node_name, None)
                return None
            info['node_id'] = target['node_id']
            info['source'] = target['source_name']
            info['capture_target'] = target['capture_target']
            return info.get('source')
        # If any stage's process died, drop the chain so callers re-route
        # the loopback back to the raw mic instead of a dead source.
        for pk in info.get('procs', []):
            proc = self.rnnoise_processes.get(pk)
            if proc is None or proc.poll() is not None:
                self.clear_channel_fx(node_name)
                return None
        return info.get('source')

    def is_channel_fx_running(self, node_name):
        return self.get_channel_fx_source(node_name) is not None

    def get_channel_effects(self, node_name):
        """Ordered list of effect ids currently running on a channel."""
        info = self.channel_fx.get(node_name)
        return list(info.get('effects', [])) if info else []

    def _find_module_by_arg(self, pattern, modules_text=None):
        """Find a pactl module ID whose argument list contains `pattern`
        as a whole token (space-separated). Substring matches would make
        `source=1` collide with `source=12`."""
        text = modules_text if modules_text is not None else self._run(['pactl', 'list', 'modules'])
        if not text:
            return None
        curr_id = None
        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Module #'):
                curr_id = stripped.split('#', 1)[1].strip()
                continue
            if not curr_id or 'Argument:' not in stripped:
                continue
            args = stripped.split('Argument:', 1)[1].strip().split()
            if pattern in args:
                return curr_id
        return None

    def _module_is_alive(self, module_id, short_text=None):
        """Cheap liveness check via `pactl list short modules`."""
        if module_id is None:
            return False
        text = short_text if short_text is not None else self._run(['pactl', 'list', 'short', 'modules'])
        if not text:
            return False
        mid = str(module_id)
        for line in text.splitlines():
            parts = line.split('\t', 1)
            if parts and parts[0].strip() == mid:
                return True
        return False

    # ── Cleanup ────────────────────────────────────────────────────

    def cleanup(self):
        """Hard cleanup of all WaveLinux PipeWire modules."""
        # Restore BT auto-switch first so other apps' HSP/HFP behaviour
        # returns to normal the moment WaveLinux exits.
        try:
            self.unlock_bluetooth_autoswitch()
        except AttributeError:
            pass
        # Tear down channel chains via the chain API so channel_fx state
        # gets freed too.
        for nname in list(self.channel_fx.keys()):
            self.clear_channel_fx(nname)
        # Anything else parked in rnnoise_processes.
        for key in list(self.rnnoise_processes.keys()):
            self.stop_rnnoise(key)

        self.virtual_sink_modules.clear()
        self.output_mixes.clear()
        self.loopback_modules.clear()
        self.submix_loopbacks.clear()
        self.submix_sources.clear()
        self.submix_state_cache.clear()
        self._pending_submix_state_reapply.clear()

        # Hard sweep using full list (short mode doesn't show arguments)
        out = self._run(['pactl', 'list', 'modules'])
        if out:
            curr_id = None
            to_unload = []
            for line in out.splitlines():
                line = line.strip()
                if line.startswith('Module #'):
                    curr_id = line.split('#')[1].strip()
                if ('wavelinux' in line or 'WaveLinux' in line) and curr_id:
                    if curr_id not in to_unload:
                        to_unload.append(curr_id)
            
            for mid in to_unload:
                self._run(['pactl', 'unload-module', mid])
