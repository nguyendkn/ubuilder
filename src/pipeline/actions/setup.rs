use super::{input, ActionContext, ActionOutcome, ActionPlanContext, InputSpec, PipelineAction};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::planner::{BuildStep, InternalAction};
use async_trait::async_trait;
use indexmap::IndexMap;
use std::process::Command;

const INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "version",
        required: false,
        default: Some(""),
        description: "Expected version prefix reported by the tool",
    },
    InputSpec {
        name: "channel",
        required: false,
        default: Some(""),
        description: "Alias for version (for example stable)",
    },
];

pub struct SetupAction {
    id: &'static str,
    tool: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

pub static SETUP_RUST: SetupAction = SetupAction {
    id: "builtin/setup-rust@v1",
    tool: "rust",
    program: "cargo",
    args: &["--version"],
};

pub static SETUP_DOTNET: SetupAction = SetupAction {
    id: "builtin/setup-dotnet@v1",
    tool: ".NET",
    program: "dotnet",
    args: &["--version"],
};

pub static SETUP_NODE: SetupAction = SetupAction {
    id: "builtin/setup-node@v1",
    tool: "Node.js",
    program: "node",
    args: &["--version"],
};

#[async_trait]
impl PipelineAction for SetupAction {
    fn id(&self) -> &'static str {
        self.id
    }

    fn description(&self) -> &'static str {
        "Verify that a native toolchain is available"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        INPUTS
    }

    fn plan(&self, _context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        Ok(vec![BuildStep::internal(
            "setup",
            &format!("verify {} toolchain", self.tool),
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let output = Command::new(self.program)
            .args(self.args)
            .output()
            .map_err(|error| {
                UbuilderError::new(
                    ErrorCode::ToolchainMissing,
                    format!("{} toolchain is missing ({error})", self.tool),
                )
                .remediation(format!(
                    "Install {} and ensure '{}' is on PATH",
                    self.tool, self.program
                ))
            })?;
        if !output.status.success() {
            return Err(UbuilderError::new(
                ErrorCode::ToolchainMissing,
                format!("'{} {}' failed", self.program, self.args.join(" ")),
            ));
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let expected = input(inputs, "version").unwrap_or("").trim();
        let expected = if expected.is_empty() {
            match input(inputs, "channel").unwrap_or("").trim() {
                "" | "stable" | "nightly" | "beta" => "",
                channel => channel,
            }
        } else {
            expected
        };
        if !expected.is_empty() && !version.starts_with(expected) {
            return Err(UbuilderError::new(
                ErrorCode::ToolchainMissing,
                format!(
                    "{} version {version} does not match required prefix {expected}",
                    self.tool
                ),
            )
            .remediation("Install the requested version or relax the 'version' input"));
        }
        Ok(
            ActionOutcome::plain(format!("{} {version}", self.tool))
                .with_output("version", version),
        )
    }
}
