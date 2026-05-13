#!/usr/bin/env python3
"""Sign a WaveLinux release manifest with an Ed25519 private key."""

from __future__ import annotations

import argparse
import base64
import os
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", help="Path to wavelinux-release-manifest.json")
    parser.add_argument("signature", help="Path to write the raw signature bytes")
    parser.add_argument(
        "--private-key-b64",
        default=os.environ.get("WAVELINUX_RELEASE_ED25519_PRIVATE_KEY_B64", ""),
        help="Base64-encoded 32-byte raw Ed25519 private key",
    )
    return parser.parse_args()


def main():
    args = parse_args()
    key_b64 = str(args.private_key_b64 or "").strip()
    if not key_b64:
        raise SystemExit(
            "Missing Ed25519 signing key. Set WAVELINUX_RELEASE_ED25519_PRIVATE_KEY_B64."
        )
    private_key = Ed25519PrivateKey.from_private_bytes(base64.b64decode(key_b64))
    manifest_path = Path(args.manifest)
    signature_path = Path(args.signature)
    payload = manifest_path.read_bytes()
    signature_path.write_bytes(private_key.sign(payload))


if __name__ == "__main__":
    main()
