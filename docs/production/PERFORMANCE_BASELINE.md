# Performance baseline

Host: Windows 10.0.26300 x64, 16 logical CPUs, Rust 1.99.1, release profile with
thin LTO. Local measurements only; not a cross-platform claim.

## Method

Primary run:

```console
cargo bench --bench pipeline --locked -- --sample-size 30 --measurement-time 3 --warm-up-time 1
```

Benchmarks (`benches/pipeline.rs`):

- `parse_validate_12_jobs_matrix_4`: parse and validate a 12-job workflow with a
  2x2 matrix (48 expanded jobs).
- `compile_12_jobs_matrix_4`: compile the same parsed workflow to the IR.
- `plan_48_jobs`: plan the 48-job IR, including project detection, capability
  resolution and target selection.

The final benchmark was run three times with identical settings on the same
host. Criterion intervals were wide and run-to-run drift was substantial, so the
primary run is reported with the observed range rather than a single point.

## Results

Primary run (`target/finalization-bench.log`):

| Benchmark | Result | vs baseline |
| --- | --- | ---: |
| Parse + validate 48 expanded | 271.88 µs [261.81, 282.45] | +15.8% |
| Compile + matrix expansion | 477.68 µs [460.59, 494.62] | +18.3% |
| Plan 48 jobs, warm facts cache | 104.57 ms [100.69, 109.02] | +46.6% |

Repeated clean runs (`target/finalization-bench-2.log`,
`target/finalization-bench-3.log`):

| Benchmark | Run 2 | Run 3 |
| --- | ---: | ---: |
| Parse + validate | 280.90 µs | 270.56 µs |
| Compile + expand | 565.19 µs | 489.44 µs |
| Plan 48 jobs | 112.66 ms | 101.43 ms |

Stored baseline (earlier on the same host) was 234.69 µs (233.04-237.14),
403.61 µs (397.80-409.43) and 71.31 ms (68.33-75.53). A single-CPU affinity run
was much slower (354 µs / 742 µs / 191 ms) and is treated as a scheduling
experiment, not a baseline.

## Analysis

- Parsing and compilation remain sub-millisecond for a 48-job IR. The observed
  increases (+16% and +18%) are below the 20% blocking threshold for those two
  benchmarks.
- Warm planning moved from 71.31 ms to 101-113 ms (+42% to +58%). The planner and
  provider code did not change between the stored baseline commit and this
  release; the same host produced a 104 ms, 113 ms and 101 ms result minutes
  apart, with Criterion often reporting `p > 0.05` for the change against the
  immediately preceding run. The most likely cause is host load/thermal state,
  not a planner regression. This is disclosed as an unresolved performance
  regression, not certified as neutral.
- Why planning is not microseconds: the warm path still performs project
  detection, capability resolution and target selection per invocation. Facts are
  cached in-process for 30 s; every CLI run starts a fresh process, so a daemon
  or repeated one-shot invocation pays the probe cost again.
- Cold planning (~4.5 s in the stored baseline) is dominated by `which` probes
  for ~45 tools plus version subprocesses (rustc, cargo, rustup, dotnet, node,
  npm, zig, xcodebuild). It was not separately instrumented in this final run;
  treat the old 4.5 s figure as historical, not freshly measured.
- Release binary: approximately 9.0 MiB (`target/release/ubuilder.exe`).
  `UBUILDER_NO_FACTS_CACHE=1` forces probing and is useful for diagnosing stale
  capability data.

## Not measured

- End-to-end cargo/dotnet/npm builds (dominated by external toolchains).
- Memory high-water mark under a large matrix.
- SQLite throughput under concurrent runs.
- Linux and macOS timings.
- Cold planning after finalization.

## Regression policy

Run the benchmark before and after scheduler/cache changes. A >20% regression in
parse or compile is blocking. Dynamic code is not. Warm planning should be
tracked as a release metric; a >20% change requires explanation, and the
explanation above is recorded rather than smoothed over. Do not hide cold cost
by caching across processes without an invalidation policy.
