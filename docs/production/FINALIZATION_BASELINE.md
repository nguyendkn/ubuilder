# Finalization baseline

## Repository state at the start of finalization

| Item | Value |
| --- | --- |
| Repository | `C:/Users/gehihi/Documents/Projects/builder` |
| Baseline commit | `bbe890a` |
| Version | `ubuilder 0.1.0` |
| Working tree | dirty (release artifacts and generated workdirs) |
| Existing production report | claimed `PRODUCTION_READY_WITH_LIMITATIONS`, 139 passing tests |
| Existing release artifacts | `dist/ubuilder-windows-x64.zip`, `dist/sbom.cdx.json`, `dist/SHA256SUMS` produced from `bbe890a` |

The repository was treated as authoritative. Every claim in the prior report was
re-checked against code and a fresh command run. Artifacts from `bbe890a` were
not reused for certification because they predate the finalization changes.

## Host

| Item | Value |
| --- | --- |
| OS | Windows 10.0.26300 x64 |
| CPU | 16 logical processors |
| Rust | rustc/cargo 1.99.1 |
| Python | 3.13.17 |
| .NET SDK | 11.0.100 (`net10.0` fixture rolls forward) |
| Node.js | v26.13.1 |
| Git | available |

Only Windows x64 was available for execution. No Linux, macOS, or Windows ARM64
host was available.

## Tool availability

| Tool | State | Consequence |
| --- | --- | --- |
| `cargo` / `rustc` | available | Rust fixture, gates |
| `rustup` | available; only `x86_64-pc-windows-msvc` installed | Linux target cannot link |
| `dotnet` | available | .NET fixture build/package/run verified |
| `node` / `npm` | available; Electron fixture has no `node_modules` | Electron not built |
| `python` | available | packaging, SBOM, manifest scripts |
| `docker`, `podman` | absent | no container backend |
| `bwrap`, `firejail`, `sandbox-exec` | absent | no verified isolation backend |
| Windows Job Objects | available via API | rejected as a complete filesystem/network sandbox |
| `zig`, `gcc`, `clang` | absent | cross-linking blocked |
| `tauri`, `electron-builder` | absent | Tauri/Electron packaging not run |
| `signtool` | absent | Windows signing unverified |
| `cargo-audit`, `cargo-deny` | absent | dependency audit blocked |
| `cargo-nextest`, `cargo-fuzz` | absent | optional runners/fuzzing not run |

## Baseline gate commands

| Command | Baseline result | Notes |
| --- | --- | --- |
| `cargo fmt --all -- --check` | pass | re-verified during finalization |
| `cargo clippy --all-targets --locked -- -D warnings` | pass | log in `target/finalization-*.log` |
| `cargo test --all-targets --locked` | 139 passed | prior report; full suite re-run at the end |
| `cargo build --release --locked` | pass | prior binary replaced during finalization |

## Defects confirmed in the baseline

The detailed register is in `FINALIZATION_FINDINGS.md`. The highest-impact
baseline gaps were:

1. A newer SQLite schema was migrated blindly instead of rejected.
2. Concurrency slots used a fixed one-hour expiry with no live owner proof.
3. Cache identity ignored host and toolchain, so identical keys could cross
   machines and toolchains.
4. Every job received every resolved secret.
5. Secret files on Windows kept inherited ACLs.
6. Artifact path containment was lexical and did not resolve junctions.
7. `isolated` security mode did not exist; workflows always ran natively.
8. The daemon webhook token was optional, unbounded, replayable and loopback was
   not enforced; the detached child inherited the caller's stdout pipe.
9. Adversarial limits were partly untested (YAML alias bombs, matrix/expression
   explosion).
10. Release artifacts had no manifest and were stale.
11. `doctor` could not report every target in one invocation.
12. The documented release/operations process did not match the code.

## BLOCKED baseline capabilities

These remained blocked after finalization and are never reported as passing:

- Native execution on Linux x64 and macOS (x64/arm64).
- Cross-build to Linux x64 (`x86_64-linux-gnu-gcc` absent).
- Any GitHub Actions run (no CI execution evidence in this environment).
- Dependency vulnerability scan (`cargo-audit`/`cargo-deny` absent).
- Code signing / macOS notarization (`signtool`, signing identity absent).
- A verified OS-level sandbox on Windows (`ISOLATED_EXECUTION_PRODUCTION`).
- Electron and Tauri native packaging (toolchains absent).
