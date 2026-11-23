use super::{
    input, ActionContext, ActionOutcome, ActionPlanContext, BuildRecord, InputSpec, PipelineAction,
};
use crate::builder::{self, BuildOptions};
use crate::config::{Config, Overrides};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::planner::{BuildStep, InternalAction};
use crate::target::{PackagingFormat, Target};
use async_trait::async_trait;
use indexmap::IndexMap;

const INPUTS: &[InputSpec] = &[
    InputSpec {
        name: "framework",
        required: false,
        default: Some("auto"),
        description: "Expected framework (auto, rust, dotnet, electron, tauri)",
    },
    InputSpec {
        name: "target",
        required: false,
        default: Some("native"),
        description: "Canonical target id, for example windows-x64 or macos-arm64",
    },
    InputSpec {
        name: "profile",
        required: false,
        default: Some(""),
        description: "Build profile (defaults to ubuilder.toml, normally debug)",
    },
    InputSpec {
        name: "offline",
        required: false,
        default: Some("false"),
        description: "Avoid network access where toolchains allow",
    },
    InputSpec {
        name: "no-cache",
        required: false,
        default: Some("false"),
        description: "Disable the action cache for this build",
    },
];

pub struct BuildAction;

#[async_trait]
impl PipelineAction for BuildAction {
    fn id(&self) -> &'static str {
        "builtin/build@v1"
    }

    fn description(&self) -> &'static str {
        "Compile the project for one target using the native adapter"
    }

    fn inputs(&self) -> &'static [InputSpec] {
        INPUTS
    }

    fn validate(&self, inputs: &IndexMap<String, String>) -> Result<()> {
        super::validate_inputs(self.id(), INPUTS, inputs)?;
        if let Some(target) =
            input(inputs, "target").filter(|value| !value.is_empty() && *value != "native")
        {
            Target::parse(target)?;
        }
        Ok(())
    }

    fn plan(&self, context: &ActionPlanContext<'_>) -> Result<Vec<BuildStep>> {
        let target = context
            .target
            .map(|target| target.id.clone())
            .unwrap_or_else(|| "native".into());
        Ok(vec![BuildStep::internal(
            "build",
            &format!("compile for {target}"),
            InternalAction::None,
        )])
    }

    async fn execute(
        &self,
        context: &mut ActionContext<'_>,
        inputs: &IndexMap<String, String>,
    ) -> Result<ActionOutcome> {
        context.check_cancel()?;
        let project = context.detect_project()?;
        if let Some(raw) =
            input(inputs, "framework").filter(|value| !value.is_empty() && *value != "auto")
        {
            let expected = crate::project::FrameworkKind::parse(raw).ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!("unknown framework '{raw}'"),
                )
                .remediation("Use auto, rust, dotnet, electron or tauri")
            })?;
            if expected != project.framework {
                return Err(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!(
                        "framework '{raw}' was requested but the project is {}",
                        project.framework.as_str()
                    ),
                ));
            }
        }
        if !project.root.starts_with(context.workspace) {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                "project was detected outside the job workspace",
            )
            .remediation("Add a builtin/checkout@v1 step before builtin/build@v1"));
        }
        let target = resolve_target(input(inputs, "target").unwrap_or("native"), context.host)?;
        let config = Config::load(&project.root, context.host, &Overrides::default())?;
        let profile = input(inputs, "profile")
            .filter(|value| !value.is_empty())
            .unwrap_or(&config.build.profile)
            .to_string();
        let output = context.workspace.join("dist").join("build");
        let options = BuildOptions {
            profile: profile.clone(),
            jobs: 1,
            formats: vec![PackagingFormat::Directory],
            output: Some(output.clone()),
            offline: parse_bool(input(inputs, "offline"), false),
            no_cache: parse_bool(input(inputs, "no-cache"), false),
            continue_on_error: false,
            release: profile == "release",
            dry_run: false,
        };
        let report = builder::build_one(
            &project,
            context.host,
            &config,
            &options,
            &target,
            context.ui,
            context.cancel,
        )
        .await?;
        if report.status == "failed" {
            return Err(UbuilderError::new(
                ErrorCode::ExecutionFailed,
                format!("build of {} for {} failed", project.name, target.id),
            )
            .details(serde_json::to_value(&report).unwrap_or_default()));
        }
        let directory = report
            .packages
            .first()
            .map(|package| package.path.clone())
            .or_else(|| {
                report
                    .artifacts
                    .first()
                    .map(|artifact| artifact.path.clone())
            })
            .ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::ExecutionFailed,
                    "build reported success but produced no payload directory",
                )
            })?;
        let sha256 = crate::artifacts::hash_path(&directory)?;
        context.builds.push(BuildRecord {
            target: target.clone(),
            project_name: project.name.clone(),
            version: project.version.clone().unwrap_or_else(|| "0.0.0".into()),
            framework: project.framework,
            directory: directory.clone(),
            sha256: sha256.clone(),
        });
        Ok(ActionOutcome::plain(format!(
            "built {} for {} ({})",
            project.name, target.id, profile
        ))
        .with_output("path", directory.to_string_lossy().to_string())
        .with_output("target", target.id.clone())
        .with_output("project", project.name.clone())
        .with_output(
            "version",
            project.version.clone().unwrap_or_else(|| "0.0.0".into()),
        )
        .with_output("framework", project.framework.as_str())
        .with_output("sha256", sha256))
    }
}

fn resolve_target(raw: &str, host: &crate::host::Host) -> Result<Target> {
    if raw == "native" || raw.is_empty() {
        return Target::all()
            .into_iter()
            .find(|target| target.is_native(host))
            .ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::UnsupportedTarget,
                    format!("no native target mapping for host {}", host.display()),
                )
            });
    }
    Target::parse(raw)
}

fn parse_bool(raw: Option<&str>, default: bool) -> bool {
    raw.map(|value| matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(default)
}
