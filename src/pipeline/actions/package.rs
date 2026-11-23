use super::{
    input, ActionContext, ActionOutcome, ActionPlanContext, InputSpec, PackageRecord,
    PipelineAction,
};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::packaging::{self, PackageContext};
use crate::planner::{BuildStep, InternalAction};
use crate::target::PackagingFormat;
use async_trait::async_trait;
use indexmap::IndexMap;

const INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "format",
        required: false,
        default: Some("portable"),
        description: "Packaging format (zip, tar.gz, deb, rpm, nsis, app, dmg, ...)",
    },
    InputSpec {
        name: "target",
        required: false,
        default: Some(""),
        description: "Override the build target; defaults to the most recent build",
    },
];

pub struct PackageAction;

#[async_trait]
impl PipelineAction for PackageAction {
    fn id(&self) -> &'static str {
        "builtin/package@v1"
    }

    fn description(&self) -> &'static str {
        "Package a prior build payload with the native packaging backend"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        INPUTS
    }

    fn validate(&self, inputs: &IndexMap<String, String>) -> Result<()> {
        super::validate_inputs(self.id(), INPUTS, inputs)?;
        let format = input(inputs, "format").unwrap_or("portable");
        if PackagingFormat::parse(format).is_none() {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("unknown packaging format '{format}'"),
            ));
        }
        Ok(())
    }

    fn plan(&self, _context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        Ok(vec![BuildStep::internal(
            "package",
            "package build payload",
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let build = context.latest_build().cloned().ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                "package action has no build to package",
            )
            .remediation("Run builtin/build@v1 earlier in the same job")
        })?;
        if let Some(raw) = input(inputs, "target")
            .filter(|value| !value.is_empty() && *value != "native" && *value != "auto")
        {
            if raw != build.target.id {
                return Err(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!(
                        "package target {raw} does not match build target {}",
                        build.target.id
                    ),
                ));
            }
        }
        let raw_format = input(inputs, "format").unwrap_or("portable");
        let format = PackagingFormat::parse(raw_format).ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("unknown packaging format '{raw_format}'"),
            )
        })?;
        let output_dir = context.workspace.join("dist").join("packages");
        let work_dir = context
            .workspace
            .join(".ubuilder")
            .join("pipeline")
            .join("package");
        std::fs::create_dir_all(&output_dir)
            .map_err(|e| UbuilderError::io("cannot create package output directory", e))?;
        std::fs::create_dir_all(&work_dir)
            .map_err(|e| UbuilderError::io("cannot create package work directory", e))?;
        let identifier = detect_identifier(context).unwrap_or(None);
        let package_context = PackageContext {
            project_name: &build.project_name,
            version: &build.version,
            target: &build.target,
            framework: build.framework,
            output_dir: &output_dir,
            work_dir: &work_dir,
            identifier: identifier.as_deref(),
        };
        let payload =
            packaging::prepare_payload(&package_context, std::slice::from_ref(&build.directory))?;
        let results = packaging::package_all(&payload, &package_context, &[format])?;
        let result = results.into_iter().next().ok_or_else(|| {
            UbuilderError::new(ErrorCode::ExecutionFailed, "packaging produced no output")
        })?;
        context.packages.push(PackageRecord {
            target: build.target.clone(),
            path: result.path.clone(),
            sha256: result.sha256.clone(),
            size: result.size,
            format: result.format.as_str().to_string(),
        });
        Ok(ActionOutcome::plain(format!(
            "packaged {} ({})",
            result.path.display(),
            result.format.as_str()
        ))
        .with_output("path", result.path.to_string_lossy().to_string())
        .with_output("sha256", result.sha256)
        .with_output("size", result.size.to_string())
        .with_output("format", result.format.as_str())
        .with_output("target", build.target.id))
    }
}

fn detect_identifier(context: &ActionContext<'_>) -> Result<Option<String>> {
    let project = match context.detect_project() {
        Ok(project) => project,
        Err(_) => return Ok(None),
    };
    Ok(match &project.metadata {
        crate::project::ProjectMetadata::Tauri(meta) => meta.identifier.clone(),
        _ => None,
    })
}
