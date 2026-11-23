//! `build`, `package` and `release` commands.

use super::{build_options, fail, host, human_report, load_config, ok, parse_targets, Cli};
use crate::builder::{self, BuildOptions};
use crate::config::SigningMode;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::telemetry::Ui;
use clap::Args;
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Args)]
pub struct BuildArgs {
    #[command(flatten)]
    pub flags: super::BuildFlags,
}

#[derive(Debug, Args)]
pub struct ReleaseArgs {
    #[command(flatten)]
    pub flags: super::BuildFlags,
    /// Signing mode: none, adhoc, developer-id, authenticode.
    #[arg(long)]
    pub signing: Option<String>,
    /// Signing identity (certificate name or SHA-1 thumbprint).
    #[arg(long)]
    pub identity: Option<String>,
    /// Certificate file for Windows Authenticode.
    #[arg(long)]
    pub certificate: Option<PathBuf>,
    /// Environment variable holding the certificate password.
    #[arg(long)]
    pub password_env: Option<String>,
    /// RFC 3161 timestamp URL.
    #[arg(long)]
    pub timestamp_url: Option<String>,
    /// Submit and staple a macOS notarization ticket.
    #[arg(long)]
    pub notarize: bool,
    /// Permit unsigned artifacts when the tool is missing.
    #[arg(long)]
    pub allow_unsigned: bool,
}

pub async fn run_build(
    cli: &Cli,
    args: &BuildArgs,
    release: bool,
    ui: &Ui,
    cancel: &CancellationToken,
) -> Result<i32> {
    run(cli, &args.flags, None, release, ui, cancel).await
}

pub async fn run_release(
    cli: &Cli,
    args: &ReleaseArgs,
    ui: &Ui,
    cancel: &CancellationToken,
) -> Result<i32> {
    run(cli, &args.flags, Some(args), true, ui, cancel).await
}

async fn run(
    cli: &Cli,
    flags: &super::BuildFlags,
    release_args: Option<&ReleaseArgs>,
    release: bool,
    ui: &Ui,
    cancel: &CancellationToken,
) -> Result<i32> {
    let command = if release {
        "release"
    } else if matches!(&cli.command, super::Command::Package(_)) {
        "package"
    } else {
        "build"
    };
    let host = host()?;
    let project = match super::discover(cli, None) {
        Ok(project) => project,
        Err(error) => return Ok(fail(ui, command, &error, None)),
    };
    let mut config = match load_config(cli, &project, &host, flags) {
        Ok(config) => config,
        Err(error) => return Ok(fail(ui, command, &error, None)),
    };
    if let Some(args) = release_args {
        apply_release_args(&mut config, args)?;
    }
    let options: BuildOptions = match build_options(cli, &config, flags, release) {
        Ok(options) => options,
        Err(error) => return Ok(fail(ui, command, &error, None)),
    };
    let targets = match parse_targets(flags, &host) {
        Ok(targets) => targets,
        Err(error) => return Ok(fail(ui, command, &error, None)),
    };

    if !ui.json() {
        ui.info(format!(
            "{} {} for {} target(s)",
            if cli.dry_run { "planning" } else { "building" },
            project.name,
            targets.len()
        ));
    }
    let results =
        builder::build_targets(&project, &host, &config, &options, &targets, ui, cancel).await;

    let mut reports = Vec::new();
    let mut errors: Vec<UbuilderError> = Vec::new();
    let mut cancelled = false;
    for result in results {
        match result {
            Ok(report) => {
                if report.status == "success" || report.status == "planned" {
                    if !cli.dry_run {
                        human_report(ui, &report);
                    } else if !ui.json() {
                        ui.success(format!(
                            "{}: planned {} step(s)",
                            report.target,
                            report.plan.as_ref().map(|p| p.steps.len()).unwrap_or(0)
                        ));
                        for step in report.plan.iter().flat_map(|p| &p.steps) {
                            ui.info(format!("  {} {}", step.id, step.name));
                        }
                    }
                    reports.push(serde_json::to_value(&report).unwrap_or(Value::Null));
                } else {
                    human_report(ui, &report);
                    let message = report
                        .outcome
                        .as_ref()
                        .and_then(|o| o.failure.as_ref())
                        .map(|f| format!("{}: {}", report.target, f.message))
                        .unwrap_or_else(|| format!("{}: {}", report.target, report.status));
                    let code = if report.status == "cancelled" {
                        cancelled = true;
                        ErrorCode::Cancelled
                    } else {
                        ErrorCode::ExecutionFailed
                    };
                    reports.push(serde_json::to_value(&report).unwrap_or(Value::Null));
                    errors.push(
                        UbuilderError::new(code, message).details(json!({ "report": report })),
                    );
                }
            }
            Err(error) => {
                if !ui.json() {
                    ui.error(error.message.clone());
                }
                errors.push(error);
            }
        }
    }

    let data = json!({
        "project": project.name,
        "framework": project.framework.as_str(),
        "build_id": reports.first().and_then(|r| r.get("build_id")).cloned().unwrap_or(Value::Null),
        "reports": reports,
        "error_count": errors.len(),
    });
    if errors.is_empty() {
        let code = ok(ui, command, data);
        return Ok(code);
    }
    if cancelled {
        let code = fail(ui, command, &errors[0], Some(data));
        return Ok(code);
    }
    Ok(fail(ui, command, &errors[0], Some(data)))
}

fn apply_release_args(config: &mut crate::config::Config, args: &ReleaseArgs) -> Result<()> {
    if let Some(mode) = &args.signing {
        config.release.signing = match mode.to_ascii_lowercase().as_str() {
            "none" => SigningMode::None,
            "adhoc" | "ad-hoc" => SigningMode::Adhoc,
            "developer-id" | "developerid" => SigningMode::DeveloperId,
            "authenticode" => SigningMode::Authenticode,
            other => {
                return Err(UbuilderError::new(
                    ErrorCode::Usage,
                    format!("unknown signing mode '{other}'"),
                )
                .remediation("Use none, adhoc, developer-id or authenticode"))
            }
        };
    }
    if let Some(identity) = &args.identity {
        config.release.identity = Some(identity.clone());
    }
    if let Some(certificate) = &args.certificate {
        config.release.certificate = Some(certificate.clone());
    }
    if let Some(password_env) = &args.password_env {
        config.release.password_env = Some(password_env.clone());
    }
    if let Some(timestamp_url) = &args.timestamp_url {
        config.release.timestamp_url = Some(timestamp_url.clone());
    }
    if args.notarize {
        config.release.notarize = true;
    }
    if args.allow_unsigned {
        config.release.allow_unsigned = true;
    }
    Ok(())
}
