use super::{
    input, resolve_workspace_path, ActionContext, ActionOutcome, ActionPlanContext, InputSpec,
    PipelineAction,
};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::planner::{BuildStep, InternalAction};
use async_trait::async_trait;
use indexmap::IndexMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "path",
        required: false,
        default: Some("dist"),
        description: "Workspace-relative file or directory to hash",
    },
    InputSpec {
        name: "output",
        required: false,
        default: Some("checksums.sha256"),
        description: "Workspace-relative checksum file",
    },
];

pub struct ChecksumAction;

#[async_trait]
impl PipelineAction for ChecksumAction {
    fn id(&self) -> &'static str {
        "builtin/checksum@v1"
    }

    fn description(&self) -> &'static str {
        "Write SHA-256 checksums for a file or artifact tree"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        INPUTS
    }

    fn plan(&self, _context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        Ok(vec![BuildStep::internal(
            "checksum",
            "write checksums.sha256",
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let source =
            resolve_workspace_path(context.workspace, input(inputs, "path").unwrap_or("dist"))?;
        if !source.exists() {
            return Err(UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("checksum path {} does not exist", source.display()),
            ));
        }
        let output = super::resolve_workspace_path(
            context.workspace,
            input(inputs, "output").unwrap_or("checksums.sha256"),
        )?;
        let files = collect_files(&source)?;
        if files.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!("checksum path {} contains no files", source.display()),
            ));
        }
        let mut lines = Vec::new();
        for file in &files {
            if file == &output {
                continue;
            }
            let relative = file
                .strip_prefix(context.workspace)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            let sha = crate::artifacts::sha256_file(file)?;
            lines.push(format!("{sha}  {relative}"));
        }
        lines.sort();
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| UbuilderError::io("cannot create checksum directory", e))?;
        }
        std::fs::write(&output, format!("{}\n", lines.join("\n")))
            .map_err(|e| UbuilderError::io("cannot write checksums", e))?;
        let sha256 = crate::artifacts::sha256_file(&output)?;
        Ok(ActionOutcome::plain(format!(
            "wrote {} checksum(s) to {}",
            lines.len(),
            output.display()
        ))
        .with_output("path", output.to_string_lossy().to_string())
        .with_output("sha256", sha256)
        .with_output("count", lines.len().to_string()))
    }
}

fn collect_files(root: &Path) -> Result<Vec<PathBuf>> {
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }
    Ok(files)
}
