//! End-to-end orchestration: detect -> plan -> execute -> package -> validate
//! -> sign -> record. The CLI is only a thin argument parser on top of this.

use crate::adapters::{self, PlanningContext};
use crate::artifacts::{ArtifactStore, ArtifactSummary, BuildManifest, ManifestStep};
use crate::cache::Cache;
use crate::capability::{BuildCapability, SupportLevel};
use crate::config::Config;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::{Executor, ExecutorConfig};
use crate::host::Host;
use crate::packaging::{self, PackageContext, PackageResult};
use crate::planner::BuildPlan;
use crate::project::{FrameworkKind, Project};
use crate::scheduler::{self, BuildOutcome, BuildStatus, Scheduler, SchedulerConfig};
use crate::signing::{self, SigningOptions, SigningReport};
use crate::storage::Db;
use crate::target::{PackagingFormat, Target};
use crate::telemetry::{now_ms, Ui};
use crate::toolchain;
use crate::validation::{self, ValidationReport};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub profile: String,
    pub jobs: usize,
    pub formats: Vec<PackagingFormat>,
    pub output: Option<PathBuf>,
    pub offline: bool,
    pub no_cache: bool,
    pub continue_on_error: bool,
    pub release: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetBuildReport {
    pub build_id: String,
    pub job_id: String,
    pub project: String,
    pub framework: FrameworkKind,
    pub target: String,
    pub target_display: String,
    pub status: String,
    pub capability: BuildCapability,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<BuildPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BuildOutcome>,
    pub packages: Vec<PackageResult>,
    pub validations: Vec<ValidationReport>,
    pub signing: Vec<SigningReport>,
    pub artifacts: Vec<ArtifactSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<PathBuf>,
}

pub fn resolve_targets(ids: &[String], all: bool, host: &Host) -> Result<Vec<Target>> {
    if all {
        return Ok(Target::all());
    }
    if ids.is_empty() {
        return Target::all()
            .into_iter()
            .find(|t| t.is_native(host))
            .ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::UnsupportedTarget,
                    format!("no native target mapping for host {}", host.display()),
                )
            })
            .map(|t| vec![t]);
    }
    ids.iter().map(|id| Target::parse(id)).collect()
}

pub async fn build_targets(
    project: &Project,
    host: &Host,
    config: &Config,
    options: &BuildOptions,
    targets: &[Target],
    ui: &Ui,
    cancel: &CancellationToken,
) -> Vec<Result<TargetBuildReport>> {
    let mut reports = Vec::new();
    for target in targets {
        if cancel.is_cancelled() {
            reports.push(Err(UbuilderError::new(
                ErrorCode::Cancelled,
                "build cancelled",
            )));
            break;
        }
        let result = build_one(project, host, config, options, target, ui, cancel).await;
        let failed = result.is_err()
            || result
                .as_ref()
                .map(|r| r.status == BuildStatus::Failed.as_str())
                .unwrap_or(false);
        reports.push(result);
        if failed && !options.continue_on_error {
            break;
        }
    }
    reports
}

pub async fn build_one(
    project: &Project,
    host: &Host,
    config: &Config,
    options: &BuildOptions,
    target: &Target,
    ui: &Ui,
    cancel: &CancellationToken,
) -> Result<TargetBuildReport> {
    let timestamp = now_ms();
    let build_id = format!("{}-{timestamp}", slug(&project.name));
    let job_id = scheduler::job_id(&project.name, &target.id, timestamp);
    let work_dir = project
        .root
        .join(".ubuilder")
        .join("work")
        .join(&build_id)
        .join(&target.id);
    std::fs::create_dir_all(&work_dir)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", work_dir.display()), e))?;

    let state = toolchain::detect(host, Some(project));
    let capability = adapters::resolve_capabilities(project, host, target, &state.facts);
    if capability.compilation == SupportLevel::Unsupported {
        let missing = capability
            .unsatisfied()
            .iter()
            .map(|r| r.description.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(UbuilderError::new(
            ErrorCode::UnsupportedTarget,
            format!(
                "{} cannot compile {} for {}",
                project.framework.as_str(),
                project.name,
                target.id
            ),
        )
        .remediation(if missing.is_empty() {
            "Run `ubuilder doctor --target <target>` for details".to_string()
        } else {
            missing
        }));
    }

    let output_dir = resolve_output_dir(project, config, options);
    if !options.dry_run {
        std::fs::create_dir_all(&output_dir)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", output_dir.display()), e))?;
    }

    let ctx = PlanningContext {
        project,
        target,
        host,
        config,
        facts: &state.facts,
        build_id: &build_id,
        work_dir: work_dir.clone(),
        cache_dir: host.cache_dir(),
        profile: options.profile.clone(),
        offline: options.offline,
        no_cache: options.no_cache,
    };
    let adapter = adapters::adapter_for(project.framework);
    let plan = adapter.plan(&ctx)?;

    let mut report = TargetBuildReport {
        build_id: build_id.clone(),
        job_id: job_id.clone(),
        project: project.name.clone(),
        framework: project.framework,
        target: target.id.clone(),
        target_display: target.display_name(),
        status: BuildStatus::Success.as_str().to_string(),
        capability: capability.clone(),
        plan: None,
        outcome: None,
        packages: Vec::new(),
        validations: Vec::new(),
        signing: Vec::new(),
        artifacts: Vec::new(),
        manifest_path: None,
    };

    if options.dry_run {
        report.status = "planned".into();
        report.plan = Some(plan);
        ui.event(
            crate::telemetry::EventKind::Summary,
            format!(
                "planned {} steps for {}",
                report.plan.as_ref().unwrap().steps.len(),
                target.id
            ),
            None,
            None,
            None,
        );
        return Ok(report);
    }

    ui.info(format!(
        "building {} for {} ({} steps)",
        project.name,
        target.id,
        plan.steps.len()
    ));

    std::fs::create_dir_all(host.data_dir())
        .map_err(|e| UbuilderError::io("cannot create data directory", e))?;
    let db = Arc::new(Db::open(&host.data_dir().join("ubuilder.db"))?);
    let cache_enabled = config.build.cache && !options.no_cache;
    let cache = Cache::open(&host.cache_dir(), cache_enabled)?;
    let executor = Executor::new(ExecutorConfig {
        log_dir: Some(work_dir.join("logs")),
        verbose: !ui.quiet(),
        ..ExecutorConfig::default()
    });
    let scheduler = Scheduler::new(
        executor,
        db.clone(),
        Some(Arc::new(cache)),
        host.clone(),
        state.fingerprint.clone(),
        SchedulerConfig {
            jobs: options.jobs.max(1),
            continue_on_error: options.continue_on_error,
            retry_attempts: 2,
        },
    );
    let outcome = scheduler.execute(&plan, &job_id, cancel, ui).await?;
    let status = outcome.status;
    report.status = status.as_str().to_string();
    report.outcome = Some(outcome.clone());
    if status == BuildStatus::Failed {
        report.plan = Some(plan);
        return Ok(report);
    }
    if status == BuildStatus::Cancelled {
        report.plan = Some(plan);
        return Ok(report);
    }

    let mut payload_artifacts = plan.expected_artifacts.clone();
    payload_artifacts.retain(|p| p.exists());
    if payload_artifacts.is_empty() {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            "build succeeded but produced no expected artifact",
        )
        .details(serde_json::json!({
            "expected": plan.expected_artifacts,
            "work_dir": work_dir,
        })));
    }

    let package_version = project.version.clone().unwrap_or_else(|| "0.0.0".into());
    let package_ctx = PackageContext {
        project_name: &project.name,
        version: &package_version,
        target,
        framework: project.framework,
        output_dir: &output_dir,
        work_dir: &work_dir,
        identifier: project_identifier(project),
    };
    let payload = packaging::prepare_payload(&package_ctx, &payload_artifacts)?;

    if options.release {
        let sign_target = payload
            .mac_app
            .clone()
            .or_else(|| payload.executable.clone())
            .ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::SigningUnsupported,
                    "payload has no executable to sign",
                )
            })?;
        let signing_options = SigningOptions {
            identity: config.release.identity.clone(),
            certificate: config.release.certificate.clone(),
            password_env: config.release.password_env.clone(),
            timestamp_url: config.release.timestamp_url.clone(),
            notarize: config.release.notarize,
        };
        let sign_report = signing::sign(
            &sign_target,
            target,
            config.release.signing,
            &signing_options,
        )?;
        let failed = sign_report.status == signing::SigningStatus::Failed
            || (sign_report.status == signing::SigningStatus::Unsupported
                && !config.release.allow_unsigned);
        report.signing.push(sign_report);
        if failed {
            report.plan = Some(plan);
            return Ok(report);
        }
    }

    let formats = if !options.formats.is_empty() {
        options.formats.clone()
    } else if !config.packaging.formats.is_empty() {
        config.packaging.formats.clone()
    } else {
        packaging::default_formats(target)
    };
    report.packages = packaging::package_all(&payload, &package_ctx, &formats)?;

    let store = ArtifactStore::new(&db);
    for package in &report.packages {
        let validation = validation::validate_artifact(&package.path, target)?;
        if validation.status == validation::ValidationStatus::Failed {
            report.status = "failed".into();
        }
        let record = store.record(
            &job_id,
            &target.id,
            project.framework.as_str(),
            package.format.as_str(),
            package.format.as_str(),
            &package.path,
            Some(validation.status.as_str()),
        )?;
        report.artifacts.push(ArtifactSummary {
            path: package.path.clone(),
            sha256: record.sha256,
            size: record.size,
            kind: package.format.as_str().into(),
            format: package.format.as_str().into(),
        });
        report.validations.push(validation);
    }

    let manifest_path = output_dir.join(format!("build-manifest-{}.json", target.id));
    let manifest = BuildManifest {
        schema_version: crate::artifacts::RECIPE_VERSION,
        build_id: build_id.clone(),
        project: project.name.clone(),
        framework: project.framework.as_str().into(),
        host: host.triple.clone(),
        target: target.id.clone(),
        profile: options.profile.clone(),
        started_at: outcome
            .steps
            .first()
            .map(|_| now_ms().saturating_sub(outcome.duration_ms))
            .unwrap_or_else(now_ms),
        finished_at: now_ms(),
        status: report.status.clone(),
        steps: outcome
            .steps
            .iter()
            .map(|s| ManifestStep {
                id: s.id.clone(),
                name: s.name.clone(),
                status: s.status.as_str().into(),
                exit_code: s.exit_code,
                duration_ms: s.duration_ms,
                log_path: s.log_path.clone(),
                error: s.error.as_ref().map(|e| e.message.clone()),
            })
            .collect(),
        artifacts: report.artifacts.clone(),
        validations: report
            .validations
            .iter()
            .map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null))
            .collect(),
        notes: plan.notes.clone(),
    };
    manifest.write(&manifest_path)?;
    db.finish_job(
        &job_id,
        &report.status,
        report
            .validations
            .iter()
            .find(|v| v.status == validation::ValidationStatus::Failed)
            .map(|v| format!("validation failed for {}", v.path.display()))
            .as_deref(),
        Some(&manifest_path),
        now_ms(),
    )?;
    report.manifest_path = Some(manifest_path);
    report.plan = Some(plan);
    Ok(report)
}

fn resolve_output_dir(project: &Project, config: &Config, options: &BuildOptions) -> PathBuf {
    if let Some(output) = &options.output {
        return absolute(project, output);
    }
    absolute(project, &config.packaging.output)
}

fn absolute(project: &Project, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project.root.join(path)
    }
}

fn project_identifier(project: &Project) -> Option<&str> {
    match &project.metadata {
        crate::project::ProjectMetadata::Tauri(meta) => meta.identifier.as_deref(),
        _ => None,
    }
}

fn slug(name: &str) -> String {
    let value: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    value.trim_matches('-').to_string()
}

/// Summary for `ubuilder build` output.
#[derive(Debug, Clone, Serialize)]
pub struct BuildSummary {
    pub project: String,
    pub framework: FrameworkKind,
    pub targets: Vec<TargetBuildReport>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_targets_defaults_to_native() {
        let host = Host::detect().unwrap();
        let targets = resolve_targets(&[], false, &host).unwrap();
        assert_eq!(targets.len(), 1);
        assert!(targets[0].is_native(&host));
    }

    #[test]
    fn resolve_targets_all_returns_seven() {
        let host = Host::detect().unwrap();
        assert_eq!(resolve_targets(&[], true, &host).unwrap().len(), 7);
    }

    #[test]
    fn slug_is_path_safe() {
        assert_eq!(slug("My App!"), "my-app");
    }
}
