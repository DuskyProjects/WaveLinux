"""
PipeWire Engine — handles all audio routing, virtual sinks, volume control,
multiple output mixes, effects chains, and RNNoise noise suppression.
"""

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
    """Represents one output mix (e.g. Monitor, Stream, Discord, VOD)."""
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
    """One-shot cache of pactl/pw-dump outputs, built at the top of a
    refresh tick so a single tick runs each heavy subprocess at most once.
    Write paths re-query directly to avoid acting on stale data."""

    __slots__ = ("modules_text", "short_modules_text", "sink_inputs_text",
                 "sinks_text", "nodes", "sinks", "_loopback_index",
                 "_sink_state_by_name", "_sink_descriptions")

    def __init__(self, modules_text="", short_modules_text="",
                 sink_inputs_text="", sinks_text="", nodes=None, sinks=None):
        self.modules_text = modules_text or ""
        self.short_modules_text = short_modules_text or ""
        self.sink_inputs_text = sink_inputs_text or ""
        self.sinks_text = sinks_text or ""
        self.nodes = nodes or []
        self.sinks = sinks or []
        self._loopback_index = None
        self._sink_state_by_name = None
        self._sink_descriptions = None


class PipeWireEngine:
    """Full-featured PipeWire audio engine."""

    # Common LADSPA search paths across distros. We additionally honour
    # $LADSPA_PATH (colon-separated, like PATH) at probe time.
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

    def __init__(self):
        self.virtual_sink_modules = {}   # safe_name -> pactl module id
        self.output_mixes = {}           # mix_name -> OutputMix
        self.rnnoise_processes = {}      # channel_key -> subprocess
        self.loopback_modules = {}       # "mix_name->hw_name" -> module id
        self.submix_loopbacks = {}       # "node_id->mix_name" -> module id
        # Source token used when each submix loopback was created. Lets
        # `route_input_to_submix` notice when an FX toggle changes a mic's
        # source (raw mic ↔ FX bus output) and rebuild the loopback.
        self.submix_sources = {}         # "node_id->mix_name" -> source_token
        # Per-channel FX chain. Keyed by stable PipeWire node.name so it
        # survives PipeWire restarts.
        self.channel_fx = {}             # node_name -> {effects, params, procs,
                                         #               source, capture_target,
                                         #               safe_key}

        # Probe LADSPA plugins once at startup; filter-chain silently
        # fails to start if it references one that isn't installed.
        self.ladspa_plugins = self._probe_ladspa_plugins()

        # Reap orphan `pipewire -c` filter-chain processes from previous
        # crashes — otherwise their virtual sinks/sources stay alive in
        # the system audio graph forever.
        self._reap_orphan_fx_processes()

        self.cleanup()

        self._bt_autoswitch_overridden = False
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

    # ── Bluetooth profile lock ─────────────────────────────────────
    #
    # WirePlumber 0.5+ defaults `bluetooth.autoswitch-to-headset-profile`
    # to true, which flips a BT device from A2DP to HSP/HFP the moment any
    # client opens its mic. The A2DP sink disappears for the duration. We
    # disable that autoswitch (volatile, no `--save`) so the headset stays
    # visible as a stereo output the whole time WaveLinux is running.

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

    @classmethod
    def _probe_ladspa_plugins(cls):
        """Return a set of LADSPA plugin names (sans .so) found on disk.
        Honours $LADSPA_PATH plus common distro locations."""
        env_path = os.environ.get("LADSPA_PATH", "")
        roots = [p for p in env_path.split(":") if p] + list(cls._LADSPA_PATHS)
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
        env_path = os.environ.get("LADSPA_PATH", "")
        roots = [p for p in env_path.split(":") if p] + list(self._LADSPA_PATHS)
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

    def effect_available(self, effect_id):
        """Return True if the filter-chain backend for this effect has
        everything it needs on disk. Keeps the FX UI from offering things
        that will silently fail at spawn time."""
        requirements = {
            'rnnoise':    ('librnnoise_ladspa',),
            'compressor': ('sc4m_1916',),
            'gate':       ('gate_1410',),
            # highpass, eq, and limiter use PipeWire's builtin nodes
            # (biquad / linear / clamp) — always available.
            'highpass':   (),
            'eq':         (),
            'limiter':    (),
        }
        needed = requirements.get(effect_id, ())
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

    def set_default_source(self, source_name):
        """Set the system default capture source. Apps that follow the
        default mic (Discord, Zoom, browsers via getUserMedia) start
        recording from `source_name` after this call without changing
        their own settings. Returns True on success."""
        if not source_name:
            return False
        return self._run(['pactl', 'set-default-source', source_name]) is not None

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

    def _list_source_outputs_on(self, source_name):
        """Return [source_output_id, ...] for streams currently capturing
        from `source_name`. Uses `pactl list short source-outputs`, where
        column 3 is the numeric source id, then resolves to name."""
        out = self._run(['pactl', 'list', 'short', 'source-outputs'])
        if not out:
            return []
        id_to_name = self._source_id_to_name()
        ids = []
        for line in out.splitlines():
            parts = line.split('\t')
            if len(parts) < 3:
                continue
            so_id = parts[0].strip()
            src_id = parts[2].strip()
            if id_to_name.get(src_id) == source_name:
                ids.append(so_id)
        return ids

    def _move_source_outputs(self, from_source, to_source):
        """Move every source-output currently capturing from `from_source`
        onto `to_source`. No-op if either is missing or the lookup
        returns nothing."""
        if not from_source or not to_source or from_source == to_source:
            return
        for sid in self._list_source_outputs_on(from_source):
            self._run(['pactl', 'move-source-output', sid, to_source])

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
        cached = getattr(self, '_snapshot_cache', None)
        cached_at = getattr(self, '_snapshot_cache_at', 0.0)
        if cached is not None and not force and (now - cached_at) < self._SNAPSHOT_TTL:
            return cached

        snap = EngineSnapshot(
            modules_text=self._run(['pactl', 'list', 'modules']) or "",
            short_modules_text=self._run(['pactl', 'list', 'short', 'modules']) or "",
            sink_inputs_text=self._run(['pactl', 'list', 'sink-inputs']) or "",
            sinks_text=self._run(['pactl', 'list', 'sinks']) or "",
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
        nodes = []
        for obj in data:
            if obj.get('type') != 'PipeWire:Interface:Node':
                continue
            props = obj.get('info', {}).get('props', {})
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
        return snap.nodes if snap else self._parse_nodes()

    @staticmethod
    def _is_internal_node_name(name):
        # Internal plumbing uses both `wavelinux_` (underscore, for sinks
        # we create via pactl) and `wavelinux.` (dot, for filter-chain
        # virtual nodes spawned by `pipewire -c`). Both must be hidden
        # from device pickers, otherwise the FX chain's own input/output
        # surfaces in the mic / output dropdowns.
        return (name.startswith('wavelinux_')
                or name.startswith('wavelinux.'))

    def get_hardware_outputs(self, snap=None):
        return [n for n in self.get_all_nodes(snap)
                if n.media_class == 'Audio/Sink'
                and not self._is_internal_node_name(n.name)]

    def get_hardware_inputs(self, snap=None):
        return [n for n in self.get_all_nodes(snap)
                if n.media_class == 'Audio/Source'
                and 'rnnoise' not in n.name.lower()
                and not self._is_internal_node_name(n.name)]

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

    def route_input_to_submix(self, node_id, node_name, media_class, mix_name, snap=None):
        """Loopback an input source (or sink monitor) into a submix.
        Idempotent on every refresh tick. When the channel's FX chain
        toggles, the source token changes (raw mic ↔ FX virtual-source) and
        the loopback gets rebuilt pointing at the new source.

        Returns True if `submix_loopbacks[key]` is up to date afterwards.
        Does NOT reset volume/mute on a fresh loopback — caller compares
        the module id before/after to detect a rebuild and re-push saved
        state, otherwise FX toggles silently clobber Monitor-mute."""
        key = f'{node_id}->{mix_name}'

        mix = self.output_mixes.get(mix_name)
        if not mix:
            return False

        short = snap.short_modules_text if snap else None

        # The "true" source token: FX chain output if the channel has any
        # effects running, otherwise the raw mic / sink-monitor.
        fx_source = self.get_channel_fx_source(node_name)
        if fx_source:
            source_id = fx_source
        elif media_class == 'Audio/Sink':
            source_id = f"{node_name}.monitor"
        else:
            source_id = str(node_id)

        # If a loopback we created earlier is still live AND its source
        # matches the current routing (FX state hasn't changed), keep it.
        known = self.submix_loopbacks.get(key)
        known_source = self.submix_sources.get(key)
        if known and known_source == source_id and self._module_is_alive(known, short_text=short):
            return True
        # Otherwise drop the stale entry. If the source changed (FX flip),
        # actively unload the module so we don't leave dangling audio.
        if known:
            if known_source != source_id:
                self._run(['pactl', 'unload-module', str(known)])
            self.submix_loopbacks.pop(key, None)
            self.submix_sources.pop(key, None)

        existing = self._find_loopback_for(source_id, mix.sink_name, snap=snap)
        if existing:
            self.submix_loopbacks[key] = existing
            self.submix_sources[key] = source_id
            return True

        out = self._run([
            'pactl', 'load-module', 'module-loopback',
            f'source={source_id}',
            f'sink={mix.sink_name}',
            'latency_msec=20',
            'adjust_time=0'
        ])
        if not out:
            return False
        self.submix_loopbacks[key] = out
        self.submix_sources[key] = source_id
        # Do NOT reset volume/mute on the new sink-input. Caller detects
        # rebuilds and re-pushes saved submix_state.
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
        si = self.get_submix_sink_input(node_id, mix_name)
        if not si:
            logging.warning(f"Could not find sink-input for {node_id}->{mix_name}")
            return False
        pct = max(0, min(int(round(self._clamp(volume) * 100)), 100))
        self._run(['pactl', 'set-sink-input-volume', si, f'{pct}%'])
        return True

    def set_submix_mute(self, node_id, mix_name, mute):
        """Set the submix mute. Same retry semantics as set_submix_volume."""
        si = self.get_submix_sink_input(node_id, mix_name)
        if not si:
            logging.warning(f"Could not find sink-input to mute for {node_id}->{mix_name}")
            return False
        self._run(['pactl', 'set-sink-input-mute', si, '1' if mute else '0'])
        return True

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

        for key in list(self.submix_loopbacks.keys()):
            if key.endswith(f"->{sink_name}"):
                self._run(['pactl', 'unload-module', str(self.submix_loopbacks.pop(key))])

        self._run(['pactl', 'unload-module', str(module_id)])
        return True

    def create_output_mix(self, name):
        """Create a mix bus: a null-sink plus a virtual source so apps like OBS
        can pick it up as a dedicated recording device (e.g. 'WaveLinux-Stream')."""
        _, safe_name = self._sanitize_channel_name(name)
        sink_name = f"wavelinux_mix_{safe_name}"
        source_name = f"wavelinux_src_{safe_name}"
        description = self._branding_label(name)

        # 1. The thing apps play *to*.
        if self.create_virtual_sink(name, custom_name=sink_name) is None:
            return None
        sink_module_id = (self.virtual_sink_modules.get(sink_name)
                          or self._find_module_by_arg(f"sink_name={sink_name}"))

        # 2. Dedicated recording source so OBS / browsers see a named device
        # instead of a generic "Monitor of null sink". Whitespace-free
        # description values so pactl's sink_properties parser can't fumble.
        src_module_id = self._find_module_by_arg(f"source_name={source_name}")
        if not src_module_id:
            src_module_id = self._run([
                'pactl', 'load-module', 'module-virtual-source',
                f'source_name={source_name}',
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
        # Remove old loopback if exists
        for key in list(self.loopback_modules.keys()):
            if key.startswith(mix_name + '->'):
                self._run(['pactl', 'unload-module', self.loopback_modules[key]])
                del self.loopback_modules[key]

        # Retry briefly — picked sink may not be visible yet (BT mid-profile
        # negotiation, USB still enumerating).
        out = None
        for attempt in range(8):
            if self._sink_visible(hw_sink_name):
                out = self._run([
                    'pactl', 'load-module', 'module-loopback',
                    f'source={mix.sink_name}.monitor',
                    f'sink={hw_sink_name}',
                    'latency_msec=20',
                    'adjust_time=0',
                ])
                if out:
                    break
            import time as _t; _t.sleep(0.1)

        if not out:
            logging.warning(
                f"route_mix_to_hardware: could not load loopback "
                f"{mix.sink_name}.monitor → {hw_sink_name}"
            )
            return False

        key = f'{mix_name}->{hw_sink_name}'
        self.loopback_modules[key] = out
        mix.hardware_output = hw_sink_name
        # Force the new sink-input to 100%/unmuted — pulse-bridge can
        # apply a stale per-app-per-sink rule that defaults it to 0%.
        si = self._sink_input_for_module(out)
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

    def full_audio_reset(self):
        """Emergency cleanup of ALL wavelinux modules."""
        logging.info("Performing full audio reset...")
        out = self._run(['pactl', 'list', 'short', 'modules'], timeout=5)
        if out:
            # First unload loopbacks to avoid dependency issues
            lines = out.splitlines()
            for line in reversed(lines):
                if 'wavelinux' in line and 'module-loopback' in line:
                    mod_id = line.split()[0]
                    logging.info(f"Unloading loopback: {mod_id}")
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

        by_node_id is keyed by the PipeWire node.id string so it can be
        cross-referenced against pw-dump AudioNode.pw_id values.
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
            node_id = current.get('node.id') or current_index
            by_node_id[str(node_id)] = entry

        for line in text.splitlines():
            stripped = line.strip()
            if stripped.startswith('Sink Input #'):
                _flush()
                current_index = stripped.split('#', 1)[1].strip()
                current = {}
            elif stripped.startswith('Sink:') and current_index is not None:
                current['_sink_id'] = stripped.split(':', 1)[1].strip()
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
        seen_pw_ids = set()

        # Primary path: pw-dump Stream/Output/Audio nodes give reliable
        # discovery even for native PipeWire clients that may not surface
        # cleanly through the PulseAudio compat text output. Each node is
        # enriched with the matching pactl properties (pid, binary, app IDs)
        # via a node.id cross-reference so _process_sink_input gets the full
        # property set it needs for name resolution.
        stream_nodes = self.get_app_streams(snap=snap)
        for node in stream_nodes:
            pw_id_str = str(node.pw_id)
            seen_pw_ids.add(pw_id_str)

            # Merge: pw-dump JSON is authoritative for identity props
            # (application.name, node.description, pid, …). pactl text adds
            # pactl-only fields (sink index, sink id) and fills in any prop
            # that pw-dump didn't carry — but must NOT overwrite a good
            # pw-dump value with a blank/wrong one from the text parser.
            pactl = by_node_id.get(pw_id_str, {})
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
            if node_id_str in seen_pw_ids:
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
        index = self._desktop_app_index()
        if not index:
            return None
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
            if c in self._BINARY_DISPLAY_NAMES:
                return self._BINARY_DISPLAY_NAMES[c]
            if c in index:
                return index[c]
        return None

    # Chromium-renderer and Electron-renderer processes report the parent
    # binary name as comm. Map the binary stem directly to a display name
    # so we don't surface raw comm strings like "brave-browser" in the UI.
    _BINARY_DISPLAY_NAMES = {
        "brave": "Brave",
        "brave-browser": "Brave",
        "brave-browser-stable": "Brave",
        "brave-browser-beta": "Brave Beta",
        "brave-browser-nightly": "Brave Nightly",
        # Origin (unstable) channel used on CachyOS.
        "brave-browser-origin": "Brave Origin Beta",
        "brave-origin": "Brave Origin Beta",
        "ferdium": "Ferdium",
        "ferdi": "Ferdi",
        "hamsket": "Hamsket",
    }

    def _app_name_from_pid(self, pid):
        """Best-effort process-name lookup, skipping wrapper binaries."""
        if not pid:
            return None
        try:
            with open(f"/proc/{pid}/comm", "r") as f:
                comm = f.read().strip()
        except OSError:
            comm = ""
        wrapper_set = {"bwrap", "flatpak", "snap", "snap-confine", "bash", "sh",
                       "python", "python3", "wine", "wine64", "wineserver"}
        comm_lower = comm.lower() if comm else ''
        # Check binary display names first so e.g. "brave-browser" → "Brave"
        # without going through title-case heuristics.
        if comm_lower in self._BINARY_DISPLAY_NAMES:
            return self._BINARY_DISPLAY_NAMES[comm_lower]
        if comm and comm_lower not in wrapper_set:
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
                return comm or None
            if not ppid or ppid in seen or ppid == "0":
                return comm or None
            seen.add(ppid)
            try:
                with open(f"/proc/{ppid}/comm", "r") as f:
                    parent_comm = f.read().strip()
            except OSError:
                return comm or None
            parent_lower = parent_comm.lower()
            if parent_lower in self._BINARY_DISPLAY_NAMES:
                return self._BINARY_DISPLAY_NAMES[parent_lower]
            if parent_comm and parent_lower not in wrapper_set:
                return parent_comm
            cur = ppid
        return comm or None

    def _process_sink_input(self, current, entries, sink_id_to_name):
        # Resolve sink name
        sink_id = current.get('sink_id')
        current['sink'] = sink_id_to_name.get(sink_id, sink_id)

        # Filter out internal wavelinux loopbacks/effects, but NOT the apps playing to them!
        node_name = current.get('node.name', '').lower()
        media_name = current.get('media.name', '').lower()
        is_internal = (
            'wavelinux_mix' in node_name or
            'wavelinux_src' in node_name or
            'wavelinux.fx' in node_name or
            'rnnoise' in node_name or
            'loopback' in node_name or
            'wavelinux_mix' in media_name
        )
        if is_internal:
            return

        # Drop sink-inputs whose visible name is just the local host —
        # PipeWire surfaces the hostname for system streams that have
        # nothing better to report (speech-dispatcher, RTP receiver, etc.).
        host = self._host_aliases()
        if host:
            for prop in ('application.name', 'node.description',
                         'node.nick', 'media.name'):
                token = self._normalize_for_host_match(current.get(prop, ''))
                if token and token in host:
                    return

        # Sandbox-probe first: Flatpak'd apps often publish a generic
        # `application.name` ("audio-src") while their real identity lives
        # in FLATPAK_ID / cgroup / env.
        pid = current.get('pid') or current.get('application.process.id')
        sandbox_name = self._identify_sandboxed_app(pid)
        # Native apps: pull display name from their .desktop entry so e.g.
        # AUR Spotify resolves to 'Spotify' instead of 'spotify'.
        desktop_name = self._identify_via_desktop(pid)

        # Any of these reverse-DNS-style ids gets run through the curated
        # _KNOWN_APP_IDS table too.
        for key in ('flatpak.app_id', 'pipewire.access.portal.app_id',
                    'application.process.host', 'application.id',
                    'application.icon_name'):
            mapped = self._canonicalize_app_id(current.get(key))
            if mapped and not self._is_generic_name(mapped):
                sandbox_name = sandbox_name or mapped
                break

        raw_app_name = current.get('application.name', '').strip()
        if self._is_generic_name(raw_app_name):
            raw_app_name = ''

        candidates = [
            sandbox_name,
            desktop_name,
            raw_app_name,
            current.get('snap.name'),
            current.get('application.display_name'),
            current.get('node.description'),         # often the real display name
            current.get('application.process.binary'),
            current.get('binary'),
            current.get('node.name'),                # lower priority; filtered below
        ]
        name = next((c for c in candidates
                     if c and not self._is_generic_name(c)), None)

        if not name:
            proc_name = self._app_name_from_pid(pid)
            if proc_name and not self._is_generic_name(proc_name):
                # The bare process name might still be a wrapper (e.g. 'aces'
                # for War Thunder) — let install-path inference upgrade it.
                inferred = self._infer_name_from_exe(pid, proc_name)
                name = inferred or proc_name

        if not name:
            # Walk node.description → node.name → media.name, skipping any
            # value that normalizes to a known-generic token so we don't
            # surface "audio-src"/"Audio Src"/"AudioSrc" as a display name.
            for fb_key in ('node.description', 'node.name', 'media.name'):
                fb = (current.get(fb_key) or '').strip()
                if fb and not self._is_generic_name(fb):
                    name = fb
                    break

        if not name:
            # Last resort: use the PW node ID to distinguish multiple
            # unidentifiable streams. Skip entirely if we have no stable
            # ID — that means we raced pw-dump vs pactl and neither had
            # the entry yet; the stream will be picked up on the next tick
            # with a real id. Skipping prevents the un-removable
            # "Media Stream #None" phantom entries.
            node_id = current.get('node.id') or current.get('index')
            if not node_id:
                return
            name = f"Media Stream #{node_id}"

        # Strip common reverse-dns prefixes (org.mozilla.firefox → firefox)
        if '.' in name and ' ' not in name and len(name.split('.')) >= 2:
            name = name.rsplit('.', 1)[-1]
        name = name.replace('-', ' ').replace('_', ' ').strip()
        if name and name.islower():
            name = name.title()

        # Re-check the final resolved name against host aliases — PID /
        # .desktop / install-path resolution can also land on the hostname
        # for system-level streams.
        if name and host and self._normalize_for_host_match(name) in host:
            return

        current['app_name'] = name or "Unknown App"
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
        return snap.sinks if snap else self._parse_short_sinks()

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
        return self._run(['pactl', 'set-card-profile', card_name, profile_name]) is not None or True

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
        header = (
            f"# WaveLinux FX spawn {key}\n"
            f"# pipewire --version: {pw_ver}\n"
            f"# config path:        {config_path}\n"
            f"# LADSPA_PATH env:    {os.environ.get('LADSPA_PATH', '')}\n"
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
                proc.kill()
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

    def get_available_effects(self):
        return [
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
        ]

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

    def _build_unified_chain_config(self, safe_key, ordered_effects, params_map):
        """Write the filter-chain config combining all enabled effects in
        one `filter.graph` with explicit inter-stage `links`. Returns
        (config_path, sink_name, source_name, used_effects) or
        (None, None, None, []) if nothing was renderable."""
        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)

        all_nodes = []
        all_links = []
        prev_exit = None
        used_effects = []

        for stage_idx, effect_id in enumerate(ordered_effects):
            values = self._resolved_params(effect_id, params_map.get(effect_id))
            nodes_text, internal_links, exit_port, entry_port = \
                self._effect_stage_blocks(effect_id, values, stage_idx)
            if nodes_text is None:
                logging.warning(
                    f"Skipping unknown / unavailable effect {effect_id} "
                    f"in chain for {safe_key}"
                )
                continue
            all_nodes.append(nodes_text)
            all_links.extend(internal_links)
            if prev_exit is not None:
                all_links.append(
                    f'{{ output = "{prev_exit}" input = "{entry_port}" }}'
                )
            prev_exit = exit_port
            used_effects.append(effect_id)

        if not used_effects:
            return None, None, None, []

        sink_name = f'wavelinux.fx.{safe_key}.input'
        source_name = f'wavelinux.fx.{safe_key}.source'

        nodes_block = '\n'.join(all_nodes)
        links_block = '\n                    '.join(all_links) if all_links else ''

        # `node.always-process` prevents the chain from suspending mid-
        # construction (which would race the upstream loopback's bind).
        # `audio.position = [ MONO ]` keeps the chain mono so stereo
        # plugins don't end up half-connected.
        # `node.virtual` + `priority.session = -1000` demote these nodes
        # in the system audio panels so they don't clutter device pickers.
        client_id = f'{safe_key}-chain'
        filter_chain_args = f"""{{
            node.description = "_WaveLinux internal: chain ({safe_key})"
            node.nick        = "_WaveLinux-chain"
            media.name       = "_WaveLinux-chain ({safe_key})"
            node.virtual     = true
            priority.session = -1000
            priority.driver  = -1000
            filter.graph = {{
                nodes = [{nodes_block}
                ]
                links = [
                    {links_block}
                ]
            }}
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
        import time as _time
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
                    return stripped
            _time.sleep(delay)
        return None

    def set_channel_fx(self, node_name, capture_target, effects, params_map=None):
        """Replace a channel's FX chain with one `pipewire -c` process
        running every enabled effect in a single `filter.graph`. Returns
        the chain's virtual source node.name, or None on failure.

        - `node_name`: stable PipeWire node.name. State is keyed by this
          so chains survive a PipeWire restart.
        - `capture_target`: source for the chain's input. For mics, the
          mic's node.name; for virtual sinks, `f"{sink_name}.monitor"`.
        - `effects`: list of effect ids (reordered to canonical flow).
        - `params_map`: {effect_id: {param_key: value}}."""
        if not node_name:
            return None
        params_map = params_map or {}

        # Always reset first so the call is idempotent.
        self.clear_channel_fx(node_name)

        ordered = [fid for fid in self._ordered_chain(effects)
                   if self.effect_available(fid)]
        if not ordered:
            return None

        safe_key = self._safe_channel_key(node_name)

        config_path, sink_name, source_name, used_effects = \
            self._build_unified_chain_config(safe_key, ordered, params_map)
        if config_path is None or not used_effects:
            return None

        log_path = self._fx_log_path(safe_key, 'chain')
        proc_key = f'chain_{safe_key}'

        if not self._spawn_fx(config_path, log_path, proc_key):
            logging.warning(
                f"Unified FX chain failed to spawn for {node_name}; "
                f"see {log_path} for the pipewire stderr."
            )
            return None

        # ONE upstream → chain.input loopback. The chain itself wires its
        # internal nodes via filter.graph links; downstream consumers
        # (submix loopbacks) bind to the chain's source.
        lb = self._wait_load_loopback(capture_target, sink_name)
        if lb is None:
            logging.warning(
                f"FX capture loopback {capture_target} → {sink_name} failed; "
                f"chain for {node_name} is dangling. Tearing it back down."
            )
            self.stop_rnnoise(proc_key)
            return None

        # Promote the FX virtual source to default + drag existing capture
        # streams onto it. Without this, apps reading the raw mic (Discord,
        # Zoom, OBS-via-mic-picker) bypass the chain entirely and still
        # hear ambient noise. `prev_default` is captured BEFORE we flip so
        # teardown can restore exactly what the user had.
        prev_default = None
        if capture_target and not capture_target.endswith('.monitor'):
            current_default = self.get_default_source()
            # Don't overwrite a save from another channel's chain — only
            # the first chain captures the user-facing default.
            if current_default and current_default != source_name:
                prev_default = current_default
            self.set_default_source(source_name)
            self._move_source_outputs(capture_target, source_name)

        self.channel_fx[node_name] = {
            'effects':   list(used_effects),
            'params':    {fid: dict(params_map.get(fid, {}))
                          for fid in used_effects},
            'procs':     [proc_key],
            'loopbacks': [lb],
            'source':    source_name,
            'capture_target': capture_target,
            'safe_key':  safe_key,
            'prev_default': prev_default,
        }
        # We just changed pactl's view of the world — drop the cache so the
        # next refresh tick observes the new chain without waiting up to
        # 250 ms for the snapshot TTL.
        self.invalidate_snapshot()
        return source_name

    def clear_channel_fx(self, node_name):
        """Tear down the FX chain on a channel. Idempotent. Order matters:
        unload the inter-stage loopbacks AND any submix loopback that was
        consuming this channel's FX output BEFORE stopping the
        filter-chain stages, so we don't leave PipeWire briefly routing
        audio into a sink that's about to disappear (which can wedge
        pipewire-pulse for a beat)."""
        info = self.channel_fx.pop(node_name, None)
        if not info:
            return False

        # 0. Restore the default source and move active capture streams
        # off the FX source BEFORE we kill it. Otherwise apps recording
        # via the chain glitch on a vanishing source instead of a clean
        # handover back to the raw mic.
        fx_source = info.get('source')
        capture_target = info.get('capture_target') or ''
        prev_default = info.get('prev_default')
        if fx_source and capture_target and not capture_target.endswith('.monitor'):
            current_default = self.get_default_source()
            if current_default == fx_source:
                self.set_default_source(prev_default or capture_target)
            self._move_source_outputs(fx_source, capture_target)

        # 1. Drop submix loopbacks whose source belongs to this chain.
        # Without this, route_input_to_submix keeps using a cached module
        # whose source is gone — silence on the submix until next spawn.
        prefix = f'wavelinux.fx.{info.get("safe_key", "")}.'
        for skey in list(self.submix_sources.keys()):
            src = self.submix_sources.get(skey, '')
            if not src or not src.startswith(prefix):
                continue
            mod_id = self.submix_loopbacks.pop(skey, None)
            self.submix_sources.pop(skey, None)
            if mod_id is not None:
                self._run(['pactl', 'unload-module', str(mod_id)])

        # 2. Unload the chain's own inter-stage wiring loopbacks.
        for mod_id in info.get('loopbacks', []):
            self._run(['pactl', 'unload-module', str(mod_id)])

        # 3. Kill the filter-chain stage processes.
        for pk in info.get('procs', []):
            self.stop_rnnoise(pk)
        self.invalidate_snapshot()
        return True

    def get_channel_fx_source(self, node_name):
        """Return the final FX virtual source for a channel, or None."""
        info = self.channel_fx.get(node_name)
        if not info:
            return None
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

    def is_channel_effect_active(self, node_name, effect_id):
        info = self.channel_fx.get(node_name)
        if not info:
            return False
        if effect_id not in info.get('effects', []):
            return False
        # Same liveness check as get_channel_fx_source — a dead stage means
        # the chain is no longer doing what the user thinks it is.
        return self.get_channel_fx_source(node_name) is not None

    def get_channel_effects(self, node_name):
        """Ordered list of effect ids currently running on a channel."""
        info = self.channel_fx.get(node_name)
        return list(info.get('effects', [])) if info else []

    def fx_chain_status(self, node_name):
        """Diagnostic snapshot of a channel's FX chain. Returns
        `{effect_id: {'state': 'running' | 'failed' | 'inactive',
                      'log': <path or None>}}`.

        - 'running'  = stage process alive and loopback exists.
        - 'failed'   = spawn attempted but the process died.
        - 'inactive' = effect never enabled (or cleared).

        The FX dialog uses this to red-border failed toggles."""
        out = {}
        info = self.channel_fx.get(node_name) or {}
        live_effects = list(info.get('effects', []))
        live_procs   = list(info.get('procs', []))
        safe_key     = info.get('safe_key', self._safe_channel_key(node_name))

        for eid in self.get_available_effects():
            fid = eid['id']
            log_path = self._fx_log_path(safe_key, f'_{fid}')
            if fid not in live_effects:
                # Either never enabled, or attempted-and-removed. We can
                # tell apart by checking the per-stage log on disk.
                stage_log = None
                for i, candidate in enumerate(live_effects + [fid]):
                    candidate_log = self._fx_log_path(safe_key, f'{i}_{fid}')
                    if os.path.exists(candidate_log):
                        stage_log = candidate_log
                        break
                out[fid] = {'state': 'inactive', 'log': stage_log}
                continue

            # Stage exists in the chain. Map effect → its proc by index.
            try:
                idx = live_effects.index(fid)
                proc_key = live_procs[idx]
            except (ValueError, IndexError):
                out[fid] = {'state': 'failed', 'log': self._fx_log_path(safe_key, f'_{fid}')}
                continue
            proc = self.rnnoise_processes.get(proc_key)
            stage_log = self._fx_log_path(safe_key, f'{idx}_{fid}')
            if proc is None or proc.poll() is not None:
                out[fid] = {'state': 'failed', 'log': stage_log}
            else:
                out[fid] = {'state': 'running', 'log': stage_log}
        return out

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
