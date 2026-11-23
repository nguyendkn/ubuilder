//! Toolchain state, verification and explicit provisioning.

use crate::cache::toolchain_fingerprint;
use crate::capability::Facts;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::project::{FrameworkKind, Project};
use crate::providers::{self, ProbeContext, ProvisionStep, ToolchainProbe};
use crate::target::Target;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct ToolchainState {
    pub host: Host,
    pub facts: Facts,
    pub probes: Vec<ToolchainProbe>,
    pub fingerprint: String,
}

pub fn detect(host: &Host, project: Option<&Project>) -> ToolchainState {
    ToolchainState::detect(host, project)
}

impl ToolchainState {
    pub fn detect(host: &Host, project: Option<&Project>) -> Self {
        let project_root = project.map(|p| p.root.as_path());
        let ctx = ProbeContext {
            host,
            project,
            project_root,
        };
        let facts = providers::collect_facts(&ctx);
        let probes = providers::probe_all(&ctx, &facts);
        let fingerprint = toolchain_fingerprint(&facts);
        Self {
            host: host.clone(),
            facts,
            probes,
            fingerprint,
        }
    }

    pub fn probe(&self, id: &str) -> Option<&ToolchainProbe> {
        self.probes.iter().find(|p| p.id == id)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub check: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub framework: FrameworkKind,
    pub target: String,
    pub checks: Vec<CheckResult>,
    pub ok: bool,
}

impl VerifyReport {
    fn conclude(mut self) -> Self {
        self.ok = self.checks.iter().all(|c| c.ok);
        self
    }
}

/// Check the concrete requirements for a framework/target combination.
pub fn verify(framework: FrameworkKind, facts: &Facts, target: &Target) -> VerifyReport {
    let provider = providers::provider_for(framework);
    let mut checks = Vec::new();
    let host = Host::detect().unwrap_or_else(|_| Host {
        platform: if cfg!(target_os = "macos") {
            crate::host::HostPlatform::Macos
        } else if cfg!(target_os = "windows") {
            crate::host::HostPlatform::Windows
        } else {
            crate::host::HostPlatform::Linux
        },
        arch: if cfg!(target_arch = "aarch64") {
            crate::host::Architecture::Arm64
        } else {
            crate::host::Architecture::X64
        },
        triple: String::new(),
        os_version: None,
    });
    let probe = provider.probe(
        &ProbeContext {
            host: &host,
            project: None,
            project_root: None,
        },
        facts,
    );
    checks.push(CheckResult {
        check: format!("{}-available", provider.id()),
        ok: probe.available,
        detail: if probe.available {
            probe.version.clone().unwrap_or_else(|| "available".into())
        } else {
            probe.notes.join("; ")
        },
    });
    if matches!(framework, FrameworkKind::Rust | FrameworkKind::Tauri) {
        for triple in target.component_triples() {
            let installed = facts.rust_target(&triple);
            checks.push(CheckResult {
                check: format!("rust-target-{triple}"),
                ok: installed,
                detail: if installed {
                    "installed".into()
                } else {
                    format!("run: rustup target add {triple}")
                },
            });
        }
    }
    if framework == FrameworkKind::Dotnet {
        checks.push(CheckResult {
            check: "dotnet-sdk".into(),
            ok: facts.dotnet_sdk.is_some(),
            detail: facts
                .dotnet_sdk
                .clone()
                .unwrap_or_else(|| "dotnet not found".into()),
        });
    }
    if framework == FrameworkKind::Electron {
        checks.push(CheckResult {
            check: "electron-builder".into(),
            ok: facts.electron_builder_version.is_some(),
            detail: facts
                .electron_builder_version
                .clone()
                .unwrap_or_else(|| "not detected".into()),
        });
    }
    VerifyReport {
        framework,
        target: target.id.clone(),
        checks,
        ok: false,
    }
    .conclude()
}

pub fn provision_plan(
    framework: FrameworkKind,
    facts: &Facts,
    target: Option<&Target>,
    project_root: Option<&Path>,
) -> Vec<ProvisionStep> {
    providers::provider_for(framework).provision_plan(facts, target, project_root)
}

/// Execute one provisioning command. The caller has already obtained consent by
/// invoking `toolchain install`.
pub fn install(step: &ProvisionStep, dry_run: bool) -> Result<()> {
    if dry_run {
        println!("would run: {}", step.command_line());
        return Ok(());
    }
    let program = which::which(&step.program).map_err(|_| {
        UbuilderError::new(
            ErrorCode::ToolchainMissing,
            format!("cannot install: {} not found", step.program),
        )
    })?;
    let output = command_for(&program, &step.args)
        .output()
        .map_err(|e| UbuilderError::io(format!("cannot run {}", program.display()), e))?;
    if !output.status.success() {
        return Err(UbuilderError::new(
            ErrorCode::ExecutionFailed,
            format!(
                "{} failed: {}",
                step.command_line(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(())
}

pub fn remove(facts: &Facts, target: &Target) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    for triple in target.component_triples() {
        if !facts.rust_target(&triple) {
            continue;
        }
        let output = Command::new("rustup")
            .args(["target", "remove", &triple])
            .output()
            .map_err(|e| UbuilderError::io("cannot run rustup", e))?;
        if !output.status.success() {
            return Err(UbuilderError::new(
                ErrorCode::ExecutionFailed,
                format!(
                    "rustup target remove {triple} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        removed.push(triple);
    }
    Ok(removed)
}

pub fn executable_path(program: &str) -> Option<PathBuf> {
    which::which(program).ok()
}

fn command_for(program: &Path, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let extension = program
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if matches!(extension.as_deref(), Some("cmd") | Some("bat")) {
            let mut command = Command::new("cmd");
            command.arg("/C").arg(program).args(args);
            return command;
        }
    }
    let mut command = Command::new(program);
    command.args(args);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_rust_reports_missing_target() {
        let facts = Facts::with_commands(&["cargo"]);
        let target = Target::parse("linux-arm64").unwrap();
        let report = verify(FrameworkKind::Rust, &facts, &target);
        assert!(!report.ok);
        assert!(report
            .checks
            .iter()
            .any(|c| c.check.contains("rust-target")));
    }

    #[test]
    fn dry_run_install_does_not_execute() {
        let step = ProvisionStep::new("x", "nothing", "definitely-not-a-program", &[]);
        install(&step, true).unwrap();
    }
}
