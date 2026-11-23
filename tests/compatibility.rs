//! Capability honesty: every target gets an explicit level, unsupported
//! combinations name unsatisfied requirements instead of claiming success.

use serde_json::Value;
use std::process::Command;

const ALL: &str =
    "windows-x64,windows-arm64,linux-x64,linux-arm64,macos-x64,macos-arm64,macos-universal";
const LEVELS: [&str; 4] = ["SUPPORTED", "CONDITIONAL", "EXPERIMENTAL", "UNSUPPORTED"];

#[test]
fn capability_matrix_is_explicit_and_unsupported_has_reasons() {
    let output = Command::new(env!("CARGO_BIN_EXE_ubuilder"))
        .args(["doctor", "--target", ALL, "--json"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    let targets = json["data"]["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 7);

    for target in targets {
        let capability = &target["capability"];
        for dimension in ["compilation", "packaging", "signing", "native_testing"] {
            let level = capability[dimension].as_str().unwrap();
            assert!(
                LEVELS.contains(&level),
                "{} {dimension} has unexpected level {level}",
                target["target"]
            );
        }
        if capability["compilation"] == "UNSUPPORTED" {
            let unsatisfied = capability["requirements"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|requirement| requirement["satisfied"] == false)
                .count();
            assert!(
                unsatisfied > 0,
                "{} is UNSUPPORTED without naming a missing requirement",
                target["target"]
            );
        }
    }
}

#[test]
fn signing_is_never_claimed_without_an_available_tool() {
    let output = Command::new(env!("CARGO_BIN_EXE_ubuilder"))
        .args(["doctor", "--target", ALL, "--json"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    for target in json["data"]["targets"].as_array().unwrap() {
        let capability = &target["capability"];
        if capability["signing"] == "SUPPORTED" {
            let facts = &json["data"]["facts"];
            let available = match target["target"]
                .as_str()
                .unwrap()
                .split('-')
                .next()
                .unwrap()
            {
                "windows" => facts["signtool"] == true,
                "macos" => facts["codesign"] == true,
                _ => continue,
            };
            assert!(
                available,
                "{} claims signing SUPPORTED without its signing tool",
                target["target"]
            );
        }
    }
}
