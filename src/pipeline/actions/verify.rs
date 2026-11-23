use super::{input, ActionContext, ActionOutcome, ActionPlanContext, InputSpec, PipelineAction};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::planner::{BuildStep, InternalAction};
use crate::target::Target;
use crate::validation::{self, ValidationStatus};
use async_trait::async_trait;
use indexmap::IndexMap;

const INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "path",
        required: false,
        default: Some(""),
        description: "Artifact path; defaults to the most recent package output",
    },
    InputSpec {
        name: "target",
        required: false,
        default: Some(""),
        description: "Target whose architecture/format should be validated",
    },
];

pub struct VerifyAction;

#[async_trait]
impl PipelineAction for VerifyAction {
    fn id(&self) -> &'static str {
        "builtin/verify@v1"
    }

    fn description(&self) -> &'static str {
        "Validate an artifact's format and architecture"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        INPUTS
    }

    fn validate(&self, inputs: &IndexMap<String, String>) -> Result<()> {
        super::validate_inputs(self.id(), INPUTS, inputs)?;
        if let Some(target) = input(inputs, "target").filter(|value| !value.is_empty()) {
            Target::parse(target)?;
        }
        Ok(())
    }

    fn plan(&self, _context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        Ok(vec![BuildStep::internal(
            "verify",
            "validate artifact format and architecture",
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let latest = context.latest_package().cloned();
        let raw_path = input(inputs, "path").filter(|value| !value.is_empty());
        let path = match raw_path {
            Some(raw) => super::resolve_workspace_path(context.workspace, raw)?,
            None => latest
                .as_ref()
                .map(|package| package.path.clone())
                .ok_or_else(|| {
                    UbuilderError::new(
                        ErrorCode::ConfigInvalid,
                        "verify action needs a path or a prior package step",
                    )
                })?,
        };
        let target = match input(inputs, "target").filter(|value| !value.is_empty()) {
            Some(raw) => Target::parse(raw)?,
            None => latest
                .map(|package| package.target.clone())
                .or_else(|| context.latest_build().map(|build| build.target.clone()))
                .unwrap_or_else(|| {
                    Target::all()
                        .into_iter()
                        .find(|target| target.is_native(context.host))
                        .unwrap_or_else(|| Target::all()[0].clone())
                }),
        };
        let report = validation::validate_artifact(&path, &target)?;
        if report.status == ValidationStatus::Failed {
            let failed = report
                .checks
                .iter()
                .filter(|check| check.status == ValidationStatus::Failed)
                .map(|check| format!("{}: {}", check.name, check.detail))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(UbuilderError::new(
                ErrorCode::ArtifactInvalid,
                format!(
                    "artifact {} failed {} validation: {failed}",
                    path.display(),
                    target.id
                ),
            )
            .details(serde_json::to_value(&report).unwrap_or_default()));
        }
        let status = report.status.as_str().to_string();
        Ok(ActionOutcome::plain(format!(
            "verified {} for {}: {}",
            path.display(),
            target.id,
            status
        ))
        .with_output("path", path.to_string_lossy().to_string())
        .with_output("status", status)
        .with_output("target", target.id))
    }
}
