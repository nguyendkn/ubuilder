//! `toolchain` subcommands.

use super::{fail, host, ok, target_required, Cli};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::project::FrameworkKind;
use crate::target::Target;
use crate::telemetry::Ui;
use crate::toolchain;
use clap::Subcommand;
use serde_json::{json, Value};

#[derive(Debug, Subcommand)]
pub enum ToolchainCommand {
    /// Probe every framework toolchain.
    List,
    /// Show one provider in detail.
    Inspect {
        /// rust, dotnet, electron or tauri.
        id: Option<String>,
    },
    /// Run the provisioning plan for a framework/target.
    Install {
        #[arg(long)]
        framework: Option<String>,
        #[arg(long)]
        target: Option<String>,
    },
    /// Verify that a framework can build a target.
    Verify {
        #[arg(long)]
        framework: Option<String>,
        #[arg(long)]
        target: Option<String>,
    },
    /// Remove installed Rust target support.
    Remove {
        #[arg(long)]
        target: Option<String>,
    },
}

pub fn run(cli: &Cli, command: &ToolchainCommand, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let project = super::discover(cli, None).ok();
    let state = toolchain::detect(&host, project.as_ref());
    let framework = |raw: &Option<String>| -> Result<FrameworkKind> {
        match raw {
            Some(value) => FrameworkKind::parse(value).ok_or_else(|| {
                UbuilderError::new(ErrorCode::Usage, format!("unknown framework '{value}'"))
                    .remediation("Use rust, dotnet, electron or tauri")
            }),
            None => Ok(project
                .as_ref()
                .map(|p| p.framework)
                .unwrap_or(FrameworkKind::Rust)),
        }
    };

    match command {
        ToolchainCommand::List => {
            if !ui.json() {
                for probe in &state.probes {
                    ui.info(format!(
                        "{}: {} {}",
                        probe.id,
                        if probe.available {
                            "available"
                        } else {
                            "missing"
                        },
                        probe.version.as_deref().unwrap_or("")
                    ));
                }
            }
            Ok(ok(
                ui,
                "toolchain list",
                serde_json::to_value(&state.probes).unwrap_or(Value::Null),
            ))
        }
        ToolchainCommand::Inspect { id } => {
            let probes: Vec<_> = match id {
                Some(id) => state
                    .probes
                    .iter()
                    .filter(|p| p.id == *id)
                    .cloned()
                    .collect(),
                None => state.probes.clone(),
            };
            if probes.is_empty() {
                let error = UbuilderError::new(
                    ErrorCode::Usage,
                    format!("unknown toolchain '{}'", id.clone().unwrap_or_default()),
                );
                return Ok(fail(ui, "toolchain inspect", &error, None));
            }
            if !ui.json() {
                for probe in &probes {
                    ui.info(format!("{} ({})", probe.id, probe.name));
                    ui.info(format!(
                        "  available={} version={}",
                        probe.available,
                        probe.version.as_deref().unwrap_or("-")
                    ));
                    if let Some(executable) = &probe.executable {
                        ui.info(format!("  executable={}", executable.display()));
                    }
                    for note in &probe.notes {
                        ui.info(format!("  note: {note}"));
                    }
                }
            }
            Ok(ok(
                ui,
                "toolchain inspect",
                serde_json::to_value(&probes).unwrap_or(Value::Null),
            ))
        }
        ToolchainCommand::Install {
            framework: raw,
            target,
        } => {
            let framework = match framework(raw) {
                Ok(framework) => framework,
                Err(error) => return Ok(fail(ui, "toolchain install", &error, None)),
            };
            let target = match target {
                Some(id) => Target::parse(id),
                None => Ok(crate::builder::resolve_targets(&[], false, &host)?.remove(0)),
            };
            let target = match target {
                Ok(target) => target,
                Err(error) => return Ok(fail(ui, "toolchain install", &error, None)),
            };
            let steps = toolchain::provision_plan(
                framework,
                &state.facts,
                Some(&target),
                project.as_ref().map(|p| p.root.as_path()),
            );
            if steps.is_empty() {
                if !ui.json() {
                    ui.success("no provisioning steps required");
                }
                return Ok(ok(
                    ui,
                    "toolchain install",
                    json!({ "steps": [], "target": target.id }),
                ));
            }
            let mut completed = Vec::new();
            for step in &steps {
                if !ui.json() {
                    ui.info(format!(
                        "{}: {}",
                        if cli.dry_run { "would run" } else { "running" },
                        step.command_line()
                    ));
                }
                if let Err(error) = toolchain::install(step, cli.dry_run) {
                    return Ok(fail(
                        ui,
                        "toolchain install",
                        &error,
                        Some(json!({ "completed": completed, "failed": step })),
                    ));
                }
                completed.push(serde_json::to_value(step).unwrap_or(Value::Null));
            }
            Ok(ok(
                ui,
                "toolchain install",
                json!({ "steps": completed, "target": target.id, "dry_run": cli.dry_run }),
            ))
        }
        ToolchainCommand::Verify {
            framework: raw,
            target,
        } => {
            let framework = match framework(raw) {
                Ok(framework) => framework,
                Err(error) => return Ok(fail(ui, "toolchain verify", &error, None)),
            };
            let target = match target {
                Some(id) => Target::parse(id),
                None => Ok(crate::builder::resolve_targets(&[], false, &host)?.remove(0)),
            };
            let target = match target {
                Ok(target) => target,
                Err(error) => return Ok(fail(ui, "toolchain verify", &error, None)),
            };
            let report = toolchain::verify(framework, &state.facts, &target);
            if !ui.json() {
                for check in &report.checks {
                    if check.ok {
                        ui.success(format!("{}: {}", check.check, check.detail));
                    } else {
                        ui.error(format!("{}: {}", check.check, check.detail));
                    }
                }
            }
            let code = ok(
                ui,
                "toolchain verify",
                serde_json::to_value(&report).unwrap_or(Value::Null),
            );
            if report.ok {
                Ok(code)
            } else {
                Ok(1)
            }
        }
        ToolchainCommand::Remove { target } => {
            let target = match target_required(target.clone()) {
                Ok(target) => target,
                Err(error) => return Ok(fail(ui, "toolchain remove", &error, None)),
            };
            let removed = match toolchain::remove(&state.facts, &target) {
                Ok(removed) => removed,
                Err(error) => return Ok(fail(ui, "toolchain remove", &error, None)),
            };
            if !ui.json() {
                if removed.is_empty() {
                    ui.info("no installed Rust targets matched");
                } else {
                    ui.success(format!("removed {}", removed.join(", ")));
                }
            }
            Ok(ok(
                ui,
                "toolchain remove",
                json!({ "removed": removed, "target": target.id }),
            ))
        }
    }
}
