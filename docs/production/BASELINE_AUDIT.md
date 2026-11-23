# Baseline audit

Date: 2026-11-23. Host: Windows 10.0.26300 x64. Rust 1.99.1.

This document records the state before the production hardening work and the
evidence used to prioritize it. Measurements taken on the host above are not
claims about Linux or macOS.

## Toolchain inventory

| Tool | Result |
| --- | --- |
| `rustc` | 1.99.1 (58cf8f9c3 2026-10-13) |
| `cargo` | 1.99.1 (5f94df478 2026-08-27) |
| `git` | 2.56.0.windows.4 |
| `dotnet` | 11.0.100 |
| `node` | v26.13.1 |
| `npm` | 11.20.0 |
| Installed Rust targets | `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` |
| `cargo-audit` | absent |
| `cargo-deny` | absent |
| `cargo-nextest` | absent |
| `cargo-fuzz` | absent |
| `zig` | absent |

## Baseline gates

All four gates passed before changes (121 tests at that point):

```console
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --locked
cargo build --release --locked
```

## Defects and risks found

| ID | Severity | Finding | Evidence |
| --- | --- | --- | --- |
| B-01 | P0 | Fail-fast serialized matrix siblings: the semaphore limit was forced to 1 whenever fail-fast was enabled. | Runtime code; fixed by using `max-parallel` or host parallelism. |
| B-02 | P0 | Secrets existed only as redacted environment text. There was no provider, no fail-closed resolution, and no CLI. | `src/secrets/mod.rs` added. |
| B-03 | P0 | Artifact upload did not re-hash copied payloads; a transient or malicious write between hashing and publishing could be recorded as valid. | `verify_object` and re-hash on upload. |
| B-04 | P1 | Run status was an unvalidated string; concurrent writers could move a run backwards or resurrect a terminal run. | Typed `RunState` + transactional transition validation. |
| B-05 | P1 | Pipeline cache key included the entire workflow digest, so an unrelated job edit evicted every step cache. | Step-scoped key; integration test. |
| B-06 | P1 | No retention policy: runs, logs and artifact objects grew without bounds. | `maintenance stats/prune/gc/pin/unpin`. |
| B-07 | P1 | `concurrency.max > 1` was ignored, and `cancel-in-progress` only parsed. Bounded slots and peer cancellation implemented. | Runtime tests. |
| B-08 | P1 | Action steps had no stdout tail or log file, so `pipeline logs` showed nothing useful for actions. | Action message persisted as output + log. |
| B-09 | P1 | Symlinked files could point outside a packaged artifact tree; ZIP extraction relied only on relative-path syntax checks. | Containment check; adversarial traversal test. |
| B-10 | P2 | `pipeline plan` probed the toolchain for every call; a 48-job plan took about 4.5 s. | Criterion baseline; 30 s facts cache plus per-target capability cache. |
| B-11 | P2 | No trigger execution path; `on:` was metadata only. | Added polling/webhook daemon. |
| B-12 | P2 | No release workflow, checksums or SBOM in the repository. | Added release workflow and deterministic SBOM script. |

## Test baseline versus final

The baseline suite had 121 tests. The final suite adds regression coverage for
the fixed defects; the exact final count is recorded in
`FINAL_READINESS_REPORT.md` and in the `implementation status` document.

## Platform evidence

- Windows: native build, full test suite and runtime fixtures execute.
- Linux: Rust target installed; no Linux host, and no verified cross-linker.
  Linux CI is configured but has not run in this environment.
- macOS: no host, SDK or CI run. `macos-latest` is configured, unverified.

No signing credentials, Apple notarization identity or Windows code-signing
certificate were available. Signing is a hook, not a verified release step.
