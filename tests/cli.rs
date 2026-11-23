//! CLI contract tests: detection, JSON envelopes, exit codes, dry runs.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn ubuilder() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ubuilder"))
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> PathBuf {
    root().join("fixtures").join(name)
}

fn isolated(mut command: Command) -> Command {
    let tmp = std::env::temp_dir().join(format!("ubuilder-test-{}", std::process::id()));
    command
        .env("UBUILDER_DATA_DIR", tmp.join("data"))
        .env("UBUILDER_CACHE_DIR", tmp.join("cache"))
        .env("NO_COLOR", "1");
    command
}

fn parse_json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON document: {error}\nstdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn selected_framework(path: &Path) -> String {
    let output = isolated(ubuilder())
        .args(["--path", path.to_str().unwrap(), "inspect", "--json"])
        .output()
        .unwrap();
    let json = parse_json(&output);
    json["data"]["selected"]["framework"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn detects_each_supported_framework_fixture() {
    for (fixture_name, expected) in [
        ("rust-cli", "rust"),
        ("dotnet-console", "dotnet"),
        ("electron-app", "electron"),
        ("tauri-app", "tauri"),
    ] {
        assert_eq!(
            selected_framework(&fixture(fixture_name)),
            expected,
            "fixture {fixture_name}"
        );
    }
}

#[test]
fn inspect_json_is_a_single_envelope() {
    let output = isolated(ubuilder())
        .args([
            "--path",
            fixture("rust-cli").to_str().unwrap(),
            "inspect",
            "--json",
        ])
        .output()
        .unwrap();
    let json = parse_json(&output);
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "inspect");
    assert_eq!(json["data"]["selected"]["name"], "hello-rust");
    assert_eq!(json["data"]["selected"]["version"], "0.2.0");
}

#[test]
fn target_list_contains_all_seven_canonical_targets() {
    let output = isolated(ubuilder())
        .args(["target", "list", "--json"])
        .output()
        .unwrap();
    let json = parse_json(&output);
    let ids: Vec<&str> = json["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|target| target["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 7);
    for id in [
        "windows-x64",
        "windows-arm64",
        "linux-x64",
        "linux-arm64",
        "macos-x64",
        "macos-arm64",
        "macos-universal",
    ] {
        assert!(ids.contains(&id), "missing {id}");
    }
}

#[test]
fn unknown_target_is_a_usage_error() {
    let output = isolated(ubuilder())
        .args(["target", "inspect", "beos-x64", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "UNSUPPORTED_TARGET");
}

#[test]
fn dry_run_plans_without_touching_output() {
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("out");
    let output = isolated(ubuilder())
        .args([
            "--path",
            fixture("rust-cli").to_str().unwrap(),
            "--dry-run",
            "build",
            "--target",
            native_target(),
            "--output",
            output_dir.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    let json = parse_json(&output);
    let report = &json["data"]["reports"][0];
    assert_eq!(report["status"], "planned");
    assert!(!report["plan"]["steps"].as_array().unwrap().is_empty());
    assert!(!output_dir.exists(), "dry run wrote artifacts");
}

#[test]
fn unsupported_targets_are_rejected_explicitly() {
    // aarch64 windows needs the Rust target and an ARM64 linker; on an x64-only
    // host without those tools, capability must say Unsupported, not fabricate.
    if !cfg!(target_os = "windows") {
        return;
    }
    let output = isolated(ubuilder())
        .args([
            "--path",
            fixture("rust-cli").to_str().unwrap(),
            "--dry-run",
            "build",
            "--target",
            "windows-arm64",
            "--json",
        ])
        .output()
        .unwrap();
    if output.status.code() == Some(0) {
        // A developer machine with an ARM64 linker may legitimately support it.
        return;
    }
    assert_eq!(output.status.code(), Some(3));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["error"]["code"], "UNSUPPORTED_TARGET");
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("windows-arm64"));
}

fn native_target() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "windows-x64"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "windows-arm64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x64"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "linux-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "macos-x64"
    } else {
        "macos-arm64"
    }
}
