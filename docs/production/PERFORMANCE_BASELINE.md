# Performance baseline

Host: Windows 10.0.26300 x64, 16 logical CPUs, Rust 1.99.1, release profile
with thin LTO. Measurements are local and are not cross-platform claims.

## Method

`cargo bench --bench pipeline -- --sample-size 10 --measurement-time 1`

Benchmarks (`benches/pipeline.rs`):

- `parse_validate_12_jobs_matrix_4`: parse and validate a 12-job workflow with a
  2x2 matrix (48 expanded jobs).
- `compile_12_jobs_matrix_4`: compile the same parsed workflow to the IR.
- `plan_48_jobs`: plan the 48-job IR, including project detection, capability
  resolution and target selection.

## Results

| Benchmark | Observed |
| --- | --- |
| Parse + validate | 234.69 µs (233.04-237.14 µs) |
| Compile + matrix expansion | 403.61 µs (397.80-409.43 µs) |
| Plan 48 jobs, warm facts cache | 71.31 ms (68.33-75.53 ms) |
| Plan 48 jobs, cold (before cache) | ~4.5 s |

Criterion also reported a 98.4% improvement for warm planning after the facts
and per-target capability caches were added.

## Analysis

- Parsing, validation and compilation are sub-millisecond for a 48-job IR.
- Cold planning is dominated by toolchain probing: `which` for ~45 tools plus
  version subprocesses (rustc, cargo, rustup, dotnet, node, npm, zig, xcodebuild).
- Warm planning still performs project detection on each call; ~71 ms for 48
  jobs.
- The facts cache has a 30 s TTL and is keyed by host triple, OS version and
  project root. `UBUILDER_NO_FACTS_CACHE=1` forces a probe.
- Release binary: 8.7 MiB (`target/release/ubuilder.exe`, stripped).

## Not measured

- End-to-end cargo/dotnet/npm builds (dominated by the external toolchain).
- Memory high-water mark under a large matrix.
- SQLite throughput under concurrent runs.
- Linux and macOS timings.

## Regression policy

Run the benchmark before and after scheduler/cache changes. Treat a >20%
regression in parse or compile as blocking. Cold planning should stay within the
toolchain probe cost; do not hide it by caching across processes without an
invalidation policy.
