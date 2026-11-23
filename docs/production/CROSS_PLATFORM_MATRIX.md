# Cross-platform matrix

Status vocabulary:

- VERIFIED: executed in this environment with the recorded evidence.
- CONFIGURED: CI or code path exists, no local execution evidence.
- UNVERIFIED: no evidence; do not claim support.
- BLOCKED: an external prerequisite is missing.

## Host and target evidence

| Platform / target | CLI build | Unit + integration tests | Native build fixture | Status |
| --- | --- | --- | --- | --- |
| Windows x64 (`x86_64-pc-windows-msvc`) | VERIFIED | VERIFIED | VERIFIED (`fixtures/rust-cli`) | VERIFIED |
| Linux x64 (`x86_64-unknown-linux-gnu`) | CONFIGURED in CI | CONFIGURED in CI | UNVERIFIED | UNVERIFIED |
| macOS arm64/x64 | CONFIGURED in CI | CONFIGURED in CI | UNVERIFIED | UNVERIFIED |
| Windows arm64 | UNVERIFIED | UNVERIFIED | UNVERIFIED | UNVERIFIED |
| Linux arm64 | UNVERIFIED | UNVERIFIED | UNVERIFIED | UNVERIFIED |
| Cross-compiled Linux from Windows | UNVERIFIED | n/a | UNVERIFIED | BLOCKED: no verified linker |

The local Rust installation lists `x86_64-unknown-linux-gnu`, but a target being
installed does not prove a linker works. No Linux binary is claimed.

## Capability classes

`ubuilder doctor --json` reports per-target capability with these levels:
`SUPPORTED`, `CONDITIONAL`, `EXPERIMENTAL`, `UNSUPPORTED`. The production matrix
adds `UNVERIFIED` for platforms that have no evidence. A plan that cannot run is
marked `executable: false` with a remediation; it is not silently attempted.

## Platform-sensitive behavior

| Behavior | Windows | Unix | Evidence |
| --- | --- | --- | --- |
| Shell default | PowerShell `pwsh`/`powershell` | `sh`/`bash` | Runtime tests use platform-specific commands. |
| Process-tree termination | `taskkill /T /F` | process group/signal | Timeout and cancel tests. |
| Secret file permissions | inherited ACLs | `0600` file, `0700` dir | Unix-only permissions; Windows residual risk. |
| Symlink containment test | not executable without privileges | unit test | `#[cfg(unix)]` test. |
| Daemon stop | `taskkill /T /F` | `kill -TERM` | Code paths; detached stop untested in CI. |

## Signing

| Platform | Requirement | Status |
| --- | --- | --- |
| Windows Authenticode | certificate + `signtool` | Hook only; no certificate. |
| macOS codesign/notarization | Developer ID + `notarytool` | Hook only; no identity. |
| Linux | optional GPG/checksums | Checksums scripted; no signing key. |
