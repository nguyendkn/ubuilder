//! Workflow planning: resolves targets and capability gates without executing.

use crate::capability::{BuildCapability, SupportLevel};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::pipeline::ir::{IrJob, IrStepKind, WorkflowIr};
use crate::target::Target;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowPlan {
    pub name: String,
    pub source_path: PathBuf,
    pub source_digest: String,
    pub host: String,
    pub waves: Vec<Vec<String>>,
    pub jobs: Vec<PlanJob>,
    pub warnings: Vec<PlanWarning>,
    pub max_matrix_jobs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanWarning {
    pub code: String,
    pub message: String,
    pub job: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanJob {
    pub id: String,
    pub base_id: String,
    pub name: String,
    pub wave: usize,
    pub needs: Vec<String>,
    pub runner: PlanRunner,
    pub target: Option<Target>,
    pub target_display: String,
    pub steps: Vec<PlanStep>,
    pub executable: bool,
    pub unsupported_reason: Option<String>,
    pub capability: Option<BuildCapability>,
    pub timeout_secs: Option<u64>,
    pub max_parallel: Option<usize>,
    pub fail_fast: bool,
    pub matrix: indexmap::IndexMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRunner {
    pub os: String,
    pub arch: String,
    pub backend: String,
    pub native: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub kind: String,
    pub action: Option<String>,
    pub detail: String,
    pub continue_on_error: bool,
    pub timeout_secs: Option<u64>,
}

pub fn plan(ir: &WorkflowIr, source_root: &Path, host: &Host) -> Result<WorkflowPlan> {
    let project = crate::detect::discover(source_root, None, None).ok();
    let facts = crate::providers::facts::collect(host, Some(source_root));
    let mut warnings = Vec::new();
    if project.is_none() {
        warnings.push(PlanWarning {
            code: "PROJECT_NOT_DETECTED".into(),
            message: format!(
                "no supported project detected at {}; compile actions will fail at runtime",
                source_root.display()
            ),
            job: None,
        });
    }
    let wave_of = |job: &str| -> usize {
        ir.waves
            .iter()
            .position(|wave| wave.iter().any(|id| id == job))
            .unwrap_or(0)
    };
    let mut jobs = Vec::new();
    let mut capabilities: std::collections::HashMap<String, BuildCapability> =
        std::collections::HashMap::new();
    for job in &ir.jobs {
        let target = target_for(job);
        if target.is_none() {
            warnings.push(PlanWarning {
                code: "UNKNOWN_RUNNER_TARGET".into(),
                message: format!(
                    "job '{}' requests {}/{}, which maps to no known target",
                    job.id, job.runner.os, job.runner.arch
                ),
                job: Some(job.id.clone()),
            });
        }
        let mut unsupported_reason = None;
        if job.runner.backend.as_str() != "native" {
            unsupported_reason = Some(format!(
                "{} execution backend is not implemented; use runs-on: auto",
                job.runner.backend.as_str()
            ));
        }
        let capability = match (&project, &target, unsupported_reason.is_none()) {
            (Some(project), Some(target), true) => {
                if let Some(cached) = capabilities.get(&target.id) {
                    Some(cached.clone())
                } else {
                    let resolved =
                        crate::adapters::resolve_capabilities(project, host, target, &facts);
                    capabilities.insert(target.id.clone(), resolved.clone());
                    Some(resolved)
                }
            }
            _ => None,
        };
        if let Some(capability) = &capability {
            if capability.compilation == SupportLevel::Unsupported {
                unsupported_reason = Some(format!(
                    "compilation for {} is unsupported on this host",
                    target.as_ref().map(|t| t.id.as_str()).unwrap_or("target")
                ));
            }
            for warning in &capability.warnings {
                warnings.push(PlanWarning {
                    code: warning.code.clone(),
                    message: warning.message.clone(),
                    job: Some(job.id.clone()),
                });
            }
        }
        let steps = job.steps.iter().map(plan_step).collect::<Vec<_>>();
        jobs.push(PlanJob {
            id: job.id.clone(),
            base_id: job.base_id.clone(),
            name: job.name.clone(),
            wave: wave_of(&job.id),
            needs: job.needs.clone(),
            runner: PlanRunner {
                os: job.runner.os.clone(),
                arch: job.runner.arch.clone(),
                backend: job.runner.backend.as_str().to_string(),
                native: job.runner.native,
            },
            target_display: target
                .as_ref()
                .map(|target| target.id.clone())
                .unwrap_or_else(|| format!("{}-{}", job.runner.os, job.runner.arch)),
            target,
            steps,
            executable: unsupported_reason.is_none(),
            unsupported_reason,
            capability,
            timeout_secs: job.timeout_secs,
            max_parallel: job.max_parallel,
            fail_fast: job.fail_fast,
            matrix: job.matrix.clone(),
        });
    }
    Ok(WorkflowPlan {
        name: ir.name.clone(),
        source_path: ir.source_path.clone(),
        source_digest: ir.source_digest.clone(),
        host: host.display(),
        waves: ir.waves.clone(),
        jobs,
        warnings,
        max_matrix_jobs: ir.max_matrix_jobs,
    })
}

fn plan_step(step: &crate::pipeline::ir::IrStep) -> PlanStep {
    let (kind, action, detail) = match &step.kind {
        IrStepKind::Run {
            command,
            shell,
            working_directory,
        } => (
            "run".to_string(),
            None,
            format!(
                "{shell}: {}{}",
                command.source,
                working_directory
                    .as_ref()
                    .map(|directory| format!(" (cwd {})", directory.source))
                    .unwrap_or_default()
            ),
        ),
        IrStepKind::Action { uses, with } => (
            "uses".to_string(),
            Some(uses.clone()),
            if with.is_empty() {
                uses.clone()
            } else {
                format!(
                    "{uses} ({})",
                    with.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            },
        ),
    };
    PlanStep {
        index: step.index,
        id: step.id.clone(),
        name: step.name.clone(),
        kind,
        action,
        detail,
        continue_on_error: step.continue_on_error,
        timeout_secs: step.timeout_secs,
    }
}

fn target_for(job: &IrJob) -> Option<Target> {
    if job.runner.arch == "universal" {
        return Target::all()
            .into_iter()
            .find(|target| target.os.as_str() == job.runner.os && target.universal);
    }
    Target::all().into_iter().find(|target| {
        target.os.as_str() == job.runner.os
            && target
                .arch()
                .map(|arch| arch.as_str() == job.runner.arch)
                .unwrap_or(false)
    })
}

pub fn ensure_executable(plan: &WorkflowPlan) -> Result<()> {
    if let Some(job) = plan.jobs.iter().find(|job| !job.executable) {
        return Err(UbuilderError::new(
            ErrorCode::UnsupportedTarget,
            format!(
                "job '{}' cannot run on {}: {}",
                job.id,
                plan.host,
                job.unsupported_reason.as_deref().unwrap_or("unsupported")
            ),
        )
        .remediation(format!(
            "Run `ubuilder pipeline plan --file {}` for the full capability report",
            plan.source_path.display()
        )));
    }
    Ok(())
}
