#!/usr/bin/env python3
"""Verify a signed WaveLinux release manifest against a local AppImage."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

ROOT_DIR = Path(__file__).resolve().parents[1]
if str(ROOT_DIR) not in sys.path:
    sys.path.insert(0, str(ROOT_DIR))

from updates import file_sha256, select_appimage_asset, verify_release_manifest_bytes


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", help="Path to wavelinux-release-manifest.json")
    parser.add_argument("signature", help="Path to wavelinux-release-manifest.sig")
    parser.add_argument("appimage", help="Path to the AppImage asset to validate")
    return parser.parse_args()


def main():
    args = parse_args()
    manifest_path = Path(args.manifest)
    signature_path = Path(args.signature)
    appimage_path = Path(args.appimage).resolve()
    manifest = verify_release_manifest_bytes(
        manifest_path.read_bytes(),
        signature_path.read_bytes(),
    )
    asset = select_appimage_asset(manifest)
    actual_name = appimage_path.name
    if actual_name != asset["name"]:
        raise SystemExit(
            f"Manifest asset name mismatch: expected {asset['name']}, got {actual_name}"
        )
    actual_sha = file_sha256(str(appimage_path))
    if actual_sha != asset["sha256"]:
        raise SystemExit(
            f"Manifest checksum mismatch: expected {asset['sha256']}, got {actual_sha}"
        )
    print(json.dumps({
        "manifest_verified": True,
        "asset_name": asset["name"],
        "sha256": actual_sha,
    }, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
