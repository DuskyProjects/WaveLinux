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

logging.basicConfig(
    filename=os.path.expanduser("~/.config/wavelinux/wavelinux.log"),
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
        all_nodes = self.get_all_nodes()
        return [n for n in all_nodes
                if n.media_class == 'Audio/Sink'
                and n.name in self.virtual_sink_modules]

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

    # ── Virtual Sink (Input Channel) Management ────────────────────

    def create_virtual_sink(self, display_name):
        safe_name = 'wavelinux_' + display_name.replace(' ', '_').lower()
        
        # Clean up any existing zombie module with this name first
        self._run(['pactl', 'unload-module', safe_name])

        out = self._run([
            'pactl', 'load-module', 'module-null-sink',
            f'sink_name={safe_name}',
            f'sink_properties=device.description="WaveLinux {display_name}" application.name="WaveLinux"'
        ])
        if out:
            # Ensure it's unmuted and at 100% volume
            self._run(['pactl', 'set-sink-mute', safe_name, '0'])
            self._run(['pactl', 'set-sink-volume', safe_name, '100%'])
            
            self.virtual_sink_modules[safe_name] = out
            return safe_name
        return None

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

    def create_virtual_sink(self, display_name, custom_name=None):
        """Create a virtual sink (null-sink). Returns module ID."""
        safe_name = custom_name or f"wavelinux_{display_name.lower().replace(' ', '_')}"
        
        # Check if already exists
        existing = self._find_module_by_arg(f"sink_name={safe_name}")
        if existing:
            logging.info(f"Using existing sink {safe_name} (ID: {existing})")
            return existing

        cmd = [
            "pactl", "load-module", "module-null-sink",
            f"sink_name={safe_name}",
            f"sink_properties=device.description='WaveLinux {display_name}' application.name='WaveLinux'"
        ]
        out = self._run(cmd)
        if out:
            # Ensure unmuted/100%
            self._run(['pactl', 'set-sink-mute', safe_name, '0'])
            self._run(['pactl', 'set-sink-volume', safe_name, '100%'])
            self.virtual_sink_modules[safe_name] = out
            return out
        return None

    def create_output_mix(self, name):
        """Create a virtual sink and a virtual source (recording device) for a mix."""
        safe_name = name.lower().replace(' ', '_')
        sink_name = f"wavelinux_mix_{safe_name}"
        source_name = f"wavelinux_src_{safe_name}"
        
        # 1. Create Sink
        sink_id = self.create_virtual_sink(name, custom_name=sink_name)
        if not sink_id:
            return None
        
        # 2. Create Virtual Source (Recording Device)
        # Check if already exists
        src_id = self._find_module_by_arg(f"source_name={source_name}")
        if not src_id:
            src_id = self._run([
                'pactl', 'load-module', 'module-virtual-source',
                f'source_name={source_name}',
                f'master={sink_name}.monitor',
                f'source_properties=device.description="WaveLinux {name}"'
            ])
        
        mix = OutputMix(name, sink_module_id=sink_id, sink_name=sink_name)
        self.output_mixes[name] = mix
        return mix

    def remove_output_mix(self, mix_name):
        mix = self.output_mixes.get(mix_name)
        if mix and mix.sink_module_id:
            self._run(['pactl', 'unload-module', mix.sink_module_id])
            # Remove any loopback routing
            for key in list(self.loopback_modules.keys()):
                if key.startswith(mix_name + '->'):
                    self._run(['pactl', 'unload-module', self.loopback_modules[key]])
                    del self.loopback_modules[key]
            del self.output_mixes[mix_name]
            return True
        return False

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

    def _process_sink_input(self, current, entries, sink_id_to_name):
        # Resolve sink name
        sink_id = current.get('sink_id')
        current['sink'] = sink_id_to_name.get(sink_id, sink_id)
        
        # Filter out internal wavelinux loopbacks/effects, but NOT the apps playing to them!
        # Internal modules usually have specific node.name or media.class
        node_name = current.get('node.name', '').lower()
        media_name = current.get('media.name', '').lower()
        is_internal = (
            'wavelinux_mix' in node_name or 
            'rnnoise' in node_name or 
            'loopback' in node_name or
            'wavelinux_mix' in media_name
        )
        
        # Refined app name logic - prioritize STABLE names
        name = (
            current.get('application.name') or 
            current.get('application.id') or                # Flatpak ID
            current.get('flatpak.app_id') or               # Flatpak specific
            current.get('pipewire.access.portal.app_id') or # Portal ID
            current.get('snap.name') or                    # Snap specific
            current.get('application.process.binary') or 
            current.get('app_name') or 
            current.get('binary') or
            current.get('node.name') or
            current.get('media.name')
        )
        
        # If name is generic, try harder with PID
        if not name or name.lower() in ["audio-src", "speech-dispatcher", "chromium-browser", "unknown"]:
            pid = current.get('pid')
            if pid:
                try:
                    # Try ps first
                    proc_name = subprocess.check_output(['ps', '-p', str(pid), '-o', 'comm='], text=True).strip()
                    if not proc_name or proc_name.lower() in ["bwrap", "flatpak"]:
                        # If it's a flatpak wrapper, try reading the cmdline or comm directly
                        with open(f"/proc/{pid}/comm", "r") as f:
                            proc_name = f.read().strip()
                    
                    if proc_name:
                        name = proc_name.title()
                except:
                    pass

        # Final fallback to avoid disappearance
        if not name:
            name = current.get('node.name') or f"App #{current.get('index', '?')}"

        if not is_internal:
            current['app_name'] = name
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

    def start_rnnoise(self, channel_key='default'):
        config_dir = os.path.expanduser('~/.config/pipewire')
        os.makedirs(config_dir, exist_ok=True)
        config_path = os.path.join(config_dir, f'wavelinux-rnnoise-{channel_key}.conf')
        config = f"""\
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
        try:
            proc = subprocess.Popen(
                ['pipewire', '-c', config_path],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            self.rnnoise_processes[channel_key] = proc
            return True
        except FileNotFoundError:
            return False

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
            filter_graph = """
                nodes = [
                    {
                        type  = ladspa
                        name  = gate
                        plugin = ladspa-gate
                        label = gate
                        control = {
                            "Threshold" = -40.0
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
        config = f"""\
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
        try:
            proc = subprocess.Popen(
                ['pipewire', '-c', config_path],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            key = f'{channel_key}_{effect_id}'
            self.rnnoise_processes[key] = proc  # reuse dict for all fx processes
            return True
        except FileNotFoundError:
            return False

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
