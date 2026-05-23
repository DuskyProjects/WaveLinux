#!/usr/bin/env python3
"""Build Tauri v2 updater latest.json metadata for WaveLinux Linux bundles."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import time


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact", required=True, help="Path to the .AppImage.tar.gz updater bundle")
    parser.add_argument("--version", required=True, help="Version without a leading v")
    parser.add_argument("--repo", default="DuskyProjects/WaveLinux", help="GitHub repo slug")
    parser.add_argument("--tag", required=True, help="GitHub release tag")
    parser.add_argument("--output", required=True, help="Output latest.json path")
    parser.add_argument("--notes", default="WaveLinux desktop update", help="Release notes")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    artifact = Path(args.artifact).resolve()
    signature = artifact.with_suffix(artifact.suffix + ".sig")
    if not artifact.is_file():
        raise SystemExit(f"Updater bundle not found: {artifact}")
    if not signature.is_file():
        raise SystemExit(f"Updater signature not found: {signature}")

    asset_name = artifact.name
    tag = str(args.tag).strip()
    repo = str(args.repo).strip()
    platform = {
        "signature": signature.read_text(encoding="utf-8").strip(),
        "url": f"https://github.com/{repo}/releases/download/{tag}/{asset_name}",
    }
    manifest = {
        "version": str(args.version).strip().lstrip("v"),
        "notes": str(args.notes),
        "pub_date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platforms": {
            "linux-x86_64": platform,
            "linux-x86_64-appimage": platform,
        },
    }

    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
