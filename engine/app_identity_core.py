"""Shared app-identity constants and presentation helpers."""

from __future__ import annotations

import logging
import os
import re
import shlex
import socket
import time


GENERIC_APP_NAMES = {
    "audio src", "audio sink", "audio stream", "audio output",
    "audiostream", "audio playback", "playback stream", "output",
    "speech dispatcher", "unknown", "libcanberra", "playback",
    "pipewire", "pipewire pulse", "pulseaudio", "alsa plugins",
    "alsa plug in", "alsa plug ins", "audiostreamforandroid",
    "audio stream for android", "application", "pw loopback", "loopback",
    "chromium", "electron", "chrome", "chrome browser",
    "qt", "qtmultimedia", "gstreamer", "sdl", "sdl audio", "media stream",
}

KNOWN_APP_IDS = {
    "com.spotify.client": "Spotify",
    "com.spotify.spotify": "Spotify",
    "spotify": "Spotify",
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

EXEC_WRAPPERS = {
    "env", "gtk-launch", "flatpak", "flatpak-spawn",
    "snap", "snap-confine", "sh", "bash", "zsh",
    "pkexec", "sudo", "gamemoderun", "mangohud", "optirun",
    "primusrun", "prime-run", "nice", "taskset", "systemd-run",
    "wine", "wine64", "wineserver",
}

PATH_TITLE_PATTERNS = (
    re.compile(r"/[Ss]team(?:[Ll]ibrary)?/steamapps/common/([^/]+)/"),
    re.compile(r"/\.steam/[^/]+/steamapps/common/([^/]+)/"),
    re.compile(r"/SteamApps/common/([^/]+)/", re.IGNORECASE),
    re.compile(r"/drive_c/(?:Program Files(?: \(x86\))?|Games|GOG Games)/([^/]+)/", re.IGNORECASE),
    re.compile(r"/(?:Games|GOG Games|gog-games|itch|Lutris/games)/([^/]+)/", re.IGNORECASE),
    re.compile(r"^/opt/([^/]+)/"),
)

BINARY_DISPLAY_NAMES = {
    "brave": "Brave",
    "brave-browser": "Brave",
    "brave-browser-stable": "Brave",
    "brave-browser-beta": "Brave Beta",
    "brave-browser-nightly": "Brave Nightly",
    "brave-browser-origin": "Brave Origin Beta",
    "brave-origin": "Brave Origin Beta",
    "google-chrome": "Chrome",
    "google-chrome-stable": "Chrome",
    "google-chrome-beta": "Chrome Beta",
    "google-chrome-unstable": "Chrome Dev",
    "chromium-browser": "Chromium",
    "firefox": "Firefox",
    "firefox-esr": "Firefox ESR",
    "firefox-developer-edition": "Firefox Dev",
    "librewolf": "LibreWolf",
    "ferdium": "Ferdium",
    "ferdi": "Ferdi",
    "hamsket": "Hamsket",
    "spotify": "Spotify",
    "vlc": "VLC",
    "mpv": "mpv",
    "rhythmbox": "Rhythmbox",
    "clementine": "Clementine",
    "strawberry": "Strawberry",
    "elisa": "Elisa",
    "discord": "Discord",
    "vesktop": "Vesktop",
    "webcord": "WebCord",
    "signal-desktop": "Signal",
    "telegram-desktop": "Telegram",
    "slack": "Slack",
    "obs": "OBS Studio",
    "obs-studio": "OBS Studio",
    "zoom": "Zoom",
}

BINARY_ICON_NAMES = {
    "brave": "brave-desktop",
    "brave-browser": "brave-desktop",
    "brave-browser-stable": "brave-desktop",
    "brave-browser-beta": "brave-browser-beta",
    "brave-browser-nightly": "brave-browser-nightly",
    "brave-browser-origin": "brave-browser-origin",
    "brave-origin": "brave-browser-origin",
    "google-chrome": "google-chrome",
    "google-chrome-stable": "google-chrome",
    "google-chrome-beta": "google-chrome-beta",
    "google-chrome-unstable": "google-chrome-unstable",
    "chromium": "chromium",
    "chromium-browser": "chromium-browser",
    "firefox": "firefox",
    "firefox-esr": "firefox-esr",
    "firefox-developer-edition": "firefox-developer-edition",
    "librewolf": "librewolf",
    "ferdium": "ferdium",
    "ferdi": "ferdi",
    "hamsket": "hamsket",
    "spotify": "spotify",
    "vlc": "vlc",
    "mpv": "mpv",
    "rhythmbox": "rhythmbox",
    "clementine": "clementine",
    "strawberry": "strawberry",
    "elisa": "elisa",
    "discord": "discord",
    "vesktop": "vesktop",
    "webcord": "webcord",
    "signal-desktop": "signal-desktop",
    "telegram-desktop": "telegram-desktop",
    "slack": "slack",
    "obs": "obs",
    "obs-studio": "obs-studio",
    "zoom": "Zoom",
}

MULTIPROCESS_CHILD_BINARIES = {
    "chrome",
    "chromium",
    "chromium-browser",
    "firefox",
    "firefox-bin",
    "renderer",
    "zygote",
    "utility",
    "gpu-process",
    "plugin-host",
    "webkitwebprocess",
}

WINDOW_IDENTITY_KEYS = (
    "application.id",
    "pipewire.access.portal.app_id",
    "xdg.portal.app_id",
    "window.app_id",
    "window.x11.wm_class",
    "window.x11.instance",
    "window.class",
    "application.icon_name",
)

TEXT_IDENTITY_KEYS = (
    "application.display_name",
    "application.name",
    "node.description",
    "node.nick",
    "node.name",
    "media.name",
)

WINDOW_TITLE_KEYS = (
    "window.title",
    "window.name",
    "media.title",
)

SYSTEM_SOUNDS_BUCKET = "System Sounds"


def normalize_app_name(value):
    if not value:
        return ""
    value = str(value).strip().lower()
    for ch in "-_.":
        value = value.replace(ch, " ")
    return " ".join(value.split())


def is_generic_name(engine, value):
    norm = engine._normalize_app_name(value)
    if not norm:
        return True
    if norm in engine._GENERIC_APP_NAMES:
        return True
    if norm.isdigit() or len(norm) <= 1:
        return True
    return False


def canonicalize_app_id(app_id):
    if not app_id:
        return None
    mapped = KNOWN_APP_IDS.get(str(app_id).lower())
    if mapped:
        return mapped
    tail = str(app_id).rsplit(".", 1)[-1]
    return tail.replace("-", " ").replace("_", " ").strip() or app_id


def normalize_for_host_match(value):
    if not value:
        return ""
    return re.sub(r"[^a-z0-9]", "", str(value).lower())


def host_aliases(engine_cls):
    cached = getattr(engine_cls, "_host_alias_cache", None)
    if cached is not None:
        return cached
    raw = set()
    try:
        host = socket.gethostname()
        if host:
            raw.add(host)
            short = host.split(".", 1)[0]
            if short:
                raw.add(short)
    except Exception:
        pass
    try:
        with open("/etc/hostname", "r") as handle:
            host = handle.read().strip()
            if host:
                raw.add(host)
                raw.add(host.split(".", 1)[0])
    except OSError:
        pass
    names = {engine_cls._normalize_for_host_match(host) for host in raw}
    names.discard("")
    if not names:
        logging.warning(
            "Could not determine hostname; host-name filter for "
            "system streams in App Routing will be inactive."
        )
    engine_cls._host_alias_cache = names
    return names


def name_matches_host(engine_cls, value):
    token = engine_cls._normalize_for_host_match(value)
    if not token:
        return False
    return token in engine_cls._host_aliases()


def normalize_app_route_token(value):
    if value is None:
        return ""
    return re.sub(r"[^a-z0-9._:+-]+", "-", str(value).strip().lower()).strip("-")


def append_icon_candidate(engine_cls, out, seen, value):
    token = str(value or "").strip()
    if not token:
        return

    def add(candidate):
        candidate = str(candidate or "").strip()
        if not candidate or candidate in seen:
            return
        seen.add(candidate)
        out.append(candidate)

    add(token)
    lowered = token.lower()
    add(lowered)
    mapped = engine_cls._BINARY_ICON_NAMES.get(lowered)
    if mapped:
        add(mapped)
    normalized = re.sub(r"[^a-z0-9.+-]+", "-", lowered).strip("-")
    if normalized:
        add(normalized)
        dotted = normalized.replace(".", "-")
        if dotted != normalized:
            add(dotted)
        parts = [part for part in re.split(r"[._-]+", normalized) if part]
        if parts:
            add(parts[-1])
        if len(parts) >= 2:
            add(f"{parts[-2]}-{parts[-1]}")
        if len(parts) >= 3:
            add(f"{parts[-3]}-{parts[-2]}-{parts[-1]}")


def theme_icon_candidates_for_app_id(engine_cls, app_id, fallback_name=None):
    candidates = []
    seen = set()
    raw = str(app_id or "").strip()
    if raw:
        engine_cls._append_icon_candidate(candidates, seen, raw)
        if ":" in raw:
            _, raw_token = raw.split(":", 1)
            engine_cls._append_icon_candidate(candidates, seen, raw_token)
    if fallback_name:
        engine_cls._append_icon_candidate(candidates, seen, fallback_name)
    return candidates


def app_icon_candidates(
    engine,
    current,
    *,
    app_id="",
    resolved_app_id="",
    app_name="",
    resolved_app_name="",
):
    candidates = []
    seen = set()
    for extra in (app_id, resolved_app_id, app_name, resolved_app_name):
        for candidate in engine.theme_icon_candidates_for_app_id(extra):
            if candidate not in seen:
                seen.add(candidate)
                candidates.append(candidate)
    for key in (
        "application.icon_name",
        "application.id",
        "pipewire.access.portal.app_id",
        "xdg.portal.app_id",
        "window.app_id",
        "window.x11.wm_class",
        "window.x11.instance",
        "window.class",
        "application.process.binary",
        "binary",
        "application.name",
    ):
        engine._append_icon_candidate(candidates, seen, current.get(key))
    return candidates


def make_app_route_key(engine_cls, prefix, value):
    token = engine_cls._normalize_app_route_token(value)
    if not token:
        return None
    return f"{prefix}:{token}"


def sanitize_app_label(engine_cls, value):
    if value is None:
        return None
    label = str(value).strip()
    if not label:
        return None
    mapped = engine_cls._BINARY_DISPLAY_NAMES.get(label.lower())
    if mapped:
        return mapped
    if "." in label and " " not in label and len(label.split(".")) >= 2:
        known = engine_cls._KNOWN_APP_IDS.get(label.lower())
        if known:
            return known
    label = label.replace("_", " ").replace("-", " ").strip()
    if label and label.islower():
        return label.title()
    return label


def display_name_for_app_id(engine_cls, app_id):
    if not app_id:
        return "Unknown App"
    if app_id == engine_cls.SYSTEM_SOUNDS_BUCKET:
        return engine_cls.SYSTEM_SOUNDS_BUCKET
    if ":" not in app_id:
        return engine_cls._sanitize_app_label(app_id) or app_id
    kind, raw = app_id.split(":", 1)
    if kind == "app":
        return engine_cls._canonicalize_app_id(raw) or engine_cls._sanitize_app_label(raw) or raw
    if kind == "snap":
        return raw.replace("-", " ").replace("_", " ").title()
    if kind == "stream":
        return f"Audio Stream #{raw}"
    return engine_cls._sanitize_app_label(raw) or raw.replace(".", " ").strip() or app_id


def is_legacy_stream_label(engine_cls, value):
    _ = engine_cls
    return isinstance(value, str) and value.startswith(("Media Stream #", "Audio Stream #"))


def is_persistent_app_id(engine_cls, app_id):
    if not app_id:
        return False
    if app_id == engine_cls.SYSTEM_SOUNDS_BUCKET:
        return True
    if engine_cls.is_legacy_stream_label(app_id):
        return False
    return not str(app_id).startswith("stream:")


def normalize_identity_override_map(engine_cls, raw):
    if not isinstance(raw, dict):
        return {}
    cleaned = {}
    for source_app_id, target_app_id in raw.items():
        if not isinstance(source_app_id, str) or not isinstance(target_app_id, str):
            continue
        source_app_id = source_app_id.strip()
        target_app_id = target_app_id.strip()
        if not source_app_id or not target_app_id:
            continue
        if source_app_id == engine_cls.SYSTEM_SOUNDS_BUCKET or target_app_id == engine_cls.SYSTEM_SOUNDS_BUCKET:
            continue
        if not engine_cls.is_persistent_app_id(source_app_id):
            continue
        if not engine_cls.is_persistent_app_id(target_app_id):
            continue
        cleaned[source_app_id] = target_app_id
    return cleaned


def normalize_label_override_map(engine_cls, raw):
    if not isinstance(raw, dict):
        return {}
    cleaned = {}
    for app_id, label in raw.items():
        if not isinstance(app_id, str):
            continue
        app_id = app_id.strip()
        if not app_id or not engine_cls.is_persistent_app_id(app_id):
            continue
        if app_id == engine_cls.SYSTEM_SOUNDS_BUCKET:
            continue
        normalized = engine_cls._sanitize_app_label(label) if label is not None else None
        if not normalized:
            continue
        cleaned[app_id] = normalized
    return cleaned


def set_app_identity_overrides(engine, overrides, labels):
    engine._app_identity_overrides = engine._normalize_identity_override_map(overrides)
    engine._app_identity_label_overrides = engine._normalize_label_override_map(labels)


def override_display_name_for_app_id(engine, app_id, fallback=None):
    label = getattr(engine, "_app_identity_label_overrides", {}).get(app_id)
    if label:
        return label
    if fallback:
        return fallback
    return engine.display_name_for_app_id(app_id)
