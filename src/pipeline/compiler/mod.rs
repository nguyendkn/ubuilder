//! Schema → IR compilation. Pure, deterministic and side-effect free.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::pipeline::expression::{Expression, Template};
use crate::pipeline::ir::{IrJob, IrRunner, IrStep, IrStepKind, WorkflowIr};
use crate::pipeline::matrix::{self, MatrixCombination, DEFAULT_MAX_MATRIX_JOBS};
use crate::pipeline::parser::ParsedWorkflow;
use crate::pipeline::schema::{
    scalar_to_string, ExecutionBackend, JobDefinition, RunnerSelector, RunnerSpec, StepDefinition,
    WorkflowDefinition,
};
use crate::target::Target;
use indexmap::IndexMap;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

pub fn compile(parsed: &ParsedWorkflow, host: &Host) -> Result<WorkflowIr> {
    let definition = &parsed.definition;
    let mut jobs = Vec::new();
    let mut base_ids: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (base_id, job) in &definition.jobs {
        let compiled = compile_job(definition, base_id, job, host, &parsed.path)?;
        let ids = compiled
            .iter()
            .map(|job| job.id.clone())
            .collect::<Vec<_>>();
        base_ids.insert(base_id.clone(), ids);
        jobs.extend(compiled);
    }

    let mut seen = BTreeSet::new();
    for job in &mut jobs {
        let mut needs = Vec::new();
        for base in &job.base_needs {
            let expanded = base_ids.get(base).ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!("job '{}' needs unknown job '{base}'", job.id),
                )
            })?;
            for id in expanded {
                if !needs.contains(id) {
                    needs.push(id.clone());
                }
            }
        }
        job.needs = needs;
        if !seen.insert(job.id.clone()) {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("duplicate compiled job id '{}'", job.id),
            ));
        }
    }
    let (order, waves) = dependency_order(&jobs)?;
    Ok(WorkflowIr {
        version: definition.version,
        name: definition.name.clone(),
        source_path: parsed.path.clone(),
        source_digest: parsed.digest.clone(),
        env: compile_templates(&definition.env, &parsed.path, "env")?,
        jobs,
        order,
        waves,
        max_matrix_jobs: DEFAULT_MAX_MATRIX_JOBS,
    })
}

fn compile_job(
    definition: &WorkflowDefinition,
    base_id: &str,
    job: &JobDefinition,
    host: &Host,
    path: &std::path::Path,
) -> Result<Vec<IrJob>> {
    let combinations = match &job.strategy {
        Some(strategy) => matrix::expand(&strategy.matrix, DEFAULT_MAX_MATRIX_JOBS)?,
        None => vec![MatrixCombination {
            values: IndexMap::new(),
        }],
    };
    let mut jobs = Vec::new();
    for combination in combinations {
        let suffix = combination.suffix();
        let id = format!("{base_id}{suffix}");
        let name = format!("{}{suffix}", job.display_name(base_id));
        let runner = resolve_runner(&job.runner, &combination, host, base_id)?;
        let display = format!("job '{id}'");
        let condition = job
            .condition
            .as_ref()
            .map(|condition| expression(condition, path, &display, "if"))
            .transpose()?;
        let job_env = compile_templates(&job.env, path, &format!("{display} env"))?;
        let steps = compile_steps(definition, job, path, &display)?;
        let outputs = compile_templates(&job.outputs, path, &format!("{display} outputs"))?;
        jobs.push(IrJob {
            id,
            base_id: base_id.to_string(),
            name,
            needs: Vec::new(),
            base_needs: job.needs.clone(),
            runner,
            condition,
            env: job_env,
            steps,
            timeout_secs: job.timeout.map(duration_secs),
            retry: job.retry.clone(),
            concurrency: job.concurrency.clone(),
            outputs,
            matrix: combination.values,
            fail_fast: job.strategy.as_ref().map(|s| s.fail_fast).unwrap_or(true),
            max_parallel: job.strategy.as_ref().and_then(|s| s.max_parallel),
        });
    }
    Ok(jobs)
}

fn compile_steps(
    definition: &WorkflowDefinition,
    job: &JobDefinition,
    path: &std::path::Path,
    display: &str,
) -> Result<Vec<IrStep>> {
    let mut steps = Vec::new();
    for (index, step) in job.steps.iter().enumerate() {
        let name = step.name(index);
        let id = step
            .id()
            .map(str::to_string)
            .unwrap_or_else(|| format!("step-{}", index + 1));
        let step_display = format!("{display} step '{}'", name);
        let condition = step
            .condition()
            .map(|condition| expression(condition, path, &step_display, "if"))
            .transpose()?;
        let env = compile_templates(step.env(), path, &format!("{step_display} env"))?;
        let kind = match step {
            StepDefinition::Run(command) => {
                let shell = command
                    .shell
                    .clone()
                    .or_else(|| definition.defaults.shell.clone())
                    .unwrap_or_else(|| default_shell(definition));
                let working_directory = command
                    .working_directory
                    .clone()
                    .or_else(|| definition.defaults.working_directory.clone())
                    .map(|raw| template(&raw, path, &format!("{step_display} working-directory")))
                    .transpose()?;
                IrStepKind::Run {
                    command: template(&command.run, path, &format!("{step_display} run"))?,
                    shell,
                    working_directory,
                }
            }
            StepDefinition::Action(action) => {
                let mut with = IndexMap::new();
                for (key, value) in &action.with {
                    let raw = scalar_to_string(value)?;
                    with.insert(
                        key.clone(),
                        template(&raw, path, &format!("{step_display} with.{key}"))?,
                    );
                }
                IrStepKind::Action {
                    uses: action.uses.clone(),
                    with,
                }
            }
        };
        steps.push(IrStep {
            id,
            index,
            name,
            condition,
            continue_on_error: step.continue_on_error(),
            timeout_secs: step.timeout().map(duration_secs),
            retry: step.retry().cloned(),
            env,
            kind,
        });
    }
    Ok(steps)
}

fn resolve_runner(
    selector: &RunnerSelector,
    combination: &MatrixCombination,
    host: &Host,
    job: &str,
) -> Result<IrRunner> {
    let mut requested = selector.spec();
    if let Some(target) = combination
        .values
        .get("target")
        .and_then(serde_json::Value::as_str)
        .filter(|target| !matches!(*target, "native" | "auto"))
    {
        let target = Target::parse(target)?;
        fill_spec(&mut requested, &target);
        return Ok(runner_for(target, requested, host));
    }
    let matrix_os = combination
        .values
        .get("os")
        .and_then(serde_json::Value::as_str);
    let matrix_arch = combination
        .values
        .get("arch")
        .and_then(serde_json::Value::as_str);
    match (
        requested.os.as_deref().or(matrix_os),
        requested.arch.as_deref().or(matrix_arch),
    ) {
        (Some(os), arch) => {
            let arch = arch.unwrap_or_else(|| host.arch.as_str());
            let target = find_target(os, arch).ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::UnsupportedTarget,
                    format!("job '{job}': no target maps to os '{os}' and architecture '{arch}'"),
                )
                .remediation("Use an os/arch pair available in `ubuilder target list`")
            })?;
            fill_spec(&mut requested, &target);
            Ok(runner_for(target, requested, host))
        }
        (None, Some(arch)) => {
            let os = match host.platform {
                crate::host::HostPlatform::Windows => "windows",
                crate::host::HostPlatform::Linux => "linux",
                crate::host::HostPlatform::Macos => "macos",
            };
            let target = find_target(os, arch).ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::UnsupportedTarget,
                    format!("job '{job}': no native target for architecture '{arch}'"),
                )
            })?;
            fill_spec(&mut requested, &target);
            Ok(runner_for(target, requested, host))
        }
        (None, None) => {
            let target = Target::all()
                .into_iter()
                .find(|target| target.is_native(host))
                .ok_or_else(|| {
                    UbuilderError::new(
                        ErrorCode::UnsupportedTarget,
                        format!("no native target for host {}", host.display()),
                    )
                })?;
            fill_spec(&mut requested, &target);
            Ok(runner_for(target, requested, host))
        }
    }
}

fn runner_for(target: Target, requested: RunnerSpec, host: &Host) -> IrRunner {
    let backend = requested.backend.unwrap_or(ExecutionBackend::Native);
    IrRunner {
        requested,
        native: target.is_native(host),
        os: target.os.as_str().to_string(),
        arch: target
            .arch()
            .map(|arch| arch.as_str().to_string())
            .unwrap_or_else(|| "universal".to_string()),
        backend,
    }
}

fn fill_spec(spec: &mut RunnerSpec, target: &Target) {
    spec.os
        .get_or_insert_with(|| target.os.as_str().to_string());
    if let Some(arch) = target.arch() {
        spec.arch.get_or_insert_with(|| arch.as_str().to_string());
    }
    spec.backend.get_or_insert(ExecutionBackend::Native);
}

fn find_target(os: &str, arch: &str) -> Option<Target> {
    let os = os.to_ascii_lowercase();
    let arch = arch.to_ascii_lowercase();
    Target::all().into_iter().find(|target| {
        target.os.as_str() == os
            && target
                .arch()
                .map(|target_arch| target_arch.as_str() == arch)
                .unwrap_or(false)
    })
}

fn default_shell(definition: &WorkflowDefinition) -> String {
    let _ = definition;
    if cfg!(windows) {
        "powershell".into()
    } else {
        "bash".into()
    }
}

fn dependency_order(jobs: &[IrJob]) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    let index: BTreeMap<&str, usize> = jobs
        .iter()
        .enumerate()
        .map(|(index, job)| (job.id.as_str(), index))
        .collect();
    let mut indegree = vec![0usize; jobs.len()];
    let mut dependents = vec![Vec::new(); jobs.len()];
    for (position, job) in jobs.iter().enumerate() {
        for need in &job.needs {
            let need_position = *index.get(need.as_str()).ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!("job '{}' needs unknown compiled job '{need}'", job.id),
                )
            })?;
            indegree[position] += 1;
            dependents[need_position].push(position);
        }
    }
    let mut order = Vec::new();
    let mut waves = Vec::new();
    let mut ready: Vec<usize> = (0..jobs.len())
        .filter(|index| indegree[*index] == 0)
        .collect();
    while !ready.is_empty() {
        ready.sort_unstable();
        let wave = ready
            .iter()
            .map(|index| jobs[*index].id.clone())
            .collect::<Vec<_>>();
        for position in &ready {
            order.push(jobs[*position].id.clone());
            for dependent in &dependents[*position] {
                indegree[*dependent] -= 1;
            }
        }
        let mut next = Vec::new();
        for position in 0..jobs.len() {
            if indegree[position] == 0 && !order.contains(&jobs[position].id) {
                next.push(position);
            }
        }
        waves.push(wave);
        ready = next;
    }
    if order.len() != jobs.len() {
        return Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            "job graph contains a cycle",
        ));
    }
    Ok((order, waves))
}

fn duration_secs(duration: Duration) -> u64 {
    duration.as_secs().max(1)
}

fn template(raw: &str, path: &std::path::Path, field: &str) -> Result<Template> {
    Template::parse(raw).map_err(|error| {
        UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("{}: {field}: {}", path.display(), error.message),
        )
    })
}

fn expression(raw: &str, path: &std::path::Path, field: &str, what: &str) -> Result<Expression> {
    Expression::parse_condition(raw).map_err(|error| {
        UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("{}: {field} {what}: {}", path.display(), error.message),
        )
    })
}

fn compile_templates(
    values: &IndexMap<String, String>,
    path: &std::path::Path,
    field: &str,
) -> Result<IndexMap<String, Template>> {
    let mut compiled = IndexMap::new();
    for (key, value) in values {
        compiled.insert(
            key.clone(),
            template(value, path, &format!("{field}.{key}"))?,
        );
    }
    Ok(compiled)
}
