# Risk register

Severity: P0 blocks production; P1 requires mitigation before a broad rollout;
P2 is a documented limitation.

| ID | Risk | Severity | Likelihood | Evidence / status | Mitigation |
| --- | --- | --- | --- | --- | --- |
| R-01 | No native Linux or macOS execution evidence | P0 for those platforms | Certain in this environment | Only Windows host; Linux target installed, macOS absent. | Linux/macOS CI added; CI run pending. Verdict is limited. |
| R-02 | Cross-linker unverified | P1 | High | No `cc`/cross linker validated for `x86_64-unknown-linux-gnu` on this host. | Treat cross builds as `UNVERIFIED`; use native hosts for release. |
| R-03 | No code signing or notarization | P1 | Certain | No certificates or Apple identity; final archive is `UNSIGNED`. | Release workflow exposes a signing hook; no signed artifact claimed. |
| R-04 | Dependency audit gap | P1 | Medium | `cargo-audit`, `cargo-deny` absent and no advisory DB queried; CI audit job has not run. | SBOM generated; run the audit gate in CI. |
| R-05 | Untrusted workflows are not sandboxed | P1 | Medium | Jobs run as the CLI user; no verified backend. `isolated` fails closed with exit 3. | Document trusted-only use; certify a backend per platform. |
| R-06 | Webhook transport has no TLS | P2 | Medium | Plaintext loopback listener; token required; in-memory replay cache. | Use a TLS reverse proxy; replay cache is not durable across restarts. |
| R-07 | Concurrency slot held for 60 s after crash | P2 | Low | Fenced lease with 15 s heartbeat; expired owner cannot renew or release. | Normal completion releases; wait 60 s or run recovery. |
| R-08 | Older binary opens newer schema | P2 | Low | Newer versions now reject with `SCHEMA_UNSUPPORTED` before DDL; migration backs up via `VACUUM INTO`. | Use a matching binary version; backups are automatic before migration. |
| R-09 | Cold planning probes many toolchains | P2 | High on Windows | Historical cold plan ~4.5 s; final warm plan 101-113 ms. | 30 s in-process facts cache; `UBUILDER_NO_FACTS_CACHE=1` bypass. |
| R-10 | Daemon serializes runs | P2 | Medium | One `pipeline run` at a time. | Adequate for local-first use; queueing is future work. |
| R-11 | Action catalog is built-in only | P2 | Medium | No remote marketplace. | Pinned built-in actions only. |
| R-12 | Windows process-tree kill uses `taskkill /T /F` | P2 | Low | Job Object termination not implemented. | `/T` kills descendants; verify with the timeout test. |
| R-13 | Secret visible in argv with `--value` | P2 | Medium | Deprecated flag still works and warns in human mode. | Prefer `--stdin`, file or prompt. |
| R-14 | Retention deletes run history irreversibly | P2 | Medium | No soft delete. | `--dry-run`, `--max-runs`, pinning; backup DB. |
