#!/usr/bin/env python3
"""Write release-manifest.json for the artifacts actually present in a directory."""
import argparse
import datetime
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

ARTIFACTS = {
    "ubuilder-windows-x64.zip": ("windows", "x64"),
    "ubuilder-linux-x64.tar.gz": ("linux", "x64"),
    "ubuilder-macos-arm64.tar.gz": ("macos", "arm64"),
    "ubuilder-macos-x64.tar.gz": ("macos", "x64"),
}


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dir", default="dist")
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", default=None)
    parser.add_argument("--smoke-status", default=None)
    parser.add_argument("--smoke-command", default=None)
    parser.add_argument("--output", default=None)
    args = parser.parse_args()
    root = Path(args.dir)
    commit = args.commit or subprocess.check_output(
        ["git", "rev-parse", "HEAD"], text=True
    ).strip()
    timestamp = os.environ.get("SOURCE_DATE_EPOCH")
    generated_at = (
        datetime.datetime.fromtimestamp(int(timestamp), datetime.timezone.utc)
        if timestamp
        else datetime.datetime.now(datetime.timezone.utc)
    ).replace(microsecond=0).isoformat().replace("+00:00", "Z")

    artifacts = []
    for name, (platform, architecture) in sorted(ARTIFACTS.items()):
        path = root / name
        if not path.is_file():
            continue
        artifacts.append(
            {
                "filename": name,
                "platform": platform,
                "architecture": architecture,
                "version": args.version,
                "sha256": sha256(path),
                "size": path.stat().st_size,
                "sbom": "sbom.cdx.json",
                "signature_status": "UNSIGNED",
            }
        )
    if not artifacts:
        raise SystemExit(f"no release archives found in {root}")

    sbom = root / "sbom.cdx.json"
    smoke = None
    if args.smoke_status:
        smoke = {
            "status": args.smoke_status,
            "command": args.smoke_command,
            "platform": "windows",
            "architecture": "x64",
        }
    manifest = {
        "schema_version": 1,
        "product": "ubuilder",
        "version": args.version,
        "commit": commit,
        "generated_at": generated_at,
        "artifacts": artifacts,
        "sbom": (
            {"filename": sbom.name, "sha256": sha256(sbom)} if sbom.is_file() else None
        ),
        "signing": {
            "status": "UNSIGNED",
            "reason": "no signing credentials were available",
        },
        "smoke_test": smoke,
    }
    output = Path(args.output) if args.output else root / "release-manifest.json"
    output.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(output)


if __name__ == "__main__":
    sys.exit(main())
