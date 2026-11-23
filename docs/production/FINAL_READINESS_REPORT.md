# Final production readiness report

Date: 2026-11-23
Host: Windows 10.0.26300 x64, 16 logical CPUs, Rust 1.99.1
Source commit: `a4cd66d0e206670872a81622d8742231c316dd42`
Version: `ubuilder 0.1.0`

**Verdict: `PRODUCTION_READY_WITH_LIMITATIONS` for the Windows x64
trusted-native scope.** Isolated execution is `NOT_PRODUCTION_READY`. Cross-build
certification covers only `windows-x64 -> windows-x64` with Rust and .NET.

## 1. What was completed

| Area | Final change |
| --- | --- |
| Schema safety | Schema v2; newer databases return `SCHEMA_UNSUPPORTED` before DDL; `VACUUM INTO` backup before migration. |
| Concurrency | Generation-fenced leases, 60 s lease, 15 s heartbeat, cancellation on lease loss; `cancel-in-progress` releases the slot. |
| Cache | Layered key: namespace + host + toolchain fingerprint + user key; corruption and traversal tests. |
| Secrets | Per-job scoping, fail-closed resolution, global redaction, Windows ACL inheritance removal, reparse-point rejection. |
| Filesystem | Canonicalized junction/symlink containment for artifacts and step paths. |
| Isolation | Provider interface with native and Linux bubblewrap implementations; isolated mode fails closed, no fallback. |
| Daemon/webhook | Required token, default loopback bind, 16 KiB headers, 64 KiB body, 60/60 s rate window, 1024-id replay cache, 401/405/409/413/429/202. |
| Diagnostics | `doctor --all` diagnoses every canonical target in a single envelope. |
| Release | Arch-specific archives, `scripts/manifest.py`, deterministic packaging, SBOM, checksums, smoke tests, release workflow. |
| Tests | New .NET executable fixture, artifact corruption test, `doctor --all` contract, structural scheduling assertions. |

Legacy `build`, `package`, `release`, YAML v1, SQLite data, artifact references,
`{ok,command,data}` envelopes and exit codes 0/1/2/3/130 are preserved.

## 2. Gate results

Command: `cargo test --all-targets --locked`, final run exit 0.

| Gate | Status | Basis |
| --- | --- | --- |
| G01 Static gates | PASS | fmt and clippy `-D warnings` pass |
| G02 Test suite | PASS (Windows) | 172/172 pass |
| G03 Isolated execution | BLOCKED | fail-closed behavior passes; no verified backend |
| G04 Filesystem/secrets | PASS | 16 adversarial PASS, 2 BLOCKED |
| G05 State/data safety | PASS | schema, lease and retention tests |
| G06 Pipeline semantics | PASS | 27 runtime + 7 contract tests |
| G07 Portability | BLOCKED | no Linux/macOS/ARM64 execution, no CI run |
| G08 Daemon/webhook | PASS | 4 daemon tests + rate/constant-time unit test |
| G09 Supply chain/release | BLOCKED | no audit, no signature |
| G10 Performance | PASS with disclosed regression | parse/compile within policy; warm plan +46.6% |

Machine-readable details: `READINESS_GATES.json`, `READINESS_EVIDENCE.md`.

## 3. Tests

| Suite | Passed | Failed |
| --- | ---: | ---: |
| Library unit tests | 123 | 0 |
| `src/main.rs` | 0 | 0 |
| `tests/builds.rs` | 2 | 0 |
| `tests/cli.rs` | 6 | 0 |
| `tests/compatibility.rs` | 3 | 0 |
| `tests/daemon.rs` | 4 | 0 |
| `tests/pipeline_contract.rs` | 7 | 0 |
| `tests/pipeline_runtime.rs` | 27 | 0 |
| **Total** | **172** | **0** |

Log: `target/finalization-final.log`; clippy log:
`target/finalization-clippy.log`. Release build: `cargo build --release
--locked`, exit 0.

Timing tests were made structural during finalization: parallel, fail-fast and
concurrency behavior is proven by marker-file intervals, not wall-clock
thresholds that failed under a loaded 27-test binary.

## 4. Native fixtures

| Fixture | Result |
| --- | --- |
| Rust CLI (`fixtures/rust-cli`) | Built for `windows-x64`; ZIP validated; manifest and SQLite record verified. |
| .NET console (`fixtures/dotnet-console`, `net10.0`) | Built with SDK 11.0.100; all validations passed; extracted `HelloDotnet.exe` printed `hello from HelloDotnet`. |
| Electron | Detection verified; packaging UNVERIFIED (`electron-builder` absent). |
| Tauri | Detection verified; packaging UNVERIFIED (`tauri` absent). |

## 5. Security

- 18 adversarial cases: 16 PASS, 2 BLOCKED (Unix symlink execution, sandbox
  escape). Full list in `adversarial-matrix.json`.
- Webhook authentication is mandatory; token is passed to the detached child only
  through the environment.
- No verified Windows sandbox. `security.mode: isolated` returns
  `SECURITY_CAPABILITY_UNAVAILABLE` (exit 3) rather than running natively.
- Dependency audit is BLOCKED: `cargo-audit`/`cargo-deny` absent and no advisory
  database queried. The CI audit job is configuration, not a run.

## 6. Cross-platform status

| Host -> target | Status | Evidence |
| --- | --- | --- |
| windows-x64 -> windows-x64 (Rust, .NET) | SUPPORTED | executed here |
| windows-x64 -> windows-x64 (Electron, Tauri) | CONDITIONAL | detection only |
| windows-x64 -> windows-arm64 | UNVERIFIED | no host |
| windows-x64 -> linux-x64 / linux-arm64 | UNVERIFIED | no linker; cross build failed |
| windows-x64 -> macos-x64 / macos-arm64 / macos-universal | UNVERIFIED | no SDK/host |

CI defines Windows, Ubuntu, macOS x64 and macOS arm64 jobs. It has not run in
this environment. No Linux, macOS or ARM64 binary is certified.

## 7. Performance

`cargo bench --bench pipeline --locked -- --sample-size 30 --measurement-time 3
--warm-up-time 1`:

| Benchmark | Baseline | Final | Change |
| --- | --- | ---: | ---: |
| Parse + validate 48 expanded | 234.69 µs | 271.88 µs [261.81, 282.45] | +15.8% |
| Compile + expand | 403.61 µs | 477.68 µs [460.59, 494.62] | +18.3% |
| Plan 48 jobs, warm | 71.31 ms | 104.57 ms [100.69, 109.02] | +46.6% |

Repeated runs produced 270.56-280.90 µs, 489.44-565.19 µs and 101.43-112.66 ms.
Parse/compile are under the 20% blocking threshold. Warm planning exceeded it;
no planner/provider code changed and Criterion frequently reported `p > 0.05`
against the preceding run, so host variance is the likely cause. The regression
is disclosed, not waived. Cold planning was not re-measured.

## 8. Release artifacts

| File | Size | SHA-256 |
| --- | ---: | --- |
| `ubuilder-windows-x64.zip` | see `RELEASE_VERIFICATION.md` | see `RELEASE_VERIFICATION.md` |
| `sbom.cdx.json` | see `RELEASE_VERIFICATION.md` | see `RELEASE_VERIFICATION.md` |
| `release-manifest.json` | generated after packaging | covers archives and SBOM |
| `SHA256SUMS` | generated last | covers every other file |

The extracted binary reports `ubuilder 0.1.0`; `--json doctor --all` exits 0
with all seven targets; `target list --json` exits 0 with seven entries. Signing
status is `UNSIGNED`; no signature or notarization is claimed.

## 9. Remaining limitations

1. Linux, macOS and ARM64 were not executed; CI has no run record.
2. No Linux cross-linker (`x86_64-linux-gnu-gcc` absent).
3. No verified isolation backend on any platform; isolated mode fails closed.
4. No dependency vulnerability scan.
5. Unsigned artifacts; no notarization or published GitHub release.
6. Warm planning regressed and cold planning was not re-measured.
7. Electron/Tauri packaging unverified; detection only.
8. Webhook is plaintext loopback with in-memory, non-durable replay protection.
9. Facts cache is process-local with a 30 s TTL.
10. No remote executors, containers/WSL backends or native installers.

## 10. Machine-readable verdict

```json
{
  "product": "ubuilder",
  "version": "0.1.0",
  "commit": "a4cd66d0e206670872a81622d8742231c316dd42",
  "verified_at": "2026-11-23T16:01:50Z",
  "host": "windows-10.0.26300-x64",
  "core_production": {
    "status": "PRODUCTION_READY_WITH_LIMITATIONS",
    "scope": "Windows x64, trusted-native execution, local release",
    "gates_passed": ["G01", "G02", "G04", "G05", "G06", "G08", "G10"],
    "gates_blocked": ["G09"],
    "blockers": [
      "no dependency audit",
      "unsigned artifacts",
      "no CI run",
      "Linux/macOS/ARM64 not executed"
    ]
  },
  "isolated_execution_production": {
    "status": "NOT_PRODUCTION_READY",
    "gates_blocked": ["G03", "G07"],
    "blockers": [
      "no verified sandbox backend",
      "fail-closed path is verified but isolation itself is unexecuted"
    ]
  },
  "cross_build_production": {
    "status": "CONDITIONAL",
    "certified_combinations": [
      {
        "host": "windows-x64",
        "target": "windows-x64",
        "frameworks": ["rust", "dotnet"],
        "status": "SUPPORTED"
      }
    ],
    "conditional_combinations": [
      {
        "host": "windows-x64",
        "target": "windows-x64",
        "frameworks": ["electron", "tauri"],
        "status": "CONDITIONAL",
        "reason": "detection verified, packaging toolchain unavailable"
      }
    ],
    "unverified_combinations": [
      "windows-x64 -> windows-arm64",
      "windows-x64 -> linux-x64",
      "windows-x64 -> linux-arm64",
      "windows-x64 -> macos-x64",
      "windows-x64 -> macos-arm64",
      "windows-x64 -> macos-universal"
    ]
  }
}
```
