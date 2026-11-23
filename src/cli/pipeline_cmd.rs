//! `ubuilder pipeline` command surface.

use super::{absolute_path, fail, host, ok, Cli};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::pipeline::compiler;
use crate::pipeline::diagnostics::{has_errors, render as render_diagnostics};
use crate::pipeline::events::Events;
use crate::pipeline::graph::{self, GraphFormat};
use crate::pipeline::history;
use crate::pipeline::parser::ParsedWorkflow;
use crate::pipeline::planner;
use crate::pipeline::runtime::{self, RunOptions};
use crate::pipeline::validator;
use crate::storage::Db;
use crate::telemetry::Ui;
use clap::{Args, Subcommand};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Subcommand)]
pub enum PipelineCommand {
    /// Create a starter workflow under .ubuilder/workflows.
    Init(InitArgs),
    /// List workflow files in this project.
    List,
    /// Show a parsed workflow definition.
    Inspect(FileArgs),
    /// Validate a workflow and report every diagnostic.
    Validate(FileArgs),
    /// Print the compiled intermediate representation.
    Compile(FileArgs),
    /// Print the resolved execution plan.
    Plan(FileArgs),
    /// Render the job dependency graph.
    Graph(GraphArgs),
    /// Execute a workflow.
    Run(RunArgs),
    /// Show a recorded run and its jobs.
    Status(StatusArgs),
    /// Print step logs for a recorded run.
    Logs(LogsArgs),
    /// List recorded runs.
    History(HistoryArgs),
    /// List built-in actions and their inputs.
    Actions,
    /// Request cancellation of a running workflow.
    Cancel(CancelArgs),
    /// Mark interrupted runs as failed.
    Recover(RecoverArgs),
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Workflow name; also the file stem.
    #[arg(long, default_value = "build")]
    pub name: String,
    /// Overwrite an existing workflow.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct FileArgs {
    /// Workflow file; defaults to the only workflow in .ubuilder/workflows.
    #[arg(long)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct GraphArgs {
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// mermaid, dot or json.
    #[arg(long, default_value = "mermaid")]
    pub format: String,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long)]
    pub file: Option<PathBuf>,
    /// Run only this job and its dependencies.
    #[arg(long)]
    pub job: Option<String>,
    /// Workflow input as KEY=VALUE (repeatable).
    #[arg(long = "input", value_name = "KEY=VALUE")]
    pub inputs: Vec<String>,
    /// Root directory for isolated job workspaces.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Explicit run id.
    #[arg(long)]
    pub run_id: Option<String>,
    /// Disable the pipeline cache.
    #[arg(long)]
    pub no_cache: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Run id; defaults to the most recent run.
    #[arg(long)]
    pub run: Option<String>,
}

#[derive(Debug, Args)]
pub struct LogsArgs {
    #[arg(long)]
    pub run: Option<String>,
    #[arg(long)]
    pub job: Option<String>,
    #[arg(long)]
    pub step: Option<String>,
    /// Number of trailing lines per log.
    #[arg(long, default_value_t = 200)]
    pub tail: usize,
}

#[derive(Debug, Args)]
pub struct HistoryArgs {
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

#[derive(Debug, Args)]
pub struct CancelArgs {
    pub run: String,
}

#[derive(Debug, Args)]
pub struct RecoverArgs {
    /// Consider runs older than this many seconds stale.
    #[arg(long, default_value_t = 86_400)]
    pub stale_secs: u64,
}

pub async fn run(
    cli: &Cli,
    command: &PipelineCommand,
    ui: &Ui,
    cancel: &CancellationToken,
) -> Result<i32> {
    match command {
        PipelineCommand::Init(args) => init(cli, args, ui),
        PipelineCommand::List => list(cli, ui),
        PipelineCommand::Inspect(args) => inspect(cli, args, ui),
        PipelineCommand::Validate(args) => validate(cli, args, ui),
        PipelineCommand::Compile(args) => compile(cli, args, ui),
        PipelineCommand::Plan(args) => plan(cli, args, ui),
        PipelineCommand::Graph(args) => graph(cli, args, ui),
        PipelineCommand::Run(args) => run_workflow(cli, args, ui, cancel).await,
        PipelineCommand::Status(args) => status(cli, args, ui),
        PipelineCommand::Logs(args) => logs(cli, args, ui),
        PipelineCommand::History(args) => history_cmd(cli, args, ui),
        PipelineCommand::Actions => actions(ui),
        PipelineCommand::Cancel(args) => cancel_run(cli, args, ui),
        PipelineCommand::Recover(args) => recover(cli, args, ui),
    }
}

fn init(cli: &Cli, args: &InitArgs, ui: &Ui) -> Result<i32> {
    let valid = !args.name.is_empty()
        && args.name.len() <= 64
        && args
            .name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if !valid {
        return Err(UbuilderError::new(
            ErrorCode::Usage,
            format!("invalid workflow name '{}'", args.name),
        )
        .remediation("Use 1-64 characters from [A-Za-z0-9._-]"));
    }
    let dir = absolute_path(&cli.path).join(".ubuilder").join("workflows");
    std::fs::create_dir_all(&dir)
        .map_err(|error| UbuilderError::io(format!("cannot create {}", dir.display()), error))?;
    let path = dir.join(format!("{}.yml", args.name));
    if path.exists() && !args.force {
        let error = UbuilderError::new(
            ErrorCode::Usage,
            format!("{} already exists", path.display()),
        )
        .remediation("Pass --force to overwrite");
        return Ok(fail(ui, "pipeline init", &error, None));
    }
    std::fs::write(&path, starter_workflow(&args.name))
        .map_err(|error| UbuilderError::io(format!("cannot write {}", path.display()), error))?;
    if !ui.json() {
        ui.success(format!("created {}", path.display()));
        ui.info("Next: ubuilder pipeline validate");
    }
    Ok(ok(ui, "pipeline init", json!({ "path": path })))
}

fn starter_workflow(name: &str) -> String {
    format!(
        r#"version: 1
name: {name}
"on":
  manual: true
env:
  UBUILDER_TERM_COLOR: always
jobs:
  build:
    name: Build desktop artifact
    runs-on: auto
    steps:
      - id: checkout
        uses: builtin/checkout@v1
      - id: setup
        uses: builtin/setup-rust@v1
      - id: build
        uses: builtin/build@v1
        with:
          target: native
          profile: release
      - id: package
        uses: builtin/package@v1
        with:
          target: native
          format: zip
      - id: checksums
        uses: builtin/checksum@v1
        with:
          path: dist
          output: dist/checksums.sha256
      - id: upload
        uses: builtin/upload-artifact@v1
        with:
          name: build-output
          path: dist
"#
    )
}

fn list(cli: &Cli, ui: &Ui) -> Result<i32> {
    let files = workflow_files(&absolute_path(&cli.path))?;
    let mut entries = Vec::new();
    for path in &files {
        let parsed = ParsedWorkflow::load(path).ok();
        entries.push(json!({
            "path": path,
            "name": parsed.as_ref().map(|workflow| workflow.definition.name.clone()),
            "jobs": parsed.as_ref().map(|workflow| workflow.definition.jobs.len()),
            "digest": parsed.map(|workflow| workflow.digest),
        }));
    }
    if !ui.json() {
        if entries.is_empty() {
            ui.warn("no workflows in .ubuilder/workflows");
        }
        for path in &files {
            ui.info(path.display().to_string());
        }
    }
    Ok(ok(ui, "pipeline list", json!({ "workflows": entries })))
}

fn inspect(cli: &Cli, args: &FileArgs, ui: &Ui) -> Result<i32> {
    let path = resolve_workflow(cli, args.file.as_ref())?;
    let parsed = ParsedWorkflow::load(&path)?;
    let jobs = parsed.definition.jobs.len();
    if !ui.json() {
        ui.info(format!("{}: {}", parsed.definition.name, path.display()));
        ui.info(format!(
            "version {} · {jobs} jobs",
            parsed.definition.version
        ));
    }
    Ok(ok(
        ui,
        "pipeline inspect",
        json!({
            "path": path,
            "digest": parsed.digest,
            "definition": parsed.definition,
        }),
    ))
}

fn validate(cli: &Cli, args: &FileArgs, ui: &Ui) -> Result<i32> {
    let path = resolve_workflow(cli, args.file.as_ref())?;
    let parsed = ParsedWorkflow::load(&path)?;
    let diagnostics = validator::diagnostics(&parsed.definition, &parsed.path, &parsed.source);
    if !ui.json() && !diagnostics.is_empty() {
        eprintln!("{}", render_diagnostics(&diagnostics));
    } else if !ui.json() {
        ui.success(format!("{} is valid", path.display()));
    }
    if has_errors(&diagnostics) {
        let error = UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!(
                "workflow validation failed: {} error(s)",
                diagnostics.iter().filter(|item| item.is_error()).count()
            ),
        )
        .details(json!(diagnostics))
        .remediation("Fix the reported workflow errors and re-run validate");
        return Ok(fail(
            ui,
            "pipeline validate",
            &error,
            Some(json!({ "diagnostics": diagnostics })),
        ));
    }
    Ok(ok(
        ui,
        "pipeline validate",
        json!({ "path": path, "diagnostics": diagnostics }),
    ))
}

fn compile(cli: &Cli, args: &FileArgs, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let path = resolve_workflow(cli, args.file.as_ref())?;
    let parsed = ParsedWorkflow::load(&path)?;
    parsed.validate()?;
    let ir = compiler::compile(&parsed, &host)?;
    if !ui.json() {
        ui.success(format!(
            "compiled {} job(s) in {} wave(s)",
            ir.jobs.len(),
            ir.waves.len()
        ));
    }
    Ok(ok(ui, "pipeline compile", json!(ir)))
}

fn plan(cli: &Cli, args: &FileArgs, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let path = resolve_workflow(cli, args.file.as_ref())?;
    let workflow_plan = compile_plan(&path, &absolute_path(&cli.path), &host)?;
    print_plan(ui, &workflow_plan);
    Ok(ok(ui, "pipeline plan", json!(workflow_plan)))
}

fn graph(cli: &Cli, args: &GraphArgs, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let format = GraphFormat::parse(&args.format)?;
    let path = resolve_workflow(cli, args.file.as_ref())?;
    let parsed = ParsedWorkflow::load(&path)?;
    parsed.validate()?;
    let ir = compiler::compile(&parsed, &host)?;
    let rendered = graph::render(&ir, format)?;
    if !ui.json() {
        println!("{rendered}");
    }
    Ok(ok(
        ui,
        "pipeline graph",
        json!({ "format": args.format, "graph": rendered }),
    ))
}

fn compile_plan(
    path: &Path,
    source_root: &Path,
    host: &Host,
) -> Result<crate::pipeline::planner::WorkflowPlan> {
    let parsed = ParsedWorkflow::load(path)?;
    parsed.validate()?;
    let ir = compiler::compile(&parsed, host)?;
    planner::plan(&ir, source_root, host)
}

fn print_plan(ui: &Ui, plan: &crate::pipeline::planner::WorkflowPlan) {
    if ui.json() {
        return;
    }
    ui.info(format!("{} on {}", plan.name, plan.host));
    for (index, wave) in plan.waves.iter().enumerate() {
        ui.info(format!("wave {}: {}", index + 1, wave.join(", ")));
    }
    for job in &plan.jobs {
        let capability = if job.executable {
            "ready"
        } else {
            "unsupported"
        };
        ui.info(format!(
            "  {} [{}] {} · {} steps",
            job.id,
            job.target_display,
            capability,
            job.steps.len()
        ));
    }
    for warning in &plan.warnings {
        ui.warn(format!("{}: {}", warning.code, warning.message));
    }
}

async fn run_workflow(
    cli: &Cli,
    args: &RunArgs,
    ui: &Ui,
    cancel: &CancellationToken,
) -> Result<i32> {
    let host = host()?;
    let source_root = absolute_path(&cli.path);
    let path = resolve_workflow(cli, args.file.as_ref())?;
    if cli.dry_run {
        let workflow_plan = compile_plan(&path, &source_root, &host)?;
        print_plan(ui, &workflow_plan);
        return Ok(ok(
            ui,
            "pipeline run",
            json!({ "dry_run": true, "plan": workflow_plan }),
        ));
    }
    let parsed = ParsedWorkflow::load(&path)?;
    parsed.validate()?;
    let ir = compiler::compile(&parsed, &host)?;
    let db = open_db(&host)?;
    let events = Events::new(cli.json, cli.quiet, cli.verbose);
    let inputs = parse_inputs(&args.inputs)?;
    let options = RunOptions {
        run_id: args.run_id.clone(),
        inputs,
        workspace: args.workspace.as_ref().map(|path| absolute_path(path)),
        job: args.job.clone(),
        no_cache: args.no_cache,
    };
    let report = runtime::run(
        &ir,
        &source_root,
        &host,
        &host.data_dir(),
        db,
        ui,
        &events,
        cancel,
        options,
    )
    .await?;
    print_run(ui, &report);
    match report.status.as_str() {
        "success" => Ok(ok(ui, "pipeline run", json!(report))),
        "cancelled" => {
            let error = UbuilderError::new(ErrorCode::Cancelled, "pipeline cancelled");
            Ok(fail(ui, "pipeline run", &error, Some(json!(report))))
        }
        _ => {
            let error = UbuilderError::new(
                ErrorCode::ExecutionFailed,
                report
                    .error
                    .clone()
                    .unwrap_or_else(|| "pipeline failed".into()),
            )
            .details(json!({ "run_id": report.run_id }))
            .remediation(format!(
                "Inspect logs with `ubuilder pipeline logs --run {}`",
                report.run_id
            ));
            Ok(fail(ui, "pipeline run", &error, Some(json!(report))))
        }
    }
}

fn print_run(ui: &Ui, report: &runtime::RunReport) {
    if ui.json() {
        return;
    }
    ui.info(format!(
        "run {} · {} · {} ms",
        report.run_id, report.status, report.duration_ms
    ));
    for job in &report.jobs {
        let line = format!("  {} · {} · {} ms", job.id, job.status, job.duration_ms);
        if job.status == "success" {
            ui.success(line);
        } else if job.status == "failure" {
            ui.error(line);
        } else {
            ui.info(line);
        }
        for step in &job.steps {
            if step.status != "success" {
                ui.info(format!("    {} · {}", step.id, step.status));
            }
        }
    }
    for artifact in &report.artifacts {
        ui.info(format!(
            "  artifact {} ({} bytes) {}",
            artifact.name,
            artifact.size,
            &artifact.sha256[..artifact.sha256.len().min(12)]
        ));
    }
}

fn status(_cli: &Cli, args: &StatusArgs, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let db = open_db(&host)?;
    let run_id = resolve_run(&db, args.run.as_deref())?;
    let status = history::status(&db, &run_id)?;
    if !ui.json() {
        ui.info(format!(
            "{} · {} · {}",
            status.run.id, status.run.workflow, status.run.status
        ));
        if let Some(error) = &status.run.error {
            ui.error(error.clone());
        }
        for job in &status.jobs {
            ui.info(format!("  {} [{}] · {}", job.id, job.base_id, job.status));
            for step in status.steps.iter().filter(|step| step.job_id == job.id) {
                ui.info(format!("    {} · {}", step.step_id, step.status));
            }
        }
        for artifact in &status.artifacts {
            ui.info(format!(
                "  artifact {} ({} bytes)",
                artifact.name, artifact.size
            ));
        }
    }
    Ok(ok(ui, "pipeline status", json!(status)))
}

fn logs(_cli: &Cli, args: &LogsArgs, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let db = open_db(&host)?;
    let run_id = resolve_run(&db, args.run.as_deref())?;
    let entries = history::logs(&db, &run_id, args.job.as_deref(), args.step.as_deref())?;
    let mut rendered = Vec::new();
    for entry in &entries {
        let text = std::fs::read_to_string(&entry.path).map_err(|error| {
            UbuilderError::io(format!("cannot read {}", entry.path.display()), error)
        })?;
        let tail = tail_lines(&text, args.tail);
        if !ui.json() {
            ui.info(format!(
                "== {} › {} ({}) ==",
                entry.job_id,
                entry.step_id,
                entry.path.display()
            ));
            print!("{tail}");
            if !tail.ends_with('\n') && !tail.is_empty() {
                println!();
            }
        }
        rendered.push(json!({
            "job_id": entry.job_id,
            "step_id": entry.step_id,
            "path": entry.path,
            "content": tail,
        }));
    }
    Ok(ok(
        ui,
        "pipeline logs",
        json!({ "run_id": run_id, "logs": rendered }),
    ))
}

fn tail_lines(text: &str, tail: usize) -> String {
    if tail == 0 {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(tail);
    let mut out = lines[start..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

fn history_cmd(_cli: &Cli, args: &HistoryArgs, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let db = open_db(&host)?;
    let runs = history::history(&db, args.limit)?;
    if !ui.json() {
        for run in &runs {
            ui.info(format!(
                "{} · {} · {} · {}",
                run.id, run.workflow, run.status, run.host
            ));
        }
    }
    Ok(ok(ui, "pipeline history", json!({ "runs": runs })))
}

fn cancel_run(_cli: &Cli, args: &CancelArgs, ui: &Ui) -> Result<i32> {
    validate_run_id(&args.run)?;
    let host = host()?;
    let db = open_db(&host)?;
    let run = db.pipeline_run(&args.run)?.ok_or_else(|| {
        UbuilderError::new(
            ErrorCode::Usage,
            format!("unknown pipeline run '{}'", args.run),
        )
    })?;
    if run.status != "running" {
        let error = UbuilderError::new(
            ErrorCode::Usage,
            format!("run '{}' is not running ({})", args.run, run.status),
        );
        return Ok(fail(ui, "pipeline cancel", &error, None));
    }
    let marker = history::cancel_marker(&host.data_dir(), &args.run);
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            UbuilderError::io(format!("cannot create {}", parent.display()), error)
        })?;
    }
    std::fs::write(&marker, b"cancel\n")
        .map_err(|error| UbuilderError::io(format!("cannot write {}", marker.display()), error))?;
    if !ui.json() {
        ui.success(format!("cancellation requested for {}", args.run));
    }
    Ok(ok(
        ui,
        "pipeline cancel",
        json!({ "run_id": args.run, "marker": marker }),
    ))
}

fn recover(_cli: &Cli, args: &RecoverArgs, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let db = open_db(&host)?;
    let stale_before =
        crate::telemetry::now_ms().saturating_sub(u128::from(args.stale_secs) * 1000);
    let report = history::recover(&db, stale_before)?;
    if !ui.json() {
        if report.recovered.is_empty() {
            ui.info("no stale runs");
        }
        for run in &report.recovered {
            ui.success(format!("marked {} as failed", run));
        }
    }
    Ok(ok(ui, "pipeline recover", json!(report)))
}

fn resolve_workflow(cli: &Cli, file: Option<&PathBuf>) -> Result<PathBuf> {
    let root = absolute_path(&cli.path);
    if let Some(file) = file {
        let path = if file.is_absolute() {
            file.clone()
        } else {
            root.join(file)
        };
        if !path.is_file() {
            return Err(UbuilderError::new(
                ErrorCode::Usage,
                format!("workflow {} does not exist", path.display()),
            )
            .remediation("Pass --file with a path under .ubuilder/workflows"));
        }
        return Ok(path);
    }
    let files = workflow_files(&root)?;
    match files.len() {
        0 => Err(
            UbuilderError::new(ErrorCode::Usage, "no workflow found in .ubuilder/workflows")
                .remediation("Run `ubuilder pipeline init` to create one"),
        ),
        1 => Ok(files[0].clone()),
        _ => Err(UbuilderError::new(
            ErrorCode::Usage,
            format!(
                "multiple workflows found ({}); pass --file",
                files
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

fn workflow_files(root: &Path) -> Result<Vec<PathBuf>> {
    let dir = root.join(".ubuilder").join("workflows");
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .map_err(|error| UbuilderError::io(format!("cannot read {}", dir.display()), error))?
    {
        let entry = entry
            .map_err(|error| UbuilderError::io(format!("cannot read {}", dir.display()), error))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("yml" | "yaml")
        ) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn open_db(host: &Host) -> Result<Arc<Db>> {
    Ok(Arc::new(Db::open(&host.data_dir().join("ubuilder.db"))?))
}

fn resolve_run(db: &Db, requested: Option<&str>) -> Result<String> {
    if let Some(id) = requested {
        validate_run_id(id)?;
        return Ok(id.to_string());
    }
    db.pipeline_runs(1)?
        .into_iter()
        .next()
        .map(|run| run.id)
        .ok_or_else(|| {
            UbuilderError::new(ErrorCode::Usage, "no pipeline runs recorded")
                .remediation("Run `ubuilder pipeline run` first")
        })
}

fn validate_run_id(id: &str) -> Result<()> {
    let valid = !id.is_empty()
        && id.len() <= 96
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(
            UbuilderError::new(ErrorCode::Usage, format!("invalid run id '{id}'"))
                .remediation("Use 1-96 characters from [A-Za-z0-9._-]"),
        )
    }
}

fn parse_inputs(values: &[String]) -> Result<indexmap::IndexMap<String, String>> {
    let mut inputs = indexmap::IndexMap::new();
    for value in values {
        let Some((key, raw)) = value.split_once('=') else {
            return Err(
                UbuilderError::new(ErrorCode::Usage, format!("invalid input '{value}'"))
                    .remediation("Use --input KEY=VALUE"),
            );
        };
        if key.trim().is_empty() {
            return Err(UbuilderError::new(ErrorCode::Usage, "input key is empty"));
        }
        inputs.insert(key.trim().to_string(), raw.to_string());
    }
    Ok(inputs)
}

fn actions(ui: &Ui) -> Result<i32> {
    let described = crate::pipeline::actions::registry::describe();
    if !ui.json() {
        if let Some(actions) = described.as_array() {
            for action in actions {
                ui.info(format!(
                    "{} · {}",
                    action["id"].as_str().unwrap_or(""),
                    action["description"].as_str().unwrap_or("")
                ));
            }
        }
    }
    Ok(ok(ui, "pipeline actions", described))
}
