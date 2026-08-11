#!/usr/bin/env python3
"""Create a deterministic manifest for a staged WaveLinux release payload."""

from __future__ import annotations

import argparse
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--assets", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()

    assets = args.assets.resolve()
    output = args.output.resolve()
    entries = []
    for path in sorted(assets.iterdir(), key=lambda candidate: candidate.name):
        if not path.is_file() or path.resolve() == output or path.name == "SHA256SUMS":
            continue
        entries.append(
            {
                "name": path.name,
                "bytes": path.stat().st_size,
                "sha256": sha256(path),
            }
        )

    if not entries:
        raise SystemExit("release asset directory is empty")

    manifest = {
        "schema": 1,
        "product": "WaveLinux6",
        "version": args.version,
        "tag": args.tag,
        "commit": args.commit,
        "created_at": datetime.now(timezone.utc).replace(microsecond=0).isoformat(),
        "artifacts": entries,
    }
    output.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
