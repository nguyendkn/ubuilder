# Architecture

`ubuilder` is a single Rust crate with two cooperating layers.

## Legacy builder

```text
src/cli            command parsing and JSON envelopes
src/config         ubuilder.toml
src/detect         project/framework detection
src/target         canonical targets and triples
src/host           host identity
src/providers      toolchain facts and provisioning
src/planner        project → build plan
src/builder        orchestrates build/package/release
src/adapters       Rust, .NET, Electron, Tauri
src/executor       process execution, streaming, timeout, cancellation
src/scheduler      legacy step scheduler
src/cache          action cache
src/artifacts      packaging and manifests
src/validation     artifact format/architecture checks
src/signing        signing hooks
src/storage        SQLite history and leases
src/security       path containment, env filtering, redaction
src/telemetry      human and JSON output
```

`build`, `package` and `release` use this layer. It remains the implementation
of actual compilation and packaging.

## Pipeline layer

`src/pipeline` adds declarative orchestration without a second build engine:

```text
parser → schema → validator → compiler → ir
                                  ↓
                               planner
                                  ↓
                               runtime → actions → legacy builder/executor
                                  ↓
                         storage / artifacts / cache / history
```

The CLI entry point is `src/cli/pipeline_cmd.rs`. See
[Pipeline architecture](PIPELINE_ARCHITECTURE.md) for stages, invariants and
storage layout, and [Runtime semantics](PIPELINE_RUNTIME.md) for execution.

## Layering rules

- Parser: no execution, no workspace access.
- Validator: pure, accumulates diagnostics.
- Compiler: deterministic, expands matrix, builds waves, parses expressions.
- Planner: host capability projection, no side effects.
- Runtime: executes IR, owns workspaces and persistence.
- Actions: thin adapters over existing builder primitives.
- Expressions: fixed AST interpreter, no shell/JS/eval.

Errors are `UbuilderError` with stable `ErrorCode`s across both layers.
