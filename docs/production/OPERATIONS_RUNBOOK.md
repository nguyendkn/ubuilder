# Operations runbook

## Installation

```console
cargo build --release --locked
install -m 0755 target/release/ubuilder ~/.local/bin/ubuilder
ubuilder --version
ubuilder doctor --json
```

On Windows, place `ubuilder.exe` in a directory on `PATH`. There is no installer
or auto-updater; upgrades are binary replacements. Keep the data directory
backed up before a major upgrade.

## Data layout

Default data directory: `%LOCALAPPDATA%\ubuilder` on Windows,
`~/Library/Application Support/ubuilder` on macOS, and the platform data dir on
Linux. `UBUILDER_DATA_DIR` overrides it.

```text
<data>/
  ubuilder.db                 SQLite history, cache index, pins, locks
  secrets.json                file-provider secrets (0600 where supported)
  secrets.json.lock
  pipeline/
    runs/<run-id>/{jobs,logs,cancel}
    artifacts/objects/<sha>/...
    cache/cas/...
  daemon/{daemon.pid,state.json,daemon.log,runs.log}
```

## Daily operations

| Task | Command |
| --- | --- |
| Health | `ubuilder doctor --json` |
| Storage | `ubuilder maintenance stats --json` |
| Run workflow | `ubuilder pipeline run --file <workflow>` |
| Inspect | `ubuilder pipeline status --run <id>` |
| Logs | `ubuilder pipeline logs --run <id> --job <job> --step <step>` |
| Cancel | `ubuilder pipeline cancel --run <id>` |
| Recover | `ubuilder pipeline recover` |
| Prune, preview | `ubuilder maintenance prune --dry-run --json` |
| Prune, apply | `ubuilder maintenance prune --max-runs 100 --max-age-days 30 --max-size-mb 4096` |
| GC objects | `ubuilder maintenance gc --dry-run` then `--` without dry-run |
| Pin release | `ubuilder maintenance pin <sha-prefix> --name v1.0.0` |
| Secrets | `ubuilder secret list`, `secret set NAME --stdin` |
| Daemon | `ubuilder daemon start`, `status`, `trigger`, `stop` |

## Triggers

- Polling: `daemon start --file <workflow> --interval-secs 30`.
- Webhook: add `--webhook 8787 --token <token>`; `POST /hook` on loopback.
- Cron: call `ubuilder daemon trigger` from Task Scheduler, cron or a systemd
  timer. The daemon itself has no cron parser.
- A run is triggered when the workflow bytes or git `HEAD` change, or when the
  trigger file appears. The daemon serializes runs.

## Secrets

`ubuilder secret set NAME` prompts; `--stdin` and `--file` avoid argv. Values
are never printed. A stored value is shadowed by `UBUILDER_SECRET_NAME` or the
bare environment variable; `inspect` warns. Missing referenced secrets fail the
run before any job starts.

## Failure handling

- `STATE_INVALID`: a lifecycle transition was rejected. Inspect the run status;
  do not edit rows blindly. Recover stale runs with `pipeline recover`.
- `CACHE_CORRUPT`: the entry was deleted. Re-run to repopulate.
- `ARTIFACT_INVALID`: hash/manifest mismatch or path escape. Inspect the object
  and the producing job; do not pin a suspect artifact.
- Concurrency wait: another run holds the group. Check `pipeline history` and
  cancel the stale run. A crashed run can hold a slot for up to 1 h.
- Disk pressure: run `maintenance stats`, then a dry-run prune/gc.

## Backup and restore

Stop the daemon, copy the whole data directory while no run is active, or use
`sqlite3 ubuilder.db ".backup backup.db"`. Restore the directory with matching
permissions. Artifact objects and the SQLite rows must be restored together.

## Log rotation

The daemon appends to `daemon.log` and `runs.log`. Rotate with the platform tool
or add them to a logrotate/Task Scheduler job. Per-run logs are removed by
`maintenance prune` with the run directory.

## Security posture

The daemon binds loopback only. Do not expose the webhook directly; put a TLS
reverse proxy in front and keep the bearer token secret. Treat workflows as code:
they run with the CLI user's filesystem permissions.
