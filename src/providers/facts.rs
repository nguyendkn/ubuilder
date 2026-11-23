//! Host toolchain probing. All subprocess work lives here so the capability
//! resolver stays pure and testable.

use crate::capability::Facts;
use crate::host::Host;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const PROBE_COMMANDS: &[&str] = &[
    "rustc",
    "cargo",
    "rustup",
    "dotnet",
    "node",
    "npm",
    "npx",
    "pnpm",
    "yarn",
    "bun",
    "gcc",
    "cc",
    "clang",
    "clang++",
    "lld-link",
    "ld.lld",
    "zig",
    "cross",
    "cargo-xwin",
    "cargo-tauri",
    "tauri",
    "electron-builder",
    "electron",
    "xcodebuild",
    "codesign",
    "xcrun",
    "notarytool",
    "signtool",
    "makensis",
    "appimagetool",
    "rpmbuild",
    "dpkg-deb",
    "hdiutil",
    "lipo",
    "o64-clang",
    "o64-clang++",
    "x86_64-apple-darwin-clang",
    "aarch64-apple-darwin-clang",
    "ditto",
    "zip",
    "tar",
    "git",
];

const FACTS_TTL: Duration = Duration::from_secs(30);

type FactsCache = HashMap<String, (Instant, Facts)>;

fn facts_cache() -> &'static Mutex<FactsCache> {
    static CACHE: OnceLock<Mutex<FactsCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Probe the host toolchain, reusing a recent result for the same host and
/// project. Probing spawns dozens of subprocesses, which dominates planning.
pub fn collect(host: &Host, project_root: Option<&Path>) -> Facts {
    if std::env::var_os("UBUILDER_NO_FACTS_CACHE").is_some() {
        return collect_uncached(host, project_root);
    }
    let key = format!(
        "{}\n{}\n{}",
        host.triple,
        host.os_version.as_deref().unwrap_or(""),
        project_root
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    if let Ok(cache) = facts_cache().lock() {
        if let Some((at, facts)) = cache.get(&key) {
            if at.elapsed() < FACTS_TTL {
                return facts.clone();
            }
        }
    }
    let facts = collect_uncached(host, project_root);
    if let Ok(mut cache) = facts_cache().lock() {
        cache.insert(key, (Instant::now(), facts.clone()));
    }
    facts
}

/// Force a fresh toolchain probe, ignoring the short-lived process cache.
pub fn collect_fresh(host: &Host, project_root: Option<&Path>) -> Facts {
    collect_uncached(host, project_root)
}

fn collect_uncached(host: &Host, project_root: Option<&Path>) -> Facts {
    let mut facts = Facts::default();
    let mut commands = BTreeSet::new();
    for name in PROBE_COMMANDS {
        if which::which(name).is_ok() {
            commands.insert((*name).to_string());
        }
        if name.contains("target") || name.contains("apple") {
            continue;
        }
    }
    facts.commands = commands;

    facts.rustc_version = version("rustc", &["--version"], None);
    facts.cargo_version = version("cargo", &["--version"], None);
    if facts.has("rustup") {
        if let Some(output) = run_capture("rustup", &["target", "list", "--installed"], None) {
            facts.installed_rust_targets = output
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| line.split_whitespace().next().unwrap_or(line).to_string())
                .collect();
        }
        if !facts
            .installed_rust_targets
            .iter()
            .any(|t| t == &host.triple)
        {
            facts.installed_rust_targets.push(host.triple.clone());
        }
    } else if facts.has("cargo") && !host.triple.is_empty() {
        facts.installed_rust_targets.push(host.triple.clone());
    }

    facts.dotnet_sdk = version("dotnet", &["--version"], project_root);
    facts.node_version = version("node", &["--version"], project_root);
    facts.npm_version = version("npm", &["--version"], project_root);
    facts.zig_version = version("zig", &["version"], None);
    facts.xcode_version = version("xcodebuild", &["-version"], None)
        .and_then(|v| v.lines().next().map(str::to_string));

    facts.codesign = facts.has("codesign");
    facts.notarytool = facts.has("xcrun");
    facts.signtool = facts.has("signtool");
    facts.makensis = facts.has("makensis");
    facts.appimagetool = facts.has("appimagetool");
    facts.rpmbuild = facts.has("rpmbuild");
    facts.dpkg_deb = facts.has("dpkg-deb");
    facts.hdiutil = facts.has("hdiutil");
    facts.lipo = facts.has("lipo");
    facts.osxcross = facts.has("o64-clang")
        || facts.has("x86_64-apple-darwin-clang")
        || facts.has("aarch64-apple-darwin-clang")
        || std::env::var_os("OSXCROSS_ROOT").is_some();
    if facts.has("cross") {
        facts.cross_version = Some("installed".into());
    }
    if facts.has("cargo-xwin") {
        facts.xwin_version = Some("installed".into());
    }

    if let Some(root) = project_root {
        facts.electron_builder_version =
            local_tool_version(root, "electron-builder").or_else(|| {
                if facts.has("npx") {
                    version_in(
                        "npx",
                        &["--no-install", "electron-builder", "--version"],
                        Some(root),
                    )
                } else {
                    None
                }
            });
        facts.tauri_cli_version = local_tool_version(root, "tauri").or_else(|| {
            if facts.has("cargo-tauri") {
                version_in("cargo", &["tauri", "--version"], Some(root))
            } else {
                None
            }
        });
    }
    facts
}

fn version(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    version_in(program, args, cwd)
}

fn version_in(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let output = run_capture_in(program, args, cwd)?;
    output
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

pub fn local_tool_version(root: &Path, tool: &str) -> Option<String> {
    let bin = local_bin(root, tool)?;
    let output = run_capture_path(&bin, &["--version"], None)?;
    output
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
}

pub fn local_bin(root: &Path, tool: &str) -> Option<PathBuf> {
    let bin_dir = root.join("node_modules").join(".bin");
    [
        bin_dir.join(format!("{tool}.cmd")),
        bin_dir.join(tool),
        bin_dir.join(format!("{tool}.bat")),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

fn run_capture(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    run_capture_in(program, args, cwd)
}

fn run_capture_in(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let path = which::which(program).ok()?;
    run_capture_path(&path, args, cwd)
}

fn run_capture_path(program: &Path, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let mut command;
    #[cfg(windows)]
    {
        let extension = program
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if matches!(extension.as_deref(), Some("cmd") | Some("bat")) {
            command = Command::new("cmd");
            command.arg("/C").arg(program);
        } else {
            command = Command::new(program);
        }
    }
    #[cfg(not(windows))]
    {
        command = Command::new(program);
    }
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return (!stderr.is_empty()).then_some(stderr);
    }
    Some(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Architecture, HostPlatform};

    #[test]
    fn commands_set_respects_exe_suffix() {
        let facts = Facts::with_commands(&["cargo", "node"]);
        assert!(facts.has("cargo"));
        assert!(facts.has("cargo.exe"));
        assert!(!facts.has("rustc"));
    }

    #[test]
    fn probes_host_without_panicking() {
        let host = Host {
            platform: HostPlatform::Windows,
            arch: Architecture::X64,
            triple: "x86_64-pc-windows-msvc".into(),
            os_version: None,
        };
        let facts = collect(&host, None);
        assert!(facts
            .installed_rust_targets
            .iter()
            .any(|t| t == &host.triple));
    }
}
