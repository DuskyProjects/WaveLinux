"""Thread-safe adapter around the legacy PipeWire engine."""

from __future__ import annotations

from contextlib import contextmanager
import copy
import threading
import time

from pipewire_engine import PipeWireEngine


class JournaledPipeWireEngine(PipeWireEngine):
    """Legacy engine with command journaling hooks."""

    def __init__(self, diagnostics=None):
        self._runtime_diagnostics = diagnostics
        super().__init__()

    def _run(self, cmd, timeout=2):
        started = time.monotonic()
        out = super()._run(cmd, timeout=timeout)
        diag = getattr(self, "_runtime_diagnostics", None)
        if diag is not None:
            diag.record_command(
                cmd,
                timeout,
                (time.monotonic() - started) * 1000.0,
                out,
                out is not None,
            )
        return out


class AudioRuntimeAdapter:
    """Locked facade used by both the UI thread and runtime worker."""

    def __init__(self, diagnostics=None):
        self._lock = threading.RLock()
        self._engine = JournaledPipeWireEngine(diagnostics=diagnostics)

    @contextmanager
    def session(self):
        with self._lock:
            yield self._engine

    def __getattr__(self, name):
        with self._lock:
            attr = getattr(self._engine, name)
            if callable(attr):
                def locked_call(*args, **kwargs):
                    with self._lock:
                        bound = getattr(self._engine, name)
                        return bound(*args, **kwargs)
                return locked_call
            if isinstance(attr, dict):
                return dict(attr)
            if isinstance(attr, list):
                return list(attr)
            if isinstance(attr, set):
                return set(attr)
            try:
                return copy.copy(attr)
            except Exception:
                return attr
