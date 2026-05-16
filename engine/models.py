"""Core data models used by the PipeWire engine."""

from __future__ import annotations


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
        self.sink_name = sink_name
        self.sink_module_id = sink_module_id
        self.source_name = None
        self.source_module_id = None
        self.channel_volumes = {}
        self.channel_mutes = {}
        self.hardware_output = None
        self.master_volume = 1.0
        self.master_muted = False


class EngineSnapshot:
    """Per-refresh cache for expensive pactl/pw-dump reads."""

    __slots__ = (
        "modules_text",
        "short_modules_text",
        "sink_inputs_text",
        "sinks_text",
        "sources_text",
        "nodes",
        "sinks",
        "_loopback_index",
        "_sink_state_by_name",
        "_sink_descriptions",
        "_source_state_by_name",
    )

    def __init__(
        self,
        modules_text="",
        short_modules_text="",
        sink_inputs_text="",
        sinks_text="",
        sources_text="",
        nodes=None,
        sinks=None,
    ):
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
