# Final production readiness report

Date: 2026-11-23
Host: Windows 10.0.26300 x64, 16 logical CPUs, Rust 1.99.1
Version: `ubuilder 0.1.0`
Verdict: **PRODUCTION_READY_WITH_LIMITATIONS**

The workflow engine is production-ready for local, trusted-user use on Windows
x64. It is not certified for Linux or macOS, not sandboxed against untrusted
workflows, and not released with signing or a dependency vulnerability scan.

## 1. Implemented changes

| Area | Change |
| --- | --- |
| Scheduling | Fail-fast no longer serializes matrix siblings; bounded concurrency slots; `cancel-in-progress` signals peers through the cancellation marker. |
| Secrets | Env/file providers, masked values, fail-closed single resolution, redaction on every serialized surface, `ubuilder secret` CLI. |
| State | Typed `RunState` and transition table for runs, jobs and steps; transactional validation; persisted legacy spellings. |
| Cache | Step-scoped cache key; unrelated workflow edits no longer evict; corruption detection retained. |
| Artifacts | Re-hash on upload, manifest verification, symlink containment, retention-safe publish. |
| Retention | `maintenance stats/prune/gc/pin/unpin`; newest terminal runs kept, active runs skipped, pinned objects protected. |
| Observability | Action steps now persist a redacted `stdout_tail` and a step log. |
| Performance | 30 s toolchain facts cache and per-target capability cache; Criterion benchmarks. |
| Automation | Daemon with git/workflow fingerprint polling, loopback webhook, staff trigger file and once mode. |
| Release | Three-OS CI, tag-driven release workflow, deterministic packaging, checksums, CycloneDX SBOM, optional signing hook. |
| Documentation | Audit, architecture, threat model, risks, gaps, roadmap, platform matrix, performance, security tests, release, operations, migration and this report. |

No second scheduler was introduced. Legacy `build`, `package` and `release`
commands remain functional.

## 2. Resolved defects

| ID | Defect | Verification |
| --- | --- | --- |
| B-01 | Fail-fast forced matrix concurrency to 1 | `fail_fast_does_not_serialize_matrix_siblings` |
| B-02 | No secret provider or fail-closed resolution | `missing_secret_fails_closed_before_running_jobs`, `secrets_are_injected_and_redacted_everywhere` |
| B-03 | Artifact upload did not re-hash | `builds::rust_fixture_builds_with_manifest_and_validated_artifact` plus unit tests |
| B-04 | Unvalidated lifecycle strings could move terminal runs backwards | `terminal_run_state_cannot_transition_back_to_running` (`STATE_INVALID`) |
| B-05 | Cache invalidated by unrelated workflow edits | `cache_survives_unrelated_workflow_edits` |
| B-06 | Unbounded run/artifact growth | `maintenance_prune_keeps_newest_runs_and_protects_active`, `maintenance_pin_protects_and_gc_removes_unreferenced_objects` |
| B-07 | `concurrency.max > 1` ignored; `cancel-in-progress` parsed only | `concurrency_group_serializes_jobs_without_deadlock`; peer cancellation code path |
| B-08 | Action steps had no output/log | `action_steps_capture_message_output` |
| B-09 | Symlink/ZIP escape gaps | `copy_tree_rejects_symlinks_escaping_the_root` (Unix), `extraction_rejects_traversal_entries` |
| B-10 | Planning dominated by toolchain probes | Criterion: 4.5 s cold -> 71 ms warm |
| B-11 | Triggers were metadata only | `daemon_once_runs_the_workflow_and_records_state`; manual polling/webhook checks |
| B-12 | No release pipeline/SBOM | `release.yml`, `scripts/package.py`, `scripts/sbom.py`, verified local artifact |

## 3. Actual test results

Command: `cargo test --all-targets --locked` (final run, 102.34 s wall).

| Suite | Passed | Failed |
| --- | ---: | ---: |
| Library unit tests | 100 | 0 |
| `tests/builds.rs` | 1 | 0 |
| `tests/cli.rs` | 6 | 0 |
| `tests/compatibility.rs` | 2 | 0 |
| `tests/pipeline_contract.rs` | 7 | 0 |
| `tests/pipeline_runtime.rs` | 23 | 0 |
| **Total** | **139** | **0** |

`cargo fmt --all -- --check`: pass.
`cargo clippy --all-targets -- -D warnings`: pass.
`cargo build --release --locked`: pass.

Criterion benchmarks (`cargo test --all-targets` runs them in test mode; the
timings below are from the dedicated benchmark run):

| Benchmark | Result |
| --- | --- |
| Parse + validate 12 jobs / 48 expanded | 234.69 µs |
| Compile + expand | 403.61 µs |
| Plan 48 jobs, warm | 71.31 ms |
| Plan 48 jobs, cold | ~4.5 s |

## 4. Cross-platform evidence

| Platform | Build | Tests | Native fixture | Evidence |
| --- | --- | --- | --- | --- |
| Windows x64 | pass | 139/139 | pass | This host |
| Linux x64 | **not verified** | CI configured | not run | Cross build fails: `x86_64-linux-gnu-gcc` not found |
| macOS | **not verified** | CI configured | not run | No host/SDK; CI never executed here |

Exact cross-build blocker:

```text
error occurred in cc-rs: failed to find tool "x86_64-linux-gnu-gcc": program not found
```

No Linux or macOS binary is claimed. CI covers Windows, Ubuntu and macOS and
includes format, clippy, tests and a release build.

## 5. Security audit

- Secret values are masked in Debug, redacted in report/log/error/JSON surfaces,
  resolved fail-closed before jobs, and never printed by the secret CLI.
- Traversal and artifact integrity tests pass; symlink containment is
  Unix-only and did not execute on this host.
- Lifecycle transitions are transactional.
- Retention never deletes active runs or pinned objects.
- Daemon webhook is loopback-only, POST-only, optional bearer token; manual
  checks returned 401 (missing token), 405 (wrong method), 202 (authorized).
- Dependency vulnerability scan: **BLOCKED** (`cargo-audit`, `cargo-deny`,
  `cargo-nextest` and `cargo-fuzz` absent). SBOM generated but is not a scan.
- No OS sandbox; workflows run with the invoking user's permissions.

## 6. Release artifact inventory

Local artifacts in `dist/` (ignored by git):

| File | Size | SHA-256 |
| --- | ---: | --- |
| `ubuilder-windows-x64.zip` | 3.6 MiB | `7b18aca911391849b3de0035ee1173be76b9ceab14d3972fdbc81c9eb911b65a` |
| `sbom.cdx.json` | 98 KiB | `b3ba576aec19310f886f956aa51233accf809044044f5de977d533e5ead0cea2` |
| `SHA256SUMS` | 173 B | verifies both files |

The archive contains `ubuilder.exe`, README and docs (24 entries); the binary
reports `ubuilder 0.1.0` and `doctor --json` returns a valid envelope. The SBOM
is CycloneDX 1.5 with 168 components and 168 dependency nodes.

The tag-driven GitHub release workflow has not executed from this environment.
No Linux/macOS archive or signature is claimed.

## 7. Remaining limitations

1. Linux and macOS are not executed; only Windows x64 is verified.
2. Cross-compilation lacks a linker; `linker-unverified`.
3. No dependency vulnerability scan.
4. No OS sandbox for untrusted workflows.
5. No code signing, macOS notarization, or published release.
6. Daemon webhook is plaintext loopback; no TLS, replay window or rate limit.
7. Detached daemon webhook behavior was manually checked, not automated in CI.
8. `cancel-in-progress` uses the cancel marker but has no dedicated test.
9. Concurrency slots are recovered by a 1 h lease, not a heartbeat.
10. A newer SQLite schema version is not rejected.
11. Cold planning costs ~4.5 s because the facts cache is per-process; each
    daemon-triggered CLI run pays that cost.
12. Remote actions, container/WSL backends and native installers are not
    implemented.

## 8. Verdict

**PRODUCTION_READY_WITH_LIMITATIONS**

All correctness, security (except dependency audit and sandboxing), retention,
recovery, performance and Windows release gates pass with 139 tests and verified
benchmarks. Full production readiness is withheld because Linux/macOS execution,
dependency audit, signing and an actual CI/release run are blocked in this
environment. Each blocker is named above; none is reported as verified.
