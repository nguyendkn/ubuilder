# Implementation status

Last verified: 2026-11-23 on Windows 10.0.26300 x64, Rust 1.99.1, PowerShell
default shell, native target `windows-x64`.

## Test suite

`cargo test --all-targets --locked` passes:

| Suite | Tests | Coverage |
| --- | ---: | --- |
| Library unit tests | 123 | parser, validator, compiler, matrix, expressions, planner, artifacts, cache, storage, schema, state machine, executor, secrets, isolation |
| `tests/builds.rs` | 2 | real Rust and .NET builds, packaging, validation, manifest, executable smoke test |
| `tests/cli.rs` | 6 | legacy CLI, detection, project reporting |
| `tests/compatibility.rs` | 3 | target compatibility matrix and `doctor --all` ordering |
| `tests/daemon.rs` | 4 | lifecycle, stale pid, webhook auth/limits/replay, token environment |
| `tests/pipeline_contract.rs` | 7 | validation, diagnostics, matrix plan, graph formats, cycles, init/list |
| `tests/pipeline_runtime.rs` | 27 | full runs, artifacts, cache, fail-fast, parallelism, timeout, cancellation, retry, continue-on-error, outputs, recovery, secrets, concurrency slots, isolation fail-closed, retention/pinning/GC, daemon once mode |
| **Total** | **172** | all passing on Windows x64 |

`cargo fmt --all -- --check` and `cargo clippy --all-targets --locked -- -D warnings`
also pass. Criterion benchmarks live in `benches/pipeline.rs`.

## Implemented and exercised

| Area | Status | Evidence |
| --- | --- | --- |
| YAML parsing with locations | Tested | unknown-field contract test, parser unit tests |
| Strict schema / unknown fields | Tested | `deny_unknown_fields`, contract test |
| Semantic validator + diagnostics | Tested | cycles, action inputs, templates |
| Deterministic matrix + include/exclude | Tested | contract plan ids, matrix unit tests |
| Dependency waves | Tested | contract plan waves, runtime tests |
| Isolated workspaces | Tested | distinct workspace paths, artifact transfer |
| Built-in actions (11) | Partial | runtime build/package/upload/download/checksum/cache; setup/verify via unit and legacy tests |
| Action stdout capture | Tested | message stored as output and step log |
| Local artifact store | Tested | content-addressed upload + download + integrity verification |
| Pipeline cache | Tested | miss/hit, unrelated-edit survival, corruption detection |
| Fail-fast matrix siblings | Tested | queued sibling skipped, running sibling not serialized (marker intervals) |
| Parallel independent jobs | Tested | marker intervals prove overlap, not wall-clock duration |
| Step timeout and process-tree kill | Tested | 1 s timeout on a 10 s sleep |
| Cross-process cancellation | Tested | `pipeline cancel`, exit 130, `cancelled` status |
| Retry policy | Tested | fail-then-succeed across attempts |
| `continue-on-error` | Tested | status `failure`, conclusion `success` |
| Job outputs / `needs` context | Tested | produced value consumed by dependent job |
| Concurrency groups | Tested | fenced 60 s lease + heartbeat; serialization and peer cancellation tested |
| `cancel-in-progress` | Tested | holder releases its lease; marker serialization proven |
| Typed lifecycle | Tested | transition table + transactional rejection (`STATE_INVALID`) |
| SQLite persistence | Tested | status/history/logs, transitions, retention |
| Secret providers and redaction | Tested | fail-closed resolution, CLI masking, report redaction |
| Retention / pin / GC | Tested | prune newest, active protection, pinned objects survive GC |
| Stale run recovery | Tested | killed process recovered to `failed` |
| JSON envelopes and exit codes | Tested | 1 / 2 / 3 / 130 cases |
| Mermaid / DOT / JSON graph | Tested | contract test |
| Trigger daemon | Tested | lifecycle, git/workflow fingerprint, authenticated loopback webhook, once mode |
| Production example workflows | Tested | validate + plan + real run on Windows |

## Partial / untested

| Area | Status | Notes |
| --- | --- | --- |
| Linux end-to-end | Untested on a Linux host | CI includes Ubuntu; not executed here |
| macOS targets | Untested | No host/SDK; CI includes macOS |
| Cross-compilation | Untested | Target installed but linker unverified |
| `container` / `wsl` backends | Not implemented | Planner rejects them with a remediation |
| Daemon detached start/stop/webhook | Tested | Lifecycle, stale pid, auth, limits and replay are integration-tested on Windows |
| OS sandbox | Fail-closed only | No verified backend; `isolated` returns exit 3 without running natively |
| Dependency audit | Not run | `cargo-audit`/`cargo-deny` absent; SBOM generated |
| Signing / notarization | Hook only | No credentials; archive is `UNSIGNED` |
| Remote action marketplace | Not implemented | Only compiled-in `builtin/*` actions |
| Artifact compression/dedup | Content-addressed only | Identical content reuses one object; no compression |

## Known limitations

- Fail-fast stops new sibling scheduling; running siblings finish.
- Cache keys are step-scoped; callers must include toolchain inputs.
- Job-level timeout is enforced around the whole body; a process that ignores
  termination may delay the report.
- A crashed run can hold a concurrency slot until its 60 s lease expires; live owners renew every 15 s.
- Planning measured 104.57 ms warm for 48 jobs in the final run (earlier baseline 71.31 ms); cold planning was not re-measured.
- A newer schema version is rejected with `SCHEMA_UNSUPPORTED` before DDL; migration backs up with `VACUUM INTO`.
- Logs and workspaces are not encrypted; retention is explicit.

See `docs/production/` for the audit, threat model, risk register, performance
baseline and final readiness verdict.
