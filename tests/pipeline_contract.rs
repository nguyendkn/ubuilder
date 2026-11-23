//! Pipeline compile-surface contract: parsing, validation, planning and graphs.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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

    fn write_workflow(&self, source: &str) -> PathBuf {
        let dir = self.temp.path().join(".ubuilder").join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("build.yml");
        std::fs::write(&path, source).unwrap();
        path
    }

    fn run(&self, args: &[&str]) -> Output {
        ubuilder()
            .args(args)
            .env("UBUILDER_DATA_DIR", self.data_dir())
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    }

    fn run_json(&self, args: &[&str]) -> (Output, Value) {
        let output = self.run(args);
        let json: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout is not JSON: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output, json)
    }
}

fn fixture() -> PathBuf {
    root().join("fixtures").join("rust-cli")
}

fn workflow_args<'a>(project: &'a Path, workflow: &'a str, rest: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["--path", project.to_str().unwrap(), "pipeline"];
    args.extend(rest);
    args.push("--file");
    args.push(workflow);
    args
}

#[test]
fn validate_accepts_the_production_example() {
    let sandbox = Sandbox::new();
    let project = root();
    let workflow = project.join(".ubuilder/workflows/desktop-release.yml");
    let args = [
        "--path",
        project.to_str().unwrap(),
        "--json",
        "pipeline",
        "validate",
        "--file",
        workflow.to_str().unwrap(),
    ];
    let (output, json) = sandbox.run_json(&args);
    assert!(output.status.success(), "{json}");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "pipeline validate");
    assert_eq!(json["data"]["diagnostics"].as_array().unwrap().len(), 0);
}

#[test]
fn unknown_field_is_a_config_error_with_a_location() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.write_workflow(
        "version: 1\nname: broken\njobs:\n  a:\n    runs-on: auto\n    unexpected: true\n    steps:\n      - run: echo a\n",
    );
    let output = sandbox.run(&[
        "--path",
        fixture().to_str().unwrap(),
        "--json",
        "pipeline",
        "inspect",
        "--file",
        workflow.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(3));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    let message = json["error"]["message"].as_str().unwrap();
    assert!(message.contains("unexpected"), "{message}");
    assert!(message.contains("line"), "{message}");
}

#[test]
fn plan_expands_a_deterministic_matrix_and_waves() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.write_workflow(
        r#"version: 1
name: Matrix
jobs:
  build:
    strategy:
      matrix:
        target: [windows-x64, linux-x64]
    steps:
      - run: echo build
  package:
    needs: [build]
    steps:
      - run: echo package
"#,
    );
    let project = fixture();
    let args = workflow_args(&project, workflow.to_str().unwrap(), &["--json", "plan"]);
    let (output, json) = sandbox.run_json(&args);
    assert!(output.status.success(), "{json}");
    let jobs = json["data"]["jobs"].as_array().unwrap();
    let ids: Vec<&str> = jobs.iter().map(|job| job["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![
            "build[target=windows-x64]",
            "build[target=linux-x64]",
            "package"
        ]
    );
    let waves = json["data"]["waves"].as_array().unwrap();
    assert_eq!(waves.len(), 2);
    assert_eq!(waves[0].as_array().unwrap().len(), 2);
    assert_eq!(waves[1][0], "package");
    assert_eq!(jobs[2]["needs"].as_array().unwrap().len(), 2);
}

#[test]
fn graph_renders_dependencies_for_mermaid_dot_and_json() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.write_workflow(
        "version: 1\nname: Graph\njobs:\n  a:\n    steps:\n      - run: echo a\n  b:\n    needs: [a]\n    steps:\n      - run: echo b\n",
    );
    for (format, marker, node_ids) in [
        ("mermaid", "graph TD", true),
        ("dot", "digraph pipeline", true),
        ("json", "\"jobs\"", false),
    ] {
        let project = fixture();
        let args = workflow_args(
            &project,
            workflow.to_str().unwrap(),
            &["--json", "graph", "--format", format],
        );
        let (output, json) = sandbox.run_json(&args);
        assert!(output.status.success(), "{json}");
        let rendered = json["data"]["graph"].as_str().unwrap();
        assert!(rendered.contains(marker), "{format}: {rendered}");
        if node_ids {
            assert!(rendered.contains("job0"), "{format}: {rendered}");
        }
    }
}

#[test]
fn dependency_cycles_are_rejected_before_execution() {
    let sandbox = Sandbox::new();
    let workflow = sandbox.write_workflow(
        "version: 1\nname: Cycle\njobs:\n  a:\n    needs: [b]\n    steps:\n      - run: echo a\n  b:\n    needs: [a]\n    steps:\n      - run: echo b\n",
    );
    let project = fixture();
    let args = workflow_args(&project, workflow.to_str().unwrap(), &["--json", "plan"]);
    let (output, json) = sandbox.run_json(&args);
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .to_ascii_lowercase()
            .contains("cycle"),
        "{json}"
    );
}

#[test]
fn init_writes_a_valid_workflow_and_list_finds_it() {
    let sandbox = Sandbox::new();
    let project = sandbox.temp.path().join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();

    let init = sandbox.run(&[
        "--path",
        project.to_str().unwrap(),
        "--json",
        "pipeline",
        "init",
    ]);
    assert!(init.status.success());
    let json: Value = serde_json::from_slice(&init.stdout).unwrap();
    let workflow = PathBuf::from(json["data"]["path"].as_str().unwrap());
    assert!(workflow.is_file());

    let (list, list_json) = sandbox.run_json(&[
        "--path",
        project.to_str().unwrap(),
        "--json",
        "pipeline",
        "list",
    ]);
    assert!(list.status.success());
    assert_eq!(list_json["data"]["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(list_json["data"]["workflows"][0]["name"], "build");

    let plan = sandbox.run(&[
        "--path",
        project.to_str().unwrap(),
        "pipeline",
        "plan",
        "--file",
        workflow.to_str().unwrap(),
    ]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
}

#[test]
fn status_of_an_unknown_run_is_a_usage_error() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&[
        "--path",
        fixture().to_str().unwrap(),
        "--json",
        "pipeline",
        "status",
        "--run",
        "missing",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["error"]["code"], "USAGE");
}
