//! Deterministic build plan / DAG model. The planner never executes commands.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::CommandSpec;
use crate::project::FrameworkKind;
use crate::target::Target;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

pub type StepId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Executable,
    Binary,
    Directory,
    File,
    Bundle,
    Archive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputSpec {
    pub path: PathBuf,
    pub kind: ArtifactKind,
    pub optional: bool,
}

impl OutputSpec {
    pub fn required(path: impl Into<PathBuf>, kind: ArtifactKind) -> Self {
        Self {
            path: path.into(),
            kind,
            optional: false,
        }
    }

    pub fn optional(path: impl Into<PathBuf>, kind: ArtifactKind) -> Self {
        Self {
            path: path.into(),
            kind,
            optional: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceRequirements {
    pub cpu: u32,
    pub memory_mb: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePolicy {
    None,
    Dependency,
    Action,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InternalAction {
    None,
    Copy {
        from: Vec<PathBuf>,
        to: PathBuf,
    },
    MergeMachO {
        inputs: Vec<PathBuf>,
        output: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildStep {
    pub id: StepId,
    pub name: String,
    pub dependencies: Vec<StepId>,
    pub command: Option<CommandSpec>,
    pub action: InternalAction,
    pub inputs: Vec<PathBuf>,
    pub outputs: Vec<OutputSpec>,
    pub resources: ResourceRequirements,
    pub cache_policy: CachePolicy,
    pub timeout: Option<u64>,
}

impl BuildStep {
    pub fn command(id: &str, name: &str, spec: CommandSpec) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            dependencies: Vec::new(),
            timeout: spec.timeout_secs,
            command: Some(spec),
            action: InternalAction::None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            resources: ResourceRequirements::default(),
            cache_policy: CachePolicy::None,
        }
    }

    pub fn internal(id: &str, name: &str, action: InternalAction) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            dependencies: Vec::new(),
            command: None,
            action,
            inputs: Vec::new(),
            outputs: Vec::new(),
            resources: ResourceRequirements::default(),
            cache_policy: CachePolicy::None,
            timeout: Some(300),
        }
    }

    pub fn depends_on(mut self, ids: &[&str]) -> Self {
        self.dependencies.extend(ids.iter().map(|s| s.to_string()));
        self
    }

    pub fn output(mut self, output: OutputSpec) -> Self {
        self.outputs.push(output);
        self
    }

    pub fn cache(mut self, policy: CachePolicy) -> Self {
        self.cache_policy = policy;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPlan {
    pub id: String,
    pub project_name: String,
    pub project_root: PathBuf,
    pub framework: FrameworkKind,
    pub target: Target,
    pub steps: Vec<BuildStep>,
    pub expected_artifacts: Vec<PathBuf>,
    pub notes: Vec<String>,
}

impl BuildPlan {
    pub fn new(
        id: impl Into<String>,
        project_name: impl Into<String>,
        project_root: impl Into<PathBuf>,
        framework: FrameworkKind,
        target: Target,
    ) -> Self {
        Self {
            id: id.into(),
            project_name: project_name.into(),
            project_root: project_root.into(),
            framework,
            target,
            steps: Vec::new(),
            expected_artifacts: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn step(mut self, step: BuildStep) -> Self {
        self.steps.push(step);
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn step_map(&self) -> BTreeMap<&str, &BuildStep> {
        self.steps.iter().map(|s| (s.id.as_str(), s)).collect()
    }

    pub fn validate(&self) -> Result<()> {
        if self.steps.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::Internal,
                format!("build plan {} has no steps", self.id),
            ));
        }
        let mut seen = BTreeSet::new();
        for step in &self.steps {
            if !seen.insert(step.id.as_str()) {
                return Err(UbuilderError::new(
                    ErrorCode::Internal,
                    format!("duplicate step id '{}'", step.id),
                ));
            }
        }
        for step in &self.steps {
            for dep in &step.dependencies {
                if dep == &step.id {
                    return Err(UbuilderError::new(
                        ErrorCode::Internal,
                        format!("step '{}' depends on itself", step.id),
                    ));
                }
                if !seen.contains(dep.as_str()) {
                    return Err(UbuilderError::new(
                        ErrorCode::Internal,
                        format!("step '{}' depends on unknown step '{dep}'", step.id),
                    ));
                }
            }
        }
        self.topological_order()?;
        Ok(())
    }

    /// Kahn's algorithm; returns deterministic order (sorted ready set).
    pub fn topological_order(&self) -> Result<Vec<&str>> {
        let mut indegree: BTreeMap<&str, usize> = self
            .steps
            .iter()
            .map(|s| (s.id.as_str(), s.dependencies.len()))
            .collect();
        let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for step in &self.steps {
            for dep in &step.dependencies {
                dependents
                    .entry(dep.as_str())
                    .or_default()
                    .push(step.id.as_str());
            }
        }
        let mut ready: VecDeque<&str> = self
            .steps
            .iter()
            .filter(|s| s.dependencies.is_empty())
            .map(|s| s.id.as_str())
            .collect();
        let mut order = Vec::new();
        while let Some(id) = ready.pop_front() {
            order.push(id);
            if let Some(children) = dependents.get(id) {
                for child in children {
                    if let Some(entry) = indegree.get_mut(child) {
                        *entry -= 1;
                        if *entry == 0 {
                            ready.push_back(child);
                        }
                    }
                }
            }
        }
        if order.len() != self.steps.len() {
            let cyclic: Vec<&str> = indegree
                .into_iter()
                .filter(|(_, n)| *n > 0)
                .map(|(id, _)| id)
                .collect();
            return Err(UbuilderError::new(
                ErrorCode::Internal,
                format!("build plan {} contains a cycle", self.id),
            )
            .details(serde_json::json!({ "cyclic_steps": cyclic })));
        }
        Ok(order)
    }

    pub fn commands(&self) -> Vec<String> {
        self.steps
            .iter()
            .filter_map(|s| s.command.as_ref().map(|c| c.display()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::Target;

    fn spec(program: &str) -> CommandSpec {
        CommandSpec::new(program, "test")
    }

    fn plan() -> BuildPlan {
        BuildPlan::new(
            "b1",
            "demo",
            "/tmp/demo",
            FrameworkKind::Rust,
            Target::parse("linux-x64").unwrap(),
        )
    }

    #[test]
    fn detects_cycles() {
        let plan = plan()
            .step(BuildStep::command("a", "a", spec("a")).depends_on(&["b"]))
            .step(BuildStep::command("b", "b", spec("b")).depends_on(&["a"]));
        let err = plan.validate().unwrap_err();
        assert!(err.message.contains("cycle"));
    }

    #[test]
    fn rejects_unknown_dependencies_and_duplicates() {
        let broken = plan().step(BuildStep::command("a", "a", spec("a")).depends_on(&["nope"]));
        assert!(broken.validate().is_err());
        let plan = plan()
            .step(BuildStep::command("a", "a", spec("a")))
            .step(BuildStep::command("a", "again", spec("a")));
        assert!(plan.validate().is_err());
    }

    #[test]
    fn topological_order_is_deterministic() {
        let plan = plan()
            .step(BuildStep::command("build", "build", spec("cargo")))
            .step(
                BuildStep::internal(
                    "merge",
                    "merge",
                    InternalAction::MergeMachO {
                        inputs: vec![],
                        output: PathBuf::new(),
                    },
                )
                .depends_on(&["build"]),
            );
        assert_eq!(plan.topological_order().unwrap(), vec!["build", "merge"]);
        plan.validate().unwrap();
    }
}
