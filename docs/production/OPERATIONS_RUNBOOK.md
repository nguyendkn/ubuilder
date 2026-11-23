# Operations runbook

Verified on Windows x64 only. Linux and macOS paths are implemented but not
executed here; see `CROSS_PLATFORM_MATRIX.md`.

## Installation

From a verified archive:

```console
# Windows: extract ubuilder-windows-x64.zip, then
ubuilder.exe --version
ubuilder.exe --json doctor --all
```

From source:

```console
cargo build --release --locked
target/release/ubuilder --version
target/release/ubuilder --json doctor --all
```

There is no installer or auto-updater; upgrade by replacing the binary. Back up
the data directory before upgrading. Version 0.1.0 migrates schema v1 to v2 and
writes a `VACUUM INTO` backup first; a newer schema is rejected rather than
downgraded.

## Data layout

`UBUILDER_DATA_DIR` overrides the platform data directory. `UBUILDER_CACHE_DIR`
overrides the build cache root.

```text
<data>/
  ubuilder.db                       SQLite: runs, jobs, steps, artifacts, cache index, pins, leases
  ubuilder.db.v<from>-<ms>.bak      pre-migration backup (schema upgrades)
  secrets.json                      file-provider secrets (0600 / restrictive ACL)
  secrets.json.lock
  pipeline/
    runs/<run-id>/{jobs,logs,cancel}
    artifacts/objects/<sha>/...
    cache/cas/...
  daemon/
    daemon.pid
    state.json
    daemon.log
    daemon.err.log
    runs.log
```

## First-run diagnostics

```console
ubuilder --json doctor --all
ubuilder target list --json
ubuilder maintenance stats --json
```

`doctor --all` reports every canonical target in one envelope. Capability levels
are probed, not inferred from a target name. A `CONDITIONAL` compilation means a
named requirement (for example a linker on `PATH`) is unsatisfied.

## Daily operations

| Task | Command |
| --- | --- |
| Health, one target | `ubuilder doctor --json` |
| Health, all targets | `ubuilder doctor --all --json` |
| Storage | `ubuilder maintenance stats --json` |
| Validate workflow | `ubuilder pipeline validate --file <workflow>` |
| Compile / plan | `ubuilder pipeline compile --file <workflow>` / `plan` |
| Run workflow | `ubuilder pipeline run --file <workflow>` |
| Inspect | `ubuilder pipeline status --run <id>` |
| History | `ubuilder pipeline history --json` |
| Logs | `ubuilder pipeline logs --run <id> --job <job> --step <step>` |
| Cancel | `ubuilder pipeline cancel --run <id>` |
| Recover | `ubuilder pipeline recover --json` |
| Prune preview | `ubuilder maintenance prune --dry-run --json` |
| Prune apply | `ubuilder maintenance prune --max-runs 100 --max-age-days 30 --max-size-mb 4096` |
| GC preview | `ubuilder maintenance gc --dry-run --json` |
| Pin release | `ubuilder maintenance pin <sha-prefix> --name v1.0.0` |
| Secrets | `ubuilder secret list`, `ubuilder secret set NAME --stdin` |

## Daemon and triggers

```console
ubuilder daemon start --file .ubuilder/workflows/build.yml --interval-secs 30 --json
ubuilder daemon status --json
ubuilder daemon trigger --json
ubuilder daemon stop --json
```

- On Windows the child is detached with `Start-Process`; `daemon start` returns
  its pid immediately.
- The loop triggers when git `HEAD` or the workflow bytes change, or when the
  trigger file appears. It serializes runs.
- `daemon trigger` is the hook for Task Scheduler, cron, or a systemd timer. The
  daemon has no cron parser and no timezone/missed-run policy.
- There is no built-in Git event source. Push/tag/branch/path filters are not
  claimed; a provider webhook must translate to `POST /hook`.

### Webhook

Authentication is mandatory and the bind address must be an IP literal. The
default bind is `127.0.0.1`.

```console
ubuilder daemon start --webhook 8787 --token "$TOKEN" --bind 127.0.0.1
curl -fsS -X POST -H "Authorization: Bearer $TOKEN" \
  -H "X-Ubuilder-Delivery: $DELIVERY_ID" http://127.0.0.1:8787/hook
```

Limits and behavior:

| Control | Value |
| --- | --- |
| Required token | `--token` or `UBUILDER_WEBHOOK_TOKEN` (never in argv for a detached child) |
| Default bind | `127.0.0.1`; any valid IP is accepted; wildcard interfaces are allowed only explicitly |
| Method/path | `POST /hook`; otherwise 405 |
| Headers | 16 KiB maximum; larger requests are rejected |
| Body | 64 KiB maximum; larger requests return 413 |
| Rate | 60 requests per 60 seconds; excess returns 429 |
| Replay | `X-Ubuilder-Delivery` / `X-GitHub-Delivery` deduplicated in a 1024-entry window; duplicate returns 409 |
| Auth failure | 401 before queueing |
| Timeouts | 5 s read/write per connection |
| Transport | Plaintext loopback; terminate TLS in a reverse proxy if remote |

The token is passed to the detached child through the environment, not the
command line.

## Database backup, migration and rollback

Stop the daemon and ensure no run is active. Then either copy the whole data
directory or use SQLite's backup API:

```console
sqlite3 ubuilder.db ".backup backup.db"
```

Migration is automatic on open and only upgrades schema v1 to v2. Before a
non-empty migration, `VACUUM INTO` writes `ubuilder.db.v<from>-<ms>.bak`.
Rollback procedure: stop the daemon, retain the new database, replace it with the
backup file, and start the previous binary. A newer schema opened by an older
binary is rejected with `SCHEMA_UNSUPPORTED` and exit 3; it is not downgraded.

## Secrets

```console
ubuilder secret set NAME --stdin
ubuilder secret list --json
ubuilder secret inspect NAME --json
```

Values are never printed by the CLI. Prefer `--stdin`, `--file`, or the hidden
interactive prompt. `--value` is deprecated and warns because it exposes the
value in shell history. Environment variables shadow file-provider values.
Referenced secrets resolve once before jobs start; a missing value fails closed.
Only names referenced by a job are exposed to that job, while every resolved
value is redacted from logs, errors, output tails and JSON.

Windows secret files have inheritance removed and grant full control only to the
current user. Reparse points are rejected. Unix uses `0600` files and `0700`
directories.

## Recovery and failure handling

| Symptom | Action |
| --- | --- |
| `SCHEMA_UNSUPPORTED` | The database is newer than the binary. Upgrade the binary; do not downgrade. |
| `STATE_INVALID` | A lifecycle transition was rejected. Inspect `pipeline status`; use `pipeline recover` for stale runs. |
| `CACHE_CORRUPT` | The entry was invalidated. Re-run to repopulate. |
| `ARTIFACT_INVALID` | Hash/manifest mismatch, missing object, or path escape. Do not pin; re-run the producing job. |
| `SECURITY_CAPABILITY_UNAVAILABLE` | `security.mode: isolated` has no verified backend. Install/select one; there is no native fallback. |
| Cancelled run | `pipeline cancel` terminates the process tree and records cancelled/130. Re-run if needed. |
| Concurrency wait | Another run holds the group. Check `pipeline history`; a live owner renews every 15 s. Expired leases are reclaimable after 60 s. |
| Interrupted run | `pipeline recover` marks it failed; side effects of in-flight steps are unknown. Non-idempotent release steps are not retried automatically. |
| Disk pressure | `maintenance stats`, dry-run prune/gc, then apply. Pinned objects and active runs are protected. |
| Daemon won't restart | Check `daemon.status`; stale pid files are removed by `daemon stop`. Inspect `daemon.err.log`. |
| Webhook 401/409/429 | Check token, delivery id, and rate window. |

## Cache repair

Cache entries are content-addressed and validated on read. A truncated or
corrupt entry is not served; it is invalidated. To clear all cached action
results deliberately:

```console
ubuilder cache clear
```

Cache identity includes workflow namespace, host triple and toolchain
fingerprint; changing either yields a different key. Do not share a cache root
across incompatible toolchains and expect reuse.

## Log rotation

The daemon appends to `daemon.log`, `daemon.err.log` and `runs.log`. Rotate them
with the platform tool. Per-run logs are removed by `maintenance prune` with the
run directory.

## Release rollback

1. Stop the daemon.
2. Replace the binary with the previous verified archive.
3. Restore the matching data-directory backup if the schema was migrated.
4. Verify `ubuilder --version`, `doctor --all --json`, and one pipeline run.
5. Keep the failed archive and its checksums for incident analysis.

## Security posture

Default mode is `trusted-native`: workflows run as the CLI user with full host
access and must be trusted. `security.mode: isolated` fails closed with
`SECURITY_CAPABILITY_UNAVAILABLE` (exit 3) unless a verified backend exists.
Windows has no verified sandbox backend in this release. Do not expose the
webhook beyond loopback without TLS and a trusted proxy.
