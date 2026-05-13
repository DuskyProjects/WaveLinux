#!/usr/bin/env python3
"""Build the signed-release manifest for a WaveLinux AppImage asset."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import time


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("--appimage", required=True, help="Path to the built AppImage")
    parser.add_argument("--version", required=True, help="WaveLinux version without a leading v")
    parser.add_argument("--repo", required=True, help="GitHub repo slug, e.g. DuskyProjects/WaveLinux")
    parser.add_argument("--tag", required=True, help="Git tag name, usually vX.Y.Z")
    parser.add_argument("--output", required=True, help="Manifest output path")
    return parser.parse_args()


def main():
    args = parse_args()
    appimage = Path(args.appimage).resolve()
    if not appimage.is_file():
        raise SystemExit(f"AppImage not found: {appimage}")
    asset_name = appimage.name
    tag = str(args.tag).strip()
    version = str(args.version).strip()
    repo = str(args.repo).strip()
    manifest = {
        "app": "WaveLinux",
        "version": version,
        "published_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "release_url": f"https://github.com/{repo}/releases/tag/{tag}",
        "assets": [
            {
                "name": asset_name,
                "kind": "appimage",
                "arch": "x86_64",
                "download_url": f"https://github.com/{repo}/releases/download/{tag}/{asset_name}",
                "sha256": sha256_file(appimage),
                "size_bytes": appimage.stat().st_size,
            }
        ],
    }
    output = Path(args.output)
    output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
