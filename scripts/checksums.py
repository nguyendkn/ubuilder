#!/usr/bin/env python3
"""Write SHA256SUMS for every regular file in a directory except itself."""
import hashlib
import sys
from pathlib import Path

root = Path(sys.argv[1] if len(sys.argv) > 1 else "dist")
lines = []
for path in sorted(root.iterdir()):
    if path.is_file() and path.name != "SHA256SUMS":
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        lines.append(f"{digest}  {path.name}")
(root / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")
print((root / "SHA256SUMS").read_text(encoding="utf-8"), end="")
