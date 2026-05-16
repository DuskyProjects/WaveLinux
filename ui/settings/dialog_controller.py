"""Settings dialog control and install-state refresh helpers."""

from __future__ import annotations

import logging
import queue
import threading
import time

from PyQt6.QtWidgets import QMessageBox

from distribution import install_state


class DialogController:
    def __init__(self, window, *, install_state_loader=install_state):
        self.window = window
        self._install_state_loader = install_state_loader

    def open_settings(self):
        if not self.window._module_enabled("settings_ui"):
            QMessageBox.information(
                self.window,
                "Settings disabled",
                "The settings module is currently disabled for diagnostics.",
            )
            return
        was_visible = self.window._settings_dialog_visible()
        if not was_visible:
            self.window.settings_dialog.show()
        self.window.settings_dialog.raise_()
        if not was_visible:
            self.schedule_active_settings_tab_refresh(force=False)

    def active_settings_tab_name(self):
        tabs = self.window.__dict__.get("_settings_tabs")
        if tabs is None:
            return ""
        try:
            index = tabs.currentIndex()
        except Exception:
            return ""
        names = self.window.__dict__.get("_settings_tab_names", ())
        if 0 <= index < len(names):
            return str(names[index] or "")
        try:
            return str(tabs.tabText(index) or "")
        except Exception:
            return ""

    def refresh_settings_tab_by_name(self, tab_name):
        tab_name = str(tab_name or "").strip()
        if tab_name == "Hidden":
            self.window._refresh_hidden_list()
        elif tab_name == "Scenes":
            if not self.window._module_enabled("scenes"):
                return
            self.window._refresh_scenes_tab()
        elif tab_name == "Health":
            if not self.window._module_enabled("health"):
                return
            self.window._refresh_system_tab(preflight=self.window._startup_preflight)
        elif tab_name == "Advanced":
            self.window._refresh_advanced_tab()
        elif tab_name == "Updates":
            if not self.window._module_enabled("updates"):
                return
            self.window._refresh_update_tab()

    def install_state_cache_is_stale(self, *, max_age_s=5.0):
        stamp = float(self.window.__dict__.get("_install_state_cache_at", 0.0) or 0.0)
        if stamp <= 0.0:
            return True
        return (time.monotonic() - stamp) >= max(0.0, float(max_age_s or 0.0))

    def invalidate_install_state_cache(self):
        self.window._install_state_cache = None
        self.window._install_state_cache_at = 0.0

    def cached_install_state(self, *, target_tabs=(), max_age_s=5.0, allow_async=True):
        state = self.window.__dict__.get("_install_state_cache")
        if (
            allow_async
            and "_install_state_refresh_queue" in self.window.__dict__
            and (state is None or self.install_state_cache_is_stale(max_age_s=max_age_s))
        ):
            self.schedule_install_state_refresh(
                target_tabs=target_tabs,
                force=(state is None),
            )
        return state

    def schedule_install_state_refresh(self, *, target_tabs=(), force=False):
        pending_tabs = set(self.window.__dict__.get("_install_state_refresh_tabs", set()) or set())
        pending_tabs.update(
            str(tab_name or "").strip()
            for tab_name in (target_tabs or ())
            if str(tab_name or "").strip()
        )
        self.window._install_state_refresh_tabs = pending_tabs
        if self.window.__dict__.get("_install_state_refresh_inflight", False):
            return
        cached = self.window.__dict__.get("_install_state_cache")
        if not force and cached is not None and not self.install_state_cache_is_stale():
            return
        self.window._install_state_refresh_inflight = True
        poll_timer = self.window.__dict__.get("_install_state_refresh_poll_timer")
        if poll_timer is not None and not poll_timer.isActive():
            poll_timer.start()
        threading.Thread(
            target=self.load_install_state_refresh_worker,
            name="wavelinux-install-state",
            daemon=True,
        ).start()

    def load_install_state_refresh_worker(self):
        queue_obj = self.window.__dict__.get("_install_state_refresh_queue")
        if queue_obj is None:
            return
        try:
            state = self._install_state_loader()
        except Exception as exc:
            queue_obj.put(("error", str(exc)))
        else:
            queue_obj.put(("result", state))

    def poll_install_state_refresh(self):
        queue_obj = self.window.__dict__.get("_install_state_refresh_queue")
        if queue_obj is None:
            return
        handled_result = False
        while True:
            try:
                kind, payload = queue_obj.get_nowait()
            except queue.Empty:
                break
            handled_result = True
            self.window._install_state_refresh_inflight = False
            if kind == "result":
                self.window._install_state_cache = payload
                self.window._install_state_cache_at = time.monotonic()
            else:
                logging.warning("Install-state refresh failed: %s", payload)
        if not handled_result and self.window.__dict__.get("_install_state_refresh_inflight", False):
            return
        timer = self.window.__dict__.get("_install_state_refresh_poll_timer")
        if timer is not None:
            timer.stop()
        if handled_result:
            self.apply_install_state_refresh()

    def apply_install_state_refresh(self):
        state = self.window.__dict__.get("_install_state_cache")
        if state is None:
            return
        pending_tabs = set(self.window.__dict__.get("_install_state_refresh_tabs", set()) or set())
        self.window._install_state_refresh_tabs = set()
        if not self.window._settings_dialog_visible():
            return
        active_tab = self.active_settings_tab_name()
        if active_tab == "Updates" and "Updates" in pending_tabs:
            self.window._refresh_update_tab(state=state, allow_async=False)
        elif active_tab == "Health" and "Health" in pending_tabs:
            self.window._refresh_system_tab(
                preflight=self.window._startup_preflight,
                state=state,
                allow_async=False,
            )

    def settings_tab_stale_seconds(self, tab_name):
        tab_name = str(tab_name or "").strip()
        return {
            "Hidden": 0.5,
            "Scenes": 0.5,
            "Health": 1.0,
            "Advanced": 1.0,
            "Updates": 5.0,
        }.get(tab_name, 1.0)

    def settings_tab_refresh_is_stale(self, tab_name, *, force=False):
        if force:
            return True
        tab_name = str(tab_name or "").strip()
        if not tab_name:
            return False
        last_refresh = float(
            (self.window.__dict__.get("_settings_tab_last_refresh_at", {}) or {}).get(tab_name, 0.0) or 0.0
        )
        if last_refresh <= 0.0:
            return True
        return (time.monotonic() - last_refresh) >= self.settings_tab_stale_seconds(tab_name)

    def mark_settings_tab_refreshed(self, tab_name):
        tab_name = str(tab_name or "").strip()
        if not tab_name:
            return
        refreshed = dict(self.window.__dict__.get("_settings_tab_last_refresh_at", {}) or {})
        refreshed[tab_name] = time.monotonic()
        self.window._settings_tab_last_refresh_at = refreshed

    def mark_settings_tab_stale(self, tab_name):
        tab_name = str(tab_name or "").strip()
        if not tab_name:
            return
        refreshed = dict(self.window.__dict__.get("_settings_tab_last_refresh_at", {}) or {})
        refreshed.pop(tab_name, None)
        self.window._settings_tab_last_refresh_at = refreshed

    def refresh_active_settings_tab(self, *, force=False):
        if not force and not self.window._settings_dialog_visible():
            return
        tab_name = self.active_settings_tab_name()
        if not self.settings_tab_refresh_is_stale(tab_name, force=force):
            return
        self.refresh_settings_tab_by_name(tab_name)
        self.mark_settings_tab_refreshed(tab_name)

    def schedule_active_settings_tab_refresh(self, *, force=False):
        if not force and not self.window._settings_dialog_visible():
            return
        tab_name = self.active_settings_tab_name()
        if not tab_name:
            return
        if not self.settings_tab_refresh_is_stale(tab_name, force=force):
            return
        self.window._pending_settings_tab_refresh = tab_name
        timer = self.window.__dict__.get("_settings_tab_refresh_timer")
        if timer is not None:
            timer.start()
        else:
            self.apply_scheduled_settings_tab_refresh()

    def apply_scheduled_settings_tab_refresh(self):
        if not self.window._settings_dialog_visible():
            return
        pending = str(self.window.__dict__.get("_pending_settings_tab_refresh", "") or "").strip()
        active = self.active_settings_tab_name()
        if pending and pending != active:
            return
        self.window._pending_settings_tab_refresh = ""
        self.refresh_active_settings_tab(force=False)

    def on_settings_tab_changed(self, index):
        _ = index
        self.schedule_active_settings_tab_refresh(force=False)
