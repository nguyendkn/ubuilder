# Security test report

Date: 2026-11-23. Host: Windows 10.0.26300 x64. Command:
`cargo test --all-targets --locked`.

Only Windows x64 executed. Unix-only tests are compiled but not claimed as
executed; Linux/macOS evidence is BLOCKED.

## Result summary

| Suite | Tests | Result |
| --- | ---: | --- |
| Library unit tests | 123 | pass |
| `src/main.rs` | 0 | pass |
| `tests/builds.rs` | 2 | pass |
| `tests/cli.rs` | 6 | pass |
| `tests/compatibility.rs` | 3 | pass |
| `tests/daemon.rs` | 4 | pass |
| `tests/pipeline_contract.rs` | 7 | pass |
| `tests/pipeline_runtime.rs` | 27 | pass |
| **Total** | **172** | **pass** |

Machine-readable log: `target/finalization-final.log`.
`cargo fmt --all -- --check` and
`cargo clippy --all-targets --locked -- -D warnings` pass.

## Security-specific evidence

| Control | Test | What it proves |
| --- | --- | --- |
| Per-job secret scope | `runtime::tests::secret_scope_only_contains_names_the_job_references` | A job receives only referenced names; unrelated resolved secrets do not enter its context. |
| Secret redaction | `secrets_are_injected_and_redacted_everywhere` | Values reach commands, while reports, errors, stdout tails and JSON contain only the redaction marker. |
| Fail-closed resolution | `missing_secret_fails_closed_before_running_jobs` | No job starts when a referenced secret is missing. |
| CLI confidentiality | `secret_cli_never_prints_values` | `secret list/set/inspect` output never contains the raw value. |
| Windows secret ACL | `secrets::tests::windows_secret_file_has_no_inherited_aces` | Inheritance is removed and only the current user retains full control. |
| Artifact integrity | `pipeline::artifacts::tests::corrupted_artifact_object_is_rejected_on_download` | A tampered immutable object fails re-hash and returns `ARTIFACT_INVALID`. |
| Junction escape | `pipeline::artifacts::tests::junction_escape_is_rejected` | A junction resolving outside the workspace is rejected. |
| Lexical traversal | `security::tests::rejects_traversal_and_absolute_paths`, `pipeline::validator::tests::rejects_step_path_escape`, `pipeline::artifacts::tests::lexical_containment_rejects_absolute_and_parent_paths` | `..`, absolute and drive paths are rejected. |
| Symlink escape (Unix) | `pipeline::artifacts::tests::symlink_escape_is_rejected` | Compiled on Windows but not executed; Linux/macOS evidence is BLOCKED. |
| ZIP traversal | `cache::tests::extraction_rejects_traversal_entries` | Escaping entries are rejected before write. |
| Cache corruption | `cache::tests::tree_roundtrip_and_corruption_detection`, `pipeline::cache::tests::corrupt_entry_is_not_restored` | A tampered entry fails its hash and is not served. |
| Cache poisoning isolation | `pipeline::cache::tests::identity_layers_change_the_key` | Host/toolchain/namespace/job identity changes the key. |
| Stale lease fencing | `storage::tests::expired_lease_cannot_be_renewed_or_released_by_the_old_owner` | An expired owner cannot renew or release a slot owned by a newer generation. |
| Newer schema | `storage::tests::newer_schema_is_rejected_before_ddl` | `SCHEMA_UNSUPPORTED` is returned before mutation. |
| Isolation fail-closed | `isolated_mode_fails_closed_without_a_backend` | Exit 3, `SECURITY_CAPABILITY_UNAVAILABLE`, no run record, no native fallback. |
| Webhook auth | `tests/daemon.rs::webhook_requires_configuration_and_authenticates_requests` | Missing token config is `CONFIG_INVALID`; wrong token is 401; duplicate delivery is 409; valid is 202. |
| Webhook token storage | `tests/daemon.rs::daemon_child_receives_webhook_token_only_through_the_environment` | The detached child's command line does not contain the token. |
| Rate limiting / constant time | `cli::daemon::tests::rate_limit_and_constant_time_comparison` | The limiter returns 429 and comparison does not short-circuit on content. |
| Body/header limits | `cli::daemon::tests` and `serve()` | 16 KiB headers, 64 KiB body, 5 s socket timeouts, 413 on oversized body. |
| Cancellation | `cancel_command_stops_a_running_workflow` | Cross-process cancellation terminates work and reports cancelled/130. |
| Cancel-in-progress | `cancel_in_progress_cancels_the_holder_and_releases_its_lease` | Holder is cancelled, relinquishes its lease, and the contender succeeds. |
| Retention safety | `maintenance_prune_keeps_newest_runs_and_protects_active`, `maintenance_pin_protects_and_gc_removes_unreferenced_objects` | Active/pinned data is not deleted; GC removes only unreferenced objects. |

The complete adversarial case list, including BLOCKED cases, is
`docs/production/adversarial-matrix.json` (16 PASS, 2 BLOCKED).

## Manual verification

- `ubuilder secret set NAME --stdin`; `secret inspect` displayed metadata, not
  the value.
- `python scripts/sbom.py` emitted CycloneDX with the locked dependency graph;
  the document parses as JSON.
- `.NET` fixture built, packaged and executed from the extracted archive; see
  `RELEASE_VERIFICATION.md` for the release smoke test.

## Dependency audit

Not executed: `cargo-audit` and `cargo-deny` are absent and no advisory database
was queried. CI defines `rustsec/audit-check@v2`, but CI has not run in this
environment. This gate is BLOCKED; no clean vulnerability scan is claimed.

## Residual findings

1. No verified OS sandbox on Windows; `trusted-native` is the default and
   requires trusted workflows. Windows Job Objects are not claimed as a complete
   filesystem/network sandbox.
2. Webhook transport is plaintext and loopback by default; TLS is external.
3. Only 16 KiB/64 KiB limits and a fixed 60/60 s rate window are enforced; there
   is no adaptive or per-IP limiting.
4. Replay protection is bounded to 1024 delivery ids in memory; it is not
   durable across daemon restarts.
5. `secret set --value` remains available (deprecated) for compatibility.
6. Unix ACL/mode and symlink behavior is implemented but not executed here.
7. No signature or notarization was verified.
8. No disk-full, SQLite-busy, or artifact-publication crash injection was run.
