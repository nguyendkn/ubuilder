//! Pipeline execution runtime.
//!
//! Jobs run in isolated workspaces, in dependency waves. Independent jobs in
//! the same wave execute concurrently, and every state transition is persisted
//! to the local database so `status`, `logs` and `history` can inspect runs.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::{CommandSpec, ExecutionOutput, Executor, ExecutorConfig};
use crate::host::Host;
use crate::pipeline::actions::{
    registry, with_defaults, ActionContext, BuildRecord, PackageRecord, PendingCacheSave,
};
use crate::pipeline::artifacts::{safe_join, ArtifactRef, ArtifactStore};
use crate::pipeline::cache::PipelineCache;
use crate::pipeline::events::{EventKind, Events};
use crate::pipeline::expression::context::Context;
use crate::pipeline::expression::Template;
use crate::pipeline::ir::{IrJob, IrStep, IrStepKind, WorkflowIr};
use crate::pipeline::planner;
use crate::pipeline::state::RunState;
use crate::secrets::SecretManager;
use crate::storage::{Db, PipelineJobRecord, PipelineRunRecord, PipelineStepRecord};
use crate::telemetry::{now_ms, Ui};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Options for one pipeline invocation.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Explicit run id; generated when absent.
    pub run_id: Option<String>,
    /// Workflow inputs exposed as `inputs.*`.
    pub inputs: IndexMap<String, String>,
    /// Overrides the root that holds per-job workspaces.
    pub workspace: Option<PathBuf>,
    /// Runs only this job and its transitive dependencies.
    pub job: Option<String>,
    /// Disables pipeline cache restore/save.
    pub no_cache: bool,
}

/// Final, serializable result of one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub run_id: String,
    pub workflow: String,
    pub source_path: PathBuf,
    pub source_digest: String,
    pub status: RunState,
    pub started_at: u128,
    pub finished_at: u128,
    pub duration_ms: u128,
    pub run_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub logs_dir: PathBuf,
    pub jobs: Vec<JobReport>,
    pub artifacts: Vec<ArtifactRef>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobReport {
    pub id: String,
    pub base_id: String,
    pub name: String,
    pub status: RunState,
    pub conclusion: Option<String>,
    pub started_at: u128,
    pub finished_at: u128,
    pub duration_ms: u128,
    pub workspace: PathBuf,
    pub log_dir: PathBuf,
    pub steps: Vec<StepReport>,
    pub outputs: IndexMap<String, String>,
    pub error: Option<String>,
    /// Raw values used for downstream `needs.*.outputs`; may contain a secret
    /// and is therefore never serialized.
    #[serde(skip)]
    pub summary_outputs: IndexMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    pub id: String,
    pub name: String,
    pub status: RunState,
    pub conclusion: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub log_path: Option<PathBuf>,
    pub outputs: IndexMap<String, String>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct JobSummary {
    status: RunState,
    outputs: IndexMap<String, String>,
}

struct Shared {
    run_id: String,
    ir: WorkflowIr,
    source_root: PathBuf,
    data_dir: PathBuf,
    host: Host,
    db: Arc<Db>,
    artifacts: ArtifactStore,
    cache: PipelineCache,
    ui: Ui,
    events: Events,
    cancel: CancellationToken,
    inputs: IndexMap<String, String>,
    workflow_env: IndexMap<String, String>,
    secret_values: BTreeMap<String, String>,
    secret_redactions: Vec<String>,
    jobs_root: PathBuf,
    logs_root: PathBuf,
    no_cache: bool,
    results: Mutex<BTreeMap<String, JobSummary>>,
    semaphores: HashMap<String, Arc<Semaphore>>,
}

impl Shared {
    fn summary(&self, job_id: &str) -> Option<JobSummary> {
        self.results
            .lock()
            .expect("pipeline result lock")
            .get(job_id)
            .cloned()
    }

    fn record_summary(&self, job_id: &str, summary: JobSummary) {
        self.results
            .lock()
            .expect("pipeline result lock")
            .insert(job_id.to_string(), summary);
    }
}

/// Executes a compiled workflow.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    ir: &WorkflowIr,
    source_root: &Path,
    host: &Host,
    data_dir: &Path,
    db: Arc<Db>,
    ui: &Ui,
    events: &Events,
    cancel: &CancellationToken,
    options: RunOptions,
) -> Result<RunReport> {
    let plan = planner::plan(ir, source_root, host)?;
    let selected = select_jobs(ir, options.job.as_deref())?;
    let run_id = match options.run_id.as_deref() {
        Some(id) => {
            validate_run_id(id)?;
            id.to_string()
        }
        None => generate_run_id(&ir.source_digest),
    };
    for warning in &plan.warnings {
        events.warning(&run_id, warning.message.clone());
    }
    let pipeline_root = data_dir.join("pipeline");
    let run_dir = pipeline_root.join("runs").join(&run_id);
    let logs_root = run_dir.join("logs");
    let workspace_root = options
        .workspace
        .clone()
        .unwrap_or_else(|| run_dir.join("jobs"));
    std::fs::create_dir_all(&logs_root)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", logs_root.display()), e))?;
    std::fs::create_dir_all(&workspace_root)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", workspace_root.display()), e))?;
    let cancel_token = cancel.clone();
    let cancel_file = crate::pipeline::history::cancel_marker(data_dir, &run_id);
    let _watcher = AbortOnDrop(tokio::spawn(async move {
        loop {
            if cancel_token.is_cancelled() {
                return;
            }
            if cancel_file.is_file() {
                cancel_token.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }));

    // Resolve only secrets named by the workflow. Values stay in memory and
    // are never written to the IR, plan, database or JSON report.
    let secret_manager = SecretManager::open(data_dir)?;
    let secret_representation = serde_json::to_string(ir)
        .map_err(|error| UbuilderError::new(ErrorCode::Internal, error.to_string()))?;
    let secret_values: BTreeMap<String, String> = secret_manager
        .resolve_referenced(&secret_representation)?
        .into_iter()
        .map(|(name, value)| (name, value.into_inner()))
        .collect();
    let secret_redactions: Vec<String> = secret_values.values().cloned().collect();
    let artifacts = ArtifactStore::open(&pipeline_root.join("artifacts"), db.clone())?;
    let cache_enabled = !options.no_cache;
    let cache = PipelineCache::open(
        &pipeline_root.join("cache"),
        cache_enabled,
        &host.display(),
        ir,
    )?;

    // Workflow-level environment is evaluated once, without matrix/job data.
    let workflow_context = base_context(
        &run_id,
        ir,
        &options.inputs,
        &IndexMap::new(),
        &IndexMap::new(),
        &[],
        &IndexMap::new(),
        &Context::new(),
        None,
        &secret_values,
    );
    let workflow_env = evaluate_env(&ir.env, &workflow_context)
        .map_err(|e| e.remediation("Check workflow-level env templates"))?;

    let mut semaphores = HashMap::new();
    for job in ir.jobs.iter().filter(|job| selected.contains(&job.id)) {
        // Fail-fast never changes the concurrency limit. Matrix siblings are
        // bounded by `max-parallel`, or by host parallelism when it is absent.
        // Already-running siblings finish; siblings that have not acquired a
        // permit are skipped once a failure is recorded.
        let default_limit = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(2)
            .max(2);
        let limit = job.max_parallel.unwrap_or(default_limit).max(1);
        semaphores
            .entry(job.base_id.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(limit)));
    }
    let shared = Arc::new(Shared {
        run_id: run_id.clone(),
        ir: ir.clone(),
        source_root: source_root.to_path_buf(),
        data_dir: data_dir.to_path_buf(),
        host: host.clone(),
        db: db.clone(),
        artifacts,
        cache,
        ui: ui.clone(),
        events: events.clone(),
        cancel: cancel.clone(),
        inputs: options.inputs.clone(),
        workflow_env,
        secret_values,
        secret_redactions,
        jobs_root: workspace_root.clone(),
        logs_root: logs_root.clone(),
        no_cache: options.no_cache,
        results: Mutex::new(BTreeMap::new()),
        semaphores,
    });

    let started_at = now_ms();
    db.insert_pipeline_run(&PipelineRunRecord {
        id: run_id.clone(),
        workflow: ir.name.clone(),
        source_path: ir.source_path.clone(),
        source_digest: ir.source_digest.clone(),
        status: RunState::Running.as_str().into(),
        created_at: started_at,
        started_at: Some(started_at),
        finished_at: None,
        host: host.display(),
        inputs_json: serde_json::to_string(&shared.inputs)
            .map_err(|e| UbuilderError::new(ErrorCode::Internal, e.to_string()))?,
        error: None,
    })?;
    for job in ir.jobs.iter().filter(|job| selected.contains(&job.id)) {
        persist_job(
            &db,
            &run_id,
            job,
            RunState::Pending,
            None,
            None,
            &IndexMap::new(),
        )?;
    }
    events.run_start(&run_id, &ir.name);

    let mut reports: HashMap<String, JobReport> = HashMap::new();
    for wave in &ir.waves {
        let wave_jobs: Vec<IrJob> = wave
            .iter()
            .filter(|id| selected.contains(*id))
            .filter_map(|id| ir.job(id).cloned())
            .collect();
        if wave_jobs.is_empty() {
            continue;
        }
        let mut set: JoinSet<JobReport> = JoinSet::new();
        for job in wave_jobs {
            let shared = shared.clone();
            set.spawn(async move { execute_job(shared, job).await });
        }
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(report) => {
                    let summary = JobSummary {
                        status: report.status,
                        outputs: report.summary_outputs.clone(),
                    };
                    shared.record_summary(&report.id, summary);
                    reports.insert(report.id.clone(), report);
                }
                Err(error) => {
                    return Err(UbuilderError::new(
                        ErrorCode::Internal,
                        format!("pipeline job task failed: {error}"),
                    ));
                }
            }
        }
        if cancel.is_cancelled() {
            break;
        }
    }

    let mut jobs = Vec::new();
    for id in &ir.order {
        if !selected.contains(id) {
            continue;
        }
        if let Some(report) = reports.remove(id) {
            jobs.push(report);
        }
    }
    // Jobs from later waves that never spawned (cancellation) are recorded.
    for job in ir.jobs.iter().filter(|job| selected.contains(&job.id)) {
        if !jobs.iter().any(|report| report.id == job.id) {
            let report = cancelled_report(&shared, job);
            jobs.push(report);
        }
    }
    jobs.sort_by_key(|report| {
        ir.order
            .iter()
            .position(|id| *id == report.id)
            .unwrap_or(usize::MAX)
    });

    let finished_at = now_ms();
    let cancelled = cancel.is_cancelled();
    let failed = jobs
        .iter()
        .any(|job| matches!(job.status, RunState::Failed | RunState::Cancelled));
    let status = if cancelled {
        RunState::Cancelled
    } else if failed {
        RunState::Failed
    } else {
        RunState::Succeeded
    };
    let error = jobs
        .iter()
        .find(|job| job.error.is_some())
        .and_then(|job| job.error.clone())
        .map(|message| crate::security::redact(&message, &shared.secret_redactions));
    db.update_pipeline_run(
        &run_id,
        status.as_str(),
        error.as_deref(),
        Some(finished_at),
    )?;
    let artifacts = shared
        .db
        .pipeline_artifacts(&run_id)?
        .into_iter()
        .map(|record| {
            let kind = artifact_kind(&record.path);
            ArtifactRef {
                name: record.name,
                sha256: record.sha256,
                size: record.size,
                path: record.path,
                kind,
            }
        })
        .collect::<Vec<_>>();
    let duration_ms = finished_at.saturating_sub(started_at);
    events.run_finish(&run_id, status.as_str(), duration_ms);
    Ok(RunReport {
        run_id,
        workflow: ir.name.clone(),
        source_path: ir.source_path.clone(),
        source_digest: ir.source_digest.clone(),
        status,
        started_at,
        finished_at,
        duration_ms,
        run_dir,
        workspace_root,
        logs_dir: logs_root,
        jobs,
        artifacts,
        error,
    })
}

fn select_jobs(ir: &WorkflowIr, filter: Option<&str>) -> Result<HashSet<String>> {
    let Some(filter) = filter else {
        return Ok(ir.jobs.iter().map(|job| job.id.clone()).collect());
    };
    if !ir.jobs.iter().any(|job| job.id == filter) {
        return Err(
            UbuilderError::new(ErrorCode::Usage, format!("unknown job '{filter}'")).remediation(
                format!(
                    "Available jobs: {}",
                    ir.jobs
                        .iter()
                        .map(|job| job.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
        );
    }
    let mut selected = HashSet::new();
    let mut stack = vec![filter.to_string()];
    while let Some(id) = stack.pop() {
        if !selected.insert(id.clone()) {
            continue;
        }
        if let Some(job) = ir.job(&id) {
            stack.extend(job.needs.iter().cloned());
        }
    }
    Ok(selected)
}

fn generate_run_id(digest: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("run-{nanos:x}-{}", &digest[..digest.len().min(8)])
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

fn safe_component(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push_str(&format!("_{:x}", ch as u32));
        }
    }
    if out.is_empty() {
        "job".into()
    } else {
        out
    }
}

#[allow(clippy::too_many_arguments)]
fn base_context(
    run_id: &str,
    ir: &WorkflowIr,
    inputs: &IndexMap<String, String>,
    matrix: &IndexMap<String, Value>,
    needs: &IndexMap<String, Value>,
    steps: &[StepReport],
    env: &IndexMap<String, String>,
    parent: &Context,
    job: Option<&IrJob>,
    secrets: &BTreeMap<String, String>,
) -> Context {
    let mut context = Context::new();
    context.success = parent.success;
    context.failure = parent.failure;
    context.cancelled = parent.cancelled;
    let runner = job.map(|job| {
        json!({
            "os": job.runner.os,
            "arch": job.runner.arch,
            "native": job.runner.native,
            "backend": job.runner.backend.as_str(),
        })
    });
    if let Some(runner) = runner {
        context.insert("runner", runner);
    }
    if let Some(job) = job {
        context.insert(
            "job",
            json!({
                "id": job.id,
                "base_id": job.base_id,
                "name": job.name,
            }),
        );
        context.insert(
            "matrix",
            Value::Object(matrix.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
        );
    }
    context.insert(
        "run",
        json!({ "id": run_id, "workflow": ir.name, "source_path": ir.source_path }),
    );
    context.insert(
        "inputs",
        Value::Object(inputs.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
    );
    context.insert(
        "needs",
        Value::Object(needs.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
    );
    let mut steps_value = serde_json::Map::new();
    for step in steps {
        steps_value.insert(
            step.id.clone(),
            json!({
                "outputs": step.outputs,
                "status": step.status,
                "conclusion": step.conclusion,
                "outcome": step.status,
            }),
        );
    }
    context.insert("steps", Value::Object(steps_value));
    context.insert(
        "env",
        Value::Object(env.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
    );
    context.insert(
        "secrets",
        Value::Object(secrets.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
    );
    context
}

fn needs_value(shared: &Shared, job: &IrJob) -> IndexMap<String, Value> {
    let mut needs = IndexMap::new();
    for need in &job.needs {
        let summary = shared.summary(need).unwrap_or(JobSummary {
            status: RunState::Skipped,
            outputs: IndexMap::new(),
        });
        needs.insert(
            need.clone(),
            json!({
                "result": summary.status,
                "outputs": summary.outputs,
            }),
        );
    }
    needs
}

fn redact_map(values: &IndexMap<String, String>, secrets: &[String]) -> IndexMap<String, String> {
    values
        .iter()
        .map(|(key, value)| (key.clone(), crate::security::redact(value, secrets)))
        .collect()
}

fn evaluate_env(
    templates: &IndexMap<String, Template>,
    context: &Context,
) -> Result<IndexMap<String, String>> {
    let mut env = IndexMap::new();
    for (key, template) in templates {
        let value = template
            .evaluate(context)
            .map_err(|error| error.remediation(format!("Check the value of env '{key}'")))?;
        env.insert(key.clone(), value);
    }
    Ok(env)
}

async fn execute_job(shared: Arc<Shared>, job: IrJob) -> JobReport {
    let started_at = now_ms();
    let workspace = shared.jobs_root.join(safe_component(&job.id));
    let log_dir = shared.logs_root.join(safe_component(&job.id));
    let mut report = JobReport {
        id: job.id.clone(),
        base_id: job.base_id.clone(),
        name: job.name.clone(),
        status: RunState::Pending,
        conclusion: None,
        started_at,
        finished_at: started_at,
        duration_ms: 0,
        workspace: workspace.clone(),
        log_dir: log_dir.clone(),
        steps: Vec::new(),
        outputs: IndexMap::new(),
        error: None,
        summary_outputs: IndexMap::new(),
    };

    if shared.cancel.is_cancelled() {
        return finish_skipped(
            shared.as_ref(),
            job,
            report,
            RunState::Cancelled,
            "run cancelled",
        );
    }
    if job.fail_fast
        && shared.ir.jobs.iter().any(|candidate| {
            candidate.base_id == job.base_id
                && shared
                    .summary(&candidate.id)
                    .map(|summary| summary.status == RunState::Failed)
                    .unwrap_or(false)
        })
    {
        return finish_skipped(
            shared.as_ref(),
            job,
            report,
            RunState::Skipped,
            "skipped because a matrix sibling failed",
        );
    }

    let permit = match shared.semaphores.get(&job.base_id) {
        Some(semaphore) => semaphore.clone().acquire_owned().await.ok(),
        None => None,
    };
    if job.fail_fast
        && shared.ir.jobs.iter().any(|candidate| {
            candidate.base_id == job.base_id
                && shared
                    .summary(&candidate.id)
                    .map(|summary| summary.status == RunState::Failed)
                    .unwrap_or(false)
        })
    {
        return finish_skipped(
            shared.as_ref(),
            job,
            report,
            RunState::Skipped,
            "skipped because a matrix sibling failed",
        );
    }

    if let Err(error) = reset_dir(&workspace) {
        return fail_job_before_start(shared.as_ref(), job, report, error);
    }
    if let Err(error) = std::fs::create_dir_all(&log_dir) {
        return fail_job_before_start(
            shared.as_ref(),
            job,
            report,
            UbuilderError::io(format!("cannot create {}", log_dir.display()), error),
        );
    }

    report.status = RunState::Running;
    let _ = persist_job(
        &shared.db,
        &shared.run_id,
        &job,
        RunState::Running,
        Some(started_at),
        None,
        &IndexMap::new(),
    );
    shared.events.job(
        EventKind::JobStart,
        &shared.run_id,
        &job.id,
        format!("job started on {}", job.runner.os),
    );

    let needs = needs_value(&shared, &job);
    let needs_ok = job.needs.iter().all(|need| {
        shared
            .summary(need)
            .map(|s| s.status == RunState::Succeeded)
            .unwrap_or(false)
    });
    let mut context = base_context(
        &shared.run_id,
        &shared.ir,
        &shared.inputs,
        &job.matrix,
        &needs,
        &[],
        &shared.workflow_env,
        &Context::new(),
        Some(&job),
        &shared.secret_values,
    );
    context.success = needs_ok;
    context.failure = !needs_ok;
    context.insert("workspace", json!(workspace.to_string_lossy()));

    let job_env = match evaluate_env(&job.env, &context) {
        Ok(env) => {
            let mut merged = shared.workflow_env.clone();
            merged.extend(env);
            merged
        }
        Err(error) => {
            return fail_job_before_start(shared.as_ref(), job, report, error);
        }
    };
    context.insert(
        "env",
        Value::Object(job_env.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
    );

    if let Some(condition) = &job.condition {
        match condition.evaluate_bool(&context) {
            Ok(true) => {}
            Ok(false) => {
                return finish_skipped(
                    shared.as_ref(),
                    job,
                    report,
                    RunState::Skipped,
                    "job condition evaluated to false",
                );
            }
            Err(error) => {
                return fail_job_before_start(
                    shared.as_ref(),
                    job,
                    report,
                    error.remediation("Check the job-level if expression"),
                );
            }
        }
    } else if !needs_ok {
        return finish_skipped(
            shared.as_ref(),
            job,
            report,
            RunState::Skipped,
            "dependency did not succeed",
        );
    }

    let lock = match acquire_concurrency(&shared, &job, &workspace).await {
        Ok(lock) => lock,
        Err(error) => {
            return fail_job_before_start(shared.as_ref(), job, report, error);
        }
    };

    let mut attempts = 0u32;
    let max_attempts = job
        .retry
        .as_ref()
        .map(|retry| retry.max_attempts)
        .unwrap_or(1);
    let outcome = loop {
        attempts += 1;
        if attempts > 1 {
            let _ = reset_dir(&workspace);
        }
        let body = execute_job_body(
            &shared, &job, &workspace, &log_dir, &needs, &job_env, &context,
        );
        let result = match job.timeout_secs {
            Some(secs) => match tokio::time::timeout(Duration::from_secs(secs), body).await {
                Ok(result) => result,
                Err(_) => JobOutcome::failed(UbuilderError::new(
                    ErrorCode::Timeout,
                    format!("job '{}' exceeded its {}s timeout", job.id, secs),
                )),
            },
            None => body.await,
        };
        let failed = result.job_failed || result.error.is_some();
        if !failed || attempts >= max_attempts || shared.cancel.is_cancelled() {
            break result;
        }
        let delay = job
            .retry
            .as_ref()
            .map(|retry| retry.delay_for(attempts))
            .unwrap_or_default();
        shared.events.job(
            EventKind::Retry,
            &shared.run_id,
            &job.id,
            format!(
                "attempt {attempts}/{max_attempts} failed; retrying in {} ms",
                delay.as_millis()
            ),
        );
        tokio::time::sleep(delay).await;
    };

    if let Some(group) = lock {
        let _ = shared
            .db
            .release_concurrency_slot(&group, &format!("{}/{}", shared.run_id, job.id));
    }

    report.steps = outcome.steps;
    report.summary_outputs = outcome.outputs.clone();
    report.outputs = redact_map(&outcome.outputs, &shared.secret_redactions);
    report.error = outcome
        .error
        .map(|error| crate::security::redact(&error.message, &shared.secret_redactions));
    report.status = if shared.cancel.is_cancelled() {
        RunState::Cancelled
    } else if outcome.job_failed || report.error.is_some() {
        RunState::Failed
    } else {
        RunState::Succeeded
    };
    report.conclusion = Some(report.status.as_str().into());
    report.finished_at = now_ms();
    report.duration_ms = report.finished_at.saturating_sub(started_at);
    let _ = persist_job(
        &shared.db,
        &shared.run_id,
        &job,
        report.status,
        Some(started_at),
        Some(report.finished_at),
        &report.outputs,
    );
    shared.events.job(
        EventKind::JobFinish,
        &shared.run_id,
        &job.id,
        format!(
            "job finished: {} ({} ms)",
            report.status, report.duration_ms
        ),
    );
    shared.record_summary(
        &report.id,
        JobSummary {
            status: report.status,
            outputs: report.summary_outputs.clone(),
        },
    );
    drop(permit);
    report
}

struct JobOutcome {
    steps: Vec<StepReport>,
    outputs: IndexMap<String, String>,
    job_failed: bool,
    error: Option<UbuilderError>,
}

impl JobOutcome {
    fn failed(error: UbuilderError) -> Self {
        Self {
            steps: Vec::new(),
            outputs: IndexMap::new(),
            job_failed: true,
            error: Some(error),
        }
    }
}

struct StepExecution {
    status: RunState,
    conclusion: Option<String>,
    exit_code: Option<i32>,
    duration_ms: u128,
    log_path: Option<PathBuf>,
    outputs: IndexMap<String, String>,
    stdout_tail: String,
    stderr_tail: String,
    error: Option<UbuilderError>,
}

impl StepExecution {
    fn ok() -> Self {
        Self {
            status: RunState::Succeeded,
            conclusion: Some("success".into()),
            exit_code: Some(0),
            duration_ms: 0,
            log_path: None,
            outputs: IndexMap::new(),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            error: None,
        }
    }

    fn failed(error: UbuilderError) -> Self {
        Self {
            status: RunState::Failed,
            conclusion: Some("failure".into()),
            exit_code: None,
            duration_ms: 0,
            log_path: None,
            outputs: IndexMap::new(),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            error: Some(error),
        }
    }

    fn cancelled() -> Self {
        Self {
            status: RunState::Cancelled,
            conclusion: Some("cancelled".into()),
            exit_code: None,
            duration_ms: 0,
            log_path: None,
            outputs: IndexMap::new(),
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            error: Some(UbuilderError::new(
                ErrorCode::Cancelled,
                "pipeline cancelled",
            )),
        }
    }
}

async fn execute_job_body(
    shared: &Shared,
    job: &IrJob,
    workspace: &Path,
    log_dir: &Path,
    needs: &IndexMap<String, Value>,
    job_env: &IndexMap<String, String>,
    job_context: &Context,
) -> JobOutcome {
    let mut steps: Vec<StepReport> = Vec::new();
    let mut builds: Vec<BuildRecord> = Vec::new();
    let mut packages: Vec<PackageRecord> = Vec::new();
    let mut pending_cache: Vec<PendingCacheSave> = Vec::new();
    let mut job_failed = false;
    let executor = Executor::new(ExecutorConfig {
        allowlist: crate::config::default_env_allowlist(),
        log_dir: Some(log_dir.to_path_buf()),
        verbose: shared.ui.verbose(),
    })
    .with_secrets(shared.secret_redactions.clone());

    for step in &job.steps {
        if shared.cancel.is_cancelled() {
            let mut report = skipped_step(step, RunState::Cancelled);
            report.conclusion = Some("cancelled".into());
            persist_step(shared, job, report.clone());
            steps.push(report);
            continue;
        }
        let mut context = base_context(
            &shared.run_id,
            &shared.ir,
            &shared.inputs,
            &job.matrix,
            needs,
            &steps,
            job_env,
            job_context,
            Some(job),
            &shared.secret_values,
        );
        context.success = !job_failed;
        context.failure = job_failed;
        context.cancelled = shared.cancel.is_cancelled();
        context.insert("workspace", json!(workspace.to_string_lossy()));

        let step_env = match evaluate_env(&step.env, &context) {
            Ok(env) => {
                let mut merged = job_env.clone();
                merged.extend(env);
                merged
            }
            Err(error) => {
                job_failed = true;
                let report = step_failure(step, error);
                persist_step(shared, job, report.clone());
                steps.push(report);
                continue;
            }
        };
        context.insert(
            "env",
            Value::Object(
                step_env
                    .iter()
                    .map(|(k, v)| (k.clone(), json!(v)))
                    .collect(),
            ),
        );

        let should_run = match &step.condition {
            Some(condition) => match condition.evaluate_bool(&context) {
                Ok(value) => value,
                Err(error) => {
                    job_failed = true;
                    let report =
                        step_failure(step, error.remediation("Check this step's if expression"));
                    persist_step(shared, job, report.clone());
                    steps.push(report);
                    continue;
                }
            },
            None => !job_failed,
        };
        if !should_run {
            let report = skipped_step(step, RunState::Skipped);
            persist_step(shared, job, report.clone());
            shared.events.step(
                EventKind::StepSkipped,
                &shared.run_id,
                &job.id,
                &step.id,
                "step skipped by condition".into(),
            );
            steps.push(report);
            continue;
        }

        shared.events.step(
            EventKind::StepStart,
            &shared.run_id,
            &job.id,
            &step.id,
            format!("step started: {}", step.name),
        );
        let started = now_ms();
        let mut execution = run_step_with_retries(
            shared,
            job,
            step,
            workspace,
            log_dir,
            &executor,
            &context,
            &step_env,
            &mut builds,
            &mut packages,
            &mut pending_cache,
        )
        .await;
        execution.duration_ms = now_ms().saturating_sub(started);
        if step.continue_on_error && execution.status == RunState::Failed {
            execution.conclusion = Some("success".into());
        }
        let failed = matches!(execution.status, RunState::Failed | RunState::Cancelled);
        if failed && !step.continue_on_error {
            job_failed = true;
        }
        let mut report = StepReport {
            id: step.id.clone(),
            name: step.name.clone(),
            status: execution.status,
            conclusion: execution.conclusion.clone(),
            exit_code: execution.exit_code,
            duration_ms: execution.duration_ms,
            log_path: execution.log_path.clone(),
            outputs: execution.outputs.clone(),
            stdout_tail: execution.stdout_tail.clone(),
            stderr_tail: execution.stderr_tail.clone(),
            error: execution
                .error
                .as_ref()
                .map(|error| crate::security::redact(&error.message, &shared.secret_redactions)),
        };
        report.conclusion = Some(
            execution
                .conclusion
                .clone()
                .unwrap_or_else(|| execution.status.as_str().into()),
        );
        persist_step(shared, job, report.clone());
        shared.events.step(
            if report.status == RunState::Succeeded {
                EventKind::StepFinish
            } else {
                EventKind::Error
            },
            &shared.run_id,
            &job.id,
            &step.id,
            format!(
                "step finished: {} ({} ms)",
                report.status, report.duration_ms
            ),
        );
        if let Some(error) = &execution.error {
            shared.events.error(&shared.run_id, error.message.clone());
        }
        steps.push(report);
    }

    for pending in &pending_cache {
        if shared.no_cache || shared.cancel.is_cancelled() {
            continue;
        }
        match shared.cache.save(&pending.key, workspace, &pending.path) {
            Ok(()) => shared.events.emit(crate::pipeline::events::PipelineEvent {
                event: EventKind::CacheMiss,
                timestamp: now_ms(),
                run_id: Some(shared.run_id.clone()),
                job_id: Some(job.id.clone()),
                step_id: None,
                message: format!("cache saved: {}", pending.key),
                data: json!({ "key": pending.key, "path": pending.path }),
            }),
            Err(error) => shared
                .events
                .warning(&shared.run_id, format!("cache save failed: {error}")),
        }
    }

    let mut outputs = IndexMap::new();
    if !job_failed && !shared.cancel.is_cancelled() {
        let mut context = base_context(
            &shared.run_id,
            &shared.ir,
            &shared.inputs,
            &job.matrix,
            needs,
            &steps,
            job_env,
            job_context,
            Some(job),
            &shared.secret_values,
        );
        context.success = true;
        context.insert("workspace", json!(workspace.to_string_lossy()));
        for (key, template) in &job.outputs {
            match template.evaluate(&context) {
                Ok(value) => {
                    outputs.insert(key.clone(), value);
                }
                Err(error) => {
                    job_failed = true;
                    return JobOutcome {
                        steps,
                        outputs,
                        job_failed,
                        error: Some(
                            error.remediation(format!("Check output '{key}' of job '{}'", job.id)),
                        ),
                    };
                }
            }
        }
    }
    let error = if job_failed {
        steps
            .iter()
            .find(|step| step.error.is_some())
            .and_then(|step| step.error.clone())
            .map(|message| UbuilderError::new(ErrorCode::ExecutionFailed, message))
    } else {
        None
    };
    JobOutcome {
        steps,
        outputs,
        job_failed,
        error,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_step_with_retries(
    shared: &Shared,
    job: &IrJob,
    step: &IrStep,
    workspace: &Path,
    log_dir: &Path,
    executor: &Executor,
    context: &Context,
    env: &IndexMap<String, String>,
    builds: &mut Vec<BuildRecord>,
    packages: &mut Vec<PackageRecord>,
    pending_cache: &mut Vec<PendingCacheSave>,
) -> StepExecution {
    let max_attempts = step
        .retry
        .as_ref()
        .map(|retry| retry.max_attempts)
        .unwrap_or(1);
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let result = match step.timeout_secs {
            Some(secs) => {
                match tokio::time::timeout(
                    Duration::from_secs(secs),
                    run_step_once(
                        shared,
                        job,
                        step,
                        workspace,
                        log_dir,
                        executor,
                        context,
                        env,
                        builds,
                        packages,
                        pending_cache,
                    ),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => StepExecution::failed(UbuilderError::new(
                        ErrorCode::Timeout,
                        format!("step '{}' exceeded its {}s timeout", step.id, secs),
                    )),
                }
            }
            None => {
                run_step_once(
                    shared,
                    job,
                    step,
                    workspace,
                    log_dir,
                    executor,
                    context,
                    env,
                    builds,
                    packages,
                    pending_cache,
                )
                .await
            }
        };
        if result.status == RunState::Succeeded
            || attempt >= max_attempts
            || shared.cancel.is_cancelled()
        {
            return result;
        }
        let delay = step
            .retry
            .as_ref()
            .map(|retry| retry.delay_for(attempt))
            .unwrap_or_default();
        shared.events.step(
            EventKind::Retry,
            &shared.run_id,
            &job.id,
            &step.id,
            format!(
                "attempt {attempt}/{max_attempts} failed; retrying in {} ms",
                delay.as_millis()
            ),
        );
        tokio::time::sleep(delay).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_step_once(
    shared: &Shared,
    job: &IrJob,
    step: &IrStep,
    workspace: &Path,
    log_dir: &Path,
    executor: &Executor,
    context: &Context,
    env: &IndexMap<String, String>,
    builds: &mut Vec<BuildRecord>,
    packages: &mut Vec<PackageRecord>,
    pending_cache: &mut Vec<PendingCacheSave>,
) -> StepExecution {
    if shared.cancel.is_cancelled() {
        return StepExecution::cancelled();
    }
    match &step.kind {
        IrStepKind::Run {
            command,
            shell,
            working_directory,
        } => {
            let script = match command.evaluate(context) {
                Ok(script) => script,
                Err(error) => return StepExecution::failed(error),
            };
            let cwd = match working_directory {
                Some(template) => match template.evaluate(context) {
                    Ok(raw) => match safe_join(workspace, &raw) {
                        Ok(path) => path,
                        Err(error) => return StepExecution::failed(error),
                    },
                    Err(error) => return StepExecution::failed(error),
                },
                None => workspace.to_path_buf(),
            };
            if !cwd.is_dir() {
                return StepExecution::failed(
                    UbuilderError::new(
                        ErrorCode::ConfigInvalid,
                        format!("working directory {} does not exist", cwd.display()),
                    )
                    .remediation("Create it in an earlier step or fix working-directory"),
                );
            }
            run_command(shared, job, step, executor, shell, &script, &cwd, env).await
        }
        IrStepKind::Action { uses, with } => {
            let Some(action) = registry::get(uses) else {
                return StepExecution::failed(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!("unknown action '{uses}'"),
                ));
            };
            let mut raw_inputs = IndexMap::new();
            for (key, template) in with {
                match template.evaluate(context) {
                    Ok(value) => {
                        raw_inputs.insert(key.clone(), value);
                    }
                    Err(error) => return StepExecution::failed(error),
                }
            }
            let inputs = with_defaults(action.inputs(), &raw_inputs);
            if let Err(error) = action.validate(&inputs) {
                return StepExecution::failed(error);
            }
            let mut action_context = ActionContext {
                run_id: &shared.run_id,
                job_id: &job.id,
                workspace,
                source_root: &shared.source_root,
                host: &shared.host,
                env,
                builds,
                packages,
                pending_cache,
                artifact_store: &shared.artifacts,
                cache: &shared.cache,
                ui: &shared.ui,
                events: &shared.events,
                cancel: &shared.cancel,
            };
            let outcome = match action.execute(&mut action_context, &inputs).await {
                Ok(outcome) => outcome,
                Err(error) => return StepExecution::failed(error),
            };
            if !outcome.message.is_empty() {
                shared.ui.info(format!("  {}", outcome.message));
            }
            for artifact in &outcome.artifacts {
                shared.events.emit(crate::pipeline::events::PipelineEvent {
                    event: EventKind::Artifact,
                    timestamp: now_ms(),
                    run_id: Some(shared.run_id.clone()),
                    job_id: Some(job.id.clone()),
                    step_id: Some(step.id.clone()),
                    message: format!("artifact uploaded: {}", artifact.name),
                    data: json!({
                        "name": artifact.name,
                        "sha256": artifact.sha256,
                        "size": artifact.size,
                    }),
                });
            }
            let mut execution = StepExecution::ok();
            execution.outputs = outcome.outputs;
            // Actions do not spawn a shell, but their message is still the
            // step's observable output. Persist it like command output.
            execution.stdout_tail =
                crate::security::redact(&outcome.message, &shared.secret_redactions);
            execution.log_path = write_action_log(log_dir, &step.id, &execution.stdout_tail);
            if let Some(hit) = execution.outputs.get("cache-hit") {
                shared.events.step(
                    if hit == "true" {
                        EventKind::CacheHit
                    } else {
                        EventKind::CacheMiss
                    },
                    &shared.run_id,
                    &job.id,
                    &step.id,
                    format!("cache {}", if hit == "true" { "hit" } else { "miss" }),
                );
            }
            execution
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_command(
    shared: &Shared,
    job: &IrJob,
    step: &IrStep,
    executor: &Executor,
    shell: &str,
    script: &str,
    cwd: &Path,
    env: &IndexMap<String, String>,
) -> StepExecution {
    let (program, args) = match shell_invocation(shell) {
        Ok(value) => value,
        Err(error) => return StepExecution::failed(error),
    };
    let mut spec = CommandSpec::new(program, format!("{}: {}", job.id, step.name))
        .args(args)
        .cwd(cwd)
        .timeout_secs(step.timeout_secs.unwrap_or(0));
    if step.timeout_secs.is_none() {
        spec.timeout_secs = None;
    }
    spec = spec.arg(script.to_string());
    for (key, value) in env {
        spec = spec.env(key.clone(), value.clone());
    }
    spec = spec
        .env("UBUILDER_RUN_ID", shared.run_id.clone())
        .env("UBUILDER_JOB_ID", job.id.clone())
        .env("UBUILDER_WORKSPACE", cwd.to_string_lossy().to_string())
        .env("UBUILDER_RUNNER_OS", job.runner.os.clone())
        .env("UBUILDER_RUNNER_ARCH", job.runner.arch.clone());
    let output = match executor
        .run(&spec, &shared.cancel, &shared.ui, &step.id)
        .await
    {
        Ok(output) => output,
        Err(error) => return StepExecution::failed(error),
    };
    output_to_step(output, script)
}

fn write_action_log(log_dir: &Path, step_id: &str, message: &str) -> Option<PathBuf> {
    if message.is_empty() {
        return None;
    }
    std::fs::create_dir_all(log_dir).ok()?;
    let name: String = step_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let path = log_dir.join(format!("{name}.log"));
    std::fs::write(&path, format!("{message}\n")).ok()?;
    Some(path)
}

fn output_to_step(output: ExecutionOutput, script: &str) -> StepExecution {
    let mut execution = StepExecution::ok();
    execution.duration_ms = output.duration_ms;
    execution.exit_code = output.exit_code;
    execution.log_path = output.log_path;
    execution.stdout_tail = output.stdout_tail;
    execution.stderr_tail = output.stderr_tail;
    if output.timed_out {
        execution.status = RunState::Failed;
        execution.conclusion = Some("failure".into());
        execution.error = Some(UbuilderError::new(
            ErrorCode::Timeout,
            "command exceeded its timeout",
        ));
    } else if output.cancelled {
        execution.status = RunState::Cancelled;
        execution.conclusion = Some("cancelled".into());
        execution.error = Some(UbuilderError::new(
            ErrorCode::Cancelled,
            "command cancelled",
        ));
    } else if output.exit_code != Some(0) {
        execution.status = RunState::Failed;
        execution.conclusion = Some("failure".into());
        execution.error = Some(
            UbuilderError::new(
                ErrorCode::ExecutionFailed,
                format!(
                    "command exited with code {}",
                    output
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "unknown".into())
                ),
            )
            .details(json!({
                "exit_code": output.exit_code,
                "command": script,
                "stdout": execution.stdout_tail.clone(),
                "stderr": execution.stderr_tail.clone(),
            })),
        );
    }
    execution
}

fn shell_invocation(shell: &str) -> Result<(String, Vec<String>)> {
    let shell = shell.trim();
    match shell {
        "powershell" => Ok((
            "powershell".into(),
            vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
            ],
        )),
        "pwsh" => Ok((
            "pwsh".into(),
            vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
            ],
        )),
        "cmd" => Ok(("cmd".into(), vec!["/D".into(), "/S".into(), "/C".into()])),
        "bash" => Ok(("bash".into(), vec!["-c".into()])),
        "sh" => Ok(("sh".into(), vec!["-c".into()])),
        "python" => Ok(("python".into(), vec!["-c".into()])),
        other => Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("unsupported shell '{other}'"),
        )
        .remediation("Use sh, bash, pwsh, powershell, cmd or python")),
    }
}

async fn acquire_concurrency(
    shared: &Shared,
    job: &IrJob,
    _workspace: &Path,
) -> Result<Option<String>> {
    let Some(policy) = &job.concurrency else {
        return Ok(None);
    };
    let Some(group) = &policy.group else {
        return Ok(None);
    };
    let max = policy.max.unwrap_or(1).max(1);
    let owner = format!("{}/{}", shared.run_id, job.id);
    let mut announced = false;
    let mut cancelled = std::collections::BTreeSet::new();
    loop {
        if shared.cancel.is_cancelled() {
            return Err(UbuilderError::new(
                ErrorCode::Cancelled,
                "pipeline cancelled while waiting for concurrency group",
            ));
        }
        if shared
            .db
            .acquire_concurrency_slot(group, &owner, max, now_ms(), 3_600_000)?
            .is_some()
        {
            return Ok(Some(group.clone()));
        }
        if policy.cancel_in_progress {
            for peer in shared.db.concurrency_owners(group)? {
                let run_id = peer.split('/').next().unwrap_or_default();
                if run_id.is_empty()
                    || run_id == shared.run_id
                    || !cancelled.insert(run_id.to_string())
                {
                    continue;
                }
                let marker = crate::pipeline::history::cancel_marker(&shared.data_dir, run_id);
                if let Some(parent) = marker.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&marker, b"cancel-in-progress");
                shared.events.job(
                    EventKind::Warning,
                    &shared.run_id,
                    &job.id,
                    format!("cancelling run '{run_id}' holding concurrency group '{group}'"),
                );
            }
        }
        if !announced {
            announced = true;
            shared.events.job(
                EventKind::Warning,
                &shared.run_id,
                &job.id,
                format!("waiting for concurrency group '{group}'"),
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn finish_skipped(
    shared: &Shared,
    job: IrJob,
    mut report: JobReport,
    status: RunState,
    reason: &str,
) -> JobReport {
    report.status = status;
    report.conclusion = Some(status.as_str().into());
    report.finished_at = now_ms();
    report.duration_ms = report.finished_at.saturating_sub(report.started_at);
    for step in &job.steps {
        let step_report = skipped_step(step, RunState::Skipped);
        persist_step(shared, &job, step_report.clone());
        report.steps.push(step_report);
    }
    report.error = if status == RunState::Cancelled {
        Some(reason.into())
    } else {
        None
    };
    let _ = persist_job(
        &shared.db,
        &shared.run_id,
        &job,
        status,
        Some(report.started_at),
        Some(report.finished_at),
        &IndexMap::new(),
    );
    shared.events.job(
        EventKind::JobFinish,
        &shared.run_id,
        &job.id,
        format!("job {status}: {reason}"),
    );
    report
}

fn fail_job_before_start(
    shared: &Shared,
    job: IrJob,
    mut report: JobReport,
    error: UbuilderError,
) -> JobReport {
    report.status = RunState::Failed;
    report.conclusion = Some("failure".into());
    report.error = Some(crate::security::redact(
        &error.message,
        &shared.secret_redactions,
    ));
    report.finished_at = now_ms();
    report.duration_ms = report.finished_at.saturating_sub(report.started_at);
    for step in &job.steps {
        let step_report = skipped_step(step, RunState::Skipped);
        persist_step(shared, &job, step_report.clone());
        report.steps.push(step_report);
    }
    let _ = persist_job(
        &shared.db,
        &shared.run_id,
        &job,
        RunState::Failed,
        Some(report.started_at),
        Some(report.finished_at),
        &IndexMap::new(),
    );
    shared.events.error(&shared.run_id, error.message.clone());
    report
}

fn skipped_step(step: &IrStep, status: RunState) -> StepReport {
    StepReport {
        id: step.id.clone(),
        name: step.name.clone(),
        status,
        conclusion: Some(status.as_str().into()),
        exit_code: None,
        duration_ms: 0,
        log_path: None,
        outputs: IndexMap::new(),
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        error: None,
    }
}

fn step_failure(step: &IrStep, error: UbuilderError) -> StepReport {
    let mut report = skipped_step(step, RunState::Failed);
    report.error = Some(error.message);
    report
}

fn cancelled_report(shared: &Shared, job: &IrJob) -> JobReport {
    let started = now_ms();
    let report = JobReport {
        id: job.id.clone(),
        base_id: job.base_id.clone(),
        name: job.name.clone(),
        status: RunState::Pending,
        conclusion: None,
        started_at: started,
        finished_at: started,
        duration_ms: 0,
        workspace: shared.jobs_root.join(safe_component(&job.id)),
        log_dir: shared.logs_root.join(safe_component(&job.id)),
        steps: Vec::new(),
        outputs: IndexMap::new(),
        error: None,
        summary_outputs: IndexMap::new(),
    };
    finish_skipped(
        shared,
        job.clone(),
        report,
        RunState::Cancelled,
        "run cancelled",
    )
}

fn persist_job(
    db: &Db,
    run_id: &str,
    job: &IrJob,
    status: RunState,
    started_at: Option<u128>,
    finished_at: Option<u128>,
    outputs: &IndexMap<String, String>,
) -> Result<()> {
    let matrix = serde_json::to_string(&job.matrix)
        .map_err(|error| UbuilderError::new(ErrorCode::Internal, error.to_string()))?;
    let outputs_json = serde_json::to_string(outputs)
        .map_err(|error| UbuilderError::new(ErrorCode::Internal, error.to_string()))?;
    db.upsert_pipeline_job(&PipelineJobRecord {
        run_id: run_id.into(),
        id: job.id.clone(),
        base_id: job.base_id.clone(),
        name: job.name.clone(),
        matrix_json: matrix,
        status: status.as_str().into(),
        conclusion: if matches!(
            status,
            RunState::Pending | RunState::Ready | RunState::Running
        ) {
            None
        } else {
            Some(status.as_str().into())
        },
        started_at,
        finished_at,
        error: None,
        outputs_json,
    })
}

fn persist_step(shared: &Shared, job: &IrJob, report: StepReport) {
    let _ = shared.db.upsert_pipeline_step(&PipelineStepRecord {
        run_id: shared.run_id.clone(),
        job_id: job.id.clone(),
        step_id: report.id,
        name: report.name,
        status: report.status.as_str().into(),
        conclusion: report.conclusion,
        exit_code: report.exit_code,
        started_at: None,
        finished_at: Some(now_ms()),
        log_path: report.log_path,
        error: report.error,
    });
}

fn reset_dir(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(|error| {
            UbuilderError::io(format!("cannot reset {}", path.display()), error)
        })?;
    }
    std::fs::create_dir_all(path)
        .map_err(|error| UbuilderError::io(format!("cannot create {}", path.display()), error))
}

fn artifact_kind(tree: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(tree) else {
        return "directory".into();
    };
    let items: Vec<_> = entries.filter_map(|entry| entry.ok()).collect();
    if items.len() == 1
        && items[0]
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
    {
        "file".into()
    } else {
        "directory".into()
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
