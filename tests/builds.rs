//! Real native build for the Rust fixture, exercising executor, cache, SQLite,
//! packaging, validation and manifest writing end to end.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

fn ubuilder() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ubuilder"))
}

#[test]
fn rust_fixture_builds_with_manifest_and_validated_artifact() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = repo.join("fixtures").join("rust-cli");
    let tmp = tempfile::tempdir().unwrap();
    let output_dir = tmp.path().join("out");
    let target = native_target();

    let output = ubuilder()
        .args([
            "--path",
            fixture.to_str().unwrap(),
            "build",
            "--target",
            target,
            "--profile",
            "debug",
            "--format",
            "zip",
            "--no-cache",
            "--output",
            output_dir.to_str().unwrap(),
            "--json",
        ])
        .env("UBUILDER_DATA_DIR", tmp.path().join("data"))
        .env("UBUILDER_CACHE_DIR", tmp.path().join("cache"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "build failed: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    let report = &json["data"]["reports"][0];
    assert_eq!(report["status"], "success");
    let artifacts = report["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 1);
    let artifact = PathBuf::from(artifacts[0]["path"].as_str().unwrap());
    assert!(artifact.is_file(), "missing {}", artifact.display());
    assert_eq!(artifacts[0]["format"], "portable-zip");
    assert_eq!(report["validations"][0]["status"], "passed");

    let manifest = PathBuf::from(report["manifest_path"].as_str().unwrap());
    let manifest: Value = serde_json::from_slice(&std::fs::read(manifest).unwrap()).unwrap();
    assert_eq!(manifest["status"], "success");
    assert_eq!(manifest["target"], target);

    let list = ubuilder()
        .args(["artifact", "list", "--json"])
        .env("UBUILDER_DATA_DIR", tmp.path().join("data"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(list.status.success());
    let records: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(records["data"][0]["validation"], "passed");
    assert_eq!(records["data"][0]["target"], target);
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
