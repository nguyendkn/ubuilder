# Pipeline security model

The pipeline runs arbitrary user-authored commands. Its security boundary is:

- **No hidden interpreter for expressions.** No `eval`, shell expansion or
  JavaScript. A fixed lexer/parser/evaluator handles `${{ ... }}`.
- **No implicit network or remote actions.** Only compiled-in `builtin/*`
  actions execute. There is no action marketplace, download or arbitrary script
  fetch.
- **Workspace confinement.** Every path derived from workflow data is joined to
  the job workspace and checked after canonicalization. `..` and absolute paths
  cannot escape. Checkout symlinks whose targets leave the project are errors.
- **Process isolation.** Jobs use separate directories. Child processes are
  launched with a cleared environment repopulated from an allowlist plus
  explicitly configured values.
- **Bounded inputs.** Workflows are limited to 1 MiB, matrix expansion to 256
  jobs, retries to 10 attempts, concurrency max to 64, run ids to 96 safe
  characters, and expression evaluation has depth/size limits.
- **Structured artifacts.** Artifact names cannot contain path separators;
  extracted/downloaded paths are joined safely. Uploads are content-addressed
  by SHA-256 and recorded with size.
- **No silent success.** Unsupported targets and missing toolchains produce
  errors or plan warnings; they are not mocked.

## Trust assumptions

The workflow file and the project source are trusted inputs. A workflow can run
any command the invoking user can run. Secrets are not injected automatically;
`secrets` is an empty map. Environment filtering reduces accidental leakage but
is not a sandbox against malicious code.

## Data at rest

Run history, logs, workspaces, artifacts and cache entries are stored under the
data directory (default platform data dir, overridable with
`UBUILDER_DATA_DIR`). They are not encrypted. Manual deletion of the data
directory is the retention mechanism; there is no automatic pruning.

## Cancellation and process cleanup

Timeouts and cancellation terminate the whole process tree. On Windows this
uses `taskkill /PID <pid> /T /F`; on Unix the child is placed in its own process
group and receives `SIGTERM` then `SIGKILL`. A tool that escapes the tree can
still write outside the workspace; that is inherent to running local commands.

## Secrets

Redaction is applied to captured stdout/stderr using values discovered from the
environment via the existing security module. Prefer passing secrets as
environment values at invocation time. Avoid committing them to workflow files:
workflow source is stored in run metadata and can be inspected later.
