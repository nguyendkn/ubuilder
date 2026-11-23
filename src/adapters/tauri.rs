//! Tauri v2 adapter: frontend build plus Cargo build of the Rust shell.

use super::{new_plan, read_toml_package_name, BuildAdapter, Detection, PlanningContext};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::CommandSpec;
use crate::planner::{ArtifactKind, BuildPlan, BuildStep, CachePolicy, InternalAction, OutputSpec};
use crate::project::{FrameworkKind, PackageManager, Project, ProjectMetadata};
use std::path::{Path, PathBuf};

pub struct TauriAdapter;

impl BuildAdapter for TauriAdapter {
    fn id(&self) -> &'static str {
        "tauri"
    }

    fn framework(&self) -> FrameworkKind {
        FrameworkKind::Tauri
    }

    fn detect(&self, project: &Project) -> Detection {
        if matches!(project.metadata, ProjectMetadata::Tauri(_)) {
            Detection::yes(100, "tauri.conf.json plus src-tauri/Cargo.toml")
        } else {
            Detection::no("not a Tauri v2 project")
        }
    }

    fn plan(&self, ctx: &PlanningContext<'_>) -> Result<BuildPlan> {
        let ProjectMetadata::Tauri(meta) = &ctx.project.metadata else {
            return Err(UbuilderError::new(
                ErrorCode::Internal,
                "tauri adapter received a non-tauri project",
            ));
        };
        ctx.ensure_work_dir()?;
        let src_tauri = meta
            .cargo_manifest
            .parent()
            .ok_or_else(|| UbuilderError::new(ErrorCode::Internal, "invalid Cargo manifest path"))?
            .to_path_buf();
        let package_name = read_toml_package_name(&meta.cargo_manifest).ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "cannot read package.name from {}",
                    meta.cargo_manifest.display()
                ),
            )
        })?;
        let suffix = ctx.target.executable_suffix();
        let mut plan = new_plan(ctx);
        let mut dependencies: Vec<String> = Vec::new();

        if ctx.project.root.join("package.json").is_file() {
            let install = BuildStep::command(
                "install-deps",
                "install frontend dependencies",
                install_spec(meta.package_manager, &ctx.project.root, ctx.offline),
            )
            .cache(CachePolicy::Dependency);
            plan = plan.step(install);
            dependencies.push("install-deps".into());
        }
        if let Some(command) = &meta.before_build_command {
            let step = BuildStep::command(
                "frontend-build",
                "beforeBuildCommand",
                shell_spec(&ctx.project.root, command)
                    .timeout_secs(ctx.config.build.timeout_secs.unwrap_or(1800)),
            )
            .depends_on(&dependencies.iter().map(String::as_str).collect::<Vec<_>>());
            plan = plan.step(step);
            dependencies.push("frontend-build".into());
        } else if meta.frontend_scripts.contains_key("build") {
            let step = BuildStep::command(
                "frontend-build",
                "build frontend",
                CommandSpec::new(meta.package_manager.command(), "build frontend")
                    .cwd(&ctx.project.root)
                    .args(["run", "build"])
                    .timeout_secs(ctx.config.build.timeout_secs.unwrap_or(1800)),
            )
            .depends_on(&dependencies.iter().map(String::as_str).collect::<Vec<_>>());
            plan = plan.step(step);
            dependencies.push("frontend-build".into());
        } else if meta.frontend_dist.is_some() {
            plan = plan.note("using the committed frontendDist directory");
        }

        let triples = ctx.component_triples();
        if triples.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::UnsupportedTarget,
                format!("no Rust triple for target {}", ctx.target.id),
            ));
        }
        let target_dir = src_tauri.join("target");
        let profile_dir = ctx.cargo_profile_dir();
        let mut build_ids = Vec::new();
        for triple in &triples {
            let step_id = format!("cargo-build-{triple}");
            let mut spec = CommandSpec::new("cargo", format!("cargo build --target {triple}"))
                .cwd(&src_tauri)
                .arg("build")
                .arg("--target")
                .arg(triple)
                .arg("--manifest-path")
                .arg(meta.cargo_manifest.to_string_lossy().to_string())
                .env("CARGO_TERM_COLOR", "never")
                .timeout_secs(ctx.config.build.timeout_secs.unwrap_or(1800));
            for arg in ctx.profile_args() {
                spec = spec.arg(arg);
            }
            if ctx.offline {
                spec = spec.arg("--offline");
            }
            let output = target_dir
                .join(triple)
                .join(profile_dir)
                .join(format!("{package_name}{suffix}"));
            let step = BuildStep::command(&step_id, &format!("cargo build ({triple})"), spec)
                .depends_on(&dependencies.iter().map(String::as_str).collect::<Vec<_>>())
                .output(OutputSpec::required(
                    output.clone(),
                    ArtifactKind::Executable,
                ));
            plan = plan.step(step);
            build_ids.push(step_id);
        }

        if ctx.target.universal {
            let merged = ctx.work_dir.join(format!("{package_name}{suffix}"));
            let inputs: Vec<PathBuf> = triples
                .iter()
                .map(|triple| {
                    target_dir
                        .join(triple)
                        .join(profile_dir)
                        .join(format!("{package_name}{suffix}"))
                })
                .collect();
            let merge = BuildStep::internal(
                "merge-universal",
                "merge universal Mach-O binary",
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
        } else {
            plan.expected_artifacts.push(
                target_dir
                    .join(&triples[0])
                    .join(profile_dir)
                    .join(format!("{package_name}{suffix}")),
            );
        }

        plan.validate()?;
        Ok(plan)
    }
}

fn install_spec(pm: PackageManager, root: &Path, offline: bool) -> CommandSpec {
    let lock = match pm {
        PackageManager::Npm => "package-lock.json",
        PackageManager::Pnpm => "pnpm-lock.yaml",
        PackageManager::Yarn => "yarn.lock",
        PackageManager::Bun => "bun.lockb",
    };
    let has_lock = root.join(lock).is_file();
    let mut spec = CommandSpec::new(pm.command(), "install frontend dependencies")
        .cwd(root)
        .timeout_secs(1800)
        .retryable(true)
        .env("npm_config_audit", "false")
        .env("npm_config_fund", "false");
    match pm {
        PackageManager::Npm => {
            spec = spec.arg(if has_lock { "ci" } else { "install" });
        }
        _ => {
            spec = spec.arg("install");
            if has_lock {
                spec = spec.arg("--frozen-lockfile");
            }
        }
    }
    if offline {
        spec = spec.arg("--offline");
    }
    spec
}

fn shell_spec(root: &Path, command: &str) -> CommandSpec {
    #[cfg(windows)]
    let spec = CommandSpec::new("cmd", "beforeBuildCommand")
        .cwd(root)
        .args(["/C", command]);
    #[cfg(not(windows))]
    let spec = CommandSpec::new("sh", "beforeBuildCommand")
        .cwd(root)
        .args(["-c", command]);
    spec
}
