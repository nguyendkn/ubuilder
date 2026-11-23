# Release process

## Preconditions

- `Cargo.toml` version is the intended release version.
- All four gates pass on every target platform.
- `docs/production/FINAL_READINESS_REPORT.md` reflects the release.
- Signing credentials are available if a signed release is claimed.

## Gate commands

```console
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
```

## Versioning

Semantic versioning for the CLI and JSON schema. The JSON envelope keys
(`ok`, `command`, `data`, `error.code`, `error.message`, `error.remediation`,
`error.details`) and exit codes (0, 1, 2, 3, 130) are stable. New optional fields
may be added; removals or meaning changes require a major version.

## Artifacts

The release workflow produces:

| Artifact | Contents |
| --- | --- |
| `ubuilder-windows-x64.zip` | `ubuilder.exe`, README, docs |
| `ubuilder-linux-x64.tar.gz` | `ubuilder`, README, docs |
| `ubuilder-macos.tar.gz` | `ubuilder`, README, docs |
| `SHA256SUMS` | Combined per-platform checksums |
| `sbom.cdx.json` | CycloneDX 1.5 SBOM of the locked dependency graph |

Local production:

```console
cargo build --release --locked
python scripts/package.py --platform windows --output dist
python scripts/package.py --platform linux --output dist
python scripts/package.py --platform macos --output dist
python scripts/sbom.py > dist/sbom.cdx.json
python scripts/checksums.py dist
```

`scripts/package.py` produces deterministic archives (sorted entries, fixed
timestamps, executable mode) and works on every host; packaging a Linux archive
on Windows is not native execution evidence. `scripts/checksums.py` hashes every
file in the output directory except `SHA256SUMS` itself.

Never publish an artifact unless its checksum was computed from the exact bytes
uploaded. Never claim a signed artifact without a verifiable signature.

## Signing hook

The workflow exposes `UBUILDER_SIGN_COMMAND`. It is evaluated by the shell after
packaging and before upload. No keys live in the repository. If the variable is
empty, the release is unsigned and must be labeled as such.

## SBOM

`scripts/sbom.py` reads `cargo metadata --locked --format-version 1` and emits a
deterministic CycloneDX document: sorted components, purl references, license
expressions and dependency edges. It does not query external vulnerability
databases.

## Publishing checklist

1. Tag `vX.Y.Z` and push.
2. Confirm the release workflow passed on Windows, Linux and macOS.
3. Download `SHA256SUMS` and verify each archive from a clean directory.
4. Validate `sbom.cdx.json` as JSON and confirm the `ubuilder` component version.
5. Record the verification in the release notes.
6. Mark unsigned artifacts explicitly.
