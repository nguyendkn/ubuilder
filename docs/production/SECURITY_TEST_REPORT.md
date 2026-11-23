# Security test report

Date: 2026-11-23. Host: Windows x64. Command:
`cargo test --all-targets --locked`.

## Result summary

| Suite | Tests | Result |
| --- | ---: | --- |
| Library unit tests | 100 | pass |
| `tests/builds.rs` | 1 | pass |
| `tests/cli.rs` | 6 | pass |
| `tests/compatibility.rs` | 2 | pass |
| `tests/pipeline_contract.rs` | 7 | pass |
| `tests/pipeline_runtime.rs` | 23 | pass |
| **Total** | **139** | **pass** |

The suite was rerun after the final state-machine and maintenance changes. The
symlink-escape unit test is `#[cfg(unix)]` and therefore does not execute on the
Windows host; it is expected to run in Linux/macOS CI.

## Security-specific evidence

| Control | Test | What it proves |
| --- | --- | --- |
| Secret masking | `secrets_are_injected_and_redacted_everywhere` | Values reach commands, while report outputs, errors, stdout tail and status JSON contain only `***`. |
| Fail-closed resolution | `missing_secret_fails_closed_before_running_jobs` | No job starts when a referenced secret is missing; exit code and remediation are correct. |
| CLI confidentiality | `secret_cli_never_prints_values` | `secret list/set/inspect` JSON and human output never contain the raw value. |
| Artifact integrity | `artifacts::tests::hash_is_stable_and_content_sensitive`, `builds::rust_fixture_builds...` | Content hashing is deterministic and the produced artifact validates. |
| Symlink escape | `copy_tree_rejects_symlinks_escaping_the_root` (`cfg(unix)`) | A link resolving outside the packaged root is rejected. |
| ZIP traversal | `cache::tests::extraction_rejects_traversal_entries` | Entries whose normalized path escapes the destination are rejected before write. |
| Cache corruption | `cache::tests::tree_roundtrip_and_corruption_detection` | A tampered blob fails the hash check instead of being served. |
| Lifecycle integrity | `storage::tests::terminal_run_state_cannot_transition_back_to_running` | A terminal run cannot be moved back to `running`; error code is `STATE_INVALID`. |
| Retention safety | `maintenance_prune_keeps_newest_runs_and_protects_active`, `maintenance_pin_protects_and_gc_removes_unreferenced_objects` | Active/pinned data is not deleted; GC deletes only unreferenced objects. |
| Cancellation | `cancel_command_stops_a_running_workflow` | Cross-process cancellation terminates work and reports `cancelled`/130. |

## Manual verification

- `ubuilder secret set` via stdin; `secret inspect` displayed `***` and a
  provider name, not the value.
- `ubuilder maintenance gc` removed one unreferenced object and left pinned
  objects intact (covered by the integration test above).
- `python scripts/sbom.py` emitted 168 components and 168 dependency nodes; the
  document parses as JSON.

## Dependency audit

Not executed: `cargo-audit` and `cargo-deny` are not installed and no network
audit database was queried. The SBOM is an inventory, not a vulnerability scan.
This gate is BLOCKED and the verdict does not claim a clean vulnerability scan.

## Residual findings

1. Workspace isolation is not an OS sandbox.
2. Webhook is plaintext loopback with an optional bearer token.
3. Windows secret-file permissions rely on inherited ACLs.
4. `secret set --value` exposes the value in argv.
5. No TLS, rate limiting or replay window on the daemon webhook.

None of these is a known exploitable defect in the default local, trusted-user
deployment, but each is disclosed in the threat model and risk register.
