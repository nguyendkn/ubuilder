#!/usr/bin/env python3
"""Package a ubuilder release binary with docs, deterministically."""
import argparse
import gzip
import io
import os
import tarfile
import time
import zipfile
from pathlib import Path

EPOCH = int(os.environ.get("SOURCE_DATE_EPOCH", "0"))
DATE = time.gmtime(max(EPOCH, 315532800))[:6]


def add_files(entries):
    entries.append((Path("README.md"), "README.md"))
    for path in sorted(Path("docs").rglob("*")):
        if path.is_file():
            entries.append((path, path.as_posix()))
    return entries


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", required=True, choices=["windows", "linux", "macos"])
    parser.add_argument("--arch", default=None, choices=["x64", "arm64"])
    parser.add_argument("--output", default="dist")
    parser.add_argument("--binary", default=None)
    args = parser.parse_args()
    binary = Path(args.binary or ("target/release/ubuilder.exe" if args.platform == "windows" else "target/release/ubuilder"))
    if not binary.is_file():
        raise SystemExit(f"missing release binary: {binary}")
    arch = args.arch or ("arm64" if args.platform == "macos" else "x64")
    out = Path(args.output)
    out.mkdir(parents=True, exist_ok=True)
    name = f"ubuilder-{args.platform}-{arch}.{'zip' if args.platform == 'windows' else 'tar.gz'}"
    archive = out / name
    entries = add_files([])
    entries.insert(0, (binary, "ubuilder.exe" if args.platform == "windows" else "ubuilder"))
    if args.platform == "windows":
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as z:
            for source, target in entries:
                info = zipfile.ZipInfo(target, DATE)
                info.external_attr = (0o755 << 16) if source == binary else (0o644 << 16)
                info.compress_type = zipfile.ZIP_DEFLATED
                z.writestr(info, source.read_bytes())
    else:
        with gzip.GzipFile(archive, "wb", mtime=EPOCH) as gz:
            with tarfile.open(fileobj=gz, mode="w") as tar:
                for source, target in entries:
                    data = source.read_bytes()
                    info = tarfile.TarInfo(target)
                    info.size = len(data)
                    info.mtime = EPOCH
                    info.mode = 0o755 if source == binary else 0o644
                    tar.addfile(info, io.BytesIO(data))
    print(archive)


if __name__ == "__main__":
    main()
