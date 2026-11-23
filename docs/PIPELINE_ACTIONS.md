# Built-in actions

Use `builtin/<name>@v1` with `uses:`. `ubuilder pipeline actions --json` prints
the authoritative input schema.

All actions run with the job workspace as their base. Relative paths are
workspace-confined. A step's outputs are available as
`${{ steps.<id>.outputs.<name> }}`.

## `builtin/checkout@v1`

Copy the project source into the workspace.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `path` | no | `.` | Workspace-relative destination |
| `snapshot` | no | `false` | Compatibility flag; local runs always copy the source |

Excludes `.git`, `target`, `node_modules`, `.ubuilder`, `dist`, `dist-smoke`.
Outputs: `path`.

## `builtin/setup-rust@v1`, `builtin/setup-dotnet@v1`, `builtin/setup-node@v1`

Verify that the native tool is installed and compatible.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `version` | no | none | Expected version prefix |
| `channel` | no | none | Alias for version; `stable`, `beta`, `nightly` and empty mean no constraint |

These actions do not download toolchains. Missing tools fail with
`TOOLCHAIN_MISSING` and a remediation.

## `builtin/build@v1`

Run the native adapter for the project and target.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `framework` | no | `auto` | `auto`, `rust`, `dotnet`, `electron`, `tauri`; validated against detection |
| `target` | no | `native` | Canonical target id, e.g. `windows-x64`; `native`/`auto` mean host |
| `profile` | no | config | Build profile |
| `offline` | no | `false` | Avoid network where supported |
| `no-cache` | no | `false` | Disable the action cache for this build |

Outputs include the build payload path and target.

## `builtin/package@v1`

Package the most recent build payload using the native packaging backend.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `format` | no | `portable` | `zip`, `tar.gz`, `deb`, `rpm`, `nsis`, `app`, `dmg`, ... |
| `target` | no | last build | Override the build target; `native`/`auto` are ignored |

Empty `packaging.formats` in project config keeps the legacy per-target native
default. Outputs: `path`, `format`, `target`, `sha256`, `size`.

## `builtin/verify@v1`

Validate an artifact's format and architecture.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `path` | no | last package output | Artifact path |
| `target` | no | last target | Target used for architecture/format validation |

Outputs: `path`, `sha256`, `size`, `target`.

## `builtin/upload-artifact@v1`

Publish a file, directory or glob to the run artifact store.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `name` | yes | — | Artifact name; no path separators |
| `path` | yes | — | Workspace-relative file, directory or glob |

A multi-match upload suffixes each artifact name with the matched filename.
Outputs: `name`, `sha256`, `size`, `count`. Artifacts are immutable snapshots
stored by content hash.

## `builtin/download-artifact@v1`

Download artifacts from earlier jobs in the same run.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `name` | one of | — | Exact name or `*` glob |
| `pattern` | one of | — | Alias for `name` |
| `path` | no | `artifacts` | Workspace-relative destination |

Outputs: `path`, `count`. Downloading a pattern that matches nothing fails the
step rather than producing an empty success.

## `builtin/checksum@v1`

Write SHA-256 checksums for a file or directory tree.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `path` | no | `dist` | File or directory |
| `output` | no | `checksums.sha256` | Workspace-relative checksum file |

The output file is excluded from the hashed set. Lines use the standard
`<sha256>  <relative/path>` format, sorted deterministically. Outputs: `path`,
`count`.

## `builtin/cache@v1`

Restore and save a workspace directory.

| Input | Required | Default | Notes |
| --- | --- | --- | --- |
| `key` | yes | — | User cache key; include lockfile/toolchain identity |
| `path` | yes | — | Workspace-relative directory |
| `restore` | no | `true` | Restore before the step |
| `save` | no | `true` | Save at the end of the job on a miss |

Outputs: `cache-hit` (`true`/`false`), `key`.
