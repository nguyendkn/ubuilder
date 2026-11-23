# Workflow DSL

Workflows live in `.ubuilder/workflows/*.yml` or any path passed with `--file`.
The document is strict: unknown keys are errors, and the trigger key must be
quoted (`"on":`) because YAML 1.1 treats bare `on` as a boolean.

```yaml
version: 1
name: desktop
"on":
  manual: true
env:
  CARGO_TERM_COLOR: never
defaults:
  shell: powershell
  working-directory: .
jobs:
  build:
    name: Build desktop
    needs: []
    runs-on: auto
    if: ${{ success() }}
    strategy:
      fail-fast: true
      max-parallel: 1
      matrix:
        target: [windows-x64]
    env:
      PROFILE: release
    timeout: 30m
    retry:
      max-attempts: 2
      delay: 2s
      backoff: exponential
    concurrency:
      group: desktop-build
      max: 1
    outputs:
      package-path: ${{ steps.package.outputs.path }}
    steps:
      - id: checkout
        uses: builtin/checkout@v1
      - name: Compile
        run: cargo build --release
        shell: powershell
        working-directory: .
        timeout: 10m
        continue-on-error: false
        env:
          RUST_BACKTRACE: "1"
```

## Top level

| Key | Required | Meaning |
| --- | --- | --- |
| `version` | yes | Must be `1` |
| `name` | yes | Human-readable workflow name |
| `"on"` | no | Trigger metadata (parsed and validated; `pipeline run` is manual) |
| `env` | no | Workflow-level environment, evaluated before jobs |
| `defaults.shell` | no | Default shell for run steps |
| `defaults.working-directory` | no | Default workspace-relative directory |
| `jobs` | yes | Ordered map of job id → job |

## Triggers

```yaml
"on":
  manual: true
  push:
    branches: [main]
    tags: [v*]
  pull_request:
    branches: [main]
  schedule:
    - "0 2 * * *"
```

Triggers are declarative metadata for CI integrations. The local runtime only
runs workflows explicitly and does not install a scheduler or watch git.

## Jobs

| Key | Default | Meaning |
| --- | --- | --- |
| `name` | job id | Display name |
| `needs` | `[]` | Job ids that must succeed first |
| `runs-on` | `auto` | `auto`, or `{os, arch, backend}` |
| `if` | always | Boolean expression |
| `strategy` | none | Matrix expansion |
| `env` | `{}` | Job environment |
| `steps` | `[]` | Ordered steps |
| `timeout` | none | Job duration (`500ms`, `2s`, `10m`, `1h30m`) |
| `retry` | none | Whole-job retry |
| `concurrency` | none | Cross-process lease |
| `outputs` | `{}` | Templates exposed to dependents |

Job ids match `[A-Za-z_][A-Za-z0-9_-]*` and must be unique. Cycles and unknown
`needs` are rejected during validation.

### `runs-on`

```yaml
runs-on: auto
runs-on:
  os: windows
  arch: x64
  backend: native
```

`auto` means the host. Only the `native` backend is implemented; `container`
and `wsl` are rejected by the planner with a remediation. Platform execution is
never silently skipped: a plan marks a non-host target unsupported and the
choice to build it remains explicit.

## Matrix strategy

```yaml
strategy:
  fail-fast: true
  max-parallel: 2
  matrix:
    target: [windows-x64, linux-x64]
    profile: [debug, release]
    include:
      - target: windows-x64
        extra: signed
    exclude:
      - target: linux-x64
        profile: debug
```

- Axes expand in declaration order, then `include`, then `exclude`.
- `include` merges into compatible combinations or adds a new combination.
- `exclude` removes combinations that exactly match all its keys.
- Limit: 256 combinations. Duplicate combinations are errors.
- Job ids become `build[target=windows-x64,profile=release]`.
- `${{ matrix.<axis> }}` is available in steps.
- With `fail-fast: true`, a failure stops new siblings from being scheduled:
  siblings that have not acquired a concurrency permit become `skipped`.
  Already-running siblings are allowed to finish. `max-parallel` controls the
  limit; when absent it defaults to host parallelism, never to 1 (fail-fast
  does not serialize the matrix).

## Steps

A step is either `run` or `uses`, optionally identified by `id`. Fallback ids are
`step-1`, `step-2`, ...

| Key | Applies | Default | Meaning |
| --- | --- | --- | --- |
| `id` | both | `step-N` | Reference from `${{ steps.<id>.outputs.* }}` |
| `name` | both | generated | Display name |
| `run` | run | required | Command text |
| `uses` | action | required | Built-in action id |
| `with` | action | `{}` | Action inputs |
| `shell` | run | defaults/host | `powershell`, `pwsh`, `cmd`, `bash`, `sh`, `python` |
| `working-directory` | run | workspace | Must stay inside the workspace |
| `if` | both | always | Boolean expression |
| `continue-on-error` | both | `false` | Failure becomes conclusion `success` |
| `timeout` | both | none | Step duration |
| `retry` | both | none | Step retry policy |
| `env` | both | `{}` | Step environment |

A failed step stops the job unless `continue-on-error: true`. `if` expressions
are evaluated before execution; false steps are `skipped`. With
`continue-on-error`, the step keeps `status: failure` and reports
`conclusion: success`; the job continues. A normal step has matching status and
conclusion.

## Expressions

Templates use `${{ ... }}`. The expression language supports:

- literals: strings, numbers, booleans, null
- paths: `matrix.target`, `needs.build.outputs.path`, `steps.package.outputs.path`;
  identifiers may contain `-`, so `steps.cache.outputs.cache-hit` is valid
- operators: `==`, `!=`, `&&`, `||`, `!`, comparisons
- functions: `success()`, `failure()`, `cancelled()`, `always()`,
  `contains(a, b)`, `startsWith(a, b)`, `endsWith(a, b)`

Available roots:

| Root | Fields |
| --- | --- |
| `run` | `id`, `workflow`, `source_path` |
| `job` | `id`, `base_id`, `name` |
| `matrix` | expanded axis values |
| `inputs` | `--input KEY=VALUE` values |
| `needs` | `<job>.result`, `<job>.outputs.<name>` |
| `steps` | `<step>.outputs`, `.status`, `.conclusion`, `.outcome` |
| `env` | current job/step environment |
| `runner` | `os`, `arch`, `native`, `backend` |
| `workspace` | absolute job workspace (inside run-code contexts) |
| `secrets` | names referenced by the workflow and resolved before the run |

Unknown roots, unknown functions and malformed syntax fail validation with the
field and location. Expressions never invoke a shell or an interpreter.

## Durations and retry

Durations require a unit: `ms`, `s`, `m`, `h`; combinations such as `1h30m` are
accepted. Retry:

```yaml
retry:
  max-attempts: 3      # 1..10
  delay: 2s
  backoff: exponential # fixed | exponential
```

## Concurrency

```yaml
concurrency:
  group: shared-release
  max: 1
```

With a group and `max: 1`, the runtime takes a SQLite lease per group. Other
runs poll until the lease is released or expires (one hour). `max` alone limits
local parallelism for jobs sharing the id; `group` coordinates across processes.
`cancel-in-progress` is parsed for compatibility and currently does not cancel
an existing lease holder.
