# Risk register

Severity: P0 blocks production; P1 requires mitigation before a broad rollout;
P2 is a documented limitation.

| ID | Risk | Severity | Likelihood | Evidence / status | Mitigation |
| --- | --- | --- | --- | --- | --- |
| R-01 | No native Linux or macOS execution evidence | P0 for those platforms | Certain in this environment | Only Windows host; Linux target installed, macOS absent. | Linux/macOS CI added; CI run pending. Verdict is limited. |
| R-02 | Cross-linker unverified | P1 | High | No `cc`/cross linker validated for `x86_64-unknown-linux-gnu` on this host. | Treat cross builds as `UNVERIFIED`; use native hosts for release. |
| R-03 | No code signing or notarization | P1 | Certain | No certificates or Apple identity. | Release workflow exposes a signing hook; no signed artifact claimed. |
| R-04 | Dependency audit gap | P1 | Medium | `cargo-audit`, `cargo-deny` absent. | SBOM generated; add audit gate in CI when available. |
| R-05 | Untrusted workflows are not sandboxed | P1 | Medium | Jobs run as the CLI user; isolation is workspace-only. | Document trusted-only use; isolation provider is future work. |
| R-06 | Webhook has no TLS/replay protection | P2 | Medium | Loopback HTTP listener. | Reverse proxy TLS; optional bearer token. |
| R-07 | Concurrency slot held for 1 h after crash | P2 | Low | Lease is 3,600,000 ms with no heartbeat. | Normal completion releases; manual DB/lease recovery documented. |
| R-08 | No schema downgrade check | P2 | Low | Version row exists but readers do not reject newer versions. | Back up DB before downgrade; migration guide states it. |
| R-09 | Cold planning probes many toolchains | P2 | High on Windows | 48-job cold plan ~4.5 s, warm ~71 ms. | 30 s facts cache; `UBUILDER_NO_FACTS_CACHE=1` bypass. |
| R-10 | Daemon serializes runs | P2 | Medium | One `pipeline run` at a time. | Adequate for local-first use; queueing is future work. |
| R-11 | Action catalog is built-in only | P2 | Medium | No remote marketplace. | Pinned built-in actions only. |
| R-12 | Windows process-tree kill uses `taskkill /T /F` | P2 | Low | Job Object termination not implemented. | `/T` kills descendants; verify with the timeout test. |
| R-13 | Secret visible in argv with `--value` | P2 | Medium | CLI supports the flag. | Prefer stdin/file/prompt. |
| R-14 | Retention deletes run history irreversibly | P2 | Medium | No soft delete. | `--dry-run`, `--max-runs`, pinning; backup DB. |
