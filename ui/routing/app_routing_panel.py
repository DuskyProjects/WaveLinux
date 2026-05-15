"""App routing panel controller."""

from __future__ import annotations

import time
from types import SimpleNamespace

from pipewire_engine import PipeWireEngine
from ui.routing.app_routing_row import AppRoutingRow


class AppRoutingPanelController:
    def __init__(self, window):
        self.window = window

    def refresh_view(self, view) -> None:
        if self.window._module_enabled("app_routing"):
            sink_rows = [
                {"name": sink.name, "display_name": sink.display_name}
                for sink in getattr(view, "sinks", []) or []
            ]
            apps_by_id = {
                app.app_id: app for app in (getattr(view, "app_views", []) or [])
            }
            now = int(time.time())
            identity_migrated = False
            for app in apps_by_id.values():
                identity_migrated |= self.window._migrate_legacy_app_identity(app.app_id, app.app_name)
                if app.active_indices:
                    self.window.app_last_seen[app.app_id] = now
            cutoff = int(time.time()) - max(1, self.window.app_prune_days) * 24 * 3600
            recently_seen = {
                app_id for app_id, ts in self.window.app_last_seen.items()
                if ts >= cutoff
            }
            all_display_app_ids = (
                set(apps_by_id.keys())
                | set(self.window.app_routing.keys())
                | set(getattr(self.window, "app_volumes", {}).keys())
                | recently_seen
                | {PipeWireEngine.SYSTEM_SOUNDS_BUCKET}
            )
            all_display_app_ids -= self.window.forgotten_apps
            sys_bucket = PipeWireEngine.SYSTEM_SOUNDS_BUCKET
            ordered_display_apps = sorted(
                all_display_app_ids,
                key=lambda app_id: (
                    0 if app_id == sys_bucket else 1,
                    self.window._display_name_for_app_id(
                        app_id,
                        apps_by_id.get(app_id).app_name if app_id in apps_by_id else None,
                    ).lower(),
                ),
            )
            routing_layout_changed = tuple(ordered_display_apps) != self.window._app_widget_order
            for app_id in ordered_display_apps:
                runtime_app = apps_by_id.get(app_id)
                active_indices = list(runtime_app.active_indices) if runtime_app is not None else []
                if runtime_app is not None:
                    row_identity = runtime_app
                else:
                    reset_sources = self.window._override_sources_for_target(app_id)
                    row_identity = SimpleNamespace(
                        app_id=app_id,
                        app_name=self.window._display_name_for_app_id(app_id),
                        resolved_app_id=reset_sources[0] if len(reset_sources) == 1 else app_id,
                        resolved_app_name=self.window._display_name_for_app_id(
                            reset_sources[0] if len(reset_sources) == 1 else app_id,
                        ),
                        identity_source="remembered",
                        override_applied=bool(reset_sources),
                        manual_override_active=bool(reset_sources or app_id in self.window.app_label_overrides),
                        reset_source_app_id=reset_sources[0] if len(reset_sources) == 1 else "",
                    )
                display_name = self.window._display_name_for_app_id(
                    app_id,
                    getattr(row_identity, "app_name", None),
                )
                ctx = self.window._app_identity_context(row_identity)
                preferred_sink = self.window.app_routing.get(app_id)
                live_sink = runtime_app.current_sink if runtime_app is not None else None
                current_sink = preferred_sink or live_sink
                current_volume = runtime_app.current_volume if runtime_app is not None else None
                saved_volume = getattr(self.window, "app_volumes", {}).get(app_id)
                if app_id not in self.window.app_widgets:
                    row = AppRoutingRow(app_id, display_name, self.window.engine, sink_rows, main_win=self.window)
                    self.window.app_widgets[app_id] = row
                    self.window.routing_layout.addWidget(row)
                    routing_layout_changed = True
                self.window.app_widgets[app_id].update_state(
                    display_name,
                    active_indices,
                    sink_rows,
                    current_sink,
                    current_volume=current_volume,
                    saved_volume=saved_volume,
                    resolved_app_id=ctx["resolved_app_id"],
                    resolved_app_name=ctx["resolved_app_name"],
                    identity_source=ctx["identity_source"],
                    override_applied=ctx["override_applied"],
                    manual_override_active=ctx["manual_override_active"],
                    reset_source_app_id=ctx["reset_source_app_id"],
                    icon_candidates=list(getattr(row_identity, "icon_candidates", []) or []),
                )
            for app_id in list(self.window.app_widgets.keys()):
                if app_id not in all_display_app_ids:
                    self.window.app_widgets[app_id].setParent(None)
                    self.window.app_widgets[app_id].deleteLater()
                    del self.window.app_widgets[app_id]
                    routing_layout_changed = True
            if routing_layout_changed:
                self.window._app_widget_order = tuple(ordered_display_apps)
                self.window.routing_container.updateGeometry()
                self.window.routing_container.adjustSize()
            if identity_migrated:
                self.window._sync_runtime_persistent_state(immediate=True)
                self.window.schedule_save()
        else:
            self.window._set_app_routing_controls_enabled(False)
