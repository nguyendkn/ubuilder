//! `init`, `inspect`, `doctor`, `clean`, `cache`, `target`, `artifact`.

use super::{fail, host, ok, Cli};
use crate::adapters;
use crate::artifacts;
use crate::builder;
use crate::cache::Cache;
use crate::capability::{self, CapabilityInput};
use crate::config::{Config, CONFIG_FILE};
use crate::detect;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::project::FrameworkKind;
use crate::storage::Db;
use crate::target::Target;
use crate::telemetry::Ui;
use crate::toolchain;
use crate::validation;
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Overwrite an existing configuration.
    #[arg(long)]
    pub force: bool,
    /// Project name (defaults to the detected manifest name).
    #[arg(long)]
    pub name: Option<String>,
    /// Framework (rust, dotnet, electron, tauri).
    #[arg(long)]
    pub framework: Option<String>,
    /// Default enabled target ids.
    #[arg(long = "target", value_delimiter = ',')]
    pub targets: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// Target to diagnose; defaults to the native target.
    #[arg(long, value_delimiter = ',')]
    pub target: Vec<String>,
    /// Framework to evaluate when no project is detected.
    #[arg(long)]
    pub framework: Option<String>,
    /// Diagnose every canonical target instead of only the native target.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Also remove cached action results.
    #[arg(long)]
    pub cache: bool,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Show cache size and entry counts.
    Stats,
    /// Remove old or oversized entries.
    Prune {
        #[arg(long)]
        older_than: Option<u64>,
        #[arg(long)]
        max_size: Option<u64>,
    },
    /// Remove every cache entry.
    Clear,
}

#[derive(Debug, Subcommand)]
pub enum TargetCommand {
    /// List canonical targets.
    List,
    /// Show one target's toolchain and packaging requirements.
    Inspect { target: String },
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    /// List artifacts recorded in the local database.
    List {
        #[arg(long)]
        target: Option<String>,
    },
    /// Inspect a file or recorded artifact by SHA-256.
    Inspect {
        /// Artifact path, or a SHA-256 prefix.
        artifact: String,
    },
    /// Validate an artifact's format and architecture.
    Verify {
        path: PathBuf,
        #[arg(long)]
        target: Option<String>,
    },
}

pub fn init(cli: &Cli, args: &InitArgs) -> Result<i32> {
    let ui = Ui::new(cli.json, cli.verbose, cli.quiet);
    let root = super::absolute_path(&cli.path);
    std::fs::create_dir_all(&root)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", root.display()), e))?;
    let path = root.join(CONFIG_FILE);
    if path.exists() && !args.force {
        let error = UbuilderError::new(
            ErrorCode::Usage,
            format!("{} already exists", path.display()),
        )
        .remediation("Pass --force to overwrite");
        return Ok(fail(&ui, "init", &error, None));
    }

    let report = detect::report(&root).ok();
    let detected = report.as_ref().and_then(|r| r.selected.clone());
    let framework = match &args.framework {
        Some(raw) => FrameworkKind::parse(raw).ok_or_else(|| {
            UbuilderError::new(ErrorCode::Usage, format!("unknown framework '{raw}'"))
                .remediation("Use rust, dotnet, electron or tauri")
        })?,
        None => detected.as_ref().map(|p| p.framework).ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::ProjectNotFound,
                "no project detected; pass --framework",
            )
        })?,
    };
    let name = args
        .name
        .clone()
        .or_else(|| detected.as_ref().map(|p| p.name.clone()))
        .or_else(|| {
            root.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "app".into());
    let template = Config::template(&name, framework, &args.targets);
    std::fs::write(&path, template)
        .map_err(|e| UbuilderError::io(format!("cannot write {}", path.display()), e))?;
    if !ui.json() {
        ui.success(format!("wrote {}", path.display()));
    }
    Ok(ok(
        &ui,
        "init",
        json!({ "path": path, "name": name, "framework": framework.as_str() }),
    ))
}

pub fn inspect(cli: &Cli, ui: &Ui) -> Result<i32> {
    let root = super::absolute_path(&cli.path);
    let report = match detect::report(&root) {
        Ok(report) => report,
        Err(error) => return Ok(fail(ui, "inspect", &error, None)),
    };
    if !ui.json() {
        ui.info(format!("root: {}", report.root.display()));
        if report.candidates.is_empty() {
            ui.warn("no supported project found");
        }
        for candidate in &report.candidates {
            ui.info(format!(
                "  {} {}",
                candidate.framework.as_str(),
                candidate.root.display()
            ));
        }
        if let Some(selected) = &report.selected {
            ui.success(format!(
                "selected: {} ({})",
                selected.name,
                selected.framework.as_str()
            ));
        }
    }
    Ok(ok(
        ui,
        "inspect",
        serde_json::to_value(&report).unwrap_or(Value::Null),
    ))
}

pub fn doctor(cli: &Cli, args: &DoctorArgs, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let project = super::discover(cli, None).ok();
    let state = toolchain::detect(&host, project.as_ref());
    let framework = match &args.framework {
        Some(raw) => FrameworkKind::parse(raw).ok_or_else(|| {
            UbuilderError::new(ErrorCode::Usage, format!("unknown framework '{raw}'"))
        })?,
        None => project
            .as_ref()
            .map(|p| p.framework)
            .unwrap_or(FrameworkKind::Rust),
    };
    let targets = if args.target.is_empty() {
        builder::resolve_targets(&[], args.all, &host)?
    } else {
        args.target
            .iter()
            .map(|id| Target::parse(id))
            .collect::<Result<Vec<_>>>()?
    };
    let mut rows = Vec::new();
    for target in &targets {
        let capability = match &project {
            Some(project) => adapters::resolve_capabilities(project, &host, target, &state.facts),
            None => capability::resolve(
                &host,
                target,
                &CapabilityInput::new(framework),
                &state.facts,
            ),
        };
        if !ui.json() {
            ui.info(format!("{} ({})", target.id, target.display_name()));
            ui.info(format!(
                "  compile={} package={} sign={} native-test={}",
                capability.compilation.as_str(),
                capability.packaging.as_str(),
                capability.signing.as_str(),
                capability.native_testing.as_str()
            ));
            for requirement in capability.unsatisfied() {
                ui.warn(format!(
                    "  missing: {}{}",
                    requirement.description,
                    requirement
                        .remediation
                        .as_deref()
                        .map(|r| format!(" -> {r}"))
                        .unwrap_or_default()
                ));
            }
            for warning in &capability.warnings {
                ui.warn(format!("  {}: {}", warning.code, warning.message));
            }
        }
        rows.push(json!({
            "target": target.id,
            "native": target.is_native(&host),
            "capability": capability,
        }));
    }
    if !ui.json() {
        ui.info(format!("host: {} ({})", host.display(), host.triple));
        for probe in &state.probes {
            ui.info(format!(
                "  {} {} {}",
                probe.id,
                if probe.available {
                    "available"
                } else {
                    "missing"
                },
                probe.version.as_deref().unwrap_or("")
            ));
            for note in &probe.notes {
                ui.info(format!("    {note}"));
            }
        }
    }
    Ok(ok(
        ui,
        "doctor",
        json!({
            "host": host,
            "framework": framework.as_str(),
            "fingerprint": state.fingerprint,
            "facts": state.facts,
            "probes": state.probes,
            "targets": rows,
        }),
    ))
}

pub fn clean(cli: &Cli, args: &CleanArgs) -> Result<i32> {
    let ui = Ui::new(cli.json, cli.verbose, cli.quiet);
    let root = super::absolute_path(&cli.path);
    let work = root.join(".ubuilder");
    let mut removed = Vec::new();
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| UbuilderError::io(format!("cannot remove {}", work.display()), e))?;
        removed.push(work.clone());
    }
    if args.cache {
        let host = host()?;
        let cache = Cache::open(&host.cache_dir(), true)?;
        cache.clear()?;
        removed.push(host.cache_dir());
    }
    if !ui.json() {
        ui.success(format!("removed {} path(s)", removed.len()));
    }
    Ok(ok(&ui, "clean", json!({ "removed": removed })))
}

pub fn cache(_cli: &Cli, command: &CacheCommand, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let cache = match Cache::open(&host.cache_dir(), true) {
        Ok(cache) => cache,
        Err(error) => return Ok(fail(ui, "cache", &error, None)),
    };
    match command {
        CacheCommand::Stats => {
            let report = match cache.stats() {
                Ok(report) => report,
                Err(error) => return Ok(fail(ui, "cache stats", &error, None)),
            };
            if !ui.json() {
                ui.info(format!(
                    "entries={} size={} bytes root={}",
                    report.metadata.entries,
                    report.metadata.total_bytes,
                    cache.root().display()
                ));
            }
            Ok(ok(
                ui,
                "cache stats",
                serde_json::to_value(&report).unwrap_or(Value::Null),
            ))
        }
        CacheCommand::Prune {
            older_than,
            max_size,
        } => {
            let report = match cache.prune(*older_than, *max_size) {
                Ok(report) => report,
                Err(error) => return Ok(fail(ui, "cache prune", &error, None)),
            };
            if !ui.json() {
                ui.success(format!(
                    "removed {} entries, {} bytes",
                    report.removed_entries, report.removed_bytes
                ));
            }
            Ok(ok(
                ui,
                "cache prune",
                serde_json::to_value(&report).unwrap_or(Value::Null),
            ))
        }
        CacheCommand::Clear => {
            if let Err(error) = cache.clear() {
                return Ok(fail(ui, "cache clear", &error, None));
            }
            if !ui.json() {
                ui.success("cache cleared");
            }
            Ok(ok(ui, "cache clear", json!({ "root": cache.root() })))
        }
    }
}

pub fn target(_cli: &Cli, command: &TargetCommand, ui: &Ui) -> Result<i32> {
    let host = host()?;
    match command {
        TargetCommand::List => {
            let targets = Target::all();
            if !ui.json() {
                for target in &targets {
                    ui.info(format!(
                        "{}  {:<28} {}{}",
                        target.id,
                        target.triple.as_deref().unwrap_or("-"),
                        target.display_name(),
                        if target.is_native(&host) {
                            " [native]"
                        } else {
                            ""
                        }
                    ));
                }
            }
            Ok(ok(
                ui,
                "target list",
                serde_json::to_value(&targets).unwrap_or(Value::Null),
            ))
        }
        TargetCommand::Inspect { target } => {
            let target = match Target::parse(target) {
                Ok(target) => target,
                Err(error) => return Ok(fail(ui, "target inspect", &error, None)),
            };
            if !ui.json() {
                ui.info(format!("{} ({})", target.id, target.display_name()));
                ui.info(format!("os={:?} arches={:?}", target.os, target.arches));
                ui.info(format!(
                    "triple={} rid={}",
                    target.triple.as_deref().unwrap_or("-"),
                    target.rid.as_deref().unwrap_or("-")
                ));
                ui.info(format!(
                    "formats={}",
                    target
                        .packaging_formats
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(",")
                ));
                if let Some(sdk) = &target.required_sdk {
                    ui.info(format!("sdk={sdk}"));
                }
                if let Some(linker) = &target.required_linker {
                    ui.info(format!("linker={linker}"));
                }
                ui.info(format!(
                    "native={} universal={}",
                    target.is_native(&host),
                    target.universal
                ));
            }
            Ok(ok(
                ui,
                "target inspect",
                json!({ "target": target.clone(), "native": target.is_native(&host) }),
            ))
        }
    }
}

pub fn artifact(_cli: &Cli, command: &ArtifactCommand, ui: &Ui) -> Result<i32> {
    let host = host()?;
    match command {
        ArtifactCommand::List { target } => {
            let db = match open_db(&host) {
                Ok(db) => db,
                Err(error) => return Ok(fail(ui, "artifact list", &error, None)),
            };
            let records = match db.list_artifacts(target.as_deref()) {
                Ok(records) => records,
                Err(error) => return Ok(fail(ui, "artifact list", &error, None)),
            };
            if !ui.json() {
                for record in &records {
                    ui.info(format!(
                        "{}  {}  {}  {}",
                        record.target,
                        record.format,
                        &record.sha256[..record.sha256.len().min(12)],
                        record.path.display()
                    ));
                }
            }
            Ok(ok(
                ui,
                "artifact list",
                serde_json::to_value(&records).unwrap_or(Value::Null),
            ))
        }
        ArtifactCommand::Inspect { artifact } => {
            let value = inspect_artifact(&host, artifact);
            match value {
                Ok(value) => {
                    if !ui.json() {
                        ui.info(serde_json::to_string_pretty(&value).unwrap_or_default());
                    }
                    Ok(ok(ui, "artifact inspect", value))
                }
                Err(error) => Ok(fail(ui, "artifact inspect", &error, None)),
            }
        }
        ArtifactCommand::Verify { path, target } => {
            let target = match target {
                Some(id) => Target::parse(id),
                None => builder::resolve_targets(&[], false, &host).map(|mut v| v.remove(0)),
            };
            let target = match target {
                Ok(target) => target,
                Err(error) => return Ok(fail(ui, "artifact verify", &error, None)),
            };
            let report = match validation::validate_artifact(path, &target) {
                Ok(report) => report,
                Err(error) => return Ok(fail(ui, "artifact verify", &error, None)),
            };
            if !ui.json() {
                ui.info(format!(
                    "{}: {} ({})",
                    report.path.display(),
                    report.status.as_str(),
                    report.kind
                ));
                for check in &report.checks {
                    ui.info(format!(
                        "  {} {} {}",
                        check.name,
                        check.status.as_str(),
                        check.detail
                    ));
                }
            }
            let code = ok(
                ui,
                "artifact verify",
                serde_json::to_value(&report).unwrap_or(Value::Null),
            );
            if report.status == validation::ValidationStatus::Failed {
                Ok(1)
            } else {
                Ok(code)
            }
        }
    }
}

fn inspect_artifact(host: &Host, artifact: &str) -> Result<Value> {
    let as_path = PathBuf::from(artifact);
    if as_path.exists() {
        let sha256 = if as_path.is_dir() {
            artifacts::hash_path(&as_path)?
        } else {
            artifacts::sha256_file(&as_path)?
        };
        let size = if as_path.is_dir() {
            std::fs::read_dir(&as_path)
                .map(|entries| entries.flatten().count() as u64)
                .unwrap_or(0)
        } else {
            std::fs::metadata(&as_path)
                .map_err(|e| UbuilderError::io("cannot stat artifact", e))?
                .len()
        };
        return Ok(json!({
            "path": as_path,
            "sha256": sha256,
            "size": size,
            "directory": as_path.is_dir(),
        }));
    }
    let db = open_db(host)?;
    let record = db
        .artifact_by_sha(artifact)?
        .or_else(|| {
            db.list_artifacts(None)
                .ok()?
                .into_iter()
                .find(|r| r.sha256.starts_with(artifact))
        })
        .ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("no artifact matches '{artifact}'"),
            )
        })?;
    Ok(serde_json::to_value(record).unwrap_or(Value::Null))
}

fn open_db(host: &Host) -> Result<Db> {
    let path = host.data_dir().join("ubuilder.db");
    if !path.exists() {
        std::fs::create_dir_all(host.data_dir())
            .map_err(|e| UbuilderError::io("cannot create data directory", e))?;
    }
    Db::open(&path)
}

#[derive(Debug, Subcommand)]
pub enum MaintenanceCommand {
    /// Show database, cache, log and artifact disk usage.
    Stats,
    /// Delete old terminal runs according to retention policy.
    Prune(PruneArgs),
    /// Remove artifact objects no retained run references.
    Gc(GcArgs),
    /// Protect an artifact object from garbage collection.
    Pin(PinArgs),
    /// Remove protection from an artifact object.
    Unpin { sha256: String },
}

#[derive(Debug, Args)]
pub struct PruneArgs {
    /// Delete terminal runs older than this many days.
    #[arg(long, default_value_t = 30)]
    pub max_age_days: u64,
    /// Always keep at least this many newest terminal runs.
    #[arg(long, default_value_t = 100)]
    pub max_runs: usize,
    /// Approximate retained artifact bytes; oldest runs are pruned first.
    #[arg(long, default_value_t = 4096)]
    pub max_size_mb: u64,
    /// Report what would be deleted without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct GcArgs {
    /// Report unreferenced objects without deleting them.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct PinArgs {
    /// Full SHA-256 or a unique prefix.
    pub sha256: String,
    /// Optional human-readable label.
    #[arg(long)]
    pub name: Option<String>,
}

pub fn maintenance(_cli: &Cli, command: &MaintenanceCommand, ui: &Ui) -> Result<i32> {
    let host = host()?;
    let data_dir = host.data_dir();
    let db = Db::open(&data_dir.join("ubuilder.db"))?;
    match command {
        MaintenanceCommand::Stats => {
            let runs = db.all_pipeline_runs()?;
            let active = runs
                .iter()
                .filter(|run| {
                    !matches!(
                        run.status.as_str(),
                        "success"
                            | "failure"
                            | "cancelled"
                            | "skipped"
                            | "blocked"
                            | "timed_out"
                            | "interrupted"
                    )
                })
                .count();
            let refs = db.pipeline_artifact_references()?;
            let artifact_bytes: u64 = refs.values().map(|(_, size)| *size).sum();
            let stats = json!({
                "data_dir": data_dir,
                "database_bytes": dir_size(&data_dir.join("ubuilder.db")),
                "pipeline_root_bytes": dir_size(&data_dir.join("pipeline")),
                "runs_root_bytes": dir_size(&data_dir.join("pipeline/runs")),
                "artifact_objects_bytes": dir_size(&data_dir.join("pipeline/artifacts/objects")),
                "cache_bytes": dir_size(&data_dir.join("pipeline/cache")),
                "runs": runs.len(),
                "active_runs": active,
                "artifact_references": refs.values().map(|(count, _)| count).sum::<u64>(),
                "artifact_referenced_bytes": artifact_bytes,
                "pinned_artifacts": db.pinned_artifacts()?.len(),
            });
            if !ui.json() {
                ui.info(format!("data: {}", data_dir.display()));
                ui.info(format!(
                    "runs: {} ({} active)",
                    stats["runs"], stats["active_runs"]
                ));
                ui.info(format!(
                    "artifact objects: {} bytes; referenced: {} bytes",
                    stats["artifact_objects_bytes"], stats["artifact_referenced_bytes"]
                ));
                ui.info(format!("cache: {} bytes", stats["cache_bytes"]));
            }
            Ok(ok(ui, "maintenance stats", stats))
        }
        MaintenanceCommand::Prune(args) => {
            let runs = db.all_pipeline_runs()?;
            let refs = db.pipeline_artifact_references()?;
            let pinned: std::collections::BTreeSet<String> = db
                .pinned_artifacts()?
                .into_iter()
                .map(|(sha, _)| sha)
                .collect();
            let now = crate::telemetry::now_ms();
            let cutoff = now.saturating_sub(args.max_age_days as u128 * 86_400_000);
            let mut deleted = Vec::new();
            let mut retained_bytes = 0u64;
            let mut kept = 0usize;
            // Newest first: retention always preserves the newest runs.
            for run in &runs {
                let terminal = matches!(
                    run.status.as_str(),
                    "success"
                        | "failure"
                        | "cancelled"
                        | "skipped"
                        | "blocked"
                        | "timed_out"
                        | "interrupted"
                );
                let run_artifacts = db.pipeline_artifacts(&run.id)?;
                let bytes: u64 = run_artifacts
                    .iter()
                    .map(|a| refs.get(&a.sha256).map(|(_, size)| *size).unwrap_or(a.size))
                    .sum();
                let protected = run_artifacts
                    .iter()
                    .any(|artifact| pinned.contains(&artifact.sha256));
                if !terminal {
                    continue;
                }
                let too_old = run.created_at < cutoff;
                let too_many = kept >= args.max_runs;
                let too_large =
                    retained_bytes.saturating_add(bytes) > args.max_size_mb * 1024 * 1024;
                let should_delete = !protected && (too_old || too_many || too_large);
                if should_delete {
                    if !args.dry_run {
                        db.delete_pipeline_run(&run.id)?;
                        let dir = data_dir.join("pipeline").join("runs").join(&run.id);
                        if dir.is_dir() {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                    }
                    deleted.push(json!({
                        "run_id": run.id,
                        "status": run.status,
                        "created_at": run.created_at,
                        "bytes": bytes,
                    }));
                } else {
                    kept += 1;
                    retained_bytes = retained_bytes.saturating_add(bytes);
                }
            }
            let report = json!({
                "dry_run": args.dry_run,
                "deleted": deleted,
                "deleted_count": deleted.len(),
                "retained_bytes": retained_bytes,
            });
            if !ui.json() {
                if deleted.is_empty() {
                    ui.info("nothing to prune");
                }
                for item in &deleted {
                    ui.info(format!(
                        "{} {} ({} bytes){}",
                        if args.dry_run {
                            "would delete"
                        } else {
                            "deleted"
                        },
                        item["run_id"],
                        item["bytes"],
                        ""
                    ));
                }
            }
            Ok(ok(ui, "maintenance prune", report))
        }
        MaintenanceCommand::Gc(args) => {
            let refs = db.pipeline_artifact_references()?;
            let pinned: std::collections::BTreeSet<String> = db
                .pinned_artifacts()?
                .into_iter()
                .map(|(sha, _)| sha)
                .collect();
            let objects = data_dir.join("pipeline").join("artifacts").join("objects");
            let mut removed = Vec::new();
            let mut removed_bytes = 0u64;
            if let Ok(entries) = std::fs::read_dir(&objects) {
                for entry in entries.flatten() {
                    if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let sha = entry.file_name().to_string_lossy().to_string();
                    if refs.contains_key(&sha) || pinned.contains(&sha) {
                        continue;
                    }
                    let bytes = dir_size(&entry.path());
                    if !args.dry_run {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                    removed_bytes += bytes;
                    removed.push(sha);
                }
            }
            let report = json!({
                "dry_run": args.dry_run,
                "removed": removed,
                "removed_count": removed.len(),
                "removed_bytes": removed_bytes,
            });
            if !ui.json() {
                if removed.is_empty() {
                    ui.info("no unreferenced artifact objects");
                } else {
                    ui.info(format!(
                        "{} {} objects ({} bytes)",
                        if args.dry_run {
                            "would remove"
                        } else {
                            "removed"
                        },
                        removed.len(),
                        removed_bytes
                    ));
                }
            }
            Ok(ok(ui, "maintenance gc", report))
        }
        MaintenanceCommand::Pin(args) => {
            let refs = db.pipeline_artifact_references()?;
            let matches: Vec<&String> = refs
                .keys()
                .filter(|sha| sha.starts_with(&args.sha256))
                .collect();
            let sha = match matches.as_slice() {
                [sha] => (*sha).clone(),
                [] => {
                    let error = UbuilderError::new(
                        ErrorCode::ArtifactInvalid,
                        format!("no artifact object matches '{}'", args.sha256),
                    );
                    return Ok(fail(ui, "maintenance pin", &error, None));
                }
                _ => {
                    let error = UbuilderError::new(
                        ErrorCode::ArtifactInvalid,
                        format!("artifact prefix '{}' is ambiguous", args.sha256),
                    );
                    return Ok(fail(ui, "maintenance pin", &error, None));
                }
            };
            let name = args.name.clone().unwrap_or_else(|| sha.clone());
            db.pin_artifact(&sha, &name)?;
            if !ui.json() {
                ui.success(format!("pinned {sha} ({name})"));
            }
            Ok(ok(
                ui,
                "maintenance pin",
                json!({ "sha256": sha, "name": name }),
            ))
        }
        MaintenanceCommand::Unpin { sha256 } => {
            let removed = db.unpin_artifact(sha256)?;
            if !ui.json() {
                if removed {
                    ui.success(format!("unpinned {sha256}"));
                } else {
                    ui.warn(format!("no pin for {sha256}"));
                }
            }
            Ok(ok(
                ui,
                "maintenance unpin",
                json!({ "sha256": sha256, "removed": removed }),
            ))
        }
    }
}

fn dir_size(path: &std::path::Path) -> u64 {
    if path.is_file() {
        return std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        total = total.saturating_add(dir_size(&entry.path()));
    }
    total
}
