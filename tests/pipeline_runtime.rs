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
