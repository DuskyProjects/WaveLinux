"""Device naming and channel-label helpers."""

from __future__ import annotations

import re


_JUNK_NAME_RE = re.compile(r"^(?:[A-Za-z]{1,3}\s?\d+\s?\d+)$|^Unknown", re.IGNORECASE)


def pretty_bt(raw):
    """Render a bluetooth node name as a readable MAC fallback."""
    raw = str(raw or "")
    match = re.search(r"([0-9A-Fa-f]{2}(?:[:_-][0-9A-Fa-f]{2}){5})", raw)
    if match:
        return "Bluetooth " + match.group(1).replace("_", ":").upper()
    return None


def friendly_name(raw):
    if not raw:
        return "Unknown"
    original = str(raw)
    name = original.strip()

    for prefix in [
        "Alsa Output.",
        "Alsa Input.",
        "alsa_output.",
        "alsa_input.",
        "bluez_output.",
        "bluez_input.",
    ]:
        if name.lower().startswith(prefix.lower()):
            name = name[len(prefix):]

    if original.lower().startswith(("bluez_output.", "bluez_input.")):
        bt = pretty_bt(original)
        if bt:
            return bt

    name = re.sub(r"pci-[0-9a-fA-F._-]+\.", "", name, flags=re.IGNORECASE)
    name = re.sub(r"Pci-[0-9a-fA-F. -]+Platform-\w+\s*", "", name, flags=re.IGNORECASE)
    name = re.sub(r"usb-[A-Za-z0-9_]+_[A-Za-z0-9_]+-\d+\.", "", name, flags=re.IGNORECASE)

    verbose_terms = [
        "High Definition Audio Controller",
        "HD Audio Controller",
        "Raptor Lake",
        "Alder Lake",
        "Comet Lake",
        "Tiger Lake",
        "Meteor Lake",
        "Cannon Lake",
        "Coffee Lake",
        "Sunrise Point",
        "Cezanne",
        "Renoir",
        "Rembrandt",
        "Phoenix",
        "Starship/Matisse",
        "Matisse",
        "Family 17h",
        "Family 19h",
        "PCH",
        "USB Audio",
        "Generic",
        "Built-in",
    ]
    for term in verbose_terms:
        name = re.sub(r"\b" + re.escape(term) + r"\b", "", name, flags=re.IGNORECASE)

    if re.search(r"\bALC\d+\b", name, re.IGNORECASE):
        suffix = re.search(r"(Analog|Digital|HDMI)\b.*", name, re.IGNORECASE)
        name = "Onboard"
        if suffix:
            name = f"Onboard {suffix.group(0).strip().title()}"

    name = name.replace("_", " ").replace(".", " ").replace("-", " ")
    name = re.sub(r"\s+", " ", name).strip()

    if not name or _JUNK_NAME_RE.match(name):
        return original

    name = name.title()
    if len(name) > 28:
        parts = name.split()
        if len(parts) > 3:
            name = " ".join(parts[-3:])
        if len(name) > 28:
            name = name[:26] + "…"

    return name or original


def sanitize_channel_name(display_name):
    """Turn user-facing channel names into stable sink-safe keys."""
    cleaned = re.sub(r"\s+", " ", str(display_name or "").strip())
    safe = re.sub(r"[^A-Za-z0-9_]+", "_", cleaned.lower()).strip("_")
    return cleaned, safe or "channel"


def branding_label(display_clean):
    """Build the visible WaveLinux-* device label."""
    if not display_clean:
        return "WaveLinux"
    compact = re.sub(r"\s+", "-", str(display_clean).strip())
    return f"WaveLinux-{compact}"
