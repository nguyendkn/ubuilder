# Security threat model

## Assets

- Secret values (environment or `<data>/secrets.json`).
- Source tree and per-job workspaces.
- Build outputs and artifact objects.
- SQLite run history, logs and JSON reports.
- Signing credentials and release artifacts.

## Actors and trust boundaries

| Actor | Trust | Capability |
| --- | --- | --- |
| CLI user | Trusted | Runs arbitrary workflows; controls the local data dir. |
| Workflow author | Semi-trusted | Controls commands, actions, paths, matrix and template expansion. |
| Built-in action code | Trusted | Runs in-process with host access. |
| Remote input | Untrusted | Webhook POSTs and future git event payloads. |
| Toolchain subprocess | Semi-trusted | Receives a filtered environment. |

The OS process boundary is a reliability boundary, not a security sandbox.
`trusted` jobs can read anything the CLI user can read. No claim of container
isolation is made.

## Threats and controls

| ID | Threat | Control | Status |
| --- | --- | --- | --- |
| T-01 | Secret printed in logs, JSON, SQLite, errors or cache keys | Resolve referenced names only; redact report outputs, error strings and action output; masked `Debug`; cache keys hash user text. | Mitigated, tested. |
| T-02 | Missing secret silently becomes empty | Resolve once before any job; fail closed with a remediation. | Mitigated, tested. |
| T-03 | Secret file read by other local users | File provider uses `0600`/`0700` where supported and temp+rename; lock file. | Mitigated on Unix; Windows ACLs rely on user profile defaults. |
| T-04 | Secret in process args via `secret set --value` | Supported but documented; `--stdin`, `--file` and prompt keep it out of argv. | Residual. |
| T-05 | Path traversal in workspace paths | `safe_join` rejects absolute, drive-prefixed and `..` paths. | Mitigated, tested. |
| T-06 | ZIP entry traversal | `validate_relative_path` on every entry; rejects absolute and `..`; corrupt cache is deleted. | Mitigated, tested. |
| T-07 | Symlink escapes artifact tree | Canonical target must stay under the packaged root; otherwise fail. | Mitigated, Unix-tested. |
| T-08 | Artifact object tampered after upload | Re-hash copied payload, verify manifest on read; mismatch fails. | Mitigated, unit-tested. |
| T-09 | Cache poisoning | Content-addressed blob hash must match filename; corrupt entry deleted; per-key store lock; atomic replace. | Mitigated, tested. |
| T-10 | Concurrent transition corrupts lifecycle | Transition validated inside a SQLite transaction. | Mitigated. |
| T-11 | Webhook spoofing | Loopback bind; optional bearer token (`--token` or `UBUILDER_WEBHOOK_TOKEN`); POST `/hook` only. | Partial: plaintext HTTP, no TLS or replay window. |
| T-12 | Untrusted command escapes isolation | No OS sandbox; job workspace is only a working directory. | Accepted limitation. |
| T-13 | Retention deletes data still needed | Active runs skipped; pinned objects protected; newest runs kept. | Mitigated, tested for runs. |
| T-14 | Dependency vulnerability | SBOM is generated; `cargo-audit`/`cargo-deny` are not installed locally. | Partial; release workflow can add a scan. |
| T-15 | Signing key exposure | Signing is an optional release hook; no key is stored in the repo. | Hook only, unverified. |

## Environment handling

A job receives an explicit environment, not the full parent environment by
default. `UBUILDER_*` runner variables are added. Secrets are injected only when
the workflow references them. This reduces accidental disclosure but does not
protect against a command that deliberately exfiltrates a value.

## Residual risks

1. No OS-level sandbox for untrusted workflows.
2. Webhook is plaintext and binds loopback; expose it through a TLS reverse proxy.
3. Windows secret-file ACLs are inherited, not explicitly restricted.
4. No dependency audit gate in this environment.
5. Signing and notarization are unverified.
6. A crashed run can hold a concurrency slot for up to the 1 h lease.
7. The daemon runs at most one workflow at a time by design.
