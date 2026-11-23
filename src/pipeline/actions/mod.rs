//! Built-in pipeline actions.
//!
//! Actions are the only place where the runtime touches project toolchains,
//! packaging and artifact storage. The YAML runtime itself is action-agnostic.

pub mod build;
pub mod cache_action;
pub mod checkout;
pub mod checksum;
pub mod package;
pub mod registry;
pub mod setup;
pub mod transfer;
pub mod verify;

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::pipeline::artifacts::{ArtifactRef, ArtifactStore};
use crate::pipeline::cache::PipelineCache;
use crate::pipeline::events::Events;
use crate::planner::BuildStep;
use crate::project::{FrameworkKind, Project};
use crate::target::Target;
use crate::telemetry::Ui;
use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy)]
pub struct InputSpec {
    pub name: &'static str,
    pub required: bool,
    pub default: Option<&'static str>,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildRecord {
    pub target: Target,
    pub project_name: String,
    pub version: String,
    pub framework: FrameworkKind,
    pub directory: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageRecord {
    pub target: Target,
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCacheSave {
    pub key: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionOutcome {
    pub outputs: IndexMap<String, String>,
    pub artifacts: Vec<ArtifactRef>,
    pub message: String,
}

impl ActionOutcome {
    pub fn plain(message: impl Into<String>) -> Self {
        Self {
            outputs: IndexMap::new(),
            artifacts: Vec::new(),
            message: message.into(),
        }
    }

    pub fn with_output(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.outputs.insert(key.into(), value.into());
        self
    }
}

pub struct ActionPlanContext<'a> {
    pub job_id: &'a str,
    pub workspace: &'a Path,
    pub target: Option<&'a Target>,
}

pub struct ActionContext<'a> {
    pub run_id: &'a str,
    pub job_id: &'a str,
    pub workspace: &'a Path,
    pub source_root: &'a Path,
    pub host: &'a Host,
    pub env: &'a IndexMap<String, String>,
    pub builds: &'a mut Vec<BuildRecord>,
    pub packages: &'a mut Vec<PackageRecord>,
    pub pending_cache: &'a mut Vec<PendingCacheSave>,
    pub artifact_store: &'a ArtifactStore,
    pub cache: &'a PipelineCache,
    pub ui: &'a Ui,
    pub events: &'a Events,
    pub cancel: &'a CancellationToken,
}

impl ActionContext<'_> {
    pub fn latest_build(&self) -> Option<&BuildRecord> {
        self.builds.last()
    }

    pub fn latest_package(&self) -> Option<&PackageRecord> {
        self.packages.last()
    }

    pub fn check_cancel(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            Err(UbuilderError::new(
                ErrorCode::Cancelled,
                "pipeline cancelled",
            ))
        } else {
            Ok(())
        }
    }

    pub fn workspace_path(&self, relative: &str) -> Result<PathBuf> {
        crate::pipeline::artifacts::safe_join(self.workspace, relative)
    }

    pub fn detect_project(&self) -> Result<Project> {
        crate::detect::discover(self.workspace, None, None)
    }
}

use std::path::PathBuf;

#[async_trait]
pub trait PipelineAction: Send + Sync {
    fn id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn inputs(&self) -> &'static [InputSpec];

    fn validate(&self, inputs: &IndexMap<String, String>) -> Result<()> {
        validate_inputs(self.id(), self.inputs(), inputs)
    }

    fn plan(&self, context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>>;

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome>;
}

pub fn validate_inputs(
    action: &str,
    specs: &[InputSpec],
    inputs: &IndexMap<String, String>,
) -> Result<()> {
    for key in inputs.keys() {
        if !specs.iter().any(|spec| spec.name == key) {
            let known = specs
                .iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("action {action} does not accept input '{key}'"),
            )
            .remediation(format!("Accepted inputs: {known}")));
        }
    }
    for spec in specs.iter().filter(|spec| spec.required) {
        if inputs
            .get(spec.name)
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("action {action} requires input '{}'", spec.name),
            )
            .remediation(spec.description.to_string()));
        }
    }
    Ok(())
}

pub fn with_defaults(
    specs: &[InputSpec],
    inputs: &IndexMap<String, String>,
) -> IndexMap<String, String> {
    let mut resolved = IndexMap::new();
    for spec in specs {
        if let Some(value) = inputs.get(spec.name) {
            resolved.insert(spec.name.to_string(), value.clone());
        } else if let Some(default) = spec.default {
            resolved.insert(spec.name.to_string(), default.to_string());
        }
    }
    for (key, value) in inputs {
        resolved.entry(key.clone()).or_insert_with(|| value.clone());
    }
    resolved
}

pub fn input<'a>(inputs: &'a IndexMap<String, String>, key: &str) -> Option<&'a str> {
    inputs.get(key).map(String::as_str)
}

pub fn require_input<'a>(inputs: &'a IndexMap<String, String>, key: &str) -> Result<&'a str> {
    input(inputs, key)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("required action input '{key}' is missing"),
            )
        })
}

/// Resolve an action path input. Relative paths are rooted at the workspace;
/// absolute paths must already be inside the workspace.
pub fn resolve_workspace_path(workspace: &Path, raw: &str) -> Result<PathBuf> {
    let candidate = Path::new(raw);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        crate::pipeline::artifacts::safe_join(workspace, raw)?
    };
    if let Ok(canonical) = joined.canonicalize() {
        let canonical_workspace = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.to_path_buf());
        if !canonical.starts_with(&canonical_workspace) {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("path '{raw}' escapes the job workspace"),
            ));
        }
    }
    Ok(joined)
}

pub fn command_word(inputs: &IndexMap<String, String>, key: &str) -> Option<String> {
    input(inputs, key).map(str::to_string)
}
