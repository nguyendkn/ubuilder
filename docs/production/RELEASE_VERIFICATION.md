# Release verification

Date: 2026-11-23
Version: `ubuilder 0.1.0`
Artifact source commit: `bb2708c90440e014df74a32b8dd3bb5681b8e4b4`
Code commit: `a4cd66d0e206670872a81622d8742231c316dd42`
Host: Windows 10.0.26300 x64, Rust 1.99.1
Signing: **UNSIGNED**

Only the Windows x64 artifact was produced and executed. No Linux, macOS, ARM64,
signed or notarized artifact is claimed.

## Commands

```console
cargo build --release --locked
python scripts/package.py --platform windows --arch x64 --output dist
python scripts/sbom.py > dist/sbom.cdx.json
# extract to dist-smoke, then run smoke commands below
python scripts/manifest.py --version 0.1.0 --dir dist \
  --smoke-status passed \
  --smoke-command "ubuilder.exe --version; ubuilder.exe --json doctor --all; ubuilder.exe target list --json"
python scripts/checksums.py dist
```

## Artifacts

| File | Size (bytes) | SHA-256 |
| --- | ---: | --- |
| `ubuilder-windows-x64.zip` | 3,805,328 | `b63a6e45c3463c10ed6739f3b0fb3557601041cfe3eaae29c015549c5cd75864` |
| `sbom.cdx.json` | 99,857 | `b3ba576aec19310f886f956aa51233accf809044044f5de977d533e5ead0cea2` |
| `release-manifest.json` | 982 | `a662a537589cef0c2a134468eec93ccb8b3c0fe3f9d546cef07ef5687279cd40` |
| `SHA256SUMS` | 262 | `88ee96447665aab5a34c85f51a310399f2fcad452f1a59d54c8697f82e8a7a79` |

`SHA256SUMS` was generated last and verifies every other file in `dist/`:

```console
$ cd dist && sha256sum -c SHA256SUMS
release-manifest.json: OK
sbom.cdx.json: OK
ubuilder-windows-x64.zip: OK
```

The archive contains 30 entries: `ubuilder.exe` (9,406,976 bytes, SHA-256
`37aeda730672321f7a97592d4c49ca0deee6c00cd8c9dfb40e7f41cfdf10b923`),
`README.md` and 28 documentation files. The binary inside the archive is the
release binary built from the code commit.

## Reproducibility

Re-running packaging into a fresh directory produced byte-identical output:

```console
$ cmp dist/ubuilder-windows-x64.zip target/repro/ubuilder-windows-x64.zip
$ sha256sum target/repro/ubuilder-windows-x64.zip
b63a6e45c3463c10ed6739f3b0fb3557601041cfe3eaae29c015549c5cd75864  target/repro/ubuilder-windows-x64.zip
```

This proves reproducible packaging for the same source tree on this host. It does
not prove that a different compiler or host produces the same executable.

## Smoke tests

Run from the extracted archive in a clean directory with a fresh data directory.

| Check | Command | Exit | Output |
| --- | --- | ---: | --- |
| Version | `ubuilder.exe --version` | 0 | `ubuilder 0.1.0` |
| Diagnostics | `ubuilder.exe --json doctor --all` | 0 | `ok: true`, command `doctor`, 7 targets |
| Target list | `ubuilder.exe target list --json` | 0 | 7 target entries |

`doctor --all` returned the canonical order:
`windows-x64, windows-arm64, linux-x64, linux-arm64, macos-x64, macos-arm64,
macos-universal`. Raw output is in `target/release-smoke-doctor.json`,
`target/release-smoke-targets.json` and `target/release-smoke-version.txt`.

`doctor --all` diagnoses capabilities; it does not execute those platforms.
`windows-x64` compilation is reported `CONDITIONAL` because the native linker is
not on `PATH`; packaging is `SUPPORTED`. Other targets remain unexecuted.

## SBOM

`dist/sbom.cdx.json` is CycloneDX 1.5, produced from
`cargo metadata --locked --format-version 1` by `scripts/sbom.py`. It contains
168 components and 168 dependency nodes. It is an inventory, not a vulnerability
scan.

## Manifest

`dist/release-manifest.json` records:

- schema version 1, product `ubuilder`, version `0.1.0`;
- commit `bb2708c90440e014df74a32b8dd3bb5681b8e4b4`;
- generated timestamp `2026-11-23T16:07:32Z`;
- artifact platform `windows`, architecture `x64`, size and SHA-256;
- SBOM filename and SHA-256;
- signing status `UNSIGNED` with reason;
- smoke test status `passed` and the exact command.

## Signing

No certificate, `signtool`, Developer ID or notarization credentials were
available. The archive is unsigned and the manifest says so. No signature
verification was performed or claimed.

## Not verified

- Linux x64, macOS x64, macOS arm64 and Windows arm64 archives.
- Any CI-produced artifact; the release workflow has not run here.
- Authenticode, codesign, notarization or GPG signatures.
- Dependency vulnerability status.
