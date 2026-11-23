#!/usr/bin/env python3
"""Emit a deterministic CycloneDX 1.5 SBOM from `cargo metadata`."""
import json
import subprocess
import sys

metadata = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        text=True,
    )
)
by_id = {package["id"]: package for package in metadata["packages"]}
ref = lambda package: f"pkg:cargo/{package['name']}@{package['version']}"
components = []
dependencies = []
for package in metadata["packages"]:
    component = {
        "type": "library",
        "bom-ref": ref(package),
        "name": package["name"],
        "version": package["version"],
        "purl": ref(package),
        "licenses": [{"expression": package["license"]}] if package.get("license") else [],
    }
    if package.get("repository"):
        component["externalReferences"] = [{"type": "vcs", "url": package["repository"]}]
    components.append(component)
for node in metadata["resolve"]["nodes"]:
    package = by_id.get(node["id"])
    if package is None:
        continue
    depends_on = [ref(by_id[dep]) for dep in node["dependencies"] if dep in by_id]
    dependencies.append({"ref": ref(package), "dependsOn": sorted(depends_on)})
sbom = {
    "bomFormat": "CycloneDX",
    "specVersion": "1.5",
    "version": 1,
    "metadata": {
        "tools": {"components": [{"type": "application", "name": "ubuilder-sbom"}]},
        "component": {
            "type": "application",
            "name": "ubuilder",
            "version": next(p["version"] for p in metadata["packages"] if p["name"] == "ubuilder"),
            "bom-ref": "pkg:cargo/ubuilder",
        },
    },
    "components": sorted(components, key=lambda item: item["bom-ref"]),
    "dependencies": sorted(dependencies, key=lambda item: item["ref"]),
}
json.dump(sbom, sys.stdout, indent=2, sort_keys=True)
sys.stdout.write("\n")
