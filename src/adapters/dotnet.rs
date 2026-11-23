//! .NET adapter (`dotnet publish`).

use super::{new_plan, BuildAdapter, Detection, PlanningContext};
use crate::config::ReleaseMode;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::CommandSpec;
use crate::planner::{ArtifactKind, BuildPlan, BuildStep, CachePolicy, InternalAction, OutputSpec};
use crate::project::{DotnetProject, FrameworkKind, Project, ProjectMetadata};
use std::path::PathBuf;

pub struct DotnetAdapter;

impl BuildAdapter for DotnetAdapter {
    fn id(&self) -> &'static str {
        "dotnet"
    }

    fn framework(&self) -> FrameworkKind {
        FrameworkKind::Dotnet
    }

    fn detect(&self, project: &Project) -> Detection {
        if matches!(project.metadata, ProjectMetadata::Dotnet(_)) {
            Detection::yes(100, ".NET project or solution")
        } else {
            Detection::no("not a .NET project")
        }
    }

    fn plan(&self, ctx: &PlanningContext<'_>) -> Result<BuildPlan> {
        let ProjectMetadata::Dotnet(meta) = &ctx.project.metadata else {
            return Err(UbuilderError::new(
                ErrorCode::Internal,
                "dotnet adapter received a non-dotnet project",
            ));
        };
        let Some(primary) = meta.projects.first() else {
            return Err(UbuilderError::new(
                ErrorCode::Usage,
                "no .NET project found in the solution",
            ));
        };
        ctx.ensure_work_dir()?;
        let suffix = ctx.target.executable_suffix();
        let mut plan = new_plan(ctx);
        if meta.projects.len() > 1 {
            plan = plan.note(format!(
                "publishing primary project {}; {} other project(s) are not packaged",
                primary.name,
                meta.projects.len() - 1
            ));
        }
        let configuration = match ctx.profile.as_str() {
            "release" => "Release".to_string(),
            "debug" | "dev" => "Debug".to_string(),
            other => other.to_string(),
        };

        let rids: Vec<String> = if ctx.target.universal {
            ctx.target
                .component_targets()
                .into_iter()
                .filter_map(|t| t.rid)
                .collect()
        } else {
            ctx.target.rid.iter().cloned().collect()
        };
        if rids.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::UnsupportedTarget,
                format!(".NET target {} has no runtime identifier", ctx.target.id),
            ));
        }

        let self_contained = meta.native_aot
            || meta.self_contained_hint
            || ctx.config.release.mode == ReleaseMode::Portable;
        let mut build_ids = Vec::new();
        let mut publish_dirs: Vec<PathBuf> = Vec::new();
        for rid in &rids {
            let publish_dir = ctx.work_subdir(&format!("publish-{rid}"));
            let step_id = format!("dotnet-publish-{rid}");
            let mut spec = CommandSpec::new("dotnet", format!("dotnet publish ({rid})"))
                .cwd(&ctx.project.root)
                .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
                .env("DOTNET_NOLOGO", "1")
                .env("DOTNET_SKIP_FIRST_TIME_EXPERIENCE", "1")
                .timeout_secs(ctx.config.build.timeout_secs.unwrap_or(1800));
            spec = spec
                .arg("publish")
                .arg(primary.path.to_string_lossy().to_string())
                .arg("--configuration")
                .arg(&configuration)
                .arg("--runtime")
                .arg(rid)
                .arg("--self-contained")
                .arg(if self_contained { "true" } else { "false" })
                .arg("--output")
                .arg(publish_dir.to_string_lossy().to_string())
                .arg("--nologo");
            if meta.native_aot {
                spec = spec.arg("-p:PublishAot=true");
            }
            if meta.projects.len() > 1 {
                spec = spec.arg("-p:BuildProjectReferences=true");
            }
            if let Some(tfm) = primary.target_frameworks.first() {
                spec = spec.arg("--framework").arg(tfm);
            }
            if ctx.offline {
                spec = spec.arg("--no-restore");
            }
            let step = BuildStep::command(&step_id, &format!("dotnet publish ({rid})"), spec)
                .output(OutputSpec::required(
                    publish_dir.clone(),
                    ArtifactKind::Directory,
                ))
                .cache(CachePolicy::None);
            plan = plan.step(step);
            build_ids.push(step_id);
            publish_dirs.push(publish_dir);
        }

        if ctx.target.universal {
            let merged = ctx.work_subdir(&format!("{}{suffix}", primary.name));
            let inputs: Vec<PathBuf> = publish_dirs
                .iter()
                .map(|dir| dir.join(format!("{}{suffix}", primary.name)))
                .collect();
            let merge = BuildStep::internal(
                "merge-universal",
                "merge universal Mach-O apphost",
                InternalAction::MergeMachO {
                    inputs,
                    output: merged.clone(),
                },
            )
            .depends_on(&build_ids.iter().map(String::as_str).collect::<Vec<_>>())
            .output(OutputSpec::required(
                merged.clone(),
                ArtifactKind::Executable,
            ));
            plan = plan.step(merge);
            plan.expected_artifacts.push(merged);
            plan = plan.note(
                "self-contained universal .NET output uses the x64 publish directory; \
                 the merged executable replaces its apphost",
            );
        } else {
            plan.expected_artifacts.extend(publish_dirs);
        }

        if meta.native_aot {
            plan = plan.note("Native AOT publishing requested (PublishAot=true)");
        }
        plan.validate()?;
        Ok(plan)
    }
}

#[allow(dead_code)]
fn flavor_label(meta: &DotnetProject) -> &'static str {
    meta.flavor.as_str()
}
