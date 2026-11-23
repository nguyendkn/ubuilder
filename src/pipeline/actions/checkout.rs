use super::{input, ActionContext, ActionOutcome, ActionPlanContext, InputSpec, PipelineAction};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::pipeline::artifacts::safe_join;
use crate::planner::{BuildStep, InternalAction};
use async_trait::async_trait;
use indexmap::IndexMap;
use std::path::Path;
use walkdir::WalkDir;

const INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "path",
        required: false,
        default: Some("."),
        description: "Workspace-relative destination directory",
    },
    InputSpec {
        name: "snapshot",
        required: false,
        default: Some("false"),
        description:
            "Copy a source snapshot into an isolated workspace (always true for local runs)",
    },
];

pub struct CheckoutAction;

#[async_trait]
impl PipelineAction for CheckoutAction {
    fn id(&self) -> &'static str {
        "builtin/checkout@v1"
    }

    fn description(&self) -> &'static str {
        "Copy the project source into the isolated job workspace"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        INPUTS
    }

    fn plan(&self, _context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        Ok(vec![BuildStep::internal(
            "checkout",
            "checkout project source",
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let relative = input(inputs, "path").unwrap_or(".");
        let destination = safe_join(context.workspace, relative)?;
        if context.source_root.canonicalize().ok() == destination.canonicalize().ok() {
            return Ok(ActionOutcome::plain("source already at workspace")
                .with_output("path", destination.to_string_lossy().to_string()));
        }
        copy_source(context.source_root, &destination)?;
        Ok(
            ActionOutcome::plain(format!("checked out {}", context.source_root.display()))
                .with_output("path", destination.to_string_lossy().to_string()),
        )
    }
}

/// Copy project files without VCS data, prior build outputs or dependency
/// directories. Symlinks are followed only when their target stays in source.
fn copy_source(source: &Path, destination: &Path) -> Result<()> {
    if !source.is_dir() {
        return Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("project root {} is not a directory", source.display()),
        ));
    }
    let canonical_source = source
        .canonicalize()
        .map_err(|e| UbuilderError::io("cannot canonicalize project source", e))?;
    if destination.exists() {
        clear_directory(destination)?;
    }
    std::fs::create_dir_all(destination)
        .map_err(|e| UbuilderError::io("cannot create checkout destination", e))?;
    for entry in WalkDir::new(source)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| !excluded(entry.path(), source))
    {
        let entry = entry.map_err(|e| {
            UbuilderError::new(
                ErrorCode::Io,
                format!("cannot walk {}: {e}", source.display()),
            )
        })?;
        let relative = entry.path().strip_prefix(source).unwrap_or(entry.path());
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| UbuilderError::io("cannot create checked-out directory", e))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| UbuilderError::io("cannot create checkout directory", e))?;
            }
            std::fs::copy(entry.path(), &target)
                .map_err(|e| UbuilderError::io("cannot copy checked-out file", e))?;
        } else if entry.file_type().is_symlink() {
            let resolved = entry
                .path()
                .canonicalize()
                .map_err(|e| UbuilderError::io("cannot resolve source symlink", e))?;
            if !resolved.starts_with(&canonical_source) {
                return Err(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!(
                        "source symlink {} points outside the project",
                        entry.path().display()
                    ),
                ));
            }
            if resolved.is_file() {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| UbuilderError::io("cannot create checkout directory", e))?;
                }
                std::fs::copy(&resolved, &target)
                    .map_err(|e| UbuilderError::io("cannot copy symlink target", e))?;
            }
        }
    }
    Ok(())
}

fn excluded(path: &Path, root: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(".git" | "target" | "node_modules" | ".ubuilder" | "dist" | "dist-smoke")
        )
    })
}

fn clear_directory(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)
        .map_err(|e| UbuilderError::io("cannot read checkout destination", e))?
    {
        let entry = entry.map_err(|e| UbuilderError::io("cannot read checkout entry", e))?;
        let target = entry.path();
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            std::fs::remove_dir_all(&target)
                .map_err(|e| UbuilderError::io("cannot clear checkout directory", e))?;
        } else {
            let _ = std::fs::remove_file(&target);
        }
    }
    Ok(())
}
