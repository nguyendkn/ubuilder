# Intermediate representation

`pipeline compile` prints the IR. The runtime never sees YAML, and the IR is
fully resolved except for runtime values (`${{ ... }}` templates).

```json
{
  "version": 1,
  "name": "ci",
  "source_path": "C:\\project\\.ubuilder\\workflows\\ci.yml",
  "source_digest": "sha256:...",
  "env": {"CARGO_TERM_COLOR": {"source": "always"}},
  "jobs": [
    {
      "id": "build[target=windows-x64]",
      "base_id": "build",
      "name": "build (target=windows-x64)",
      "needs": [],
      "base_needs": [],
      "runner": {
        "requested": {"os": "windows", "arch": "x64", "backend": "native"},
        "os": "windows", "arch": "x64", "native": true, "backend": "native"
      },
      "matrix": {"target": "windows-x64"},
      "fail_fast": false,
      "max_parallel": null,
      "condition": null,
      "env": {},
      "timeout_secs": null,
      "retry": null,
      "concurrency": null,
      "outputs": {},
      "steps": []
    }
  ],
  "order": ["build[target=windows-x64]"],
  "waves": [["build[target=windows-x64]"]],
  "max_matrix_jobs": 256
}
```

## `WorkflowIr`

| Field | Meaning |
| --- | --- |
| `version` | Schema version |
| `name` | Workflow name |
| `source_path` | Absolute workflow path |
| `source_digest` | SHA-256 of the source, used in cache keys |
| `env` | Compiled workflow env templates |
| `jobs` | Jobs in deterministic order |
| `order` | Topological job order |
| `waves` | Job ids grouped by dependency depth |
| `max_matrix_jobs` | Expansion limit (256) |

## `IrJob`

| Field | Meaning |
| --- | --- |
| `id` | Unique id, matrix suffix included |
| `base_id` | Job id before matrix expansion |
| `name` | Display name |
| `needs` | Resolved dependency ids |
| `base_needs` | Dependency base ids before matrix suffixing |
| `runner` | Requested spec plus resolved OS/arch/backend/native flag |
| `matrix` | Axis values for this instance |
| `fail_fast` | Matrix sibling policy |
| `max_parallel` | Explicit sibling limit |
| `condition` | Compiled `if` expression |
| `env` | Compiled job env templates |
| `timeout_secs` | Whole-job timeout |
| `retry` | Retry policy |
| `concurrency` | Concurrency policy |
| `outputs` | Compiled output templates |
| `steps` | Ordered `IrStep`s |

## `IrStep`

| Field | Meaning |
| --- | --- |
| `id`, `index`, `name` | Identity |
| `condition` | Optional compiled `if` |
| `continue_on_error` | Failure policy |
| `timeout_secs` | Step timeout |
| `retry` | Step retry policy |
| `env` | Compiled env templates |
| `kind` | `{"type":"run", command, shell, working_directory}` or `{"type":"action", uses, with}` |

`Template` and `Expression` values are serialized with a `"source"` field
holding the original text, for example `{"source":"always"}`. They are parsed
once at compile time; runtime evaluation only walks the AST, so malformed
expressions cannot reach execution.

## Plan output

`pipeline plan` is a projection of the IR with host capability data:

```json
{
  "name": "ci",
  "source_path": "...",
  "source_digest": "...",
  "host": "Windows x64",
  "waves": [["build"]],
  "jobs": [
    {
      "id": "build",
      "base_id": "build",
      "needs": [],
      "wave": 0,
      "runner": {"os": "windows", "arch": "x64", "native": true, "backend": "native"},
      "target": {"id": "windows-x64", "triple": "x86_64-pc-windows-msvc", "...": "..."},
      "capability": {"compilation": "CONDITIONAL", "native_testing": "SUPPORTED", "...": "..."},
      "executable": true,
      "unsupported_reason": null,
      "steps": [
        {"kind": "uses", "detail": "builtin/build@v1", "timeout_secs": null}
      ]
    }
  ],
  "warnings": [
    {"code": "linker-unverified", "job": "build", "message": "..."}
  ]
}
```

`executable` describes host compatibility only, not whether every required
toolchain is installed. The builder reports missing tools at execution time.
