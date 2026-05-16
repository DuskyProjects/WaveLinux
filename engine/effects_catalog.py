"""Effect catalog and filter-graph rendering helpers."""

from __future__ import annotations

import logging
import os
import re


FX_PREAMBLE = ""

AVAILABLE_EFFECTS = (
    {"id": "rnnoise", "name": "Noise Suppression", "icon": "🎙️", "desc": "AI-powered background noise removal"},
    {"id": "highpass", "name": "High-Pass Filter", "icon": "🎵", "desc": "Roll off low rumble (fans, handling noise)"},
    {"id": "eq", "name": "3-Band EQ", "icon": "🎚️", "desc": "Shape tone with low shelf / mid peak / high shelf"},
    {"id": "compressor", "name": "Compressor", "icon": "📉", "desc": "Smooth out loud/quiet differences"},
    {"id": "gate", "name": "Noise Gate", "icon": "🚪", "desc": "Cut audio below a threshold"},
    {"id": "limiter", "name": "Limiter", "icon": "🛡️", "desc": "Prevent audio clipping"},
)

EFFECT_PARAMS = {
    "rnnoise": [
        ("VAD Threshold (%)", "VAD Threshold", 0.0, 100.0, 50.0, "%"),
        ("VAD Grace Period (ms)", "Hold Open", 0.0, 2000.0, 200.0, " ms"),
        ("Retroactive VAD Grace (ms)", "Lead-In", 0.0, 500.0, 0.0, " ms"),
    ],
    "highpass": [
        ("Freq", "Cutoff", 20.0, 500.0, 80.0, " Hz"),
    ],
    "eq": [
        ("Low Freq", "Low Freq", 40.0, 400.0, 120.0, " Hz"),
        ("Low Gain", "Low Gain", -12.0, 12.0, 0.0, " dB"),
        ("Mid Freq", "Mid Freq", 300.0, 4000.0, 1000.0, " Hz"),
        ("Mid Gain", "Mid Gain", -12.0, 12.0, 0.0, " dB"),
        ("High Freq", "High Freq", 2000.0, 12000.0, 6000.0, " Hz"),
        ("High Gain", "High Gain", -12.0, 12.0, 0.0, " dB"),
    ],
    "compressor": [
        ("Threshold level (dB)", "Threshold", -60.0, 0.0, -20.0, " dB"),
        ("Ratio (1:n)", "Ratio", 1.0, 20.0, 4.0, ":1"),
        ("Attack time (ms)", "Attack", 0.1, 200.0, 5.0, " ms"),
        ("Release time (ms)", "Release", 5.0, 1000.0, 100.0, " ms"),
        ("Makeup gain (dB)", "Makeup", 0.0, 24.0, 0.0, " dB"),
    ],
    "gate": [
        ("Threshold (dB)", "Threshold", -80.0, 0.0, -40.0, " dB"),
        ("Attack (ms)", "Attack", 0.1, 100.0, 2.5, " ms"),
        ("Hold (ms)", "Hold", 0.0, 500.0, 10.0, " ms"),
        ("Decay (ms)", "Release", 10.0, 2000.0, 200.0, " ms"),
        ("Range (dB)", "Range", -80.0, 0.0, -40.0, " dB"),
    ],
    "limiter": [
        ("Input gain (dB)", "Input Gain", -20.0, 20.0, 0.0, " dB"),
        ("Limit (dB)", "Ceiling", -20.0, 0.0, -1.0, " dB"),
    ],
}

EFFECT_HELP = {
    "rnnoise": (
        "AI-powered noise suppression. Removes steady background noise "
        "(fans, keyboard, street). VAD threshold controls how aggressive "
        "it is — higher numbers cut more but risk chopping quiet speech. "
        "Hold Open keeps word endings from being cut off, and Lead-In "
        "adds a small pre-roll before voice detection if starts sound clipped."
    ),
    "highpass": (
        "Rolls off low-frequency rumble below the cutoff. 80 Hz is a "
        "safe default for voice; push to 100–120 Hz for very rumbly "
        "rooms, drop to 40–60 Hz for music or deep voices."
    ),
    "eq": (
        "Three-band tone shaping. Low shelf warms or thins the bass, "
        "mid peak carves out muddiness or adds presence around 1–3 kHz, "
        "high shelf brightens or tames sibilance."
    ),
    "compressor": (
        "Evens out loud vs. quiet moments. Threshold is where it starts "
        "working, ratio is how hard it clamps (4:1 is a solid broadcast "
        "setting), makeup brings the level back up afterwards."
    ),
    "gate": (
        "Silences the channel when it's below the threshold. Useful on "
        "mics to kill room tone between words. Range is how much to "
        "attenuate when closed; too strong makes breaths choppy."
    ),
    "limiter": (
        "A brick-wall ceiling on the signal so nothing clips. Leave "
        "'Ceiling' at -1 dB for broadcast. Bump 'Input Gain' if your "
        "mic is quiet and you want it to ride harder against the ceiling."
    ),
}

EFFECT_PRESETS = {
    "rnnoise": [
        ("Gentle", {"VAD Threshold (%)": 25.0, "VAD Grace Period (ms)": 250.0, "Retroactive VAD Grace (ms)": 0.0}),
        ("Broadcast", {"VAD Threshold (%)": 50.0, "VAD Grace Period (ms)": 200.0, "Retroactive VAD Grace (ms)": 0.0}),
        ("Aggressive", {"VAD Threshold (%)": 75.0, "VAD Grace Period (ms)": 150.0, "Retroactive VAD Grace (ms)": 0.0}),
    ],
    "highpass": [
        ("Voice 80 Hz", {"Freq": 80.0}),
        ("Rumble 120 Hz", {"Freq": 120.0}),
        ("Music 40 Hz", {"Freq": 40.0}),
    ],
    "eq": [
        (
            "Flat",
            {
                "Low Freq": 120.0,
                "Low Gain": 0.0,
                "Mid Freq": 1000.0,
                "Mid Gain": 0.0,
                "High Freq": 6000.0,
                "High Gain": 0.0,
            },
        ),
        (
            "Broadcast Voice",
            {
                "Low Freq": 120.0,
                "Low Gain": -2.0,
                "Mid Freq": 2500.0,
                "Mid Gain": 2.0,
                "High Freq": 8000.0,
                "High Gain": 1.5,
            },
        ),
        (
            "Warm Music",
            {
                "Low Freq": 100.0,
                "Low Gain": 2.0,
                "Mid Freq": 800.0,
                "Mid Gain": -1.0,
                "High Freq": 10000.0,
                "High Gain": 2.0,
            },
        ),
    ],
    "compressor": [
        (
            "Gentle 2:1",
            {
                "Threshold level (dB)": -20.0,
                "Ratio (1:n)": 2.0,
                "Attack time (ms)": 10.0,
                "Release time (ms)": 120.0,
                "Makeup gain (dB)": 2.0,
            },
        ),
        (
            "Broadcast 4:1",
            {
                "Threshold level (dB)": -18.0,
                "Ratio (1:n)": 4.0,
                "Attack time (ms)": 5.0,
                "Release time (ms)": 100.0,
                "Makeup gain (dB)": 3.0,
            },
        ),
        (
            "Streaming 6:1",
            {
                "Threshold level (dB)": -16.0,
                "Ratio (1:n)": 6.0,
                "Attack time (ms)": 3.0,
                "Release time (ms)": 80.0,
                "Makeup gain (dB)": 4.0,
            },
        ),
    ],
    "gate": [
        (
            "Soft -60 dB",
            {
                "Threshold (dB)": -60.0,
                "Range (dB)": -20.0,
                "Attack (ms)": 5.0,
                "Hold (ms)": 20.0,
                "Decay (ms)": 200.0,
            },
        ),
        (
            "Room mic -40 dB",
            {
                "Threshold (dB)": -40.0,
                "Range (dB)": -40.0,
                "Attack (ms)": 2.5,
                "Hold (ms)": 10.0,
                "Decay (ms)": 120.0,
            },
        ),
        (
            "Noisy mic -30 dB",
            {
                "Threshold (dB)": -30.0,
                "Range (dB)": -50.0,
                "Attack (ms)": 1.0,
                "Hold (ms)": 10.0,
                "Decay (ms)": 80.0,
            },
        ),
    ],
    "limiter": [
        ("Gentle -3 dB", {"Input gain (dB)": 0.0, "Limit (dB)": -3.0}),
        ("Broadcast -1 dB", {"Input gain (dB)": 0.0, "Limit (dB)": -1.0}),
        ("Loud -0.5 dB", {"Input gain (dB)": 3.0, "Limit (dB)": -0.5}),
    ],
}

CHAIN_ORDER = ("rnnoise", "highpass", "eq", "compressor", "gate", "limiter")


def fx_client_config(client_id, filter_chain_args):
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


def get_available_effects(engine_cls):
    return engine_cls._AVAILABLE_EFFECTS


def get_effect_params(engine_cls, effect_id):
    return list(engine_cls._EFFECT_PARAMS.get(effect_id, []))


def get_effect_help(engine_cls, effect_id):
    return engine_cls._EFFECT_HELP.get(effect_id, "")


def get_effect_presets(engine_cls, effect_id):
    return list(engine_cls._EFFECT_PRESETS.get(effect_id, []))


def resolved_params(engine, effect_id, overrides):
    ranges = {
        key: (mn, mx)
        for (key, _label, mn, mx, _default, _suffix) in engine._EFFECT_PARAMS.get(effect_id, [])
    }
    out = {
        key: default
        for (key, _label, _mn, _mx, default, _suffix) in engine._EFFECT_PARAMS.get(effect_id, [])
    }
    if overrides:
        for key, value in overrides.items():
            if key not in out:
                continue
            try:
                value = float(value)
            except (TypeError, ValueError):
                continue
            mn, mx = ranges[key]
            out[key] = max(mn, min(mx, value))
    return out


def render_control_block(params):
    lines = []
    for key, value in params.items():
        lines.append(f'                            "{key}" = {float(value):.3f}')
    body = "\n".join(lines)
    return f"                        control = {{\n{body}\n                        }}"


def ladspa_node(engine, name, plugin, label, values):
    path = engine.ladspa_plugin_path(plugin) or plugin
    return f"""
                nodes = [
                    {{
                        type   = ladspa
                        name   = {name}
                        plugin = "{path}"
                        label  = {label}
{engine._render_control_block(values)}
                    }}
                ]
                inputs  = [ "{name}:Input" ]
                outputs = [ "{name}:Output" ]
"""


def build_filter_graph(engine, effect_id, values):
    if effect_id == "rnnoise":
        return engine._ladspa_node("rnnoise", "librnnoise_ladspa", "noise_suppressor_mono", values)
    if effect_id == "highpass":
        return f"""
                nodes = [
                    {{
                        type  = builtin
                        name  = highpass
                        label = bq_highpass
{engine._render_control_block(values)}
                    }}
                ]
                inputs  = [ "highpass:In" ]
                outputs = [ "highpass:Out" ]
"""
    if effect_id == "eq":
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
    if effect_id == "gate":
        return engine._ladspa_node("gate", "gate_1410", "gate", values)
    if effect_id == "compressor":
        return engine._ladspa_node("compressor", "sc4m_1916", "sc4m", values)
    if effect_id == "limiter":
        ceiling_db = float(values.get("Limit (dB)", -1.0))
        input_db = float(values.get("Input gain (dB)", 0.0))
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


def ordered_chain(engine_cls, effects):
    rank = {effect_id: index for index, effect_id in enumerate(engine_cls._CHAIN_ORDER)}
    return sorted(effects, key=lambda effect_id: (rank.get(effect_id, len(rank)), effect_id))


def safe_channel_key(node_name):
    cleaned = re.sub(r"[^A-Za-z0-9]+", "_", (node_name or "").lower()).strip("_")
    return cleaned or "chan"


def effect_stage_blocks(engine, effect_id, values, stage_idx):
    prefix = f"s{stage_idx}_"

    if effect_id == "rnnoise":
        path = engine.ladspa_plugin_path("librnnoise_ladspa") or "librnnoise_ladspa"
        name = f"{prefix}rnnoise"
        nodes = f"""
                {{
                    type   = ladspa
                    name   = {name}
                    plugin = "{path}"
                    label  = noise_suppressor_mono
{engine._render_control_block(values)}
                }}"""
        return nodes, [], f"{name}:Output", f"{name}:Input"

    if effect_id == "highpass":
        name = f"{prefix}highpass"
        nodes = f"""
                {{
                    type  = builtin
                    name  = {name}
                    label = bq_highpass
{engine._render_control_block(values)}
                }}"""
        return nodes, [], f"{name}:Out", f"{name}:In"

    if effect_id == "eq":
        low = f"{prefix}eq_low"
        mid = f"{prefix}eq_mid"
        high = f"{prefix}eq_high"
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
        return nodes, internal, f"{high}:Out", f"{low}:In"

    if effect_id == "compressor":
        path = engine.ladspa_plugin_path("sc4m_1916") or "sc4m_1916"
        name = f"{prefix}compressor"
        nodes = f"""
                {{
                    type   = ladspa
                    name   = {name}
                    plugin = "{path}"
                    label  = sc4m
{engine._render_control_block(values)}
                }}"""
        return nodes, [], f"{name}:Output", f"{name}:Input"

    if effect_id == "gate":
        path = engine.ladspa_plugin_path("gate_1410") or "gate_1410"
        name = f"{prefix}gate"
        nodes = f"""
                {{
                    type   = ladspa
                    name   = {name}
                    plugin = "{path}"
                    label  = gate
{engine._render_control_block(values)}
                }}"""
        return nodes, [], f"{name}:Output", f"{name}:Input"

    if effect_id == "limiter":
        ceiling_db = float(values.get("Limit (dB)", -1.0))
        input_db = float(values.get("Input gain (dB)", 0.0))
        ceiling = max(0.0001, min(1.0, 10 ** (ceiling_db / 20.0)))
        in_gain = 10 ** (input_db / 20.0)
        lin = f"{prefix}lim_in"
        clp = f"{prefix}lim_out"
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
        return nodes, internal, f"{clp}:Out", f"{lin}:In"

    return None, None, None, None


def build_unified_filter_graph(engine, ordered_effects, params_map):
    all_nodes = []
    all_links = []
    first_entry = None
    prev_exit = None
    used_effects = []

    for stage_idx, effect_id in enumerate(ordered_effects):
        values = engine._resolved_params(effect_id, params_map.get(effect_id))
        nodes_text, internal_links, exit_port, entry_port = engine._effect_stage_blocks(effect_id, values, stage_idx)
        if nodes_text is None:
            logging.warning("Skipping unknown / unavailable effect %s in live filter graph", effect_id)
            continue
        if first_entry is None:
            first_entry = entry_port
        all_nodes.append(nodes_text)
        all_links.extend(internal_links)
        if prev_exit is not None:
            all_links.append(f'{{ output = "{prev_exit}" input = "{entry_port}" }}')
        prev_exit = exit_port
        used_effects.append(effect_id)

    if not used_effects or first_entry is None or prev_exit is None:
        return None, []

    nodes_block = "\n".join(all_nodes)
    links_block = "\n                    ".join(all_links) if all_links else ""
    graph = (
        "{\n"
        "    nodes = ["
        f"{nodes_block}\n"
        "    ]\n"
        "    links = [\n"
        f"        {links_block}\n"
        "    ]\n"
        f'    inputs = [ "{first_entry}" ]\n'
        f'    outputs = [ "{prev_exit}" ]\n'
        "}"
    )
    return graph, used_effects


def build_unified_chain_config(engine, safe_key, ordered_effects, params_map, stamp=None):
    config_dir = os.path.expanduser("~/.config/pipewire")
    os.makedirs(config_dir, exist_ok=True)

    graph_text, used_effects = engine._build_unified_filter_graph(ordered_effects, params_map)
    if graph_text is None or not used_effects:
        return None, None, None, []

    stamp_str = f".{stamp}" if stamp else ""
    sink_name = f"wavelinux.fx.{safe_key}{stamp_str}.input"
    source_name = f"wavelinux.fx.{safe_key}{stamp_str}.source"
    client_id = f"{safe_key}-chain-{stamp}" if stamp else f"{safe_key}-chain"
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
                audio.channels      = 1
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
                audio.channels      = 1
                audio.position      = [ MONO ]
                node.always-process = true
            }}
        }}"""
    config = engine._fx_client_config(client_id, filter_chain_args)
    config_path = os.path.join(config_dir, f"wavelinux-chain-{safe_key}.conf")
    tmp_path = config_path + ".tmp"
    with open(tmp_path, "w") as handle:
        handle.write(config)
    os.replace(tmp_path, config_path)
    return config_path, sink_name, source_name, used_effects


def build_fx_stage_config(engine, safe_key, idx, effect_id, params):
    config_dir = os.path.expanduser("~/.config/pipewire")
    os.makedirs(config_dir, exist_ok=True)

    values = engine._resolved_params(effect_id, params)
    filter_graph = engine._build_filter_graph(effect_id, values)
    if filter_graph is None:
        return None, None, None

    sink_name = f"wavelinux.fx.{safe_key}.{idx}.{effect_id}.input"
    source_name = f"wavelinux.fx.{safe_key}.{idx}.{effect_id}.source"
    client_id = f"{safe_key}-{idx}-{effect_id}"
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
    config = engine._fx_client_config(client_id, filter_chain_args)
    config_path = os.path.join(config_dir, f"wavelinux-chain-{safe_key}-{idx}-{effect_id}.conf")
    with open(config_path, "w") as handle:
        handle.write(config)
    return config_path, sink_name, source_name
