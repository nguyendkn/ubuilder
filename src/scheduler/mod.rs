//! DAG scheduler: dependency order, bounded parallelism, retries, fail-fast,
//! cancellation and cache integration.

use crate::artifacts;
use crate::cache::Cache;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::{CommandSpec, ExecutionOutput, Executor};
use crate::host::Host;
use crate::planner::{BuildPlan, BuildStep, CachePolicy, InternalAction};
use crate::storage::{Db, JobRecord, StepRecord};
use crate::telemetry::{now_ms, EventKind, Ui};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Success,
    Failed,
    Skipped,
    Cancelled,
}

impl StepStatus {
    fn terminal(self) -> bool {
        !matches!(self, StepStatus::Pending | StepStatus::Running)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Success => "success",
            StepStatus::Failed => "failed",
            StepStatus::Skipped => "skipped",
            StepStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepError {
    pub code: ErrorCode,
    pub message: String,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutcome {
    pub id: String,
    pub name: String,
    pub status: StepStatus,
    pub duration_ms: u128,
    pub exit_code: Option<i32>,
    pub cached: bool,
    pub attempts: u32,
    pub log_path: Option<PathBuf>,
    pub error: Option<StepError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    Success,
    Failed,
    Cancelled,
}

impl BuildStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            BuildStatus::Success => "success",
            BuildStatus::Failed => "failed",
            BuildStatus::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOutcome {
    pub job_id: String,
    pub plan_id: String,
    pub target: String,
    pub status: BuildStatus,
    pub duration_ms: u128,
    pub steps: Vec<StepOutcome>,
    pub failure: Option<StepError>,
    pub cached_steps: usize,
}

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub jobs: usize,
    pub continue_on_error: bool,
    pub retry_attempts: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            jobs: 1,
            continue_on_error: false,
            retry_attempts: 2,
        }
    }
}

pub struct Scheduler {
    pub executor: Executor,
    pub db: Arc<Db>,
    pub cache: Option<Arc<Cache>>,
    pub host: Host,
    pub toolchain_fingerprint: String,
    pub config: SchedulerConfig,
}

struct StepResult {
    id: String,
    status: StepStatus,
    output: Option<ExecutionOutput>,
    error: Option<UbuilderError>,
    cached: bool,
    attempts: u32,
    duration_ms: u128,
}

impl Scheduler {
    pub fn new(
        executor: Executor,
        db: Arc<Db>,
        cache: Option<Arc<Cache>>,
        host: Host,
        toolchain_fingerprint: String,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            executor,
            db,
            cache,
            host,
            toolchain_fingerprint,
            config: SchedulerConfig {
                jobs: config.jobs.clamp(1, 64),
                ..config
            },
        }
    }

    pub async fn execute(
        &self,
        plan: &BuildPlan,
        job_id: &str,
        cancel: &CancellationToken,
        ui: &Ui,
    ) -> Result<BuildOutcome> {
        let started = Instant::now();
        let started_at = now_ms();
        plan.validate()?;
        self.db.upsert_job(&JobRecord {
            id: job_id.to_string(),
            project: plan.project_name.clone(),
            framework: plan.framework.as_str().to_string(),
            target: plan.target.id.clone(),
            host: self.host.triple.clone(),
            status: "running".into(),
            started_at,
            finished_at: None,
            manifest_path: None,
            log_path: self.executor.config().log_dir.clone(),
            error: None,
        })?;

        let order: Vec<String> = plan
            .topological_order()?
            .into_iter()
            .map(str::to_string)
            .collect();
        let mut statuses: BTreeMap<String, StepStatus> = order
            .iter()
            .map(|id| (id.clone(), StepStatus::Pending))
            .collect();
        let mut outcomes: BTreeMap<String, StepOutcome> = BTreeMap::new();
        let mut running: JoinSet<StepResult> = JoinSet::new();
        let mut failure: Option<UbuilderError> = None;
        let mut stop_scheduling = false;
        let run_cancel = cancel.child_token();
        let step_map = plan.step_map();

        loop {
            if cancel.is_cancelled() {
                stop_scheduling = true;
                run_cancel.cancel();
            }
            // Cascade skips for dependents of failed/skipped/cancelled steps.
            loop {
                let mut changed = false;
                for id in &order {
                    if statuses.get(id) != Some(&StepStatus::Pending) {
                        continue;
                    }
                    let step = step_map[id.as_str()];
                    let blocked = step.dependencies.iter().any(|dep| {
                        matches!(
                            statuses.get(dep),
                            Some(StepStatus::Failed | StepStatus::Skipped | StepStatus::Cancelled)
                        )
                    });
                    if blocked {
                        statuses.insert(id.clone(), StepStatus::Skipped);
                        let outcome = StepOutcome {
                            id: id.clone(),
                            name: step.name.clone(),
                            status: StepStatus::Skipped,
                            duration_ms: 0,
                            exit_code: None,
                            cached: false,
                            attempts: 0,
                            log_path: None,
                            error: Some(StepError {
                                code: ErrorCode::Cancelled,
                                message: "skipped because a dependency did not succeed".into(),
                                remediation: None,
                            }),
                        };
                        record_step(&self.db, job_id, &outcome)?;
                        outcomes.insert(id.clone(), outcome);
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }

            if stop_scheduling {
                for id in &order {
                    if statuses.get(id) == Some(&StepStatus::Pending) {
                        let step = step_map[id.as_str()];
                        statuses.insert(id.clone(), StepStatus::Skipped);
                        let status = if cancel.is_cancelled() {
                            StepStatus::Cancelled
                        } else {
                            StepStatus::Skipped
                        };
                        let outcome = StepOutcome {
                            id: id.clone(),
                            name: step.name.clone(),
                            status,
                            duration_ms: 0,
                            exit_code: None,
                            cached: false,
                            attempts: 0,
                            log_path: None,
                            error: Some(StepError {
                                code: if cancel.is_cancelled() {
                                    ErrorCode::Cancelled
                                } else {
                                    ErrorCode::ExecutionFailed
                                },
                                message: if cancel.is_cancelled() {
                                    "build cancelled".into()
                                } else {
                                    "not started because an earlier step failed".into()
                                },
                                remediation: None,
                            }),
                        };
                        record_step(&self.db, job_id, &outcome)?;
                        outcomes.insert(id.clone(), outcome);
                    }
                }
            }

            while running.len() < self.config.jobs && !stop_scheduling {
                let next = order.iter().find(|id| {
                    statuses.get(*id) == Some(&StepStatus::Pending)
                        && step_map[id.as_str()]
                            .dependencies
                            .iter()
                            .all(|dep| statuses.get(dep) == Some(&StepStatus::Success))
                });
                let Some(id) = next.cloned() else {
                    break;
                };
                let step = step_map[id.as_str()].clone();
                statuses.insert(id.clone(), StepStatus::Running);
                ui.event(
                    EventKind::StepStart,
                    format!("→ {}", step.name),
                    Some(&plan.target.id),
                    Some(&id),
                    None,
                );
                record_step_start(&self.db, job_id, &step)?;
                let executor = self.executor.clone();
                let cache = self.cache.clone();
                let host = self.host.clone();
                let fingerprint = self.toolchain_fingerprint.clone();
                let cancel = run_cancel.clone();
                let ui = ui.clone();
                let plan = plan.clone();
                running.spawn(async move {
                    run_step(
                        &plan,
                        &step,
                        &executor,
                        cache,
                        &host,
                        &fingerprint,
                        &cancel,
                        &ui,
                    )
                    .await
                });
            }

            if running.is_empty() {
                if statuses.values().all(|s| s.terminal()) {
                    break;
                }
                // Should not happen after validation; avoid an infinite loop.
                return Err(UbuilderError::new(
                    ErrorCode::Internal,
                    format!("scheduler stalled in plan {}", plan.id),
                ));
            }

            let joined = running.join_next().await.ok_or_else(|| {
                UbuilderError::new(ErrorCode::Internal, "scheduler join set ended")
            })?;
            let result = joined.map_err(|e| {
                UbuilderError::new(ErrorCode::Internal, format!("step task panicked: {e}"))
            })?;
            let step = step_map[result.id.as_str()];
            let status = result.status;
            statuses.insert(result.id.clone(), status);
            let outcome = StepOutcome {
                id: result.id.clone(),
                name: step.name.clone(),
                status,
                duration_ms: result.duration_ms,
                exit_code: result.output.as_ref().and_then(|o| o.exit_code),
                cached: result.cached,
                attempts: result.attempts,
                log_path: result.output.as_ref().and_then(|o| o.log_path.clone()),
                error: result.error.as_ref().map(|e| StepError {
                    code: e.code,
                    message: e.message.clone(),
                    remediation: e.remediation.clone(),
                }),
            };
            record_step(&self.db, job_id, &outcome)?;
            ui.event(
                EventKind::StepFinish,
                format!(
                    "{} {} ({} ms){}",
                    status_icon(status),
                    step.name,
                    outcome.duration_ms,
                    if outcome.cached { " [cached]" } else { "" }
                ),
                Some(&plan.target.id),
                Some(&result.id),
                serde_json::to_value(&outcome).ok(),
            );
            outcomes.insert(result.id.clone(), outcome);
            if status == StepStatus::Failed || status == StepStatus::Cancelled {
                if failure.is_none() {
                    if let Some(error) = result.error {
                        failure = Some(error);
                    }
                }
                if !self.config.continue_on_error {
                    stop_scheduling = true;
                    run_cancel.cancel();
                }
            }
        }

        let outcomes_cached = outcomes.values().filter(|o| o.cached).count();
        let status = if failure.is_some() {
            BuildStatus::Failed
        } else if outcomes.values().any(|o| o.status == StepStatus::Cancelled) {
            BuildStatus::Cancelled
        } else if outcomes.values().all(|o| o.status == StepStatus::Success) {
            BuildStatus::Success
        } else {
            BuildStatus::Failed
        };
        let error_text = failure.as_ref().map(|e| e.to_string());
        self.db.finish_job(
            job_id,
            status.as_str(),
            error_text.as_deref(),
            None,
            now_ms(),
        )?;
        if let Some(failure) = &failure {
            ui.error(format!("build failed: {}", failure.message));
        } else if status == BuildStatus::Success {
            ui.success("build succeeded");
        }
        Ok(BuildOutcome {
            job_id: job_id.to_string(),
            plan_id: plan.id.clone(),
            target: plan.target.id.clone(),
            status,
            duration_ms: started.elapsed().as_millis(),
            steps: outcomes.into_values().collect(),
            failure: failure.as_ref().map(|e| StepError {
                code: e.code,
                message: e.message.clone(),
                remediation: e.remediation.clone(),
            }),
            cached_steps: outcomes_cached,
        })
    }
}

fn status_icon(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Success => "✓",
        StepStatus::Failed => "✗",
        StepStatus::Skipped => "⊘",
        StepStatus::Cancelled => "■",
        _ => "·",
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_step(
    plan: &BuildPlan,
    step: &BuildStep,
    executor: &Executor,
    cache: Option<Arc<Cache>>,
    host: &Host,
    fingerprint: &str,
    cancel: &CancellationToken,
    ui: &Ui,
) -> StepResult {
    let started = Instant::now();
    let mut attempts = 0;
    let max_attempts = if step.command.as_ref().map(|c| c.retryable).unwrap_or(false) {
        2
    } else {
        1
    };

    let cache_key = if step.cache_policy == CachePolicy::Action {
        match cache.as_ref() {
            Some(cache) => match cache.step_key(plan, step, host, fingerprint) {
                Ok(key) => Some(key),
                Err(error) => {
                    ui.warn(format!("cannot compute cache key for {}: {error}", step.id));
                    None
                }
            },
            None => None,
        }
    } else {
        None
    };
    if let (Some(cache), Some(key)) = (cache.as_ref(), cache_key.as_ref()) {
        if let Some(output) = step.outputs.first() {
            if output.kind == crate::planner::ArtifactKind::Directory {
                match cache.restore_tree(key, &output.path) {
                    Ok(true) => {
                        return StepResult {
                            id: step.id.clone(),
                            status: StepStatus::Success,
                            output: None,
                            error: None,
                            cached: true,
                            attempts: 0,
                            duration_ms: started.elapsed().as_millis(),
                        };
                    }
                    Ok(false) => {}
                    Err(error) => ui.warn(format!("cache restore failed: {}", error.message)),
                }
            }
        }
    }

    loop {
        attempts += 1;
        if cancel.is_cancelled() {
            return StepResult {
                id: step.id.clone(),
                status: StepStatus::Cancelled,
                output: None,
                error: Some(UbuilderError::new(
                    ErrorCode::Cancelled,
                    format!("step {} cancelled", step.id),
                )),
                cached: false,
                attempts,
                duration_ms: started.elapsed().as_millis(),
            };
        }
        let attempt = execute_once(step, executor, cancel, ui).await;
        match attempt {
            Ok(output) => {
                let needs_retry = !output.success() && !output.cancelled && attempts < max_attempts;
                if needs_retry {
                    tokio::time::sleep(std::time::Duration::from_millis(200 * attempts as u64))
                        .await;
                    continue;
                }
                if output.cancelled {
                    return StepResult {
                        id: step.id.clone(),
                        status: StepStatus::Cancelled,
                        output: Some(output),
                        error: Some(UbuilderError::new(
                            ErrorCode::Cancelled,
                            format!("step {} cancelled", step.id),
                        )),
                        cached: false,
                        attempts,
                        duration_ms: started.elapsed().as_millis(),
                    };
                }
                if output.timed_out {
                    return StepResult {
                        id: step.id.clone(),
                        status: StepStatus::Failed,
                        error: Some(
                            UbuilderError::new(
                                ErrorCode::Timeout,
                                format!(
                                    "step {} timed out after {} ms",
                                    step.id, output.duration_ms
                                ),
                            )
                            .remediation("Increase the step timeout in ubuilder.toml"),
                        ),
                        output: Some(output),
                        cached: false,
                        attempts,
                        duration_ms: started.elapsed().as_millis(),
                    };
                }
                if let Some(code) = output.exit_code {
                    if code != 0 {
                        return StepResult {
                            id: step.id.clone(),
                            status: StepStatus::Failed,
                            error: Some(
                                UbuilderError::new(
                                    ErrorCode::ExecutionFailed,
                                    format!("step {} failed with exit code {code}", step.id),
                                )
                                .details(serde_json::json!({
                                    "stderr": output.stderr_tail,
                                    "stdout": output.stdout_tail,
                                })),
                            ),
                            output: Some(output),
                            cached: false,
                            attempts,
                            duration_ms: started.elapsed().as_millis(),
                        };
                    }
                }
                if let Err(error) = verify_outputs(step) {
                    return StepResult {
                        id: step.id.clone(),
                        status: StepStatus::Failed,
                        output: Some(output),
                        error: Some(error),
                        cached: false,
                        attempts,
                        duration_ms: started.elapsed().as_millis(),
                    };
                }
                if let (Some(cache), Some(key)) = (cache.as_ref(), cache_key.as_ref()) {
                    if let Some(output_spec) = step.outputs.first() {
                        if output_spec.kind == crate::planner::ArtifactKind::Directory {
                            if let Err(error) = cache.store_tree(key, &output_spec.path) {
                                ui.warn(format!("cache store failed: {}", error.message));
                            }
                        }
                    }
                }
                return StepResult {
                    id: step.id.clone(),
                    status: StepStatus::Success,
                    output: Some(output),
                    error: None,
                    cached: false,
                    attempts,
                    duration_ms: started.elapsed().as_millis(),
                };
            }
            Err(error) => {
                if attempts < max_attempts {
                    tokio::time::sleep(std::time::Duration::from_millis(200 * attempts as u64))
                        .await;
                    continue;
                }
                return StepResult {
                    id: step.id.clone(),
                    status: StepStatus::Failed,
                    output: None,
                    error: Some(error),
                    cached: false,
                    attempts,
                    duration_ms: started.elapsed().as_millis(),
                };
            }
        }
    }
}

async fn execute_once(
    step: &BuildStep,
    executor: &Executor,
    cancel: &CancellationToken,
    ui: &Ui,
) -> Result<ExecutionOutput> {
    match (&step.action, &step.command) {
        (InternalAction::None, Some(command)) => {
            let mut command = command.clone();
            if command.timeout_secs.is_none() {
                command.timeout_secs = step.timeout;
            }
            executor.run(&command, cancel, ui, &step.id).await
        }
        (InternalAction::Copy { from, to }, _) => {
            let started = Instant::now();
            for source in from {
                let target = if from.len() == 1 && !to.is_dir() {
                    to.clone()
                } else {
                    to.join(source.file_name().ok_or_else(|| {
                        UbuilderError::new(
                            ErrorCode::ArtifactInvalid,
                            format!("source has no file name: {}", source.display()),
                        )
                    })?)
                };
                artifacts::copy_any(source, &target)?;
            }
            Ok(ExecutionOutput {
                exit_code: Some(0),
                stdout_tail: String::new(),
                stderr_tail: String::new(),
                duration_ms: started.elapsed().as_millis(),
                timed_out: false,
                cancelled: false,
                log_path: None,
            })
        }
        (InternalAction::MergeMachO { inputs, output }, _) => {
            let started = Instant::now();
            if inputs.is_empty() {
                return Err(UbuilderError::new(
                    ErrorCode::Internal,
                    "merge step has no inputs",
                ));
            }
            let digest = crate::packaging::merge_macho(inputs, output)?;
            Ok(ExecutionOutput {
                exit_code: Some(0),
                stdout_tail: format!("merged {}", digest),
                stderr_tail: String::new(),
                duration_ms: started.elapsed().as_millis(),
                timed_out: false,
                cancelled: false,
                log_path: None,
            })
        }
        (InternalAction::None, None) => Err(UbuilderError::new(
            ErrorCode::Internal,
            format!(
                "step {} has neither a command nor an internal action",
                step.id
            ),
        )),
    }
}

fn verify_outputs(step: &BuildStep) -> Result<()> {
    for output in &step.outputs {
        if output.optional {
            continue;
        }
        if !output.path.exists() {
            return Err(UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!(
                    "step '{}' did not produce expected output {}",
                    step.id,
                    output.path.display()
                ),
            )
            .details(serde_json::json!({
                "step": step.id,
                "expected": output.path.to_string_lossy(),
                "kind": format!("{:?}", output.kind),
            })));
        }
    }
    Ok(())
}

fn record_step_start(db: &Db, job_id: &str, step: &BuildStep) -> Result<()> {
    db.upsert_step(&StepRecord {
        job_id: job_id.to_string(),
        step_id: step.id.clone(),
        name: step.name.clone(),
        status: StepStatus::Running.as_str().into(),
        exit_code: None,
        started_at: Some(now_ms()),
        finished_at: None,
        log_path: None,
        error: None,
    })
}

fn record_step(db: &Db, job_id: &str, outcome: &StepOutcome) -> Result<()> {
    db.upsert_step(&StepRecord {
        job_id: job_id.to_string(),
        step_id: outcome.id.clone(),
        name: outcome.name.clone(),
        status: outcome.status.as_str().into(),
        exit_code: outcome.exit_code,
        started_at: None,
        finished_at: Some(now_ms()),
        log_path: outcome.log_path.clone(),
        error: outcome.error.as_ref().map(|e| e.message.clone()),
    })
}

/// Utility used by builders to make deterministic job ids.
pub fn job_id(project: &str, target: &str, timestamp_ms: u128) -> String {
    let slug: String = project
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{slug}-{target}-{timestamp_ms}")
}

#[allow(dead_code)]
fn unique_deps(steps: &[BuildStep]) -> BTreeSet<String> {
    steps
        .iter()
        .flat_map(|s| s.dependencies.iter().cloned())
        .collect()
}

#[allow(dead_code)]
fn command_of(step: &BuildStep) -> Option<&CommandSpec> {
    step.command.as_ref()
}

#[allow(dead_code)]
fn path_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::ExecutorConfig;
    use crate::planner::{ArtifactKind, BuildStep, InternalAction, OutputSpec};
    use crate::project::FrameworkKind;
    use crate::target::Target;

    fn internal_plan(tmp: &Path) -> BuildPlan {
        let src = tmp.join("source.txt");
        std::fs::write(&src, "data").unwrap();
        let out1 = tmp.join("one.txt");
        let out2 = tmp.join("two.txt");
        BuildPlan::new(
            "plan",
            "demo",
            tmp,
            FrameworkKind::Rust,
            Target::parse("linux-x64").unwrap(),
        )
        .step(
            BuildStep::internal(
                "first",
                "first",
                InternalAction::Copy {
                    from: vec![src],
                    to: out1.clone(),
                },
            )
            .output(OutputSpec::required(out1, ArtifactKind::File)),
        )
        .step(
            BuildStep::internal(
                "second",
                "second",
                InternalAction::Copy {
                    from: vec![tmp.join("one.txt")],
                    to: out2.clone(),
                },
            )
            .depends_on(&["first"])
            .output(OutputSpec::required(out2, ArtifactKind::File)),
        )
    }

    fn host() -> Host {
        Host {
            platform: crate::host::HostPlatform::Linux,
            arch: crate::host::Architecture::X64,
            triple: "x86_64-unknown-linux-gnu".into(),
            os_version: None,
        }
    }

    fn scheduler(db: Arc<Db>) -> Scheduler {
        Scheduler::new(
            Executor::new(ExecutorConfig::default()),
            db,
            None,
            host(),
            "fp".into(),
            SchedulerConfig {
                jobs: 2,
                continue_on_error: false,
                retry_attempts: 2,
            },
        )
    }

    #[tokio::test]
    async fn executes_dependencies_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open_in_memory().unwrap());
        let scheduler = scheduler(db);
        let plan = internal_plan(tmp.path());
        let outcome = scheduler
            .execute(
                &plan,
                "job",
                &CancellationToken::new(),
                &Ui::new(false, false, true),
            )
            .await
            .unwrap();
        assert_eq!(outcome.status, BuildStatus::Success);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("two.txt")).unwrap(),
            "data"
        );
        assert_eq!(outcome.steps.len(), 2);
    }

    #[tokio::test]
    async fn failure_skips_dependents() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open_in_memory().unwrap());
        let scheduler = scheduler(db);
        let missing = tmp.path().join("missing.txt");
        let plan = BuildPlan::new(
            "plan",
            "demo",
            tmp.path(),
            FrameworkKind::Rust,
            Target::parse("linux-x64").unwrap(),
        )
        .step(
            BuildStep::internal(
                "first",
                "first",
                InternalAction::Copy {
                    from: vec![missing],
                    to: tmp.path().join("x"),
                },
            )
            .output(OutputSpec::required(
                tmp.path().join("x"),
                ArtifactKind::File,
            )),
        )
        .step(
            BuildStep::command(
                "second",
                "second",
                CommandSpec::new("definitely-not-a-program", "noop"),
            )
            .depends_on(&["first"]),
        );
        let outcome = scheduler
            .execute(
                &plan,
                "job2",
                &CancellationToken::new(),
                &Ui::new(false, false, true),
            )
            .await
            .unwrap();
        assert_eq!(outcome.status, BuildStatus::Failed);
        let second = outcome.steps.iter().find(|s| s.id == "second").unwrap();
        assert_eq!(second.status, StepStatus::Skipped);
    }

    #[tokio::test]
    async fn missing_required_output_fails_step() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open_in_memory().unwrap());
        let scheduler = scheduler(db);
        let plan = BuildPlan::new(
            "plan",
            "demo",
            tmp.path(),
            FrameworkKind::Rust,
            Target::parse("linux-x64").unwrap(),
        )
        .step(
            BuildStep::internal(
                "first",
                "first",
                InternalAction::Copy {
                    from: vec![tmp.path().join("source.txt")],
                    to: tmp.path().join("one.txt"),
                },
            )
            .output(OutputSpec::required(
                tmp.path().join("nope.txt"),
                ArtifactKind::File,
            )),
        );
        std::fs::write(tmp.path().join("source.txt"), "x").unwrap();
        let outcome = scheduler
            .execute(
                &plan,
                "job3",
                &CancellationToken::new(),
                &Ui::new(false, false, true),
            )
            .await
            .unwrap();
        assert_eq!(outcome.status, BuildStatus::Failed);
    }
}
