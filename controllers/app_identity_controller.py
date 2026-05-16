"""Persistent app-identity and override management helpers."""

from __future__ import annotations

import time

from PyQt6.QtWidgets import QInputDialog, QMessageBox

from pipewire_engine import PipeWireEngine


class AppIdentityController:
    def __init__(self, window):
        self.window = window

    def _attrs(self):
        return self.window.__dict__

    def _sync_window_state(self):
        sync = getattr(type(self.window), "_sync_window_state", None)
        if sync is not None:
            sync(self.window)

    @staticmethod
    def normalize_app_identity_overrides(raw):
        return PipeWireEngine._normalize_identity_override_map(raw)

    @staticmethod
    def normalize_app_label_overrides(raw):
        return PipeWireEngine._normalize_label_override_map(raw)

    def set_engine_identity_overrides(self):
        engine = self._attrs().get("engine")
        if engine is None or not hasattr(engine, "set_app_identity_overrides"):
            return
        engine.set_app_identity_overrides(
            self._attrs().get("app_identity_overrides", {}),
            self._attrs().get("app_label_overrides", {}),
        )

    def identity_dialog_parent(self):
        return self._attrs().get("settings_dialog") or self.window

    def all_scene_app_ids(self):
        app_ids = set()
        for snapshot in (self._attrs().get("scenes", {}) or {}).values():
            if not isinstance(snapshot, dict):
                continue
            for mapping_name in ("app_routing", "app_volumes"):
                mapping = snapshot.get(mapping_name, {}) or {}
                if not isinstance(mapping, dict):
                    continue
                for app_id in mapping.keys():
                    if PipeWireEngine.is_persistent_app_id(app_id):
                        app_ids.add(app_id)
        return app_ids

    def known_persistent_app_ids(self):
        app_ids = set()
        sources = (
            set(getattr(self.window, "app_routing", {}).keys())
            | set(getattr(self.window, "app_volumes", {}).keys())
            | set(getattr(self.window, "app_last_seen", {}).keys())
            | set(getattr(self.window, "app_display_names", {}).keys())
            | set(getattr(self.window, "forgotten_apps", set()))
            | set(self._attrs().get("app_identity_overrides", {}).keys())
            | set(self._attrs().get("app_identity_overrides", {}).values())
            | set(self._attrs().get("app_label_overrides", {}).keys())
            | self.window._all_scene_app_ids()
        )
        for app_id in sources:
            if PipeWireEngine.is_persistent_app_id(app_id):
                app_ids.add(app_id)
        view = self._attrs().get("_runtime_view_state")
        for app_view in getattr(view, "app_views", []) or []:
            app_id = getattr(app_view, "app_id", "")
            if PipeWireEngine.is_persistent_app_id(app_id):
                app_ids.add(app_id)
        app_ids.discard(PipeWireEngine.SYSTEM_SOUNDS_BUCKET)
        return app_ids

    def override_sources_for_target(self, target_app_id, *, exclude_source=None):
        sources = []
        for source_app_id, mapped_target in self._attrs().get("app_identity_overrides", {}).items():
            if mapped_target != target_app_id:
                continue
            if exclude_source and source_app_id == exclude_source:
                continue
            sources.append(source_app_id)
        return sorted(set(sources))

    def app_id_has_runtime_or_saved_references(self, app_id):
        if not app_id:
            return False
        if app_id in getattr(self.window, "app_routing", {}):
            return True
        if app_id in getattr(self.window, "app_volumes", {}):
            return True
        if app_id in getattr(self.window, "app_last_seen", {}):
            return True
        if app_id in getattr(self.window, "forgotten_apps", set()):
            return True
        for snapshot in (self._attrs().get("scenes", {}) or {}).values():
            if not isinstance(snapshot, dict):
                continue
            if app_id in (snapshot.get("app_routing", {}) or {}):
                return True
            if app_id in (snapshot.get("app_volumes", {}) or {}):
                return True
        return False

    def cleanup_orphaned_custom_identity(self, app_id):
        if not isinstance(app_id, str) or not app_id.startswith("custom:"):
            return
        if self.window._override_sources_for_target(app_id):
            return
        if self.window._app_id_has_runtime_or_saved_references(app_id):
            return
        self.window.app_label_overrides.pop(app_id, None)
        self.window.app_display_names.pop(app_id, None)
        self._sync_window_state()

    def allocate_custom_app_id(self, label, *, keep_existing=""):
        base_id = PipeWireEngine._make_app_route_key("custom", label)
        if keep_existing and keep_existing.startswith("custom:"):
            base_id = keep_existing
        if not base_id:
            base_id = "custom:app"
        candidate = base_id
        known = self.window._known_persistent_app_ids()
        suffix = 2
        while candidate in known and candidate != keep_existing:
            candidate = f"{base_id}-{suffix}"
            suffix += 1
        return candidate

    def migrate_scene_library_app_identity(self, source_app_id, target_app_id):
        if not source_app_id or not target_app_id or source_app_id == target_app_id:
            return False
        changed = False
        for snapshot in (self._attrs().get("scenes", {}) or {}).values():
            if not isinstance(snapshot, dict):
                continue
            for mapping_name in ("app_routing", "app_volumes"):
                mapping = snapshot.get(mapping_name, {}) or {}
                if source_app_id not in mapping:
                    continue
                if target_app_id not in mapping:
                    mapping[target_app_id] = mapping[source_app_id]
                mapping.pop(source_app_id, None)
                changed = True
        if changed:
            self._sync_window_state()
        return changed

    def migrate_app_identity_state(self, source_app_id, target_app_id):
        if not source_app_id or not target_app_id or source_app_id == target_app_id:
            return False
        changed = False
        if source_app_id in self.window.app_routing:
            if target_app_id not in self.window.app_routing:
                self.window.app_routing[target_app_id] = self.window.app_routing[source_app_id]
            self.window.app_routing.pop(source_app_id, None)
            changed = True
        if source_app_id in self.window.app_volumes:
            if target_app_id not in self.window.app_volumes:
                self.window.app_volumes[target_app_id] = self.window.app_volumes[source_app_id]
            self.window.app_volumes.pop(source_app_id, None)
            changed = True
        if source_app_id in self.window.app_last_seen:
            source_seen = int(self.window.app_last_seen.pop(source_app_id))
            target_seen = int(self.window.app_last_seen.get(target_app_id, 0) or 0)
            self.window.app_last_seen[target_app_id] = max(source_seen, target_seen)
            changed = True
        source_label = self.window.app_display_names.pop(source_app_id, None)
        if target_app_id in self.window.app_label_overrides:
            self.window.app_display_names[target_app_id] = self.window.app_label_overrides[target_app_id]
            changed = True
        elif source_label and target_app_id not in self.window.app_display_names:
            self.window.app_display_names[target_app_id] = source_label
            changed = True
        if source_app_id in self.window.forgotten_apps:
            self.window.forgotten_apps.discard(source_app_id)
            self.window.forgotten_apps.add(target_app_id)
            changed = True
        if self.window._migrate_scene_library_app_identity(source_app_id, target_app_id):
            changed = True
        self.window._cleanup_orphaned_custom_identity(source_app_id)
        if changed:
            self._sync_window_state()
        return changed

    def display_name_for_app_id(self, app_id, fallback=None):
        override = self._attrs().get("app_label_overrides", {}).get(app_id)
        if override:
            self.window.app_display_names[app_id] = override
            return override
        if fallback:
            self.window.app_display_names[app_id] = fallback
            return fallback
        cached = self.window.app_display_names.get(app_id)
        if cached:
            return cached
        return PipeWireEngine.display_name_for_app_id(app_id)

    def app_identity_context(self, app_view_or_row):
        app_id = str(getattr(app_view_or_row, "app_id", "") or "").strip()
        app_name = str(getattr(app_view_or_row, "app_name", "") or "").strip()
        resolved_app_id = str(
            getattr(app_view_or_row, "resolved_app_id", "") or app_id
        ).strip()
        resolved_app_name = str(
            getattr(app_view_or_row, "resolved_app_name", "") or app_name
        ).strip()
        reset_source_app_id = str(
            getattr(app_view_or_row, "reset_source_app_id", "") or ""
        ).strip()
        sources_for_target = self.window._override_sources_for_target(app_id)
        if not reset_source_app_id and len(sources_for_target) == 1:
            reset_source_app_id = sources_for_target[0]
        label_override_active = app_id in self._attrs().get("app_label_overrides", {})
        manual_override_active = bool(
            getattr(app_view_or_row, "manual_override_active", False)
            or getattr(app_view_or_row, "override_applied", False)
            or bool(reset_source_app_id)
            or label_override_active
        )
        source_app_id = ""
        if (
            resolved_app_id
            and resolved_app_id != PipeWireEngine.SYSTEM_SOUNDS_BUCKET
            and PipeWireEngine.is_persistent_app_id(resolved_app_id)
        ):
            source_app_id = resolved_app_id
        elif (
            app_id
            and app_id != PipeWireEngine.SYSTEM_SOUNDS_BUCKET
            and PipeWireEngine.is_persistent_app_id(app_id)
        ):
            source_app_id = app_id
        display_name = self.window._display_name_for_app_id(
            app_id,
            app_name or resolved_app_name,
        )
        return {
            "app_id": app_id,
            "app_name": display_name,
            "resolved_app_id": resolved_app_id,
            "resolved_app_name": resolved_app_name,
            "source_app_id": source_app_id,
            "reset_source_app_id": reset_source_app_id,
            "manual_override_active": manual_override_active,
            "identity_source": str(getattr(app_view_or_row, "identity_source", "") or ""),
            "override_applied": bool(getattr(app_view_or_row, "override_applied", False)),
        }

    def migrate_legacy_app_identity(self, app_id, display_name):
        if not app_id or not display_name:
            return False
        changed = False
        if app_id not in self.window.app_routing and display_name in self.window.app_routing:
            self.window.app_routing[app_id] = self.window.app_routing.pop(display_name)
            changed = True
        if app_id not in self.window.app_last_seen and display_name in self.window.app_last_seen:
            self.window.app_last_seen[app_id] = self.window.app_last_seen.pop(display_name)
            changed = True
        if app_id not in self.window.app_volumes and display_name in self.window.app_volumes:
            self.window.app_volumes[app_id] = self.window.app_volumes.pop(display_name)
            changed = True
        if app_id not in self.window.forgotten_apps and display_name in self.window.forgotten_apps:
            self.window.forgotten_apps.discard(display_name)
            self.window.forgotten_apps.add(app_id)
            changed = True
        if app_id != display_name and display_name in self.window.app_display_names:
            legacy_label = self.window.app_display_names.pop(display_name, None)
            if legacy_label and app_id not in self.window.app_display_names:
                self.window.app_display_names[app_id] = legacy_label
            changed = True
        if self.window.app_display_names.get(app_id) != display_name:
            self.window.app_display_names[app_id] = display_name
            changed = True
        if changed:
            self._sync_window_state()
        return changed

    def apply_app_identity_changes(self, status_message):
        self.window._set_engine_identity_overrides()
        self.window._sync_runtime_persistent_state(immediate=True)
        self.window.save_config()
        runtime = self._attrs().get("runtime")
        if runtime is not None and hasattr(runtime, "refresh_now"):
            runtime.refresh_now("app-identity-change")
        status_lbl = self._attrs().get("status_lbl")
        if status_lbl is not None and hasattr(status_lbl, "setText"):
            status_lbl.setText(status_message)
        refresh = self._attrs().get("_refresh")
        if callable(refresh):
            refresh()
        self._sync_window_state()

    def pin_app_identity(self, app_view_or_row):
        ctx = self.window._app_identity_context(app_view_or_row)
        source_app_id = ctx["source_app_id"]
        if not source_app_id:
            QMessageBox.information(
                self.window._identity_dialog_parent(),
                "Pin App Identity",
                "WaveLinux needs a stable app signature before it can pin this stream.",
            )
            return False

        current_label = self.window._display_name_for_app_id(
            ctx["app_id"],
            ctx["app_name"] or ctx["resolved_app_name"],
        )
        label, ok = QInputDialog.getText(
            self.window._identity_dialog_parent(),
            "Pin / Rename App",
            "Display label:",
            text=current_label,
        )
        if not ok:
            return False
        label = PipeWireEngine._sanitize_app_label(label)
        if not label:
            QMessageBox.information(
                self.window._identity_dialog_parent(),
                "Pin / Rename App",
                "Enter a non-empty app label.",
            )
            return False

        current_app_id = ctx["app_id"]
        manual_override_active = ctx["manual_override_active"]
        if current_app_id.startswith("custom:") or manual_override_active:
            target_app_id = current_app_id
        elif current_app_id.startswith(("app:", "snap:")):
            target_app_id = current_app_id
        else:
            target_app_id = self.window._allocate_custom_app_id(label)

        if target_app_id != current_app_id:
            self.window._migrate_app_identity_state(current_app_id, target_app_id)
            self.window.app_identity_overrides[source_app_id] = target_app_id
        elif (
            source_app_id != current_app_id
            and PipeWireEngine.is_persistent_app_id(source_app_id)
            and manual_override_active
        ):
            self.window.app_identity_overrides[source_app_id] = current_app_id

        self.window.app_label_overrides[target_app_id] = label
        self.window.app_display_names[target_app_id] = label
        if target_app_id != current_app_id:
            self.window.app_display_names.pop(current_app_id, None)
        self.apply_app_identity_changes(f"Pinned app identity: {label}")
        return True

    def merge_app_identity(self, app_view_or_row):
        ctx = self.window._app_identity_context(app_view_or_row)
        source_app_id = ctx["source_app_id"]
        if not source_app_id:
            QMessageBox.information(
                self.window._identity_dialog_parent(),
                "Merge App Identity",
                "WaveLinux needs a stable app signature before it can merge this stream.",
            )
            return False

        candidate_ids = sorted(
            (
                app_id for app_id in self.window._known_persistent_app_ids()
                if app_id != ctx["app_id"]
                and app_id != PipeWireEngine.SYSTEM_SOUNDS_BUCKET
                and PipeWireEngine.is_persistent_app_id(app_id)
            ),
            key=lambda app_id: (
                self.window._display_name_for_app_id(app_id).lower(),
                app_id,
            ),
        )
        if not candidate_ids:
            QMessageBox.information(
                self.window._identity_dialog_parent(),
                "Merge App Identity",
                "No other saved app identities are available to merge into yet.",
            )
            return False

        labels = [
            f"{self.window._display_name_for_app_id(app_id)} [{app_id}]"
            for app_id in candidate_ids
        ]
        selection, ok = QInputDialog.getItem(
            self.window._identity_dialog_parent(),
            "Merge Into Existing App",
            "Route this app identity into:",
            labels,
            0,
            False,
        )
        if not ok or not selection:
            return False
        target_app_id = candidate_ids[labels.index(selection)]
        current_app_id = ctx["app_id"]

        self.window.app_identity_overrides[source_app_id] = target_app_id
        if current_app_id != target_app_id:
            self.window._migrate_app_identity_state(current_app_id, target_app_id)
        self.window._cleanup_orphaned_custom_identity(current_app_id)
        target_label = self.window.app_label_overrides.get(target_app_id)
        if target_label:
            self.window.app_display_names[target_app_id] = target_label
        self.apply_app_identity_changes(
            f"Merged app identity into {self.window._display_name_for_app_id(target_app_id)}"
        )
        return True

    def reset_app_identity_override(self, app_view_or_row):
        ctx = self.window._app_identity_context(app_view_or_row)
        current_app_id = ctx["app_id"]
        source_app_id = ctx["reset_source_app_id"]
        had_source_override = bool(
            source_app_id
            and self.window.app_identity_overrides.get(source_app_id) == current_app_id
        )
        had_label_override = current_app_id in self.window.app_label_overrides
        if not had_source_override and not had_label_override:
            return False

        if had_source_override:
            self.window.app_identity_overrides.pop(source_app_id, None)
            remaining_sources = self.window._override_sources_for_target(
                current_app_id,
                exclude_source=source_app_id,
            )
            if current_app_id.startswith("custom:") and not remaining_sources:
                self.window._migrate_app_identity_state(current_app_id, source_app_id)
                self.window.app_label_overrides.pop(current_app_id, None)
                self.window.app_display_names.pop(current_app_id, None)
                self.window.app_display_names[source_app_id] = PipeWireEngine.display_name_for_app_id(
                    source_app_id,
                )
                self.window._cleanup_orphaned_custom_identity(current_app_id)
        elif had_label_override:
            self.window.app_label_overrides.pop(current_app_id, None)
            self.window.app_display_names[current_app_id] = PipeWireEngine.display_name_for_app_id(
                current_app_id,
            )

        self.apply_app_identity_changes("Reset app identity to automatic detection")
        return True

    def forget_app(self, app_id):
        if app_id == PipeWireEngine.SYSTEM_SOUNDS_BUCKET:
            return
        self.window.app_routing.pop(app_id, None)
        self.window.app_volumes.pop(app_id, None)
        self.window.app_last_seen.pop(app_id, None)
        self.window.app_display_names.pop(app_id, None)
        self.window.forgotten_apps.add(app_id)
        widget = self._attrs().get("app_widgets", {}).pop(app_id, None)
        if widget is not None:
            widget.setParent(None)
            widget.deleteLater()
        self.window.save_config()
        self.window._refresh()
        self._sync_window_state()

    def prune_stale_apps(self):
        if self.window.app_prune_days <= 0:
            return
        cutoff = int(time.time()) - self.window.app_prune_days * 24 * 3600
        stale_routed = [
            name for name in (
                set(self.window.app_routing.keys())
                | set(getattr(self.window, "app_volumes", {}).keys())
            )
            if self.window.app_last_seen.get(name) is not None
            and self.window.app_last_seen.get(name, 0) < cutoff
        ]
        for name in stale_routed:
            self.window.app_routing.pop(name, None)
            self.window.app_volumes.pop(name, None)
            self.window.app_last_seen.pop(name, None)
            if name not in self.window.forgotten_apps:
                self.window.app_display_names.pop(name, None)
        stale_seen = [
            name for name, ts in list(self.window.app_last_seen.items())
            if ts < cutoff
        ]
        for name in stale_seen:
            self.window.app_last_seen.pop(name, None)
            if (
                name not in self.window.app_routing
                and name not in getattr(self.window, "app_volumes", {})
                and name not in self.window.forgotten_apps
            ):
                self.window.app_display_names.pop(name, None)
        total = len(stale_routed) + len(stale_seen)
        if total:
            import logging

            logging.info(
                "Pruned %s stale app entries (%s routed, %s seen-only)",
                total,
                len(stale_routed),
                len(stale_seen),
            )
            self._sync_window_state()
