# Production roadmap

The order follows the specification: correctness, security, reliability,
cross-platform, automation, observability, performance, release engineering,
final verification.

## Phase 1 - Correctness (complete on Windows)

- Fail-fast scheduling no longer serializes matrix siblings.
- Typed run/job/step lifecycle with transactional transitions.
- Step-scoped cache keys; tests prove unrelated edits do not evict.
- Artifact upload re-hashes payloads and verifies manifests.
- Regression tests for every fixed defect.

Exit test: full suite green; matrix, dependency, cancellation and recovery tests.

## Phase 2 - Security (complete except sandbox)

- Secret provider abstraction, environment/file providers, masked values.
- Fail-closed resolution before jobs; redaction on every serialized surface.
- `ubuilder secret list|set|delete|inspect`.
- Path containment, ZIP traversal and symlink escape tests.
- Threat model with residual risks.

Remaining: OS-level isolation for untrusted actions; dependency audit tool.

## Phase 3 - Reliability (complete for the local engine)

- Bounded concurrency slots, cancel-in-progress peer cancellation.
- Transactional state validation.
- Retention policy with active-run protection and pinning.
- Corrupt cache detection and artifact integrity checks.
- Stale run recovery.

Remaining: slot heartbeat, schema downgrade check.

## Phase 4 - Cross-platform (interfaces complete, evidence partial)

- Cross-platform executor paths, path handling and Unix permissions.
- CI matrix added for Windows, Ubuntu and macOS.
- Capability reporting distinguishes supported, conditional, experimental,
  unsupported and unverified.

Blocked: Linux/macOS native runs; macOS SDK and signing; verified cross-linker.

## Phase 5 - Automation (complete for local triggers)

- Git/workflow fingerprint polling daemon with loopback webhook.
- Staff trigger file and once mode.
- `maintenance` retention automation and JSON output.

Cron is delegated to the OS scheduler (Task Scheduler, cron, systemd timer).

## Phase 6 - Observability (complete baseline)

- Existing event stream + per-step logs retained.
- Action steps now write logs and `stdout_tail`.
- Stable JSON envelope and exit codes.
- Run/job/step state and durations persisted.

Remaining: `tracing` spans, metrics endpoint, log rotation.

## Phase 7 - Performance (baseline complete)

- Criterion benchmarks for parse, compile and plan.
- Toolchain facts cache and per-target capability cache.
- Final warm 48-job plan: 104.57 ms [100.69, 109.02]; historical baseline 71.31 ms. Cold planning was not re-measured (~4.5 s historically).

## Phase 8 - Release engineering (pipeline added, not executed here)

- `ci.yml`: fmt, clippy, tests and release build on three OSes.
- `release.yml`: gates, archive, SHA256SUMS, CycloneDX SBOM, signing hook.
- Deterministic `scripts/sbom.py` and `scripts/manifest.py` (archive hash, size, commit, SBOM, smoke status, `UNSIGNED`).
- Local Windows x64 archive verified, smoke-tested and byte-identical on re-packaging.

Blocked: GitHub Actions run, signing identity, published artifacts.

## Phase 9 - Final verification (complete)

- Four gates run: fmt, clippy, 172 tests, release build.
- Actual output in `READINESS_EVIDENCE.md`, `RELEASE_VERIFICATION.md` and `target/`.
- Scoped verdicts and blocked gates in `READINESS_GATES.json` and `FINAL_READINESS_REPORT.md`.

## Definition of done

A task is done when the code path is implemented, has a regression test where a
consumer-visible behavior is involved, is documented, and the narrowest relevant
gate passes. Platform claims additionally require execution on that platform.
