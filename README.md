# ubuilder

Universal Desktop Builder is a local-first build and packaging CLI for desktop
applications. It detects Rust, .NET, Electron and Tauri projects, resolves
canonical targets, builds them with the native adapter, and records artifacts.

The pipeline layer turns the same primitives into a declarative YAML workflow
engine (`ubuilder pipeline`) with isolated workspaces, a DAG scheduler, matrix
jobs, local artifacts, caching, retries, timeouts, cancellation and SQLite run
history. Legacy `ubuilder build`, `package` and `release` keep working.

## Build

```console
cargo build --release
cargo test
```

Requires stable Rust. `ubuilder doctor --json` reports which host toolchains and
target capabilities are actually available.

## Quick start

```console
ubuilder pipeline init
ubuilder pipeline validate
ubuilder pipeline plan
ubuilder pipeline run
ubuilder pipeline status --run <run-id>
ubuilder pipeline logs --run <run-id> --job build --step step-1
ubuilder pipeline history --limit 10
```

`pipeline init` writes `.ubuilder/workflows/ci.yml`. When the project has exactly
one workflow file, `validate`, `plan`, `run` and `graph` use it automatically.

## Workflow example

```yaml
version: 1
name: ci
on:
  manual: true
jobs:
  build:
    steps:
      - uses: builtin/checkout@v1
      - uses: builtin/setup-rust@v1
      - uses: builtin/build@v1
      - uses: builtin/package@v1
        with:
          format: zip
      - uses: builtin/upload-artifact@v1
        with:
          name: desktop
          path: dist
  release:
    needs: [build]
    steps:
      - uses: builtin/download-artifact@v1
        with:
          name: desktop
          path: dist
      - uses: builtin/checksum@v1
        with:
          path: dist
          output: dist/checksums.sha256
```

Every job runs in its own workspace under the data directory. Artifacts and job
outputs (through the `needs` context) are the supported channels between jobs.

## Commands

| Command | Purpose |
| --- | --- |
| `pipeline init` | Create a starter workflow |
| `pipeline list` | List workflows in the project |
| `pipeline inspect` | Show the parsed workflow |
| `pipeline validate` | Report every schema/expression diagnostic |
| `pipeline compile` | Print the intermediate representation |
| `pipeline plan` | Print waves, jobs and capability status |
| `pipeline graph` | Render Mermaid, DOT or JSON |
| `pipeline run` | Execute a workflow |
| `pipeline status` | Inspect a recorded run |
| `pipeline logs` | Print recorded step logs |
| `pipeline history` | List recorded runs |
| `pipeline cancel` | Request cancellation of a run |
| `pipeline recover` | Fail interrupted `running` runs |
| `pipeline actions` | List built-in actions and inputs |
| `secret` | `list`, `set`, `delete`, `inspect` secret metadata |
| `maintenance` | `stats`, `prune`, `gc`, `pin`, `unpin` run/artifact retention |
| `daemon` | `start`, `stop`, `status`, `run`, `trigger` local triggers |

All commands support `--json`; failures use the stable envelope
`{"ok":false,"command":...,"error":{"code","message","remediation","details"}}`
and exit codes `0` success, `1` execution/build failure, `2` usage, `3` invalid
configuration or unsupported toolchain, `130` cancelled.

## Documentation

- [Architecture overview](docs/ARCHITECTURE.md)
- [Pipeline architecture](docs/PIPELINE_ARCHITECTURE.md)
- [Workflow DSL](docs/PIPELINE_DSL.md)
- [Intermediate representation](docs/PIPELINE_IR.md)
- [Runtime semantics](docs/PIPELINE_RUNTIME.md)
- [Security model](docs/PIPELINE_SECURITY.md)
- [Built-in actions](docs/PIPELINE_ACTIONS.md)
- [Examples](docs/PIPELINE_EXAMPLES.md)
- [Implementation status](docs/PIPELINE_IMPLEMENTATION_STATUS.md)
- [Production readiness](docs/production/FINAL_READINESS_REPORT.md)
- [Operations runbook](docs/production/OPERATIONS_RUNBOOK.md)
- [Example workflow](examples/native-matrix.yml)

## Tested platforms

The pipeline is exercised on Windows 10.0.26300 x64 (PowerShell default shell)
with the `x86_64-pc-windows-msvc` Rust target. Linux and macOS jobs are
configured in CI but have not been executed in this environment; no native
claim is made for them. Cross-target builds require the relevant linker and
SDK; `pipeline plan` marks such targets unsupported or unverified rather than
pretending success. See `docs/production/CROSS_PLATFORM_MATRIX.md`.
