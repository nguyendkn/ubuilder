# Finalization findings

Every finding below was reproduced or inspected in the repository before the
change. `FIXED` means code and tests changed; `VERIFIED` means a command was run
on this host; `BLOCKED` means the capability could not be executed here and is
not claimed.

## Register

| ID | Severity | Finding | Resolution | Status | Evidence |
| --- | --- | --- | --- | --- | --- |
| F-01 | P0 | Opening a database written by a newer schema ran migrations and could corrupt unknown columns. | Read `MAX(version)` before DDL; reject newer databases with `SCHEMA_UNSUPPORTED` (exit 3) and structured details; back up a non-empty database with `VACUUM INTO` before migrating; run DDL inside `BEGIN IMMEDIATE`. | FIXED | `storage::tests::newer_schema_is_rejected_before_ddl`, `version_one_database_is_backed_up_and_migrated` |
| F-02 | P0 | Concurrency leases could be stolen after a fixed one-hour expiry even while the owner was alive; there was no fencing token. | Added generation/token fencing, acquire/renew/release/expire operations, a 60 s lease with a 15 s heartbeat, and cancellation when renewal loses the lease. | FIXED | `storage::tests::expired_lease_cannot_be_renewed_or_released_by_the_old_owner`, `pipeline_runtime::cancel_in_progress_cancels_the_holder_and_releases_its_lease` |
| F-03 | P1 | Pipeline cache keys did not include host or toolchain; a shared cache could return binaries for another platform or compiler. | Key is now `sha256(pipeline/<namespace>/v1/<host>/<toolchain>\n<user_key>)`; namespace includes workflow name and file name; cache actions append the job id. | FIXED | `pipeline::cache::tests::identity_layers_change_the_key`, `corrupt_entry_is_not_restored` |
| F-04 | P0 | Every job received every resolved secret, widening exposure and making per-job least privilege impossible. | Added `secrets_by_job`, scoped to names referenced by the job; all values still participate in redaction. | FIXED | `pipeline_runtime::secrets_are_injected_and_redacted_everywhere`, `runtime::tests::secret_scope_only_contains_names_the_job_references` |
| F-05 | P1 | On Windows, secret files and directories inherited ACLs from their parent, so other local users could read them. | Applied `icacls <path> /inheritance:r /grant:r <current-user>:F`; reject reparse points; separate Unix mode handling. | FIXED | `secrets::tests::windows_secret_file_has_no_inherited_aces` |
| F-06 | P0 | Artifact path validation was lexical. A junction or symlink inside a workspace could escape the root. | `safe_join` canonicalizes the root and existing prefixes; `validate_relative_path` rejects absolute/parent paths; validator and artifact copy use it. | FIXED | `artifacts::tests::junction_escape_is_rejected` (Windows), `symlink_escape_is_rejected` (Unix), `lexical_containment_rejects_absolute_and_parent_paths`, `validator::tests::rejects_step_path_escape` |
| F-07 | P0 | `security.mode: isolated` did not exist; untrusted workflows ran with host access, or would have to silently fall back. | Added an isolation provider trait, native and Linux bubblewrap providers, policy schema, capability reporting, and fail-closed selection (`SECURITY_CAPABILITY_UNAVAILABLE`, exit 3). No fallback. | FIXED; backend BLOCKED | `pipeline_runtime::isolated_mode_fails_closed_without_a_backend`; no verified Windows backend |
| F-08 | P0 | The webhook accepted unauthenticated requests, any interface, unbounded headers/body, unlimited replay, and had no rate limit. | Token required (`--token`/`UBUILDER_WEBHOOK_TOKEN`), loopback bind by default with validated `--bind`, 16 KiB headers, 64 KiB body, 60 requests/60 s, 1024-entry replay cache, constant-time comparison, explicit 400/401/405/409/413/429/202 responses. | FIXED | `tests/daemon.rs::webhook_requires_configuration_and_authenticates_requests`, `daemon_child_receives_webhook_token_only_through_the_environment` |
| F-09 | P1 | `daemon start` left the caller's stdout pipe open for the daemon's lifetime, hanging scripts and tests. | Windows starts the child through `Start-Process -PassThru` without `-RedirectStandard*`; Unix keeps detached stdio; pid is returned; `kill_process` captures output. | FIXED | `tests/daemon.rs` lifecycle tests; `daemon start \| cat` returns in under 1 s |
| F-10 | P1 | Parser, matrix and expression limits existed but adversarial exhaustion was untested and some limits were unenforced. | Added alias-bomb detection through YAML repetition limits, matrix job limit enforcement, and expression/template size, depth, node and segment limits. | FIXED | `parser::tests::rejects_yaml_alias_expansion_bombs`, `matrix::tests::rejects_explosion_beyond_the_default_limit`, `expression::tests::rejects_oversized_expressions_and_templates` |
| F-11 | P2 | `secret set --value` exposed values in shell history with no warning. | Documented as deprecated and warns in human mode; JSON output unchanged; internal tests use `--stdin`. | FIXED | `cli/secret_cmd.rs`; `tests/pipeline_runtime.rs::set_secret` |
| F-12 | P2 | `doctor` only diagnosed the native target unless every id was repeated. | Added `doctor --all` and a test that its order matches the canonical target list. | FIXED | `tests/compatibility.rs::doctor_all_matches_the_explicit_target_list` |
| F-13 | P1 | Artifact objects were re-hashed on reuse, but no test proved tampering was rejected. | Added a test that corrupts an immutable object and asserts `ARTIFACT_INVALID` on download. | FIXED | `pipeline::artifacts::tests::corrupted_artifact_object_is_rejected_on_download` |
| F-14 | P1 | Release archives had no machine-readable manifest and the checked-in artifacts came from an older commit. | Added `scripts/manifest.py`; artifacts regenerated from the final commit with SHA-256, size, SBOM reference, unsigned status and smoke test. | FIXED | `dist/release-manifest.json`, `RELEASE_VERIFICATION.md` |
| F-15 | P2 | CI did not enforce `--locked`, did not run a dependency audit, and a YAML file was being treated as CI evidence. | CI now pins `--locked`, denies clippy warnings, and has an audit job. No CI run is claimed. | FIXED; run BLOCKED | `.github/workflows/ci.yml`; GitHub Actions not executed here |
| F-16 | P1 | Cross-platform documentation claimed inherited Windows secret ACLs were acceptable and a junction escape was impossible. | Rewrote the cross-platform matrix and security test report around the implemented behavior. | FIXED | `CROSS_PLATFORM_MATRIX.md`, `SECURITY_TEST_REPORT.md` |

## Security boundary results

| Adversarial input | Result | Status |
| --- | --- | --- |
| YAML alias expansion bomb | rejected as `CONFIG_INVALID` by repetition limit | PASS (Windows) |
| Matrix expansion beyond 256 jobs | rejected with the configured limit | PASS (Windows) |
| Oversized expression/template | rejected at 4096 bytes / 128 segments / depth/node limits | PASS (Windows) |
| `../` and absolute artifact paths | rejected | PASS (Windows) |
| Junction escape | rejected by canonicalized containment | PASS (Windows) |
| Symlink escape | rejected | PASS (Unix code); not executed on this host |
| ZIP traversal on cache extraction | traversal entry rejected | PASS (Windows) |
| Tampered artifact object | rejected on download | PASS (Windows) |
| Cache poisoning by a corrupt entry | corrupt entry not restored | PASS (Windows) |
| Missing secret | fails closed before jobs run | PASS (Windows) |
| Secret leakage in logs/JSON | redacted on every serialized surface | PASS (Windows) |
| Unauthenticated webhook | 401, request not queued | PASS (Windows) |
| Replayed webhook delivery | 409 duplicate | PASS (Windows) |
| Stale lease owner renewal/release | rejected after expiry | PASS (Windows) |
| Newer schema | rejected before DDL | PASS (Windows) |
| Isolation backend unavailable | exit 3, `SECURITY_CAPABILITY_UNAVAILABLE`, no run record | PASS (Windows) |
| Sandbox filesystem/network escape | no backend exists to attack | BLOCKED |
| Resource exhaustion outside configured limits | only expression/matrix limits covered | PARTIAL |

## Unresolved limitations

These are not hidden by the verdict:

1. No native Linux or macOS execution; no ARM64 execution.
2. No Linux cross-link (`x86_64-linux-gnu-gcc` absent).
3. No verified Windows sandbox. `ISOLATED_EXECUTION_PRODUCTION` is blocked.
4. No dependency vulnerability scan.
5. No signature, notarization, or published GitHub release.
6. No CI run evidence; the workflow file is configuration, not proof.
7. Electron and Tauri fixtures are detected but not packaged on this host.
8. Remote executors, containers/WSL backends, and native installers remain
   unimplemented.
9. Cold planning still pays toolchain-probe cost per process; the facts cache is
   process-local with a 30 s TTL.
10. Webhook transport is plaintext loopback; TLS termination is external.
11. The isolated provider interface is implemented, but only bubblewrap on Linux
    has a provider; Windows has none.
