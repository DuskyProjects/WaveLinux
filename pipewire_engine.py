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
    def __init__(self, pw_id, name, description, media_class, app_name=None):
        self.pw_id = pw_id
        self.name = name
        self.description = description
        self.media_class = media_class
        self.app_name = app_name
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
    """One-shot cache of pactl/pw-dump outputs. Built at the top of a
    refresh tick and threaded through engine helpers, so a single 2-second
    tick runs each heavy subprocess call at most once instead of 5+ times.

    Write paths (load/unload-module, move-sink-input) don't use the snapshot
    — they always re-query to avoid acting on stale data."""

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
        self._loopback_index = None     # lazily built
        self._sink_state_by_name = None # lazily built: name -> (vol, muted)
        self._sink_descriptions = None  # lazily built: name -> description


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
        # Tracks the *source* token used when each submix loopback was
        # created. When effects on a mic toggle on/off, the loopback's
        # source has to swap from the raw mic to the FX bus output (or
        # back) — this lets `route_input_to_submix` notice the change and
        # rebuild the loopback rather than silently keeping the old wiring.
        self.submix_sources = {}         # "node_id->mix_name" -> source_token
        # FX bus per channel. Keyed by the stable PipeWire node.name (mic
        # name or virtual sink name), so this survives PipeWire restarts.
        # Each entry is the active chain on a channel: an ordered list of
        # effect_ids, the per-effect parameter map, the spawned filter-chain
        # process keys, the capture target (raw mic / sink monitor), and
        # the resulting virtual-source name that downstream loopbacks pull
        # from. None / missing = no chain running on that channel.
        self.channel_fx = {}             # node_name -> {effects, params, procs,
                                         #               source, capture_target,
                                         #               safe_key}

        # Which LADSPA plugins are actually present on this system —
        # filter-chain will silently fail-to-start if we reference one that
        # isn't installed, so we probe once at startup.
        self.ladspa_plugins = self._probe_ladspa_plugins()

        # Ensure clean state from any previous crashes
        self.cleanup()

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
        """Find the full filesystem path to a LADSPA plugin .so. PipeWire's
        filter-chain technically accepts a bare plugin name (it walks
        $LADSPA_PATH internally) but using the absolute path eliminates
        a class of "plugin not found" silent failures we've seen in the
        wild — particularly on systems where the librnnoise .so lives in
        a non-default directory the spawned `pipewire -c` process didn't
        inherit. Returns None if the plugin isn't on disk anywhere we
        searched."""
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
        that will silently fail at spawn time.

        The limiter is special — when the LADSPA fast_lookahead_limiter is
        missing we still expose the effect because PipeWire's builtin
        `clamp` is a usable brick-wall fallback. The graph builder picks
        which path to render at spawn time."""
        requirements = {
            'rnnoise':    ('librnnoise_ladspa',),
            'compressor': ('sc4_1882',),
            'gate':       ('gate_1410',),
            # highpass and eq use PipeWire's builtin biquad — always available.
            'highpass':   (),
            'eq':         (),
            # Limiter: LADSPA preferred, builtin clamp fallback — always offered.
            'limiter':    (),
        }
        needed = requirements.get(effect_id, ())
        return all(self.ladspa_plugin_available(n) for n in needed)

    # ── Helpers ─────────────────────────────────────────────────────

    def _run(self, cmd, timeout=2):
        # Defensive: drop None entries and stringify everything. Historically
        # this helper crashed the whole UI when any caller accidentally passed
        # None (e.g. the App Routing "System Default" case), because even the
        # error-logging path did `' '.join(cmd)` which trips on None.
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

    # ── Per-refresh snapshot ───────────────────────────────────────

    def create_snapshot(self):
        """Fetch every expensive state dump once so a whole refresh tick can
        share them. Safe to call with PipeWire misbehaving — missing outputs
        degrade to empty strings/lists."""
        return EngineSnapshot(
            modules_text=self._run(['pactl', 'list', 'modules']) or "",
            short_modules_text=self._run(['pactl', 'list', 'short', 'modules']) or "",
            sink_inputs_text=self._run(['pactl', 'list', 'sink-inputs']) or "",
            sinks_text=self._run(['pactl', 'list', 'sinks']) or "",
            nodes=self._parse_nodes(),
            sinks=self._parse_short_sinks(),
        )

    @staticmethod
    def _parse_sink_descriptions(text):
        """Return {sink_name: friendly_description} from `pactl list sinks`.
        We use descriptions for UI labels because PipeWire puts model info
        (e.g. 'Sony WH-1000XM4') there, while node.name is usually
        something like 'bluez_output.8C_1D_96_4A_59_0B.1'."""
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
            return []
        try:
            data = json.loads(raw)
        except json.JSONDecodeError:
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
        """Turn a PipeWire Bluetooth node.name
        ('bluez_output.8C_1D_96_4A_59_0B.1') or an ALSA dashed form into
        a MAC. We can't derive the model from the MAC — callers need to
        prefer description for Bluetooth — but at least we won't output
        'Bd 96 1' garbage."""
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

    def get_hardware_outputs(self, snap=None):
        return [n for n in self.get_all_nodes(snap)
                if n.media_class == 'Audio/Sink'
                and not n.name.startswith('wavelinux_')]

    def get_hardware_inputs(self, snap=None):
        return [n for n in self.get_all_nodes(snap)
                if n.media_class == 'Audio/Source'
                and 'rnnoise' not in n.name.lower()
                and not n.name.startswith('wavelinux_')]

    def get_virtual_sinks(self, snap=None):
        """User-created WaveLinux channels only (no internal mix/source sinks)."""
        return [n for n in self.get_all_nodes(snap)
                if n.media_class == 'Audio/Sink'
                and n.name in self.virtual_sink_modules
                and not n.name.startswith('wavelinux_mix_')
                and not n.name.startswith('wavelinux_src_')]

    def get_app_streams(self, snap=None):
        return [n for n in self.get_all_nodes(snap)
                if n.media_class == 'Stream/Output/Audio']

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
        """Create a loopback connecting an input source (or sink monitor) to a submix.
        Called on every refresh tick — idempotent: if the loopback we created
        earlier is still alive AND its source still matches the current FX
        state, do nothing. When the channel's FX chain toggles on or off the
        source token changes (raw mic ↔ FX virtual-source), in which case we
        unload the stale loopback and re-create it pointing at the new
        source. Without that swap, enabling effects would leave the audio
        flowing direct from the mic to the mixes, bypassing the chain."""
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
        # We just created the module; sink-input table is stale in the snapshot.
        si = self.get_submix_sink_input(node_id, mix_name)
        if si:
            self._run(['pactl', 'set-sink-input-volume', si, '100%'])
            self._run(['pactl', 'set-sink-input-mute', si, '0'])
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
        si = self.get_submix_sink_input(node_id, mix_name)
        if si:
            pct = max(0, min(int(round(self._clamp(volume) * 100)), 100))
            self._run(['pactl', 'set-sink-input-volume', si, f'{pct}%'])
        else:
            logging.warning(f"Could not find sink-input for {node_id}->{mix_name}")

    def set_submix_mute(self, node_id, mix_name, mute):
        si = self.get_submix_sink_input(node_id, mix_name)
        if si:
            self._run(['pactl', 'set-sink-input-mute', si, '1' if mute else '0'])
        else:
            logging.warning(f"Could not find sink-input to mute for {node_id}->{mix_name}")

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
        """Build the visible device label that shows in KDE's Audio Volume
        panel, pavucontrol, OBS, etc.

        The hard rule: NO WHITESPACE. `pactl`'s `sink_properties=...`
        splits on whitespace to find key=value pairs, and its handling of
        quoted values differs between PulseAudio and PipeWire's pipewire-pulse
        bridge. The only way to guarantee every front-end shows the right
        name is to make each property value a single token.
        """
        if not display_clean:
            return "WaveLinux"
        # Collapse internal whitespace to a single hyphen so 'Voice Chat'
        # → 'WaveLinux-Voice-Chat'.
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
        """Route an output mix to a hardware output using a loopback."""
        mix = self.output_mixes.get(mix_name)
        if not mix:
            return False
        # Remove old loopback if exists
        for key in list(self.loopback_modules.keys()):
            if key.startswith(mix_name + '->'):
                self._run(['pactl', 'unload-module', self.loopback_modules[key]])
                del self.loopback_modules[key]

        # Create loopback from mix sink monitor to hardware sink
        out = self._run([
            'pactl', 'load-module', 'module-loopback',
            f'source={mix.sink_name}.monitor',
            f'sink={hw_sink_name}',
            'latency_msec=20',
            'adjust_time=0',
            'source_dont_move=true',
            'sink_dont_move=true'
        ])
        if out:
            key = f'{mix_name}->{hw_sink_name}'
            self.loopback_modules[key] = out
            mix.hardware_output = hw_sink_name
            return True
        return False

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

    def get_sink_inputs(self, snap=None):
        sinks = self.get_all_sinks(snap=snap)
        sink_id_to_name = {s['index']: s['name'] for s in sinks}

        out = snap.sink_inputs_text if snap else self._run(['pactl', 'list', 'sink-inputs'])
        if not out:
            return []
        entries = []
        current = {}
        for line in out.splitlines():
            line = line.strip()
            if line.startswith('Sink Input #'):
                if current:
                    self._process_sink_input(current, entries, sink_id_to_name)
                current = {'index': line.split('#')[1]}
            elif line.startswith('Sink:'):
                current['sink_id'] = line.split(':', 1)[1].strip()
            elif '=' in line:
                parts = line.split('=', 1)
                if len(parts) == 2:
                    key = parts[0].strip()
                    val = parts[1].strip().strip('"')
                    current[key] = val
                    # Handle specific PipeWire property names
                    if key == 'pipewire.sec.pid' or key == 'application.process.id':
                        current['pid'] = val
                    elif key in ['application.name', 'application.name ']:
                        current['app_name'] = val
                    elif key == 'application.process.binary':
                        current['binary'] = val
                    elif key == 'media.name':
                        current['media_name'] = val

        if current:
            self._process_sink_input(current, entries, sink_id_to_name)
        return entries

    # Known-generic names that should trigger a deeper lookup instead of being displayed.
    _GENERIC_APP_NAMES = {
        "audio-src", "audio-sink", "speech-dispatcher", "unknown",
        "libcanberra", "playback", "pipewire", "pipewire-pulse",
        "pulseaudio", "alsa-plugins", "alsa plug-in", "alsa-plug-in",
        "audiostreamforandroid", "application", "pw-loopback",
        # Chromium/Electron Flatpak apps often default to these:
        "chromium", "electron", "chrome",
    }

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

    @classmethod
    def _host_aliases(cls):
        """Return the set of strings that identify *this machine* — used to
        filter sink-inputs that PipeWire hangs off the local host instead of
        a real app. Cached so we don't re-stat /proc on every refresh."""
        cached = getattr(cls, '_host_alias_cache', None)
        if cached is not None:
            return cached
        names = set()
        try:
            h = socket.gethostname()
            if h:
                names.add(h.lower())
                # `socket.gethostname()` may return the FQDN — also keep just
                # the short hostname so 'duskypc.local' still matches 'duskypc'.
                short = h.split('.', 1)[0]
                if short:
                    names.add(short.lower())
        except Exception:
            pass
        try:
            with open('/etc/hostname', 'r') as f:
                h = f.read().strip()
                if h:
                    names.add(h.lower())
                    names.add(h.split('.', 1)[0].lower())
        except OSError:
            pass
        cls._host_alias_cache = names
        return names

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
        """Build a one-shot index of every .desktop file on the system mapping
        the binary basename → display Name.  Lets us name a native app like
        AUR Spotify ('spotify' on $PATH) by reading its own /usr/share/
        applications/spotify.desktop instead of relying on a hand-curated alias
        table.  Cached for 60 s so refresh ticks don't keep re-scanning."""
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
        """When a process's comm is a generic launcher binary (e.g. 'aces' for
        War Thunder, 'eldenring' for Elden Ring, 'launcher' for too many
        games to count) but its on-disk path includes the title, use the
        directory name as the app's friendly name. The current `comm` is
        passed in only so we can prefer it when nothing better is found."""
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
            if c in index:
                return index[c]
        return None

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
                return comm or None
            if not ppid or ppid in seen or ppid == "0":
                return comm or None
            seen.add(ppid)
            try:
                with open(f"/proc/{ppid}/comm", "r") as f:
                    parent_comm = f.read().strip()
            except OSError:
                return comm or None
            if parent_comm and parent_comm.lower() not in wrapper_set:
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

        # Filter out sink-inputs that look like the local machine itself.
        # PipeWire surfaces its own host (e.g. "DuskyPC") as an app whenever a
        # system-level stream — `speech-dispatcher`, the X11 bell, RTP receiver,
        # etc. — has nothing better to report. We are not an app on our own
        # mixer, so suppress anything whose visible name *is* our hostname.
        host = self._host_aliases()
        if host:
            check = (
                current.get('application.name', '').strip().lower(),
                current.get('node.description', '').strip().lower(),
                current.get('node.nick', '').strip().lower(),
                current.get('media.name', '').strip().lower(),
            )
            if any(c and c in host for c in check):
                return

        # Stable-first name resolution. Flatpak'd apps (Spotify, Discord…)
        # often set `application.name` to "audio-src" while their real
        # identity lives in FLATPAK_ID / cgroup / env, so we ALWAYS run
        # the sandbox probe first and let its result win over a generic
        # `application.name`.
        pid = current.get('pid') or current.get('application.process.id')
        sandbox_name = self._identify_sandboxed_app(pid)
        # Native (non-sandboxed) apps still publish a .desktop file with the
        # real display name. Read that — it's the truth about what AUR/dpkg
        # think the app is called — so e.g. native Spotify resolves to
        # 'Spotify' instead of 'audio-src' or 'spotify'.
        desktop_name = self._identify_via_desktop(pid)

        # Any of these reverse-DNS-style ids gets run through the curated
        # _KNOWN_APP_IDS table too.
        for key in ('flatpak.app_id', 'pipewire.access.portal.app_id',
                    'application.process.host', 'application.id',
                    'application.icon_name'):
            mapped = self._canonicalize_app_id(current.get(key))
            if mapped and mapped.lower() not in self._GENERIC_APP_NAMES:
                sandbox_name = sandbox_name or mapped
                break

        raw_app_name = current.get('application.name', '').strip()
        if raw_app_name.lower() in self._GENERIC_APP_NAMES:
            raw_app_name = ''

        candidates = [
            sandbox_name,
            desktop_name,
            raw_app_name,
            current.get('snap.name'),
            current.get('application.display_name'),
            current.get('application.process.binary'),
            current.get('binary'),
        ]
        name = next((c for c in candidates if c and c.strip()
                     and c.lower() not in self._GENERIC_APP_NAMES), None)

        if not name or name.lower() in self._GENERIC_APP_NAMES:
            proc_name = self._app_name_from_pid(pid)
            if proc_name:
                # The bare process name might still be a wrapper (e.g. 'aces'
                # for War Thunder) — let install-path inference upgrade it.
                inferred = self._infer_name_from_exe(pid, proc_name)
                name = inferred or proc_name

        if not name:
            name = current.get('node.name') or current.get('media.name') or f"App #{current.get('index', '?')}"

        # Strip common reverse-dns prefixes (org.mozilla.firefox → firefox)
        if '.' in name and ' ' not in name and len(name.split('.')) >= 2:
            name = name.rsplit('.', 1)[-1]
        name = name.replace('-', ' ').replace('_', ' ').strip()
        if name and name.islower():
            name = name.title()

        current['app_name'] = name or "Unknown App"
        entries.append(current)

    # Single source of truth for "0..1.0 is unity". Everything that writes
    # a volume into PipeWire clamps to 100% so audio can't silently clip
    # past unity.
    MAX_VOLUME = 1.0

    def _clamp(self, volume):
        try:
            return max(0.0, min(float(volume), self.MAX_VOLUME))
        except (TypeError, ValueError):
            return 1.0

    def move_app_to_sink(self, sink_input_index, sink_name):
        """Move a running app's sink-input to `sink_name`. `sink_name=None`
        means "System Default" — route back to whatever PipeWire calls the
        default sink right now."""
        if sink_name is None:
            sink_name = self.get_default_sink()
        if not sink_name:
            # If we still don't know where to send it, just leave it alone
            # rather than raising.
            return
        self._run(['pactl', 'move-sink-input', str(sink_input_index), sink_name])

    def set_sink_input_volume(self, sink_input_index, volume):
        """App-stream volume. pactl works on sink-input indices; wpctl
        wants numeric PipeWire node IDs which don't match, which is why
        the previous wpctl path silently no-opped."""
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

    def apply_clipguard(self, mix_name, enable):
        """Wave Link's 'Clipguard' is a master-bus limiter. Reuse the FX
        filter-chain with the limiter preset, keyed off the mix sink."""
        mix = self.output_mixes.get(mix_name)
        if not mix:
            return False
        channel_key = f'mix_{mix_name.lower()}'
        if enable:
            return self.apply_effect(channel_key, 'limiter')
        return self.remove_effect(channel_key, 'limiter')

    def is_clipguard_active(self, mix_name):
        return self.is_effect_active(f'mix_{mix_name.lower()}', 'limiter')

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
    # A `pipewire -c <conf>` invocation starts a NEW pipewire instance with
    # the given conf as its COMPLETE config (no merge with the system
    # default). For that instance to participate in the user's running
    # audio graph — i.e. for its filter-chain's Audio/Sink and Audio/Source
    # nodes to actually appear in `pactl list sinks` / `pactl list sources`
    # so we can pipe audio into them — the conf must:
    #
    #   1. Set `core.daemon = false` so it doesn't try to BE the system
    #      pipewire daemon.
    #   2. Load the basic SPA libs (`audioconvert`, `support`).
    #   3. Load the basic client modules (`rt`, `protocol-native`,
    #      `client-node`, `adapter`, `metadata`).
    #   4. THEN load `libpipewire-module-filter-chain` with our filter graph.
    #
    # Earlier versions of this file shipped only step 1, so the spawned
    # process either ran as an orphan audio system with no devices (and
    # thus processed nothing) or failed silently to register its nodes
    # with the running daemon. The user heard their mic untouched and
    # — with `noise_suppressor_mono` clearly installed — would understandably
    # conclude "the effects don't actually do anything".

    @staticmethod
    def _fx_client_config(client_id, filter_chain_args):
        """Build a complete `pipewire -c` config that runs as a CLIENT
        of the user's running daemon (not as a standalone audio system)
        and loads exactly one `libpipewire-module-filter-chain` whose
        args are the SPA-JSON object passed in.

        The module list and SPA libs match — exactly — PipeWire's own
        canonical `filter-chain.conf` (the file pipewire(1) loads when
        you run `pipewire -c filter-chain.conf`). That config is the
        upstream reference for "run a filter chain as a client of the
        running daemon", so anything more or less is a guess. In
        particular: do NOT add `libpipewire-module-metadata` here —
        canonical omits it, and on some setups loading metadata as a
        client of an existing daemon throws an ENOENT and aborts the
        entire client. We want filter-chain spawns to come up reliably,
        not depend on every distro's metadata-interface init."""
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

    # Old _FX_PREAMBLE-based callers still exist (apply_effect / start_rnnoise
    # are kept as a legacy path used by Clipguard). They get the same
    # complete client preamble via `_fx_client_config` now — preserved here
    # only as a stub so an unupgraded checkout doesn't NameError on import.
    _FX_PREAMBLE = ""

    @staticmethod
    def _fx_log_path(channel_key, effect_id):
        log_dir = os.path.expanduser('~/.config/wavelinux/fx-logs')
        os.makedirs(log_dir, exist_ok=True)
        return os.path.join(log_dir, f'{effect_id}-{channel_key}.log')

    def _spawn_fx(self, config_path, log_path, key):
        """Spawn a `pipewire -c <config>` process. Returns True if the
        process is still alive after a 1.5 s settle window, in which case
        it's parked in `rnnoise_processes[key]` for later teardown.

        The settle window is the only crash signal we have — a config
        with a bad LADSPA path / wrong port name / unknown SPA factory
        exits immediately, so waiting longer than the worst-case startup
        gives us a reliable pass/fail. 1.5 s is empirical: filter-chain
        spawn on a typical Linux system completes in ~250 ms; the headroom
        catches slower setups (Bluetooth audio waking up, NUMA-pinned
        rt scheduling, etc.) without leaving the UI feeling sluggish.

        We also dump a debug header into the log file (the config we ran,
        the env, the pipewire version) so post-mortem on a failed spawn
        doesn't require re-running anything — `cat <log>` shows what we
        tried and why it didn't fly. Without this header it's impossible
        to tell from outside whether a config-syntax error or a missing
        plugin or a daemon-connection failure is the culprit."""
        try:
            # Read config back so the debug header is the FILE we ran
            # (not whatever the caller intended). Avoids mismatch when
            # someone races us with a hand-edit.
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
            log_file = open(log_path, 'wb')
            header = (
                f"# WaveLinux FX spawn {key}\n"
                f"# pipewire --version: {pw_ver}\n"
                f"# config path:        {config_path}\n"
                f"# LADSPA_PATH env:    {os.environ.get('LADSPA_PATH', '')}\n"
                f"# ──────── config ─────────\n"
                f"{rendered_config}"
                f"\n# ──────── pipewire stderr/stdout ────────\n"
            )
            log_file.write(header.encode('utf-8'))
            log_file.flush()
            proc = subprocess.Popen(
                ['pipewire', '-c', config_path],
                stdout=log_file, stderr=log_file,
            )
        except FileNotFoundError:
            logging.error("`pipewire` binary not found — cannot spawn filter chain")
            return False
        try:
            proc.wait(timeout=1.5)
        except subprocess.TimeoutExpired:
            # Still running = success.
            self.rnnoise_processes[key] = proc
            return True
        logging.error(f"FX process for {key} exited immediately; see {log_path}")
        return False

    def start_rnnoise(self, channel_key='default', params=None):
        """Legacy single-effect rnnoise spawn (kept for backwards-compat).
        New code should go through `set_channel_fx` which routes the mic
        through the chain explicitly. This path is only used when something
        directly calls `apply_effect('rnnoise', …)` — currently nothing
        in-tree does so."""
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
            ('threshold_db', 'Threshold', -60.0, 0.0, -20.0, ' dB'),
            ('ratio', 'Ratio', 1.0, 20.0, 4.0, ':1'),
            ('attack_ms', 'Attack', 0.1, 200.0, 5.0, ' ms'),
            ('release_ms', 'Release', 5.0, 1000.0, 100.0, ' ms'),
            ('makeup_gain_db', 'Makeup', 0.0, 24.0, 0.0, ' dB'),
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
            ('Release time (s)', 'Release', 0.01, 2.0, 0.1, ' s'),
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
            "'Ceiling' at -1 dB for broadcast. Release sets how quickly it "
            "recovers — too fast sounds pumpy, too slow ducks audio.",
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
             {"threshold_db": -20.0, "ratio": 2.0,
              "attack_ms": 10.0, "release_ms": 120.0, "makeup_gain_db": 2.0}),
            ("Broadcast 4:1",
             {"threshold_db": -18.0, "ratio": 4.0,
              "attack_ms": 5.0, "release_ms": 100.0, "makeup_gain_db": 3.0}),
            ("Streaming 6:1",
             {"threshold_db": -16.0, "ratio": 6.0,
              "attack_ms": 3.0, "release_ms": 80.0, "makeup_gain_db": 4.0}),
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
             {"Input gain (dB)": 0.0, "Limit (dB)": -3.0, "Release time (s)": 0.2}),
            ("Broadcast -1 dB",
             {"Input gain (dB)": 0.0, "Limit (dB)": -1.0, "Release time (s)": 0.1}),
            ("Loud -0.5 dB",
             {"Input gain (dB)": 3.0, "Limit (dB)": -0.5, "Release time (s)": 0.05}),
        ],
    }

    @classmethod
    def get_effect_help(cls, effect_id):
        return cls._EFFECT_HELP.get(effect_id, "")

    @classmethod
    def get_effect_presets(cls, effect_id):
        return list(cls._EFFECT_PRESETS.get(effect_id, []))

    def _resolved_params(self, effect_id, overrides):
        """Merge user overrides on top of defaults from _EFFECT_PARAMS."""
        out = {key: default for (key, _l, _mn, _mx, default, _u)
               in self._EFFECT_PARAMS.get(effect_id, [])}
        if overrides:
            for k, v in overrides.items():
                if k in out:
                    out[k] = float(v)
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
        """Render a single LADSPA node block for filter.graph, using the
        full filesystem path to the .so when we can find it. Falls back
        to the bare name (which pipewire resolves via $LADSPA_PATH) when
        the plugin isn't on disk in any path we know about."""
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
        """Render the `filter.graph` body for a given effect. Returns None for
        unknown effect ids. Shared by both the legacy single-effect spawn
        path (used by clipguard) and the per-stage chain path (`set_channel_fx`)."""
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
            return self._ladspa_node('compressor', 'sc4_1882', 'sc4', values)
        if effect_id == 'limiter':
            # Prefer the swh-plugins fast lookahead limiter when present —
            # real lookahead, real release. Without it, fall back to a
            # builtin chain: a `linear` gain stage for the input-gain
            # parameter, then `clamp` set to the user-chosen ceiling.
            # That isn't a true broadcast limiter (no soft knee, no
            # release behaviour) but it stops audio from clipping, which
            # is what Clipguard exists to do.
            if self.ladspa_plugin_available('fast_lookahead_limiter_1913'):
                return self._ladspa_node('limiter', 'fast_lookahead_limiter_1913',
                                         'fastLookaheadLimiter', values)
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
        """Apply a single effect via filter-chain. Used by the master-bus
        clipguard (limiter on the Stream mix). For per-channel mic effects,
        prefer `set_channel_fx` — it builds a real chain and routes the mic
        through it, where this just spawns a free-floating filter-chain."""
        # rnnoise keeps its dedicated config path because the original
        # codebase shipped it as the default mic effect — keeping the file
        # name predictable means stale configs from prior runs don't pile up.
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
    # The legacy apply_effect / remove_effect / is_effect_active functions
    # spawn a free-floating filter-chain that processes audio it never
    # actually sees. The chain API below replaces that with a real bus:
    # each enabled effect is its own filter-chain process exposing both
    # an Audio/Sink (its input) and an Audio/Source (its output). We wire
    # the chain together with explicit `module-loopback` modules — same
    # mechanism we use for submix routing — so the mic feeds stage 0's
    # sink, stage N's source feeds stage N+1's sink, and the final
    # stage's source is what the submix loopbacks pull from. That last
    # link is what makes the effects audible: without it the chain runs
    # but processes nothing.

    # Order to apply effects in the chain. Anything not in this list
    # appears at the end in user-specified order.
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

    def _build_fx_stage_config(self, safe_key, idx, effect_id, params):
        """Write a per-stage filter-chain config to disk and return its
        path along with the stage's input-sink and output-source node
        names. Each stage exposes BOTH:

          - a `media.class = Audio/Sink`   on capture.props (the stage
            sink — anything routed here gets processed),
          - a `media.class = Audio/Source` on playback.props (the stage
            source — what the next stage / submix loopback consumes).

        Connecting them is then a normal `module-loopback` from the
        upstream source into this stage's sink — the same machinery
        we already use for submix routing. Robust across pipewire
        versions; doesn't depend on `target.object` binding.

        Returns (config_path, sink_name, source_name) or (None, None, None)
        for an unknown effect."""
        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)

        values = self._resolved_params(effect_id, params)
        filter_graph = self._build_filter_graph(effect_id, values)
        if filter_graph is None:
            return None, None, None

        sink_name   = f'wavelinux.fx.{safe_key}.{idx}.{effect_id}.input'
        source_name = f'wavelinux.fx.{safe_key}.{idx}.{effect_id}.source'

        client_id = f'{safe_key}-{idx}-{effect_id}'
        # `node.always-process = true` forces the filter chain to run even
        # when nothing is currently pulling on the playback side or pushing
        # on the capture side. Without it, pipewire is free to suspend the
        # chain until both sides have an active link — which during chain
        # construction is briefly true and creates a chicken-and-egg with
        # the loopback wiring (the loopback is what creates the link, but
        # the link is what activates the chain).
        # `audio.position = [ MONO ]` pins the channel layout to a single
        # channel so a stereo LADSPA plugin doesn't get spliced onto a
        # mono mic with one half of its inputs floating.
        filter_chain_args = f"""{{
            node.description = "WaveLinux-{effect_id} ({safe_key}#{idx})"
            media.name       = "WaveLinux-{effect_id} ({safe_key}#{idx})"
            filter.graph = {{{filter_graph}
            }}
            capture.props = {{
                node.name           = "{sink_name}"
                media.class         = Audio/Sink
                audio.rate          = 48000
                audio.position      = [ MONO ]
                node.always-process = true
            }}
            playback.props = {{
                node.name           = "{source_name}"
                media.class         = Audio/Source
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
        up to ~2 s while pipewire-pulse registers a freshly-spawned node.
        Inter-stage FX wiring would race the spawn otherwise: pipewire-pulse
        catalogues the new sink a beat after the filter-chain process comes
        up, and a one-shot `pactl load-module` sees an unknown sink and
        fails. Returns the loopback module id (str) or None.

        `latency_msec=20` matches the default the rest of the codebase
        uses for submix loopbacks. Lower values (we briefly used 5) were
        unstable on a couple of test rigs — pulse-bridge would create the
        module successfully but the chain wouldn't actually flow audio,
        because filter-chain's internal scheduler couldn't keep up at
        a 5ms quantum. 20 ms is quiet enough for live monitoring (well
        under the perception threshold for self-monitoring) and reliable."""
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
        """Replace this channel's effect chain. Tears down whatever was
        running, then spawns one filter-chain process per enabled effect.
        Each stage exposes a sink (input) and a source (output); we
        wire the chain explicitly with `module-loopback` modules — same
        pattern we use for submix routing, so it doesn't rely on
        `target.object` semantics that vary by pipewire version.

        Returns the final source node.name on success (at least one stage
        spawned and was loopback-ed in), else None.

        - `node_name`: stable PipeWire node.name. State is keyed by this so
          chains survive PipeWire restarts.
        - `capture_target`: the source the FIRST stage should receive
          audio from. For mics this is the mic's node.name; for virtual
          sinks pass `f"{sink_name}.monitor"`.
        - `effects`: effect_id list; reordered to canonical signal flow.
        - `params_map`: {effect_id: {param_key: value}}.
        """
        if not node_name:
            return None
        params_map = params_map or {}

        # Always reset first — makes the call idempotent and gives a clean
        # baseline when respawning after a parameter change.
        self.clear_channel_fx(node_name)

        ordered = [fid for fid in self._ordered_chain(effects)
                   if self.effect_available(fid)]
        if not ordered:
            return None

        safe_key = self._safe_channel_key(node_name)
        proc_keys     = []   # filter-chain stage process keys
        loopback_ids  = []   # module-loopback ids wiring stages together
        upstream_src  = capture_target

        for i, effect_id in enumerate(ordered):
            params = params_map.get(effect_id) or None
            config_path, sink_name, source_name = self._build_fx_stage_config(
                safe_key, i, effect_id, params,
            )
            if config_path is None:
                continue

            log_path = self._fx_log_path(safe_key, f'{i}_{effect_id}')
            proc_key = f'chain_{safe_key}_{i}_{effect_id}'

            if not self._spawn_fx(config_path, log_path, proc_key):
                logging.warning(
                    f"FX chain stage {i} ({effect_id}) failed to spawn for "
                    f"{node_name}; remaining stages will be skipped."
                )
                # Stop here — chaining over a missing stage produces silence.
                break

            # Wire upstream → this stage's input sink. Without this loopback
            # the stage processes silence and the user hears nothing.
            lb = self._wait_load_loopback(upstream_src, sink_name)
            if lb is None:
                logging.warning(
                    f"FX loopback {upstream_src} → {sink_name} failed; "
                    f"chain stage {i} ({effect_id}) is dangling."
                )
                # Kill the orphan stage and stop building the chain.
                self.stop_rnnoise(proc_key)
                break

            proc_keys.append(proc_key)
            loopback_ids.append(lb)
            upstream_src = source_name

        if not proc_keys:
            return None

        final_source = upstream_src
        applied_effects = list(ordered[:len(proc_keys)])
        self.channel_fx[node_name] = {
            'effects':   applied_effects,
            'params':    {fid: dict(params_map.get(fid, {}))
                          for fid in applied_effects},
            'procs':     list(proc_keys),
            'loopbacks': list(loopback_ids),
            'source':    final_source,
            'capture_target': capture_target,
            'safe_key':  safe_key,
        }
        return final_source

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

        # 1. Drop submix loopbacks whose source is part of this channel's
        # chain — without this, route_input_to_submix would happily keep
        # using the cached loopback (still alive in `pactl list modules`)
        # even though its source was just unloaded, and the user gets
        # silence on the submix until the next chain spawn re-uses the
        # exact same source name. We can't compare by source identity
        # cheaply, so we match the well-known FX-source prefix.
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
        """Diagnostic snapshot of a channel's FX chain. Returns a dict of
        `{effect_id: {'state': 'running' | 'failed' | 'inactive',
                      'log': <path or None>}}`.

        - 'running'  = the stage's filter-chain process is alive AND its
                       inbound module-loopback exists.
        - 'failed'   = we attempted to spawn it but the process died, OR
                       the inbound loopback couldn't be created. The log
                       path points at the per-stage spawn log so the UI
                       can offer a "click to inspect" hint.
        - 'inactive' = the user never enabled this effect (or it was
                       cleared). No log to show.

        Used by the FX dialog to show a red border + tooltip on toggles
        whose chain didn't actually come up — without that, a failed
        spawn is invisible and the user understandably thinks the
        feature is broken when it's really just missing a plugin."""
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
        """Hard cleanup of all wavelinux PipeWire modules."""
        # Tear down channel chains first so their stage processes go away
        # cleanly (they're tracked in rnnoise_processes too — clear_channel_fx
        # routes through stop_rnnoise — but doing them via the chain API
        # also frees the channel_fx state).
        for nname in list(self.channel_fx.keys()):
            self.clear_channel_fx(nname)
        # Anything still parked in rnnoise_processes (legacy single-effect
        # spawns, master-bus clipguard) gets the same treatment.
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
