"""
PipeWire Engine — handles all audio routing, virtual sinks, volume control,
multiple output mixes, effects chains, and RNNoise noise suppression.
"""

import subprocess
import json
import os
import signal
import re
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


class EffectSlot:
    """An effect slot in a channel's FX chain."""
    def __init__(self, effect_type, label, params=None):
        self.effect_type = effect_type  # 'eq', 'compressor', 'gate', 'rnnoise', 'ladspa'
        self.label = label
        self.params = params or {}
        self.enabled = True
        self.process = None
        self.filter_id = None


class PipeWireEngine:
    """Full-featured PipeWire audio engine."""

    def __init__(self):
        self.virtual_sink_modules = {}   # safe_name -> pactl module id
        self.output_mixes = {}           # mix_name -> OutputMix
        self.channel_effects = {}        # channel_key -> [EffectSlot]
        self.app_groups = {}             # group_name -> [app_name, ...]
        self.rnnoise_processes = {}      # channel_key -> subprocess
        self.loopback_modules = {}       # "mix_name->hw_name" -> module id
        self.submix_loopbacks = {}       # "node_id->mix_name" -> module id
        
        # Ensure clean state from any previous crashes
        self.cleanup()

    # ── Helpers ─────────────────────────────────────────────────────

    def _run(self, cmd, timeout=2):
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

    @staticmethod
    def friendly_name(raw):
        if not raw:
            return "Unknown"
        name = raw
        
        # Strip common ALSA prefixes
        for prefix in ['Alsa Output.', 'Alsa Input.', 'alsa_output.', 'alsa_input.']:
            if name.lower().startswith(prefix.lower()):
                name = name[len(prefix):]
                
        # Remove PCI addresses
        name = re.sub(r'pci-[0-9a-fA-F._-]+\.', '', name, flags=re.IGNORECASE)
        name = re.sub(r'Pci-[0-9a-fA-F. -]+Platform-\w+\s*', '', name, flags=re.IGNORECASE)
        
        # Strip verbose hardware descriptions
        verbose_terms = [
            'High Definition Audio Controller',
            'HD Audio Controller',
            'Raptor Lake', 'Alder Lake', 'Comet Lake', 'Tiger Lake',
            'Starship/Matisse', 'Family 17h', 'Family 19h',
            'USB Audio', 'Generic'
        ]
        for term in verbose_terms:
            name = re.sub(r'\b' + term + r'\b', '', name, flags=re.IGNORECASE)
            
        # Clean up separators
        name = name.replace('_', ' ').replace('.', ' ').replace('-', ' ')
        
        # Remove duplicate spaces
        name = re.sub(r'\s+', ' ', name).strip()
        
        if name:
            name = name.title()
            
        # Truncate if still too long
        if len(name) > 24:
            # If it's very long, maybe just grab the last words which often contain the actual profile (e.g. Analog Stereo)
            parts = name.split()
            if len(parts) > 3:
                name = " ".join(parts[-3:])
            if len(name) > 24:
                name = name[:22] + '…'
                
        return name or raw

    # ── Node Discovery ──────────────────────────────────────────────

    def get_all_nodes(self):
        raw = self._run(['pw-dump'])
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

    def get_hardware_outputs(self):
        return [n for n in self.get_all_nodes()
                if n.media_class == 'Audio/Sink'
                and not n.name.startswith('wavelinux_')]

    def get_hardware_inputs(self):
        return [n for n in self.get_all_nodes()
                if n.media_class == 'Audio/Source'
                and 'rnnoise' not in n.name.lower()
                and not n.name.startswith('wavelinux_')]

    def get_virtual_sinks(self):
        """User-created WaveLinux channels only (no internal mix/source sinks)."""
        all_nodes = self.get_all_nodes()
        return [n for n in all_nodes
                if n.media_class == 'Audio/Sink'
                and n.name in self.virtual_sink_modules
                and not n.name.startswith('wavelinux_mix_')
                and not n.name.startswith('wavelinux_src_')]

    def get_app_streams(self):
        return [n for n in self.get_all_nodes()
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
        pct = max(0, min(int(round(volume * 100)), 150))
        self._run(['pactl', 'set-sink-volume', sink_name, f'{pct}%'])

    def get_sink_volume_by_name(self, sink_name):
        out = self._run(['pactl', 'get-sink-volume', sink_name])
        if not out:
            return 1.0, False
        # "Volume: front-left: 65536 / 100% / 0.00 dB,   front-right: ..."
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

    def route_input_to_submix(self, node_id, node_name, media_class, mix_name):
        """Create a loopback connecting an input source (or sink monitor) to a submix."""
        key = f'{node_id}->{mix_name}'
        
        mix = self.output_mixes.get(mix_name)
        if not mix: return False
        
        source_id = str(node_id)
        if media_class == 'Audio/Sink':
            source_id = f"{node_name}.monitor"

        # Check if already exists
        existing = self._find_module_by_arg(f"source={source_id}")
        if existing:
            # Verify sink matches
            full_info = self._run(['pactl', 'list', 'modules'])
            if full_info and f"sink={mix.sink_name}" in full_info.split(f"Module #{existing}")[1].split("Module #")[0]:
                self.submix_loopbacks[key] = existing
                return True

        out = self._run([
            'pactl', 'load-module', 'module-loopback',
            f'source={source_id}',
            f'sink={mix.sink_name}',
            'latency_msec=20',
            'adjust_time=0'
        ])
        if out:
            self.submix_loopbacks[key] = out
            # Ensure unmuted/100%
            si = self.get_submix_sink_input(node_id, mix_name)
            if si:
                self._run(['pactl', 'set-sink-input-volume', si, '100%'])
                self._run(['pactl', 'set-sink-input-mute', si, '0'])
            return True
        return False

    def remove_node_routing(self, node_id):
        """Clean up all loopbacks associated with a removed node."""
        node_id = str(node_id)
        for key in list(self.submix_loopbacks.keys()):
            if key.startswith(f"{node_id}->"):
                mod_id = self.submix_loopbacks.pop(key)
                self._run(['pactl', 'unload-module', str(mod_id)])

    def get_submix_sink_input(self, node_id, mix_name):
        module_id = str(self.submix_loopbacks.get(f'{node_id}->{mix_name}'))
        if not module_id or module_id == 'None':
            return None
            
        # Use pactl list sink-inputs and parse for module.id
        out = self._run(['pactl', 'list', 'sink-inputs'])
        if out:
            current_si = None
            for line in out.splitlines():
                line = line.strip()
                if line.startswith('Sink Input #'):
                    current_si = line.split('#')[1].strip()
                elif 'module.id =' in line and f'"{module_id}"' in line:
                    return current_si
                elif 'Owner Module:' in line and module_id in line:
                    return current_si
        return None

    def set_submix_volume(self, node_id, mix_name, volume):
        si = self.get_submix_sink_input(node_id, mix_name)
        if si:
            self._run(['pactl', 'set-sink-input-volume', si, f'{int(volume*100)}%'])
        else:
            logging.warning(f"Could not find sink-input for {node_id}->{mix_name}")

    def set_submix_mute(self, node_id, mix_name, mute):
        si = self.get_submix_sink_input(node_id, mix_name)
        if si:
            self._run(['pactl', 'set-sink-input-mute', si, '1' if mute else '0'])
        else:
            logging.warning(f"Could not find sink-input to mute for {node_id}->{mix_name}")

    # ── Output Mix Management ──────────────────────────────────────

    @staticmethod
    def _sanitize_channel_name(display_name):
        """Turn 'Game  ' into 'game', '  My  Mic ' into 'my_mic'."""
        cleaned = re.sub(r'\s+', ' ', (display_name or '').strip())
        safe = re.sub(r'[^A-Za-z0-9_]+', '_', cleaned.lower()).strip('_')
        return cleaned, safe or 'channel'

    def create_virtual_sink(self, display_name, custom_name=None):
        """Create a virtual sink (null-sink). Returns the sink name on success."""
        display_clean, safe_tail = self._sanitize_channel_name(display_name)
        safe_name = custom_name or f"wavelinux_{safe_tail}"
        description = f"WaveLinux {display_clean}" if display_clean else "WaveLinux"

        existing = self._find_module_by_arg(f"sink_name={safe_name}")
        if existing:
            logging.info(f"Using existing sink {safe_name} (ID: {existing})")
            # Only track user-created sinks; mix internals are tracked separately.
            if not safe_name.startswith('wavelinux_mix_'):
                self.virtual_sink_modules[safe_name] = existing
            return safe_name

        # Escape any embedded double-quote in the description so sink_properties parses.
        desc_escaped = description.replace('"', '\\"')
        cmd = [
            "pactl", "load-module", "module-null-sink",
            f"sink_name={safe_name}",
            f'sink_properties=device.description="{desc_escaped}" application.name="WaveLinux" media.class=Audio/Sink'
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
        can pick it up as a dedicated recording device (e.g. 'WaveLinux Stream')."""
        _, safe_name = self._sanitize_channel_name(name)
        sink_name = f"wavelinux_mix_{safe_name}"
        source_name = f"wavelinux_src_{safe_name}"
        description = f"WaveLinux {name}"
        desc_escaped = description.replace('"', '\\"')

        # 1. Sink (the thing apps play *to*).
        if self.create_virtual_sink(name, custom_name=sink_name) is None:
            return None
        sink_module_id = (self.virtual_sink_modules.get(sink_name)
                          or self._find_module_by_arg(f"sink_name={sink_name}"))

        # 2. Dedicated recording source so OBS / browsers see a named device
        # instead of a generic "Monitor of null sink".
        src_module_id = self._find_module_by_arg(f"source_name={source_name}")
        if not src_module_id:
            src_module_id = self._run([
                'pactl', 'load-module', 'module-virtual-source',
                f'source_name={source_name}',
                f'master={sink_name}.monitor',
                (f'source_properties=device.description="{desc_escaped}" '
                 f'application.name="WaveLinux" media.class=Audio/Source '
                 f'device.class=sound node.nick="{desc_escaped}"'),
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

    def get_sink_inputs(self):
        sinks = self.get_all_sinks()
        sink_id_to_name = {s['index']: s['name'] for s in sinks}
        
        out = self._run(['pactl', 'list', 'sink-inputs'])
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
    }

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
        """Resolve a friendly name for Flatpak/Snap/AppImage wrappers."""
        if not pid:
            return None
        env = self._read_proc_env(pid)

        # Flatpak exposes its app id via env and/or /.flatpak-info
        flatpak_id = env.get("FLATPAK_ID")
        if not flatpak_id:
            try:
                with open(f"/proc/{pid}/root/.flatpak-info", "r") as f:
                    for line in f:
                        if line.startswith("name="):
                            flatpak_id = line.split("=", 1)[1].strip()
                            break
            except OSError:
                pass
        if flatpak_id:
            # com.discordapp.Discord → Discord
            tail = flatpak_id.rsplit('.', 1)[-1]
            return tail.replace('-', ' ').replace('_', ' ').strip() or flatpak_id

        # Snap exposes SNAP_NAME / SNAP_INSTANCE_NAME
        snap_name = env.get("SNAP_INSTANCE_NAME") or env.get("SNAP_NAME")
        if snap_name:
            return snap_name.replace('-', ' ').replace('_', ' ').title()

        cgroup = self._read_proc_cgroup(pid)
        m = re.search(r'snap\.([A-Za-z0-9_-]+)', cgroup)
        if m:
            return m.group(1).replace('-', ' ').replace('_', ' ').title()
        m = re.search(r'app-flatpak-([A-Za-z0-9_.+-]+?)-\d+\.scope', cgroup)
        if m:
            return m.group(1).rsplit('.', 1)[-1].replace('-', ' ').strip()

        # AppImage mounts under /tmp/.mount_* — fall through to comm lookup
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
            'rnnoise' in node_name or
            'loopback' in node_name or
            'wavelinux_mix' in media_name
        )
        if is_internal:
            return

        # Stable-first name resolution. We deliberately try the sandbox id
        # BEFORE application.name, because a Flatpak'd app often sets
        # application.name to a generic "audio-src" while the real identity
        # lives in FLATPAK_ID / cgroup.
        pid = current.get('pid') or current.get('application.process.id')

        sandbox_name = None
        raw_app_name = current.get('application.name', '').strip()
        if not raw_app_name or raw_app_name.lower() in self._GENERIC_APP_NAMES:
            sandbox_name = self._identify_sandboxed_app(pid)

        candidates = [
            sandbox_name,
            current.get('flatpak.app_id'),
            current.get('pipewire.access.portal.app_id'),
            current.get('snap.name'),
            raw_app_name if raw_app_name.lower() not in self._GENERIC_APP_NAMES else None,
            current.get('application.process.binary'),
            current.get('binary'),
        ]
        name = next((c for c in candidates if c and c.strip()), None)

        if not name or name.lower() in self._GENERIC_APP_NAMES:
            proc_name = self._app_name_from_pid(pid)
            if proc_name:
                name = proc_name

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

    def move_app_to_sink(self, sink_input_index, sink_name):
        self._run(['pactl', 'move-sink-input', str(sink_input_index), sink_name])

    def set_sink_input_volume(self, sink_input_index, volume):
        # volume is 0.0 to 1.0
        self._run(['wpctl', 'set-volume', str(sink_input_index), str(volume)])

    def get_sink_input_volume(self, sink_input_index):
        # wpctl get-volume returns "Volume: 0.50"
        out = self._run(['wpctl', 'get-volume', str(sink_input_index)])
        if out and 'Volume:' in out:
            try:
                return float(out.split(':', 1)[1].strip().split()[0])
            except:
                pass
        return 1.0

    def get_all_sinks(self):
        out = self._run(['pactl', 'list', 'short', 'sinks'])
        if not out:
            return []
        sinks = []
        for line in out.splitlines():
            parts = line.split('\t')
            if len(parts) >= 2:
                sinks.append({'index': parts[0], 'name': parts[1]})
        return sinks

    # ── App Grouping ───────────────────────────────────────────────

    def create_app_group(self, group_name, app_names):
        """Group multiple apps under one channel name."""
        self.app_groups[group_name] = list(app_names)

    def remove_app_group(self, group_name):
        if group_name in self.app_groups:
            del self.app_groups[group_name]

    def add_app_to_group(self, group_name, app_name):
        if group_name not in self.app_groups:
            self.app_groups[group_name] = []
        if app_name not in self.app_groups[group_name]:
            self.app_groups[group_name].append(app_name)

    def remove_app_from_group(self, group_name, app_name):
        if group_name in self.app_groups and app_name in self.app_groups[group_name]:
            self.app_groups[group_name].remove(app_name)

    # ── Effects / RNNoise ──────────────────────────────────────────

    # Boilerplate that keeps spawned `pipewire -c` processes from trying to
    # become the system daemon and re-route all audio.
    _FX_PREAMBLE = """\
context.properties = {
    core.daemon = false
    core.name   = wavelinux-fx
    log.level   = 2
}
"""

    @staticmethod
    def _fx_log_path(channel_key, effect_id):
        log_dir = os.path.expanduser('~/.config/wavelinux/fx-logs')
        os.makedirs(log_dir, exist_ok=True)
        return os.path.join(log_dir, f'{effect_id}-{channel_key}.log')

    def _spawn_fx(self, config_path, log_path, key):
        try:
            log_file = open(log_path, 'wb')
            proc = subprocess.Popen(
                ['pipewire', '-c', config_path],
                stdout=log_file, stderr=log_file,
            )
        except FileNotFoundError:
            logging.error("`pipewire` binary not found — cannot spawn filter chain")
            return False
        # Give it a moment to fail loudly (missing plugin, syntax error, etc.)
        try:
            proc.wait(timeout=0.4)
        except subprocess.TimeoutExpired:
            # Still running = success.
            self.rnnoise_processes[key] = proc
            return True
        logging.error(f"FX process for {key} exited immediately; see {log_path}")
        return False

    def start_rnnoise(self, channel_key='default'):
        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)
        config_path = os.path.join(config_dir, f'wavelinux-rnnoise-{channel_key}.conf')
        config = self._FX_PREAMBLE + f"""
context.modules = [
    {{ name = libpipewire-module-filter-chain
        args = {{
            node.description = "WaveLinux Denoise ({channel_key})"
            media.name       = "WaveLinux Denoise ({channel_key})"
            filter.graph = {{
                nodes = [
                    {{
                        type   = ladspa
                        name   = rnnoise
                        plugin = librnnoise_ladspa
                        label  = noise_suppressor_mono
                        control = {{
                            "VAD Threshold (%)" = 50.0
                        }}
                    }}
                ]
            }}
            capture.props = {{
                node.name    = "wavelinux.rnnoise.{channel_key}.capture"
                node.passive = true
                audio.rate   = 48000
            }}
            playback.props = {{
                node.name    = "wavelinux.rnnoise.{channel_key}.source"
                media.class  = Audio/Source
                audio.rate   = 48000
            }}
        }}
    }}
]
"""
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
            {'id': 'compressor', 'name': 'Compressor', 'icon': '📉',
             'desc': 'Smooth out loud/quiet differences'},
            {'id': 'gate', 'name': 'Noise Gate', 'icon': '🚪',
             'desc': 'Cut audio below a threshold'},
            {'id': 'limiter', 'name': 'Limiter', 'icon': '🛡️',
             'desc': 'Prevent audio clipping'},
        ]

    def apply_effect(self, channel_key, effect_id):
        """Apply a built-in effect to a channel via filter-chain."""
        if effect_id == 'rnnoise':
            return self.start_rnnoise(channel_key)
        # For other effects, we create PipeWire filter chains
        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)

        if effect_id == 'gate':
            # gate_1410 ships in swh-plugins (which install.sh already installs).
            filter_graph = """
                nodes = [
                    {
                        type  = ladspa
                        name  = gate
                        plugin = gate_1410
                        label = gate
                        control = {
                            "LF key filter (Hz)" = 100.0
                            "HF key filter (Hz)" = 10000.0
                            "Threshold (dB)" = -40.0
                            "Attack (ms)" = 2.5
                            "Hold (ms)" = 10.0
                            "Decay (ms)" = 200.0
                            "Range (dB)" = -40.0
                        }
                    }
                ]
"""
        elif effect_id == 'compressor':
            filter_graph = """
                nodes = [
                    {
                        type  = ladspa
                        name  = compressor
                        plugin = sc4_1882
                        label = sc4
                        control = {
                            "threshold_db" = -20.0
                            "ratio" = 4.0
                            "attack_ms" = 5.0
                            "release_ms" = 100.0
                            "makeup_gain_db" = 0.0
                        }
                    }
                ]
"""
        elif effect_id == 'limiter':
            filter_graph = """
                nodes = [
                    {
                        type  = ladspa
                        name  = limiter
                        plugin = fast_lookahead_limiter_1913
                        label = fastLookaheadLimiter
                        control = {
                            "Input gain (dB)" = 0.0
                            "Limit (dB)" = -1.0
                            "Release time (s)" = 0.1
                        }
                    }
                ]
"""
        else:
            return False  # Unknown effect

        config_path = os.path.join(config_dir, f'wavelinux-fx-{channel_key}-{effect_id}.conf')
        config = self._FX_PREAMBLE + f"""
context.modules = [
    {{ name = libpipewire-module-filter-chain
        args = {{
            node.description = "WaveLinux {effect_id} ({channel_key})"
            media.name       = "WaveLinux {effect_id} ({channel_key})"
            filter.graph = {{{filter_graph}
            }}
            capture.props = {{
                node.name    = "wavelinux.fx.{channel_key}.{effect_id}.capture"
                node.passive = true
                audio.rate   = 48000
            }}
            playback.props = {{
                node.name    = "wavelinux.fx.{channel_key}.{effect_id}.source"
                media.class  = Audio/Source
                audio.rate   = 48000
            }}
        }}
    }}
]
"""
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

    def _find_module_by_arg(self, pattern):
        """Find a pactl module ID by its arguments."""
        out = self._run(['pactl', 'list', 'modules'])
        if not out: return None
        
        curr_id = None
        for line in out.splitlines():
            line = line.strip()
            if line.startswith('Module #'):
                curr_id = line.split('#')[1].strip()
            if 'Argument:' in line and pattern in line:
                return curr_id
        return None

    # ── Cleanup ────────────────────────────────────────────────────

    def cleanup(self):
        """Hard cleanup of all wavelinux PipeWire modules."""
        # Clean local process trackers
        for key in list(self.rnnoise_processes.keys()):
            self.stop_rnnoise(key)
            
        self.virtual_sink_modules.clear()
        self.output_mixes.clear()
        self.loopback_modules.clear()
        self.submix_loopbacks.clear()

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
