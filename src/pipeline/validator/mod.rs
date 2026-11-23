//! Semantic validation with actionable diagnostics. Never executes anything.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::actions::registry;
use crate::pipeline::diagnostics::{has_errors, Diagnostic};
use crate::pipeline::expression::{Ast, Expression, Template};
use crate::pipeline::matrix;
use crate::pipeline::schema::scalar_to_string;
use crate::pipeline::schema::{StepDefinition, WorkflowDefinition};
use indexmap::IndexMap;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

pub const MAX_MATRIX_JOBS: usize = 256;

pub fn validate(definition: &WorkflowDefinition, path: &Path, source: &str) -> Result<()> {
    let diagnostics = diagnostics(definition, path, source);
    if has_errors(&diagnostics) {
        let details = serde_json::to_value(&diagnostics).unwrap_or_default();
        let summary = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        return Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("workflow validation failed:\n{summary}"),
        )
        .details(details)
        .remediation("Fix the reported workflow errors, then run `ubuilder pipeline validate`"));
    }
    Ok(())
}

pub fn diagnostics(definition: &WorkflowDefinition, path: &Path, source: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if definition.version != 1 {
        diagnostics.push(
            diag(path, source, "UNSUPPORTED_VERSION", "version: 1").with_message(format!(
                "unsupported workflow version {}; only version 1 is supported",
                definition.version
            )),
        );
    }
    if definition.name.trim().is_empty() {
        diagnostics.push(
            diag(path, source, "INVALID_NAME", "name:").with_message("workflow name is empty"),
        );
    }
    if definition.jobs.is_empty() {
        diagnostics
            .push(diag(path, source, "NO_JOBS", "jobs:").with_message("workflow has no jobs"));
    }
    for (key, value) in &definition.env {
        check_env_name(&mut diagnostics, path, source, None, None, key);
        check_template(
            &mut diagnostics,
            path,
            source,
            None,
            None,
            &format!("env.{key}"),
            value,
        );
    }

    let job_ids: BTreeSet<String> = definition.jobs.keys().cloned().collect();
    for (job_id, job) in &definition.jobs {
        let line_hint = format!("  {job_id}:");
        if !valid_job_id(job_id) {
            diagnostics.push(
                diag(path, source, "INVALID_JOB_ID", &format!("{job_id}:"))
                    .with_message(format!(
                        "job id '{job_id}' must match [A-Za-z_][A-Za-z0-9_-]{{0,63}}"
                    ))
                    .job(job_id),
            );
        }
        if job.steps.is_empty() {
            diagnostics.push(
                diag(path, source, "NO_STEPS", &line_hint)
                    .with_message(format!("job '{job_id}' has no steps"))
                    .job(job_id),
            );
        }
        if job.needs.iter().any(|need| need == job_id) {
            diagnostics.push(
                diag(path, source, "SELF_DEPENDENCY", &line_hint)
                    .with_message(format!("job '{job_id}' needs itself"))
                    .job(job_id),
            );
        }
        for need in &job.needs {
            if !job_ids.contains(need) {
                diagnostics.push(
                    diag(path, source, "UNKNOWN_NEED", &line_hint)
                        .with_message(format!("job '{job_id}' needs unknown job '{need}'"))
                        .job(job_id)
                        .remediation(format!(
                            "Declare '{need}' under jobs or remove it from needs"
                        )),
                );
            }
        }
        if let Some(condition) = &job.condition {
            check_expression(
                &mut diagnostics,
                path,
                source,
                Some(job_id),
                None,
                "if",
                condition,
                true,
            );
        }
        if let Some(runner) = runner_error(job) {
            diagnostics.push(
                diag(path, source, "UNSUPPORTED_RUNNER", &line_hint)
                    .with_message(runner)
                    .job(job_id)
                    .remediation("Use 'runs-on: auto' on this host"),
            );
        }
        if let Some(strategy) = &job.strategy {
            if strategy
                .max_parallel
                .is_some_and(|max| max == 0 || max > MAX_MATRIX_JOBS)
            {
                diagnostics.push(
                    diag(path, source, "INVALID_MAX_PARALLEL", "max-parallel")
                        .with_message(format!(
                            "matrix max-parallel must be between 1 and {MAX_MATRIX_JOBS}"
                        ))
                        .job(job_id),
                );
            }
            if let Err(error) = matrix::expand(&strategy.matrix, MAX_MATRIX_JOBS) {
                diagnostics.push(
                    diag(path, source, "INVALID_MATRIX", "matrix:")
                        .with_message(error.message.to_string())
                        .job(job_id),
                );
            }
        }
        for (key, value) in &job.env {
            check_env_name(&mut diagnostics, path, source, Some(job_id), None, key);
            check_template(
                &mut diagnostics,
                path,
                source,
                Some(job_id),
                None,
                &format!("env.{key}"),
                value,
            );
        }
        if let Some(timeout) = job.timeout {
            if timeout.is_zero() {
                diagnostics.push(
                    diag(path, source, "INVALID_TIMEOUT", "timeout:")
                        .with_message("job timeout must be greater than zero")
                        .job(job_id),
                );
            }
        }
        if let Some(retry) = &job.retry {
            if let Err(error) = retry.validate() {
                diagnostics.push(
                    diag(path, source, "INVALID_RETRY", "retry:")
                        .with_message(error.message.to_string())
                        .job(job_id),
                );
            }
        }
        if let Some(concurrency) = &job.concurrency {
            if let Err(error) = concurrency.validate() {
                diagnostics.push(
                    diag(path, source, "INVALID_CONCURRENCY", "concurrency:")
                        .with_message(error.message.to_string())
                        .job(job_id),
                );
            }
        }
        for (key, value) in &job.outputs {
            check_template(
                &mut diagnostics,
                path,
                source,
                Some(job_id),
                None,
                &format!("outputs.{key}"),
                value,
            );
        }
        let mut step_ids = HashSet::new();
        for (index, step) in job.steps.iter().enumerate() {
            let name = step.name(index);
            validate_step(
                &mut diagnostics,
                path,
                source,
                job_id,
                index,
                &name,
                step,
                &mut step_ids,
            );
        }
    }
    detect_cycles(definition, path, source, &mut diagnostics);
    diagnostics
}

#[allow(clippy::too_many_arguments)]
fn validate_step(
    diagnostics: &mut Vec<Diagnostic>,
    path: &Path,
    source: &str,
    job_id: &str,
    index: usize,
    name: &str,
    step: &StepDefinition,
    step_ids: &mut HashSet<String>,
) {
    if let Err(error) = step.validate() {
        diagnostics.push(
            diag(
                path,
                source,
                "INVALID_STEP",
                &format!("- {}", step_marker(step)),
            )
            .with_message(error.message.to_string())
            .job(job_id)
            .step(name),
        );
    }
    if let Some(id) = step.id() {
        if !valid_job_id(id) {
            diagnostics.push(
                diag(path, source, "INVALID_STEP_ID", &format!("id: {id}"))
                    .with_message(format!(
                        "step id '{id}' must match [A-Za-z_][A-Za-z0-9_-]{{0,63}}"
                    ))
                    .job(job_id)
                    .step(name),
            );
        }
        if !step_ids.insert(id.to_string()) {
            diagnostics.push(
                diag(path, source, "DUPLICATE_STEP_ID", &format!("id: {id}"))
                    .with_message(format!("duplicate step id '{id}' in job '{job_id}'"))
                    .job(job_id)
                    .step(name),
            );
        }
    }
    if let Some(condition) = step.condition() {
        check_expression(
            diagnostics,
            path,
            source,
            Some(job_id),
            Some(name),
            "if",
            condition,
            true,
        );
    }
    if let Some(retry) = step.retry() {
        if let Err(error) = retry.validate() {
            diagnostics.push(
                diag(path, source, "INVALID_RETRY", "retry:")
                    .with_message(error.message.to_string())
                    .job(job_id)
                    .step(name),
            );
        }
    }
    for (key, value) in step.env() {
        check_env_name(diagnostics, path, source, Some(job_id), Some(name), key);
        check_template(
            diagnostics,
            path,
            source,
            Some(job_id),
            Some(name),
            &format!("env.{key}"),
            value,
        );
    }
    match step {
        StepDefinition::Run(command) => {
            check_template(
                diagnostics,
                path,
                source,
                Some(job_id),
                Some(name),
                "run",
                &command.run,
            );
            if let Some(directory) = &command.working_directory {
                check_template(
                    diagnostics,
                    path,
                    source,
                    Some(job_id),
                    Some(name),
                    "working-directory",
                    directory,
                );
                if !directory.contains("${{") {
                    if let Err(error) =
                        crate::pipeline::artifacts::safe_join(Path::new("workspace"), directory)
                    {
                        diagnostics.push(
                            diag(path, source, "PATH_ESCAPE", "working-directory")
                                .with_message(error.message.to_string())
                                .job(job_id)
                                .step(name),
                        );
                    }
                }
            }
        }
        StepDefinition::Action(action) => {
            let Some(registered) = registry::get(&action.uses) else {
                diagnostics.push(
                    diag(
                        path,
                        source,
                        "UNKNOWN_ACTION",
                        &format!("uses: {}", action.uses),
                    )
                    .with_message(format!("unknown action '{}'", action.uses))
                    .job(job_id)
                    .step(name)
                    .remediation(format!("Built-in actions: {}", registry::ids().join(", "))),
                );
                return;
            };
            let mut raw_inputs = IndexMap::new();
            let mut templated = false;
            for (key, value) in &action.with {
                match scalar_to_string(value) {
                    Ok(text) => {
                        if text.contains("${{") {
                            templated = true;
                            check_template(
                                diagnostics,
                                path,
                                source,
                                Some(job_id),
                                Some(name),
                                &format!("with.{key}"),
                                &text,
                            );
                        }
                        raw_inputs.insert(key.clone(), text);
                    }
                    Err(error) => diagnostics.push(
                        diag(path, source, "INVALID_ACTION_INPUT", &format!("with.{key}"))
                            .with_message(error.message.to_string())
                            .job(job_id)
                            .step(name),
                    ),
                }
            }
            if let Err(error) = crate::pipeline::actions::validate_inputs(
                action.uses.as_str(),
                registered.inputs(),
                &raw_inputs,
            ) {
                diagnostics.push(
                    diag(
                        path,
                        source,
                        "INVALID_ACTION_INPUT",
                        &format!("uses: {}", action.uses),
                    )
                    .with_message(error.message.to_string())
                    .job(job_id)
                    .step(name),
                );
            } else if !templated {
                if let Err(error) = registered.validate(&raw_inputs) {
                    diagnostics.push(
                        diag(
                            path,
                            source,
                            "INVALID_ACTION_INPUT",
                            &format!("uses: {}", action.uses),
                        )
                        .with_message(error.message.to_string())
                        .job(job_id)
                        .step(name),
                    );
                }
            }
            if index == 0 && action.uses == "builtin/package@v1" {
                diagnostics.push(
                    diag(
                        path,
                        source,
                        "PACKAGE_WITHOUT_BUILD",
                        "uses: builtin/package@v1",
                    )
                    .with_message("package action must follow a build action in the same job")
                    .job(job_id)
                    .step(name),
                );
            }
        }
    }
}

fn step_marker(step: &StepDefinition) -> String {
    match step {
        StepDefinition::Run(step) => format!("run: {}", first_line(&step.run)),
        StepDefinition::Action(step) => format!("uses: {}", step.uses),
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

#[allow(clippy::too_many_arguments)]
fn check_expression(
    diagnostics: &mut Vec<Diagnostic>,
    path: &Path,
    source: &str,
    job: Option<&str>,
    step: Option<&str>,
    field: &str,
    raw: &str,
    condition: bool,
) {
    let parsed = if condition {
        Expression::parse_condition(raw)
    } else {
        Expression::parse(raw)
    };
    match parsed {
        Ok(expression) => {
            if let Ok(ast) = crate::pipeline::expression::parse(&expression.source) {
                check_functions(diagnostics, path, source, job, step, field, &ast);
            }
        }
        Err(error) => {
            let mut diagnostic = diag(path, source, "INVALID_EXPRESSION", field)
                .with_message(error.message.to_string());
            if let Some(job) = job {
                diagnostic = diagnostic.job(job);
            }
            if let Some(step) = step {
                diagnostic = diagnostic.step(step);
            }
            diagnostics.push(diagnostic);
        }
    }
}

fn check_functions(
    diagnostics: &mut Vec<Diagnostic>,
    path: &Path,
    source: &str,
    job: Option<&str>,
    step: Option<&str>,
    field: &str,
    ast: &Ast,
) {
    match ast {
        Ast::Call { name, args } => {
            if !matches!(
                name.as_str(),
                "success"
                    | "failure"
                    | "cancelled"
                    | "always"
                    | "contains"
                    | "startsWith"
                    | "endsWith"
            ) {
                let mut diagnostic = diag(
                    path,
                    source,
                    "UNKNOWN_FUNCTION",
                    &format!("{field}: {name}()"),
                )
                .with_message(format!("unknown expression function '{name}'"));
                if let Some(job) = job {
                    diagnostic = diagnostic.job(job);
                }
                if let Some(step) = step {
                    diagnostic = diagnostic.step(step);
                }
                diagnostics.push(diagnostic);
            }
            for arg in args {
                check_functions(diagnostics, path, source, job, step, field, arg);
            }
        }
        Ast::UnaryNot(inner) => check_functions(diagnostics, path, source, job, step, field, inner),
        Ast::Binary { left, right, .. } => {
            check_functions(diagnostics, path, source, job, step, field, left);
            check_functions(diagnostics, path, source, job, step, field, right);
        }
        Ast::Literal(_) | Ast::Path(_) => {}
    }
}

fn check_template(
    diagnostics: &mut Vec<Diagnostic>,
    path: &Path,
    source: &str,
    job: Option<&str>,
    step: Option<&str>,
    field: &str,
    raw: &str,
) {
    if !raw.contains("${{") {
        return;
    }
    match Template::parse(raw) {
        Ok(template) => {
            let mut rest = template.source.as_str();
            while let Some(start) = rest.find("${{") {
                let after = &rest[start + 3..];
                let Some(end) = after.find("}}") else {
                    break;
                };
                let expression = after[..end].trim();
                if let Ok(ast) = crate::pipeline::expression::parse(expression) {
                    check_functions(diagnostics, path, source, job, step, field, &ast);
                }
                rest = &after[end + 2..];
            }
        }
        Err(error) => {
            let mut diagnostic = diag(path, source, "INVALID_TEMPLATE", field)
                .with_message(error.message.to_string());
            if let Some(job) = job {
                diagnostic = diagnostic.job(job);
            }
            if let Some(step) = step {
                diagnostic = diagnostic.step(step);
            }
            diagnostics.push(diagnostic);
        }
    }
}

fn check_env_name(
    diagnostics: &mut Vec<Diagnostic>,
    path: &Path,
    source: &str,
    job: Option<&str>,
    step: Option<&str>,
    name: &str,
) {
    let valid = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if !valid {
        let mut diagnostic = diag(path, source, "INVALID_ENV_NAME", &format!("env.{name}"))
            .with_message(format!("environment variable name '{name}' is invalid"));
        if let Some(job) = job {
            diagnostic = diagnostic.job(job);
        }
        if let Some(step) = step {
            diagnostic = diagnostic.step(step);
        }
        diagnostics.push(diagnostic);
    }
}

fn runner_error(job: &crate::pipeline::schema::JobDefinition) -> Option<String> {
    match &job.runner {
        crate::pipeline::schema::RunnerSelector::Auto(value) if value == "auto" => None,
        crate::pipeline::schema::RunnerSelector::Auto(value) => Some(format!(
            "unsupported runs-on value '{value}'; use 'auto' or an object with os/arch"
        )),
        crate::pipeline::schema::RunnerSelector::Explicit(spec) => {
            if spec.backend == Some(crate::pipeline::schema::ExecutionBackend::Container)
                || spec.backend == Some(crate::pipeline::schema::ExecutionBackend::Wsl)
            {
                Some(
                    "container and WSL execution backends are not implemented on this host; use native"
                        .to_string(),
                )
            } else {
                None
            }
        }
    }
}

fn detect_cycles(
    definition: &WorkflowDefinition,
    path: &Path,
    source: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        New,
        Visiting,
        Done,
    }
    let mut states: IndexMap<&str, State> = definition
        .jobs
        .keys()
        .map(|id| (id.as_str(), State::New))
        .collect();
    let mut stack = Vec::new();
    fn visit<'a>(
        id: &'a str,
        definition: &'a WorkflowDefinition,
        states: &mut IndexMap<&'a str, State>,
        stack: &mut Vec<&'a str>,
        diagnostics: &mut Vec<Diagnostic>,
        path: &Path,
        source: &str,
    ) {
        match states.get(id).copied() {
            Some(State::Done) => return,
            Some(State::Visiting) => {
                if let Some(position) = stack.iter().position(|entry| *entry == id) {
                    let cycle = stack[position..]
                        .iter()
                        .map(|entry| entry.to_string())
                        .chain(std::iter::once(id.to_string()))
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    diagnostics.push(
                        diag(path, source, "DEPENDENCY_CYCLE", &format!("  {id}:"))
                            .with_message(format!("dependency cycle: {cycle}"))
                            .job(id)
                            .remediation("Remove one edge from the needs chain"),
                    );
                }
                return;
            }
            _ => {}
        }
        states.insert(id, State::Visiting);
        stack.push(id);
        if let Some(job) = definition.jobs.get(id) {
            for need in &job.needs {
                if definition.jobs.contains_key(need) {
                    visit(need, definition, states, stack, diagnostics, path, source);
                }
            }
        }
        stack.pop();
        states.insert(id, State::Done);
    }
    for id in definition.jobs.keys() {
        visit(
            id,
            definition,
            &mut states,
            &mut stack,
            diagnostics,
            path,
            source,
        );
    }
}

fn valid_job_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn diag(path: &Path, source: &str, code: &str, needle: &str) -> DiagnosticBuilder {
    let line = source
        .lines()
        .position(|line| line.contains(needle))
        .map(|index| index + 1);
    DiagnosticBuilder {
        path: path.to_path_buf(),
        code: code.into(),
        line,
    }
}

struct DiagnosticBuilder {
    path: std::path::PathBuf,
    code: String,
    line: Option<usize>,
}

impl DiagnosticBuilder {
    fn with_message(self, message: impl Into<String>) -> Diagnostic {
        Diagnostic::error(&self.path, &self.code, message).at(self.line, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::parser::ParsedWorkflow;

    fn check(source: &str) -> Vec<Diagnostic> {
        let parsed = ParsedWorkflow::parse(Path::new("workflow.yml"), source.into()).unwrap();
        diagnostics(&parsed.definition, &parsed.path, &parsed.source)
    }

    fn has(diagnostics: &[Diagnostic], code: &str) -> bool {
        diagnostics.iter().any(|diagnostic| diagnostic.code == code)
    }

    #[test]
    fn finds_cycles_and_unknown_needs() {
        let diagnostics = check(
            "version: 1\nname: t\njobs:\n  a:\n    needs: [b]\n    steps: [{run: echo a}]\n  b:\n    needs: [a, missing]\n    steps: [{run: echo b}]\n",
        );
        assert!(has(&diagnostics, "DEPENDENCY_CYCLE"));
        assert!(has(&diagnostics, "UNKNOWN_NEED"));
    }

    #[test]
    fn finds_unknown_actions_and_duplicate_step_ids() {
        let diagnostics = check(
            "version: 1\nname: t\njobs:\n  a:\n    steps:\n      - id: x\n        run: echo a\n      - id: x\n        uses: builtin/nope@v1\n",
        );
        assert!(has(&diagnostics, "DUPLICATE_STEP_ID"));
        assert!(has(&diagnostics, "UNKNOWN_ACTION"));
    }

    #[test]
    fn rejects_step_path_escape() {
        let diagnostics = check(
            "version: 1\nname: t\njobs:\n  a:\n    steps:\n      - run: echo a\n        working-directory: ../outside\n",
        );
        assert!(has(&diagnostics, "PATH_ESCAPE"));
    }

    #[test]
    fn detects_expression_errors() {
        let diagnostics = check(
            "version: 1\nname: t\njobs:\n  a:\n    if: nope(matrix.target)\n    steps:\n      - run: echo a\n",
        );
        assert!(has(&diagnostics, "UNKNOWN_FUNCTION"));
    }
}
