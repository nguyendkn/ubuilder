# Release process

Verified on Windows x64. Linux and macOS packaging is defined but has not been
executed from this workspace; do not publish those archives without running the
release flow on the target OS.

## Preconditions

- `Cargo.toml` version is the intended release version.
- The working tree is clean and the commit is recorded.
- All four gates pass on every platform included in the release scope.
- `docs/production/FINAL_READINESS_REPORT.md` reflects the actual scope.
- Signing credentials are available **only** if a signed release is claimed.

## Gate commands

```console
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
```

The CI workflow (`.github/workflows/ci.yml`) runs these with `--locked` on
Windows, Ubuntu and macOS plus an audit job. A workflow definition is not proof
that CI ran; record the run URL and commit before claiming CI evidence.

## Versioning

Semantic versioning for the CLI and JSON schema. The JSON envelope keys (`ok`,
`command`, `data`, `error.code`, `error.message`, `error.remediation`,
`error.details`) and exit codes (0, 1, 2, 3, 130) are stable. New optional fields
may be added; removals or meaning changes require a major version.

## Artifacts

| Artifact | Contents |
| --- | --- |
| `ubuilder-windows-x64.zip` | `ubuilder.exe`, README, docs |
| `ubuilder-linux-x64.tar.gz` | `ubuilder`, README, docs |
| `ubuilder-macos-arm64.tar.gz` | `ubuilder`, README, docs |
| `ubuilder-macos-x64.tar.gz` | `ubuilder`, README, docs |
| `SHA256SUMS` | Per-file checksums for every other artifact |
| `sbom.cdx.json` | CycloneDX 1.5 SBOM of the locked dependency graph |
| `release-manifest.json` | Version, commit, per-artifact SHA-256/size, SBOM reference, signature status, smoke test |

Only produce archives for platforms actually built. Packaging a Linux archive on
Windows is not native execution evidence.

## Local release sequence

```console
# 1. Build from the exact commit being released.
cargo build --release --locked

# 2. Package only the native artifacts that exist.
python scripts/package.py --platform windows --arch x64 --output dist

# 3. Generate the SBOM from the locked graph.
python scripts/sbom.py > dist/sbom.cdx.json

# 4. Extract the archive and run smoke tests before writing the manifest.
#    Record the exact commands and exit codes.
python scripts/manifest.py --version 0.1.0 --dir dist \
  --smoke-status passed \
  --smoke-command "ubuilder.exe --version; ubuilder.exe --json doctor --all; ubuilder.exe target list --json"

# 5. Checksums last: SHA256SUMS covers archives, SBOM and manifest.
python scripts/checksums.py dist
```

`scripts/package.py` produces deterministic archives (sorted entries, fixed
timestamps, executable mode). `--arch` selects the filename and defaults to
`x64` for Windows/Linux and `arm64` for macOS. The archive binary is always named
`ubuilder.exe` on Windows and `ubuilder` elsewhere.

`scripts/manifest.py` derives the commit with `git rev-parse HEAD`, hashes the
files actually present, and refuses to write a manifest with no archive. Its
`signature_status` is `UNSIGNED` and the signing reason is recorded for every
artifact unless a signing process updates it with verified evidence.

## Smoke tests

For each produced archive:

1. Extract to a clean directory.
2. Run `<binary> --version` and require exit 0.
3. Run `<binary> --json doctor --all` and require exit 0 with seven targets.
4. Run `<binary> target list --json` and require exit 0.
5. Record the archive SHA-256 and the output in `RELEASE_VERIFICATION.md`.

Do not record a smoke test as passed unless the extracted binary was executed.

## Signing hook

`UBUILDER_SIGN_COMMAND` is exposed by the release workflow and runs after
packaging, before upload. No keys live in the repository. If the variable is
empty, artifacts are unsigned and every manifest entry says `UNSIGNED`.

Recommended verification, when credentials exist:

| Platform | Sign | Verify |
| --- | --- | --- |
| Windows | `signtool sign /fd SHA256 ...` | `signtool verify /pa /all <file>` |
| macOS | `codesign --sign "Developer ID Application: ..."` | `codesign --verify --deep --strict`, then `xcrun notarytool submit` and `stapler staple` |
| Linux | detached GPG signature or provenance attestation | verify against the published key |

Signing and notarization verification are BLOCKED in this workspace: no
`signtool`, certificate, Developer ID or notarization credentials.

## SBOM

`scripts/sbom.py` reads `cargo metadata --locked --format-version 1` and emits a
deterministic CycloneDX document: sorted components, purl references, license
expressions and dependency edges. It does not query vulnerability databases; an
SBOM is an inventory, not a security scan.

## Reproducibility

- Dependencies are locked by `Cargo.lock` and every gate uses `--locked`.
- Archives sort entries and use a fixed timestamp (`SOURCE_DATE_EPOCH` or epoch
  0, clamped to a valid ZIP date).
- The binary itself is not byte-reproducible across compilers, linkers or hosts;
  document the toolchain with every release. Digests are valid for the exact
  uploaded bytes only.
- `release-manifest.json` contains the source commit and generation time.

## Publishing checklist

1. Tag `vX.Y.Z` at the verified commit and push.
2. Confirm the release workflow passed on every claimed platform; record run
   URLs.
3. Download `SHA256SUMS` and verify every archive from a clean directory.
4. Validate the archive contents and run the smoke tests from the extracted
   binary.
5. Validate `sbom.cdx.json` as JSON and confirm the `ubuilder` version.
6. Confirm `release-manifest.json` hashes match the uploaded artifacts and the
   signature status is truthful.
7. Record the verification in the release notes.
8. Mark unsigned artifacts explicitly.

Never publish an artifact unless its checksum was computed from the exact bytes
uploaded.
