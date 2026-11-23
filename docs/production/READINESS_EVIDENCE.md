# Readiness evidence

All evidence is local to this workspace. Paths under `target/` are ignored by
git but were produced by the commands shown. No CI run, Linux execution, macOS
execution, signature, or vulnerability scan is claimed.

## Environment

| Item | Value |
| --- | --- |
| Host OS | Windows 10.0.26300 x64 |
| Logical CPUs | 16 |
| Rust / Cargo | 1.99.1 |
| Python | 3.13.17 |
| .NET SDK | 11.0.100 |
| Node.js | v26.13.1 |
| Installed Rust target | `x86_64-pc-windows-msvc` |
| Missing tools | docker, podman, bwrap, firejail, sandbox-exec, zig, gcc, clang, tauri, electron-builder, signtool, cargo-audit, cargo-deny, cargo-nextest, cargo-fuzz |

## Quality gates

Commands were run from the repository root. The final source commit is recorded
in `READINESS_GATES.json`; the documentation commit follows it.

| Gate | Command | Result | Evidence |
| --- | --- | --- | --- |
| Format | `cargo fmt --all -- --check` | exit 0 | terminal |
| Clippy | `cargo clippy --all-targets --locked -- -D warnings` | exit 0 | `target/finalization-clippy.log` |
| Tests | `cargo test --all-targets --locked` | 172 passed, 0 failed, exit 0 | `target/finalization-final.log` |
| Release build | `cargo build --release --locked` | exit 0 | terminal |

### Test counts

| Target | Passed |
| --- | ---: |
| Library unit tests | 123 |
| `src/main.rs` | 0 |
| `tests/builds.rs` | 2 |
| `tests/cli.rs` | 6 |
| `tests/compatibility.rs` | 3 |
| `tests/daemon.rs` | 4 |
| `tests/pipeline_contract.rs` | 7 |
| `tests/pipeline_runtime.rs` | 27 |
| **Total** | **172** |

## Functional evidence

| Area | Evidence |
| --- | --- |
| Legacy CLI | `tests/cli.rs`, `tests/builds.rs`; `build`/`package`/`release` remain and `--help` lists all legacy commands |
| YAML v1 / IR | `tests/pipeline_contract.rs::validate_accepts_the_production_example`, `examples/native-matrix.yml` |
| Matrix and DAG | `plan_expands_a_deterministic_matrix_and_waves`, `matrix::tests::*`, cycle rejection |
| Fail-fast | `matrix_fail_fast_skips_remaining_siblings`, `fail_fast_does_not_serialize_matrix_siblings` |
| Continue-on-error | `continue_on_error_keeps_the_job_running` |
| Retry / timeout | `retry_policy_reruns_a_failed_job`, `step_timeout_fails_the_job` |
| Cancellation | `cancel_command_stops_a_running_workflow`, `cancel_in_progress_cancels_the_holder_and_releases_its_lease` |
| Recovery | `recover_marks_an_interrupted_run_as_failed` |
| Schema safety | `newer_schema_is_rejected_before_ddl`, `version_one_database_is_backed_up_and_migrated` |
| Lease fencing | `expired_lease_cannot_be_renewed_or_released_by_the_old_owner` |
| Cache identity | `identity_layers_change_the_key`, `cache_survives_unrelated_workflow_edits` |
| Artifact integrity | `corrupted_artifact_object_is_rejected_on_download`, `two_jobs_transfer_artifacts_between_isolated_workspaces` |
| Retention | `maintenance_prune_keeps_newest_runs_and_protects_active`, `garbage_collection_keeps_objects_shared_by_retained_runs`, `maintenance_pin_protects_and_gc_removes_unreferenced_objects` |
| Secrets | `secrets_are_injected_and_redacted_everywhere`, `missing_secret_fails_closed_before_running_jobs`, `secret_scope_only_contains_names_the_job_references` |
| Isolation fail-closed | `isolated_mode_fails_closed_without_a_backend` |
| Daemon/webhook | 4 tests in `tests/daemon.rs` |
| Capability honesty | 3 tests in `tests/compatibility.rs`, including `doctor --all` |

## Native fixture evidence

### Rust

`tests/builds.rs::rust_fixture_builds_with_manifest_and_validated_artifact`:

- fixture: `fixtures/rust-cli`
- target: `windows-x64`, debug, portable ZIP, no cache
- result: exit 0, report `success`, artifact validates, manifest written
- artifact recorded in SQLite with validation `passed`

### .NET

`tests/builds.rs::dotnet_fixture_packages_a_runnable_executable`:

- fixture: `fixtures/dotnet-console`, `net10.0`, SDK 11.0.100
- target: `windows-x64`, debug, portable ZIP
- result: exit 0, report `success`, all validations `passed`
- extracted `HelloDotnet.exe` executed and printed `hello from HelloDotnet`

### Electron / Tauri

Detection only (`tests/cli.rs::detects_each_supported_framework_fixture`). No
`node_modules`, `electron-builder`, or `tauri` CLI was available, so packaging is
UNVERIFIED.

## Security evidence

`docs/production/adversarial-matrix.json` contains 18 cases: 16 PASS, 2 BLOCKED.

- PASS cases were executed on Windows x64.
- BLOCKED: Unix symlink-escape test (not executed on this host) and sandbox
  filesystem/network escape (no backend).
- No mock is substituted for a sandbox escape test.

## Performance evidence

Final benchmark log: `target/finalization-bench.log`. Comparison and regression
analysis: `docs/production/PERFORMANCE_BASELINE.md`.

| Benchmark | Baseline | Final |
| --- | --- | --- |
| Parse + validate 48 jobs | 234.69 µs | 272.60 µs |
| Compile + expand | 403.61 µs | 446.59 µs |
| Plan 48 jobs, warm | 71.31 ms | 90.03 ms |
| Plan 48 jobs, cold | ~4.5 s | not separately instrumented; toolchain probes remain dominant |

Parse and compile changes are below the 20% blocking threshold defined by the
performance policy. Warm planning is 26% higher and is explained, not required to
block; no planner code changed during finalization.

## Release evidence

`docs/production/RELEASE_VERIFICATION.md` records the archive hash, extracted
smoke tests, SBOM validation, manifest and unsigned status. `SHA256SUMS` is the
authority for the exact bytes in `dist/`.

## Known BLOCKED evidence

| Capability | Why blocked |
| --- | --- |
| Ubuntu x64 and macOS (x64/arm64) execution | no host available; CI not run |
| Cross-link to Linux | `x86_64-linux-gnu-gcc` absent |
| CI run record | no GitHub execution in this environment |
| Dependency audit | `cargo-audit`/`cargo-deny` absent; advisory DB not queried |
| Signing/notarization | no credentials/tools |
| Verified isolation backend | no bwrap/firejail/sandbox-exec or certified Windows backend |
| Windows ARM64 | no host |
