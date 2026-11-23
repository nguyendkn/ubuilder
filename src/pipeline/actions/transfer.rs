use super::{
    input, require_input, resolve_workspace_path, ActionContext, ActionOutcome, ActionPlanContext,
    InputSpec, PipelineAction,
};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::artifacts::{safe_filename, safe_join};
use crate::planner::{BuildStep, InternalAction};
use async_trait::async_trait;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};

const UPLOAD_INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "name",
        required: true,
        default: None,
        description: "Artifact name used by download-artifact",
    },
    InputSpec {
        name: "path",
        required: true,
        default: None,
        description: "Workspace-relative file, directory or glob",
    },
];

const DOWNLOAD_INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "name",
        required: false,
        default: None,
        description: "Artifact name or glob from an earlier job in this run",
    },
    InputSpec {
        name: "pattern",
        required: false,
        default: None,
        description: "Alias for name when selecting artifacts by glob",
    },
    InputSpec {
        name: "path",
        required: false,
        default: Some("artifacts"),
        description: "Workspace-relative download destination",
    },
];

pub struct UploadArtifactAction;
pub struct DownloadArtifactAction;

#[async_trait]
impl PipelineAction for UploadArtifactAction {
    fn id(&self) -> &'static str {
        "builtin/upload-artifact@v1"
    }

    fn description(&self) -> &'static str {
        "Publish files or directories to the run artifact store"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        UPLOAD_INPUTS
    }

    fn validate(&self, inputs: &IndexMap<String, String>) -> Result<()> {
        super::validate_inputs(self.id(), UPLOAD_INPUTS, inputs)?;
        crate::pipeline::artifacts::validate_name(require_input(inputs, "name")?)
    }

    fn plan(&self, _context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        Ok(vec![BuildStep::internal(
            "upload-artifact",
            "upload artifact",
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let name = require_input(inputs, "name")?;
        let raw_path = require_input(inputs, "path")?;
        let paths = expand_paths(context.workspace, raw_path)?;
        if paths.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("upload path '{raw_path}' matched nothing"),
            ));
        }
        let multiple = paths.len() > 1;
        let mut refs = Vec::new();
        for path in paths {
            let artifact_name = if multiple {
                let filename = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("artifact");
                format!("{name}-{}", safe_filename(filename))
            } else {
                name.to_string()
            };
            let artifact = context.artifact_store.upload(
                context.run_id,
                context.job_id,
                &artifact_name,
                &path,
            )?;
            refs.push(artifact);
        }
        let names = refs
            .iter()
            .map(|artifact| artifact.name.clone())
            .collect::<Vec<_>>()
            .join(",");
        let first = refs.first().expect("non-empty");
        let outcome = ActionOutcome {
            outputs: IndexMap::from([
                ("name".to_string(), names.clone()),
                ("sha256".to_string(), first.sha256.clone()),
                ("size".to_string(), first.size.to_string()),
                ("count".to_string(), refs.len().to_string()),
            ]),
            artifacts: refs.clone(),
            message: format!("uploaded {names} ({} bytes)", first.size),
        };
        Ok(outcome)
    }
}

#[async_trait]
impl PipelineAction for DownloadArtifactAction {
    fn id(&self) -> &'static str {
        "builtin/download-artifact@v1"
    }

    fn description(&self) -> &'static str {
        "Download artifacts produced by an earlier job"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        DOWNLOAD_INPUTS
    }

    fn validate(&self, inputs: &IndexMap<String, String>) -> Result<()> {
        super::validate_inputs(self.id(), DOWNLOAD_INPUTS, inputs)?;
        let selector = input(inputs, "name")
            .or_else(|| input(inputs, "pattern"))
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    "download-artifact requires 'name' or 'pattern'",
                )
            })?;
        if selector.contains(['/', '\\']) {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                "artifact name pattern must not contain path separators",
            ));
        }
        Ok(())
    }

    fn plan(&self, _context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        Ok(vec![BuildStep::internal(
            "download-artifact",
            "download artifact",
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let name = input(inputs, "name")
            .or_else(|| input(inputs, "pattern"))
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    "download-artifact requires 'name' or 'pattern'",
                )
            })?;
        let destination = safe_join(
            context.workspace,
            input(inputs, "path").unwrap_or("artifacts"),
        )?;
        let paths = context
            .artifact_store
            .download(context.run_id, name, &destination)?;
        Ok(ActionOutcome::plain(format!(
            "downloaded {} artifact path(s) into {}",
            paths.len(),
            destination.display()
        ))
        .with_output("path", destination.to_string_lossy().to_string())
        .with_output("count", paths.len().to_string()))
    }
}

fn expand_paths(workspace: &Path, raw: &str) -> Result<Vec<PathBuf>> {
    if !raw.contains(['*', '?', '[']) {
        let path = resolve_workspace_path(workspace, raw)?;
        return if path.exists() {
            Ok(vec![path])
        } else {
            Ok(Vec::new())
        };
    }
    let base = raw.split(['*', '?', '[']).next().unwrap_or("");
    let base_dir = Path::new(base)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default();
    let _ = safe_join(workspace, &base_dir)?;
    let pattern = workspace.join(raw).to_string_lossy().to_string();
    let canonical_workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut matches = Vec::new();
    for entry in glob::glob(&pattern).map_err(|error| {
        UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("invalid upload glob '{raw}': {error}"),
        )
    })? {
        let path = entry.map_err(|error| {
            UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("cannot read upload glob match: {error}"),
            )
        })?;
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !canonical.starts_with(&canonical_workspace) {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("upload path '{}' escapes the workspace", path.display()),
            ));
        }
        matches.push(path);
    }
    matches.sort();
    matches.dedup();
    Ok(matches)
}
