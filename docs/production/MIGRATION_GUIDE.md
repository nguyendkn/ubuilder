# Migration guide

## Database schema

Current schema version: **1**. `schema_migrations` stores `(version, applied_at)`.
Schema creation is idempotent (`CREATE TABLE IF NOT EXISTS`) and all additive
tables use `IF NOT EXISTS`.

Upgrade procedure:

1. Stop the daemon and active runs.
2. Copy `<data>/ubuilder.db` and `<data>/pipeline/`.
3. Replace the binary.
4. Run `ubuilder maintenance stats --json` and `ubuilder pipeline history --limit 1`.
5. Run one small workflow and verify status/logs.

Downgrade: version 1 is the only schema. A newer binary may add tables or
columns; the current reader does not reject a higher `schema_migrations`
version. Restore the backup taken before upgrade if the old binary misbehaves.

## State spelling compatibility

Persisted state strings keep the historical values:

| `RunState` variant | Persisted string |
| --- | --- |
| Pending | `queued` |
| Succeeded | `success` |
| Failed | `failure` |
| TimedOut | `timed_out` |
| Interrupted | `interrupted` |

Readers accept both `pending`/`queued`, `success`/`succeeded`,
`failure`/`failed`, `cancelled`/`canceled`, `timed_out`/`timeout`. JSON reports
continue to use the persisted spellings.

## Cache changes

- Pipeline cache keys are step-scoped and host-specific:
  `sha256("pipeline/v1/<host>\n<user-key>")`.
- The workflow digest is intentionally excluded. If a step's output depends on
  a toolchain or global input, that input must be part of the step key.
- Old keys that included the workflow digest are not read; the first run after
  upgrade misses and repopulates. Use `ubuilder cache clear` to reclaim space if
  the old cache is large.

## Artifact changes

- Objects are content-addressed and verified on read and upload.
- `maintenance gc` deletes objects not referenced by retained runs and not
  pinned. Pin release artifacts before pruning history.
- Deleting a run no longer deletes a shared object immediately; GC is explicit.

## Secrets

Before this version secrets were environment text only. Migration:

```console
ubuilder secret set TOKEN --stdin < token.txt
ubuilder secret list --json
```

Environment values (`UBUILDER_SECRET_TOKEN` or `TOKEN`) take precedence over the
file store. Remove the plaintext source after migration and rotate if it was
stored in shell history.

## Concurrency

Legacy `pipeline_locks` rows are not read by the new bounded-slot scheduler.
Expired slots are cleaned on acquisition. If an old run left a group blocked,
wait for the lease or delete its slot rows after confirming no process is alive.

## CLI and JSON

No command was removed. New commands: `secret`, `maintenance`, `daemon`.
The JSON envelope is unchanged; new `maintenance`, `daemon`, secret and state
fields are additive. Exit codes are unchanged.
