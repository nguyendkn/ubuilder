//! Pipeline runtime behaviour against a real Rust fixture and local shells.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture() -> PathBuf {
    root().join("fixtures").join("rust-cli")
}

fn ubuilder() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ubuilder"))
}

struct Sandbox {
    temp: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            temp: tempfile::tempdir().unwrap(),
        }
    }

    fn data_dir(&self) -> PathBuf {
        self.temp.path().join("data")
    }

    fn workflow(&self, source: &str) -> PathBuf {
        let dir = self.temp.path().join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("build.yml");
        std::fs::write(&path, source).unwrap();
        path
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = ubuilder();
        command
            .args(args)
            .env("UBUILDER_DATA_DIR", self.data_dir())
            .env("UBUILDER_CACHE_DIR", self.temp.path().join("cache"))
            .env("NO_COLOR", "1");
        command
    }

    fn run(&self, workflow: &Path, extra: &[&str]) -> Output {
        let project = fixture();
        let mut args = vec![
            "--path",
            project.to_str().unwrap(),
            "--json",
            "pipeline",
            "run",
            "--file",
            workflow.to_str().unwrap(),
        ];
        args.extend(extra);
        self.command(&args).output().unwrap()
    }

    fn run_json(&self, workflow: &Path, extra: &[&str]) -> (Output, Value) {
        let output = self.run(workflow, extra);
        let json: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout is not JSON: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output, json)
    }

    fn status(&self, run_id: &str) -> Output {
        self.command(&[
            "--path",
            fixture().to_str().unwrap(),
            "--json",
            "pipeline",
            "status",
            "--run",
            run_id,
        ])
        .output()
        .unwrap()
    }
}

fn sleep_command(seconds: u32) -> String {
    if cfg!(windows) {
        format!("Start-Sleep -Seconds {seconds}")
    } else {
        format!("sleep {seconds}")
    }
}

fn matrix_fail_fast_command() -> &'static str {
    if cfg!(windows) {
        "if ($env:UBUILDER_JOB_ID -like '*one*') { exit 1 }\nexit 0"
    } else {
        "case \"$UBUILDER_JOB_ID\" in *one*) exit 1;; esac\nexit 0"
    }
}

fn cache_fill_command() -> &'static str {
    if cfg!(windows) {
        "New-Item -ItemType Directory -Force -Path cached | Out-Null\nSet-Content -Path cached/marker.txt -Value \"cached\""
    } else {
        "mkdir -p cached\nprintf cached > cached/marker.txt"
    }
}

fn job<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["data"]["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|job| job["id"] == id)
        .unwrap_or_else(|| panic!("missing job {id}: {report}"))
}

#[test]
fn two_jobs_transfer_artifacts_between_isolated_workspaces() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Transfer
jobs:
  build:
    strategy:
      matrix:
        target: [native]
    steps:
      - uses: builtin/checkout@v1
      - uses: builtin/setup-rust@v1
      - uses: builtin/build@v1
        with:
          target: ${{ matrix.target }}
          profile: release
      - uses: builtin/package@v1
        with:
          format: zip
      - uses: builtin/upload-artifact@v1
        with:
          name: desktop-${{ matrix.target }}
          path: dist
  release:
    needs: [build]
    steps:
      - uses: builtin/download-artifact@v1
        with:
          pattern: desktop-*
          path: dist
      - uses: builtin/checksum@v1
        with:
          path: dist
          output: dist/checksums.sha256
      - uses: builtin/upload-artifact@v1
        with:
          name: release
          path: dist/checksums.sha256
"#,
    );
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "transfer", "--no-cache"]);
    assert!(
        output.status.success(),
        "{json}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json["data"]["status"], "success");
    let build = job(&json, "build[target=native]");
    let release = job(&json, "release");
    assert_eq!(build["status"], "success");
    assert_eq!(release["status"], "success");
    assert_ne!(build["workspace"], release["workspace"]);
    let build_output = build["steps"][2]["outputs"]["path"].as_str().unwrap();
    assert_ne!(build_output, release["workspace"].as_str().unwrap());

    let artifacts = json["data"]["artifacts"].as_array().unwrap();
    let names: Vec<&str> = artifacts
        .iter()
        .map(|artifact| artifact["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "{json}");
    assert!(names.contains(&"desktop-native"));
    assert!(names.contains(&"release"));
    let checksum = roundtrip_status(&sandbox, "transfer");
    assert_eq!(checksum["data"]["run"]["status"], "success");
    assert_eq!(checksum["data"]["jobs"].as_array().unwrap().len(), 2);
}

fn roundtrip_status(sandbox: &Sandbox, run_id: &str) -> Value {
    let output = sandbox.status(run_id);
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn failure_blocks_dependents_and_reports_exit_code() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Failure
jobs:
  bad:
    steps:
      - run: exit 1
  after:
    needs: [bad]
    steps:
      - run: echo should-not-run
"#,
    );
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "failure"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json["ok"], false);
    assert_eq!(json["data"]["status"], "failure");
    assert_eq!(job(&json, "bad")["status"], "failure");
    assert_eq!(job(&json, "bad")["steps"][0]["exit_code"], 1);
    assert_eq!(job(&json, "after")["status"], "skipped");
    assert_eq!(json["error"]["code"], "EXECUTION_FAILED");
}

#[test]
fn job_condition_false_skips_without_failing_the_run() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Conditional
jobs:
  run-me:
    steps:
      - run: echo yes
  skip-me:
    if: false
    steps:
      - run: exit 1
"#,
    );
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "conditional"]);
    assert!(output.status.success(), "{json}");
    assert_eq!(job(&json, "run-me")["status"], "success");
    assert_eq!(job(&json, "skip-me")["status"], "skipped");
    assert_eq!(json["data"]["status"], "success");
}

#[test]
fn matrix_fail_fast_skips_remaining_siblings() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: FailFast
jobs:
  build:
    strategy:
      fail-fast: true
      max-parallel: 1
      matrix:
        name: [one, two]
    steps:
      - run: |
          {}
"#,
        matrix_fail_fast_command().replace('\n', "\n          ")
    ));
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "fail-fast"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(job(&json, "build[name=one]")["status"], "failure");
    assert_eq!(job(&json, "build[name=two]")["status"], "skipped");
}

#[test]
fn fail_fast_does_not_serialize_matrix_siblings() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: FailFastParallel
jobs:
  build:
    strategy:
      fail-fast: true
      matrix:
        name: [one, two]
    steps:
      - run: {}
"#,
        sleep_command(2)
    ));
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "fail-fast-parallel"]);
    assert!(output.status.success(), "{json}");
    let duration = json["data"]["duration_ms"].as_u64().unwrap();
    assert!(
        duration < 4000,
        "fail-fast serialized matrix siblings: {duration} ms"
    );
}

#[test]
fn job_filter_selects_only_requested_jobs() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Filter
jobs:
  alpha:
    steps:
      - run: echo alpha
  beta:
    steps:
      - run: echo beta
"#,
    );
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "filtered", "--job", "beta"]);
    assert!(output.status.success(), "{json}");
    let jobs = json["data"]["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["id"], "beta");
}

#[test]
fn independent_jobs_run_in_parallel() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Parallel
jobs:
  left:
    steps:
      - run: {}
  right:
    steps:
      - run: {}
"#,
        sleep_command(2),
        sleep_command(2)
    ));
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "parallel"]);
    assert!(output.status.success(), "{json}");
    let duration = json["data"]["duration_ms"].as_u64().unwrap();
    assert!(duration < 4000, "jobs ran serially: {duration} ms");
}

#[test]
fn step_timeout_fails_the_job() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Timeout
jobs:
  slow:
    steps:
      - run: {}
        timeout: 1s
"#,
        sleep_command(10)
    ));
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "timeout"]);
    assert_eq!(output.status.code(), Some(1));
    let step = &job(&json, "slow")["steps"][0];
    assert!(
        step["duration_ms"].as_u64().unwrap() < 8000,
        "timeout was not enforced: {json}"
    );
    assert_eq!(step["status"], "failure");
    assert!(
        step["error"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("timeout"),
        "{json}"
    );
}

#[test]
fn cancel_command_stops_a_running_workflow() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Cancel
jobs:
  slow:
    steps:
      - run: {}
"#,
        sleep_command(30)
    ));
    let child = sandbox
        .command(&[
            "--path",
            fixture().to_str().unwrap(),
            "--json",
            "pipeline",
            "run",
            "--file",
            workflow.to_str().unwrap(),
            "--run-id",
            "cancel-me",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = sandbox.status("cancel-me");
        if status.status.success() {
            break;
        }
        assert!(Instant::now() < deadline, "run never started");
        std::thread::sleep(Duration::from_millis(100));
    }

    let cancel = sandbox
        .command(&[
            "--path",
            fixture().to_str().unwrap(),
            "--json",
            "pipeline",
            "cancel",
            "cancel-me",
        ])
        .output()
        .unwrap();
    assert!(
        cancel.status.success(),
        "cancel failed: {}",
        String::from_utf8_lossy(&cancel.stderr)
    );

    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["data"]["status"], "cancelled");
    assert_eq!(json["error"]["code"], "CANCELLED");
}

#[test]
fn cache_action_misses_then_hits_across_runs() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Cache
jobs:
  restore:
    steps:
      - id: cache
        uses: builtin/cache@v1
        with:
          key: deps-v1
          path: cached
      - run: |
          {}
"#,
        cache_fill_command().replace('\n', "\n          ")
    ));
    let (first, first_json) = sandbox.run_json(&workflow, &["--run-id", "cache-1"]);
    assert!(first.status.success(), "{first_json}");
    assert_eq!(
        job(&first_json, "restore")["steps"][0]["outputs"]["cache-hit"],
        "false"
    );

    let (second, second_json) = sandbox.run_json(&workflow, &["--run-id", "cache-2"]);
    assert!(second.status.success(), "{second_json}");
    assert_eq!(
        job(&second_json, "restore")["steps"][0]["outputs"]["cache-hit"],
        "true"
    );
}

#[test]
fn retry_policy_reruns_a_failed_job() {
    let sandbox = Sandbox::new();
    let marker = sandbox.temp.path().join("retry-marker");
    let command = if cfg!(windows) {
        format!(
            "if (Test-Path '{}') {{ exit 0 }}; New-Item -ItemType File -Path '{}' | Out-Null; exit 1",
            marker.display(),
            marker.display()
        )
    } else {
        format!(
            "if [ -f '{}' ]; then exit 0; fi; touch '{}'; exit 1",
            marker.display(),
            marker.display()
        )
    };
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Retry
jobs:
  flaky:
    retry:
      max-attempts: 2
      delay: 10ms
    steps:
      - run: {}
"#,
        command
    ));
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "retry"]);
    assert!(output.status.success(), "{json}");
    assert_eq!(job(&json, "flaky")["status"], "success");
    assert!(marker.is_file(), "retry never ran the command twice");
}

#[test]
fn continue_on_error_keeps_the_job_running() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Continue
jobs:
  tolerant:
    steps:
      - run: exit 1
        continue-on-error: true
      - run: echo after
"#,
    );
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "continue"]);
    assert!(output.status.success(), "{json}");
    let steps = job(&json, "tolerant")["steps"].as_array().unwrap();
    assert_eq!(steps[0]["status"], "failure");
    assert_eq!(steps[0]["conclusion"], "success");
    assert_eq!(steps[1]["status"], "success");
}

#[test]
fn job_outputs_flow_through_the_needs_context() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Outputs
jobs:
  produce:
    outputs:
      hit: ${{{{ steps.cache.outputs.cache-hit }}}}
    steps:
      - id: cache
        uses: builtin/cache@v1
        with:
          key: outputs-v1
          path: cached
      - run: |
          {}
  consume:
    needs: [produce]
    if: ${{{{ needs.produce.outputs.hit == 'false' }}}}
    steps:
      - run: echo "hit=${{{{ needs.produce.outputs.hit }}}}"
"#,
        cache_fill_command().replace('\n', "\n          ")
    ));
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "outputs"]);
    assert!(output.status.success(), "{json}");
    assert_eq!(job(&json, "produce")["outputs"]["hit"], "false");
    let consume = job(&json, "consume");
    assert_eq!(consume["status"], "success");
    assert!(
        consume["steps"][0]["stdout_tail"]
            .as_str()
            .unwrap()
            .contains("hit=false"),
        "{json}"
    );
}

#[test]
fn recover_marks_an_interrupted_run_as_failed() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Recover
jobs:
  slow:
    steps:
      - run: {}
"#,
        sleep_command(30)
    ));
    let mut child = sandbox
        .command(&[
            "--path",
            fixture().to_str().unwrap(),
            "--json",
            "pipeline",
            "run",
            "--file",
            workflow.to_str().unwrap(),
            "--run-id",
            "interrupted",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = sandbox.status("interrupted");
        if status.status.success() {
            break;
        }
        assert!(Instant::now() < deadline, "run never started");
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill().unwrap();
    child.wait().unwrap();

    let recover = sandbox
        .command(&[
            "--path",
            fixture().to_str().unwrap(),
            "--json",
            "pipeline",
            "recover",
            "--stale-secs",
            "0",
        ])
        .output()
        .unwrap();
    assert!(
        recover.status.success(),
        "recover failed: {}",
        String::from_utf8_lossy(&recover.stderr)
    );
    let status = roundtrip_status(&sandbox, "interrupted");
    assert_eq!(status["data"]["run"]["status"], "failed");
}

#[test]
fn secrets_are_injected_and_redacted_everywhere() {
    let sandbox = Sandbox::new();
    let secret = "s3cr3t-value-42";
    let name = "UBUILDER_TEST_ONLY_TOKEN";
    let set = sandbox
        .command(&["--json", "secret", "set", name, "--value", secret])
        .output()
        .unwrap();
    assert!(
        set.status.success(),
        "secret set failed: {}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert!(!String::from_utf8_lossy(&set.stdout).contains(secret));

    let command = if cfg!(windows) {
        "if ($env:TOKEN -ne 's3cr3t-value-42') { exit 1 }\nWrite-Output $env:TOKEN".to_string()
    } else {
        "[ \"$TOKEN\" = \"s3cr3t-value-42\" ] || exit 1\nprintf '%s\\n' \"$TOKEN\"".to_string()
    };
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Secrets
jobs:
  use:
    outputs:
      token: ${{{{ env.TOKEN }}}}
    env:
      TOKEN: ${{{{ secrets.{} }}}}
    steps:
      - run: |
          {}
"#,
        name,
        command.replace('\n', "\n          ")
    ));
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "secrets"]);
    assert!(
        output.status.success(),
        "{json}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains(secret),
        "secret leaked in run JSON: {stdout}"
    );
    let report = job(&json, "use");
    assert_eq!(report["outputs"]["token"], "***");
    assert!(report["steps"][0]["stdout_tail"]
        .as_str()
        .unwrap()
        .contains("***"));

    let status = sandbox.status("secrets");
    assert!(status.status.success());
    let status_text = String::from_utf8_lossy(&status.stdout);
    assert!(
        !status_text.contains(secret),
        "secret leaked in status: {status_text}"
    );
    let history = sandbox
        .command(&[
            "--path",
            fixture().to_str().unwrap(),
            "--json",
            "pipeline",
            "history",
        ])
        .output()
        .unwrap();
    assert!(history.status.success());
    assert!(!String::from_utf8_lossy(&history.stdout).contains(secret));
}

#[test]
fn missing_secret_fails_closed_before_running_jobs() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Missing secret
jobs:
  use:
    env:
      TOKEN: ${{ secrets.DEFINITELY_MISSING_SECRET }}
    steps:
      - run: echo should-not-run
"#,
    );
    let output = sandbox.run(&workflow, &["--run-id", "missing-secret"]);
    assert!(!output.status.success());
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("DEFINITELY_MISSING_SECRET"));
}

#[test]
fn cache_survives_unrelated_workflow_edits() {
    let sandbox = Sandbox::new();
    let make = |extra: &str| {
        format!(
            r#"version: 1
name: Cache stability
jobs:
  restore:
    steps:
      - id: cache
        uses: builtin/cache@v1
        with:
          key: stable-v1
          path: cached
      - run: |
          {}
{extra}"#,
            cache_fill_command().replace('\n', "\n          ")
        )
    };
    let first = sandbox.workflow(&make(""));
    let (output, json) = sandbox.run_json(&first, &["--run-id", "stable-1"]);
    assert!(output.status.success(), "{json}");

    let changed = "  unrelated:\n    steps:\n      - run: echo changed\n".to_string();
    let second = sandbox.workflow(&make(&changed));
    let (output, json) = sandbox.run_json(&second, &["--run-id", "stable-2"]);
    assert!(output.status.success(), "{json}");
    assert_eq!(
        job(&json, "restore")["steps"][0]["outputs"]["cache-hit"],
        "true"
    );
}

#[test]
fn secret_cli_never_prints_values() {
    let sandbox = Sandbox::new();
    let secret = "cli-secret-99";
    let set = sandbox
        .command(&["--json", "secret", "set", "CLI_TOKEN", "--value", secret])
        .output()
        .unwrap();
    assert!(set.status.success());
    let text = String::from_utf8_lossy(&set.stdout);
    assert!(!text.contains(secret), "{text}");
    assert!(text.contains("file"));

    let list = sandbox
        .command(&["--json", "secret", "list"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let list_text = String::from_utf8_lossy(&list.stdout);
    assert!(!list_text.contains(secret), "{list_text}");
    let list_json: Value = serde_json::from_slice(&list.stdout).unwrap();
    let entries = list_json["data"]["secrets"].as_array().unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry["name"] == "CLI_TOKEN")
        .unwrap();
    assert_eq!(entry["length"], secret.chars().count());

    let inspect = sandbox
        .command(&["--json", "secret", "inspect", "CLI_TOKEN"])
        .output()
        .unwrap();
    assert!(inspect.status.success());
    assert!(!String::from_utf8_lossy(&inspect.stdout).contains(secret));
}

#[test]
fn maintenance_prune_keeps_newest_runs_and_protects_active() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Retention
jobs:
  ok:
    steps:
      - run: echo ok
"#,
    );
    for run in ["retain-1", "retain-2", "retain-3"] {
        let (output, json) = sandbox.run_json(&workflow, &["--run-id", run]);
        assert!(output.status.success(), "{json}");
        std::thread::sleep(Duration::from_millis(5));
    }
    let prune = sandbox
        .command(&[
            "--json",
            "maintenance",
            "prune",
            "--max-runs",
            "1",
            "--max-age-days",
            "3650",
            "--max-size-mb",
            "4096",
        ])
        .output()
        .unwrap();
    assert!(
        prune.status.success(),
        "{}",
        String::from_utf8_lossy(&prune.stderr)
    );
    let json: Value = serde_json::from_slice(&prune.stdout).unwrap();
    assert_eq!(json["data"]["deleted_count"], 2, "{json}");

    let history = sandbox
        .command(&["--json", "pipeline", "history"])
        .output()
        .unwrap();
    assert!(history.status.success());
    let history: Value = serde_json::from_slice(&history.stdout).unwrap();
    let runs = history["data"]["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{history}");
    assert_eq!(runs[0]["id"], "retain-3");

    let stats = sandbox
        .command(&["--json", "maintenance", "stats"])
        .output()
        .unwrap();
    assert!(stats.status.success());
    let stats: Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(stats["data"]["runs"], 1, "{stats}");
}

#[test]
fn concurrency_group_serializes_jobs_without_deadlock() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(&format!(
        r#"version: 1
name: Concurrency
jobs:
  first:
    concurrency:
      group: exclusive-test
      max: 1
    steps:
      - run: {}
  second:
    concurrency:
      group: exclusive-test
      max: 1
    steps:
      - run: {}
"#,
        sleep_command(2).replace('\n', "\n          "),
        sleep_command(2).replace('\n', "\n          ")
    ));
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "concurrency"]);
    assert!(
        output.status.success(),
        "{json}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let first = job(&json, "first")["steps"][0]["duration_ms"]
        .as_u64()
        .unwrap();
    let second = job(&json, "second")["steps"][0]["duration_ms"]
        .as_u64()
        .unwrap();
    let total = json["data"]["duration_ms"].as_u64().unwrap();
    assert!(first < 3_500 && second < 3_500, "step timing wrong: {json}");
    assert!(
        total >= 3_800,
        "concurrency group did not serialize: {json}"
    );
}

#[test]
fn action_steps_capture_message_output() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Action output
jobs:
  inspect:
    steps:
      - id: checkout
        uses: builtin/checkout@v1
"#,
    );
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "action-output"]);
    assert!(
        output.status.success(),
        "{json}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let step = &job(&json, "inspect")["steps"][0];
    assert!(
        !step["stdout_tail"].as_str().unwrap_or_default().is_empty(),
        "action step did not capture output: {json}"
    );
    assert!(step["log_path"].as_str().is_some(), "{json}");
}

#[test]
fn daemon_once_runs_the_workflow_and_records_state() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Daemon
jobs:
  ok:
    steps:
      - run: echo daemon
"#,
    );
    let output = sandbox
        .command(&[
            "--path",
            fixture().to_str().unwrap(),
            "--json",
            "daemon",
            "run",
            "--file",
            workflow.to_str().unwrap(),
            "--once",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let state: Value = serde_json::from_slice(
        &std::fs::read(sandbox.data_dir().join("daemon/state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["total_runs"], 1, "{state}");
    assert_eq!(state["last_status"], "success", "{state}");
    assert!(state["last_digest"].as_str().is_some(), "{state}");
}

#[test]
fn maintenance_pin_protects_and_gc_removes_unreferenced_objects() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.workflow(
        r#"version: 1
name: Pin
jobs:
  build:
    steps:
      - uses: builtin/checkout@v1
      - uses: builtin/setup-rust@v1
      - uses: builtin/build@v1
        with:
          target: native
          profile: release
      - uses: builtin/package@v1
        with:
          format: zip
      - uses: builtin/upload-artifact@v1
        with:
          name: desktop
          path: dist
"#,
    );
    let (output, json) = sandbox.run_json(&workflow, &["--run-id", "pinned"]);
    assert!(output.status.success(), "{json}");
    let artifacts = json["data"]["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 1, "{json}");
    let sha = artifacts[0]["sha256"].as_str().unwrap().to_string();

    let pin = sandbox
        .command(&["--json", "maintenance", "pin", &sha, "--name", "release"])
        .output()
        .unwrap();
    assert!(
        pin.status.success(),
        "{}",
        String::from_utf8_lossy(&pin.stderr)
    );

    let prune = |extra: &[&str]| {
        let mut args = vec!["--json", "maintenance", "prune", "--max-runs", "0"];
        args.extend(extra);
        sandbox.command(&args).output().unwrap()
    };
    let kept = prune(&[]);
    assert!(kept.status.success());
    let kept: Value = serde_json::from_slice(&kept.stdout).unwrap();
    assert_eq!(kept["data"]["deleted_count"], 0, "{kept}");

    let unpin = sandbox
        .command(&["--json", "maintenance", "unpin", &sha])
        .output()
        .unwrap();
    assert!(unpin.status.success());
    let removed = prune(&[]);
    assert!(removed.status.success());
    let removed: Value = serde_json::from_slice(&removed.stdout).unwrap();
    assert_eq!(removed["data"]["deleted_count"], 1, "{removed}");

    let gc = sandbox
        .command(&["--json", "maintenance", "gc"])
        .output()
        .unwrap();
    assert!(
        gc.status.success(),
        "{}",
        String::from_utf8_lossy(&gc.stderr)
    );
    let gc: Value = serde_json::from_slice(&gc.stdout).unwrap();
    assert_eq!(gc["data"]["removed_count"], 1, "{gc}");
    let stats = sandbox
        .command(&["--json", "maintenance", "stats"])
        .output()
        .unwrap();
    let stats: Value = serde_json::from_slice(&stats.stdout).unwrap();
    assert_eq!(stats["data"]["artifact_objects_bytes"], 0, "{stats}");
    assert_eq!(stats["data"]["pinned_artifacts"], 0, "{stats}");
}
