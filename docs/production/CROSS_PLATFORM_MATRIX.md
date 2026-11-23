# Cross-platform matrix

Status vocabulary:

- `SUPPORTED`: implemented and verified in the scope where it is claimed.
- `CONDITIONAL`: works only when named host tools and inputs are present.
- `EXPERIMENTAL`: implemented, not covered by an acceptance test.
- `UNSUPPORTED`: the target cannot be produced/run with the available toolchain.
- `UNVERIFIED`: no execution evidence on this host; never treated as supported.
- `BLOCKED`: an external prerequisite is missing (recorded as a blocker, not a
  pass or a support claim).

Only Windows x64 was executed. No Linux, macOS, or ARM64 machine was available.

## Host and target execution

| Host -> target | CLI build | Tests | Native fixture build | Cross-build status |
| --- | --- | --- | --- | --- |
| Windows x64 -> Windows x64 | VERIFIED | VERIFIED (all gates) | VERIFIED: Rust (`fixtures/rust-cli`) and .NET (`fixtures/dotnet-console`) built, packaged, validated and executed | `SUPPORTED` |
| Windows x64 -> Windows ARM64 | UNVERIFIED | UNVERIFIED | UNVERIFIED | `UNVERIFIED` |
| Windows x64 -> Linux x64 | BLOCKED | BLOCKED | UNVERIFIED | `UNVERIFIED` (`x86_64-unknown-linux-gnu` target is installed; no `x86_64-linux-gnu-gcc` linker) |
| Windows x64 -> Linux ARM64 | BLOCKED | BLOCKED | UNVERIFIED | `UNSUPPORTED` (no target or linker installed) |
| Windows x64 -> macOS x64/ARM64 | BLOCKED | BLOCKED | UNVERIFIED | `UNSUPPORTED` (no SDK or toolchain; macOS cannot be cross-built from Windows here) |
| Linux/macOS -> any | not executed | not executed | not executed | `UNVERIFIED` |

The local Rust installation also lists `x86_64-unknown-linux-gnu`, but an
installed target is not proof of a working linker. No non-Windows binary is
claimed.

## Native capability report (Windows host)

Command: `ubuilder --json doctor --all` (exit 0, seven canonical targets).

| Target | Native | Compilation | Packaging | Signing | Native testing |
| --- | --- | --- | --- | --- | --- |
| `windows-x64` | yes | `CONDITIONAL` | `SUPPORTED` | `UNSUPPORTED` | `SUPPORTED` |
| `windows-arm64` | no | `UNSUPPORTED` | `SUPPORTED` | `UNSUPPORTED` | `UNSUPPORTED` |
| `linux-x64` | no | `UNSUPPORTED` | `SUPPORTED` | `UNSUPPORTED` | `UNSUPPORTED` |
| `linux-arm64` | no | `UNSUPPORTED` | `SUPPORTED` | `UNSUPPORTED` | `UNSUPPORTED` |
| `macos-x64` | no | `UNSUPPORTED` | `SUPPORTED` | `UNSUPPORTED` | `UNSUPPORTED` |
| `macos-arm64` | no | `UNSUPPORTED` | `SUPPORTED` | `UNSUPPORTED` | `UNSUPPORTED` |
| `macos-universal` | no | `UNSUPPORTED` | `SUPPORTED` | `UNSUPPORTED` | `UNSUPPORTED` |

`windows-x64` compilation is `CONDITIONAL` because `ubuilder` did not find a
native linker on `PATH`; it reports `linker-unverified` and names the missing
"native linker" requirement. Rust builds still succeeded because rustc located
its MSVC linker independently. Non-native `packaging: SUPPORTED` means a portable
archive recipe exists; it does not mean a native installer or a successful build
was verified.

## Framework fixtures

| Fixture | Detection | Build | Package | Runtime | Status |
| --- | --- | --- | --- | --- | --- |
| Rust (`fixtures/rust-cli`) | pass | pass | portable ZIP, validated | test harness | VERIFIED |
| .NET (`fixtures/dotnet-console`) | pass | pass (`net10.0`, SDK 11.0.100) | portable ZIP, all validations passed | extracted `HelloDotnet.exe` printed `hello from HelloDotnet` | VERIFIED |
| Electron (`fixtures/electron-app`) | pass | not run (no `node_modules`/`electron-builder`) | not run | not run | UNVERIFIED |
| Tauri (`fixtures/tauri-app`) | pass | not run (no `tauri` CLI) | not run | not run | UNVERIFIED |

## Platform-sensitive behavior

| Behavior | Windows | Unix | Evidence |
| --- | --- | --- | --- |
| Shell default | PowerShell `pwsh`/`powershell` | `sh`/`bash` | Runtime tests use platform-specific commands. |
| Process-tree termination | `taskkill /T /F` | process group/signal | Timeout and cancel tests. |
| Secret file permissions | inheritance removed with `icacls`; only the current user has full control; reparse points rejected | `0600` file, `0700` directory | `windows_secret_file_has_no_inherited_aces`; Unix mode tests |
| Path containment | canonicalized before join; directory junctions rejected | canonicalized; symlinks rejected | Windows junction test executed; Unix symlink test compiled but not executed |
| Daemon stop | `taskkill /T /F` with captured output | `kill -TERM` | Daemon lifecycle tests executed on Windows. |
| Webhook bind default | loopback (`127.0.0.1`); non-IP bind rejected | same | `webhook_requires_configuration_and_authenticates_requests` |

## Signing

| Platform | Requirement | Status |
| --- | --- | --- |
| Windows Authenticode | certificate + `signtool` | Hook only; `signtool` and certificate absent; artifact labeled `UNSIGNED`. |
| macOS codesign/notarization | Developer ID + `notarytool` | Hook only; no identity, no macOS execution. |
| Linux | optional GPG/checksums | `SHA256SUMS` generated; no signing key. |
