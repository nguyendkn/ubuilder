# Gap analysis against the production acceptance criteria

Status values: DONE, PARTIAL, BLOCKED. BLOCKED means the environment cannot
produce evidence; the interface may still exist.

## Correctness

| Criterion | Status | Evidence / gap |
| --- | --- | --- |
| Existing tests pass | DONE | Full suite at the final gate. |
| No known P0/P1 correctness defects | DONE | Baseline defects B-01..B-09 fixed with regressions. |
| Matrix execution | DONE | Matrix, include/exclude, max-parallel and fail-fast tests. |
| Dependency ordering | DONE | Waves, needs outputs, failure blocking tests. |
| Cancellation | DONE | Cross-process marker, exit 130 and report status test. |
| Concurrency groups | DONE | Bounded slots and serialization integration test. |
| Recovery | DONE | Stale `running` runs recovered to `failed`. |

## Security

| Criterion | Status | Evidence / gap |
| --- | --- | --- |
| Secrets protected | DONE | Fail-closed resolution, redaction test, CLI never prints values. |
| Path traversal prevented | DONE | `safe_join`, ZIP entry validation tests. |
| Artifact extraction safe | DONE | Re-hash, manifest verification, symlink containment. |
| Untrusted execution policy | PARTIAL | No OS sandbox; trusted-workspace isolation only. |
| No critical dependency vulnerabilities | BLOCKED | No `cargo-audit`/`cargo-deny`; SBOM exists. |

## Reliability

| Criterion | Status | Evidence / gap |
| --- | --- | --- |
| Safe SQLite migrations | PARTIAL | Idempotent schema + version 1 row; no downgrade rejection. |
| Artifact integrity | DONE | Unit + integration tests. |
| Cache corruption handled | DONE | Hash mismatch deletes entry and errors. |
| Retention preserves active work | DONE | Prune skips non-terminal runs and pinned objects; test. |
| Interrupted workflows recover | DONE | `pipeline recover` test. |

## Cross-platform

| Criterion | Status | Evidence / gap |
| --- | --- | --- |
| CLI builds/runs Windows | DONE | Native host. |
| CLI builds/runs Linux | BLOCKED | No Linux host; CI configured. |
| CLI builds/runs macOS | BLOCKED | No macOS host; CI configured. |
| Native execution tests | PARTIAL | Windows only. |
| Unsupported combinations accurate | DONE | Capability report and `UNVERIFIED` markers. |
| No fabricated compatibility | DONE | Docs distinguish verified from inferred. |

## Operations

| Criterion | Status | Evidence / gap |
| --- | --- | --- |
| Logs/diagnostics usable | DONE | Per-step logs/tails; JSON errors with remediation. |
| Stable JSON | DONE | Envelope tests. |
| Triggers as documented | PARTIAL | Git/workflow polling + loopback webhook; cron is OS-scheduled. |
| Daemon behaves | PARTIAL | Once mode tested; detached start/stop and webhook not CI-tested. |
| Install/upgrade documented | DONE | Operations runbook and migration guide. |

## Release

| Criterion | Status | Evidence / gap |
| --- | --- | --- |
| Release artifacts exist | PARTIAL | Buildable locally; the release workflow has not run. |
| Checksums verified | PARTIAL | Scripted in release workflow; no published artifact here. |
| SBOM generated | DONE | `scripts/sbom.py` produces CycloneDX 1.5 JSON locally. |
| Release pipeline passes | BLOCKED | No GitHub Actions run from this environment. |
| Docs match implementation | DONE | This directory and updated implementation status. |
| Limitations disclosed | DONE | Risk register, final report. |

## Functional gaps not required by acceptance criteria

- Remote action marketplace and action version pinning beyond `builtin/*`.
- Container/WSL execution backends (planner rejects them).
- Distributed queue or multi-machine runners.
- Native installers for every OS; packaging support is capability-gated.
- Hardware-backed secret stores (Keychain, Credential Manager).
