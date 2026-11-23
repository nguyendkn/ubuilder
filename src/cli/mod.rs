//! Command-line surface. Parses arguments, loads configuration and delegates
//! real work to the library modules.

mod build_cmd;
mod maintenance;
mod pipeline_cmd;
mod toolchains;

use crate::builder::{self, BuildOptions, TargetBuildReport};
use crate::config::{Config, Overrides};
use crate::detect;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::project::{FrameworkKind, Project};
use crate::target::{PackagingFormat, Target};
use crate::telemetry::Ui;
use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(
    name = "ubuilder",
    version,
    about = "Universal Desktop Builder: cross-platform builds with honest capabilities",
    propagate_version = true
)]
pub struct Cli {
    /// Project directory to operate on.
    #[arg(long, global = true, default_value = ".")]
    pub path: PathBuf,
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,
    /// Verbose diagnostics.
    #[arg(long, short = 'v', global = true)]
    pub verbose: bool,
    /// Only errors and final results.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,
    /// Disable ANSI colours.
    #[arg(long, global = true)]
    pub no_color: bool,
    /// Plan without executing.
    #[arg(long, global = true)]
    pub dry_run: bool,
    /// Explicit configuration file.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write a starting ubuilder.toml.
    Init(maintenance::InitArgs),
    /// Detect the project and list candidates.
    Inspect,
    /// Diagnose host toolchains and target capabilities.
    Doctor(maintenance::DoctorArgs),
    /// Build one or more targets.
    Build(build_cmd::BuildArgs),
    /// Build and package.
    Package(build_cmd::BuildArgs),
    /// Build, sign and package for distribution.
    Release(build_cmd::ReleaseArgs),
    /// Remove local build scratch space.
    Clean(maintenance::CleanArgs),
    /// Inspect or prune the build cache.
    Cache {
        #[command(subcommand)]
        command: maintenance::CacheCommand,
    },
    /// Probe, verify and provision toolchains.
    Toolchain {
        #[command(subcommand)]
        command: toolchains::ToolchainCommand,
    },
    /// List and inspect canonical build targets.
    Target {
        #[command(subcommand)]
        command: maintenance::TargetCommand,
    },
    /// Inspect recorded artifacts.
    Artifact {
        #[command(subcommand)]
        command: maintenance::ArtifactCommand,
    },
    /// Declarative workflow pipeline.
    Pipeline {
        #[command(subcommand)]
        command: pipeline_cmd::PipelineCommand,
    },
}

#[derive(Debug, Clone, Args, Default)]
pub struct BuildFlags {
    /// Canonical target id (repeatable or comma separated).
    #[arg(long, value_delimiter = ',')]
    pub target: Vec<String>,
    /// Build every supported target.
    #[arg(long)]
    pub all: bool,
    /// Maximum concurrent steps.
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Cargo/build profile.
    #[arg(long)]
    pub profile: Option<String>,
    /// Artifact output directory.
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Avoid network access where the toolchain allows it.
    #[arg(long)]
    pub offline: bool,
    /// Disable the action cache.
    #[arg(long)]
    pub no_cache: bool,
    /// Continue after a target fails.
    #[arg(long)]
    pub continue_on_error: bool,
    /// Packaging format, repeatable.
    #[arg(long = "format")]
    pub formats: Vec<String>,
}

pub async fn run(cli: Cli) -> Result<i32> {
    let cancel = CancellationToken::new();
    let signal_token = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_token.cancel();
        }
    });

    let ui = Ui::new(cli.json, cli.verbose, cli.quiet);
    match &cli.command {
        Command::Init(args) => maintenance::init(&cli, args),
        Command::Inspect => maintenance::inspect(&cli, &ui),
        Command::Doctor(args) => maintenance::doctor(&cli, args, &ui),
        Command::Build(args) | Command::Package(args) => {
            build_cmd::run_build(&cli, args, false, &ui, &cancel).await
        }
        Command::Release(args) => build_cmd::run_release(&cli, args, &ui, &cancel).await,
        Command::Clean(args) => maintenance::clean(&cli, args),
        Command::Cache { command } => maintenance::cache(&cli, command, &ui),
        Command::Toolchain { command } => toolchains::run(&cli, command, &ui),
        Command::Target { command } => maintenance::target(&cli, command, &ui),
        Command::Artifact { command } => maintenance::artifact(&cli, command, &ui),
        Command::Pipeline { command } => pipeline_cmd::run(&cli, command, &ui, &cancel).await,
    }
}

fn host() -> Result<Host> {
    Host::detect()
}

fn discover(cli: &Cli, framework: Option<FrameworkKind>) -> Result<Project> {
    detect::discover(&cli.path, None, framework)
}

fn load_config(cli: &Cli, project: &Project, host: &Host, flags: &BuildFlags) -> Result<Config> {
    let overrides = Overrides {
        profile: flags.profile.clone(),
        parallelism: flags.jobs,
        cache: flags.no_cache.then_some(false),
        output: flags.output.clone(),
        targets: (!flags.target.is_empty()).then(|| flags.target.clone()),
        framework: None,
        offline: flags.offline,
        continue_on_error: flags.continue_on_error,
        config_file: cli.config.clone(),
    };
    Config::load(&project.root, host, &overrides)
}

fn build_options(
    cli: &Cli,
    config: &Config,
    flags: &BuildFlags,
    release: bool,
) -> Result<BuildOptions> {
    let mut formats = Vec::new();
    for raw in &flags.formats {
        let parsed = PackagingFormat::parse(raw).ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::Usage,
                format!("unknown packaging format '{raw}'"),
            )
            .remediation(
                "Use zip, directory, tar-gz, deb, rpm, appimage, nsis, app, mac-zip or dmg",
            )
        })?;
        formats.push(parsed);
    }
    Ok(BuildOptions {
        profile: config.build.profile.clone(),
        jobs: config.build.parallelism,
        formats,
        output: flags.output.clone(),
        offline: flags.offline,
        no_cache: flags.no_cache,
        continue_on_error: flags.continue_on_error,
        release,
        dry_run: cli.dry_run,
    })
}

fn output(value: Value) {
    match serde_json::to_string_pretty(&value) {
        Ok(text) => println!("{text}"),
        Err(err) => eprintln!("failed to encode JSON: {err}"),
    }
}

pub(crate) fn ok(ui: &Ui, command: &str, data: Value) -> i32 {
    if ui.json() {
        output(json!({ "ok": true, "command": command, "data": data }));
    }
    0
}

pub(crate) fn fail(ui: &Ui, command: &str, error: &UbuilderError, data: Option<Value>) -> i32 {
    if ui.json() {
        let mut value = error.to_json();
        if let Value::Object(map) = &mut value {
            map.insert("command".into(), Value::String(command.into()));
            if let Some(data) = data {
                map.insert("data".into(), data);
            }
        }
        output(value);
    } else {
        ui.error(error.message.clone());
        if let Some(remediation) = error.remediation.as_deref() {
            eprintln!("hint: {remediation}");
        }
    }
    error.code.exit_code()
}

pub fn print_error(json: bool, error: &UbuilderError) {
    if json {
        output(error.to_json());
    } else {
        eprintln!("error[{}]: {}", error.code.as_str(), error.message);
        if let Some(remediation) = error.remediation.as_deref() {
            eprintln!("hint: {remediation}");
        }
    }
}

pub(crate) fn human_report(ui: &Ui, report: &TargetBuildReport) {
    if ui.json() {
        return;
    }
    if report.status == "success" {
        ui.success(format!("{}: {}", report.target, report.status));
        for package in &report.packages {
            ui.info(format!(
                "  {} ({}, {} bytes) sha256={}",
                package.path.display(),
                package.format.as_str(),
                package.size,
                &package.sha256[..package.sha256.len().min(12)]
            ));
        }
        if let Some(path) = &report.manifest_path {
            ui.info(format!("  manifest: {}", path.display()));
        }
        for validation in &report.validations {
            if validation.status != crate::validation::ValidationStatus::Passed {
                ui.warn(format!(
                    "  validation {}: {}",
                    validation.status.as_str(),
                    validation.path.display()
                ));
            }
        }
    } else {
        let reason = report
            .outcome
            .as_ref()
            .and_then(|o| o.failure.as_ref())
            .map(|f| f.message.clone())
            .or_else(|| {
                report
                    .signing
                    .iter()
                    .find(|s| s.status == crate::signing::SigningStatus::Failed)
                    .map(|s| s.detail.clone())
            })
            .unwrap_or_else(|| report.status.clone());
        ui.error(format!("{}: {} ({})", report.target, report.status, reason));
    }
}

pub(crate) fn parse_targets(flags: &BuildFlags, host: &Host) -> Result<Vec<Target>> {
    builder::resolve_targets(&flags.target, flags.all, host)
}

pub(crate) fn target_required(target: Option<String>) -> Result<Target> {
    let value = target.ok_or_else(|| {
        UbuilderError::new(ErrorCode::Usage, "--target is required")
            .remediation("Example: --target windows-x64")
    })?;
    Target::parse(&value)
}

pub(crate) fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}
