//! Rust / Cargo adapter.

use super::{cargo_binary_path, new_plan, BuildAdapter, Detection, PlanningContext};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::CommandSpec;
use crate::planner::{ArtifactKind, BuildPlan, BuildStep, InternalAction, OutputSpec};
use crate::project::{FrameworkKind, Project, ProjectMetadata};
use std::path::PathBuf;

pub struct RustAdapter;

impl BuildAdapter for RustAdapter {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn framework(&self) -> FrameworkKind {
        FrameworkKind::Rust
    }

    fn detect(&self, project: &Project) -> Detection {
        if matches!(project.metadata, ProjectMetadata::Rust(_)) {
            Detection::yes(100, "Cargo manifest with binary or library targets")
        } else {
            Detection::no("not a Cargo project")
        }
    }

    fn plan(&self, ctx: &PlanningContext<'_>) -> Result<BuildPlan> {
        let ProjectMetadata::Rust(meta) = &ctx.project.metadata else {
            return Err(UbuilderError::new(
                ErrorCode::Internal,
                "rust adapter received a non-rust project",
            ));
        };
        if meta.all_binaries().is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::Usage,
                format!(
                    "project '{}' has no binary targets; only applications can be built",
                    ctx.project.name
                ),
            )
            .remediation("Add a [[bin]] target or a src/main.rs entry point"));
        }
        ctx.ensure_work_dir()?;
        let target_dir = &meta.target_directory;
        let suffix = ctx.target.executable_suffix();
        let profile_dir = ctx.cargo_profile_dir();
        let mut plan = new_plan(ctx);

        let triples = ctx.component_triples();
        if triples.is_empty() {
            return Err(UbuilderError::new(
                ErrorCode::UnsupportedTarget,
                format!("no Rust triple for target {}", ctx.target.id),
            ));
        }

        let mut build_ids: Vec<String> = Vec::new();
        for triple in &triples {
            let step_id = format!("cargo-build-{triple}");
            let mut step = BuildStep::command(
                &step_id,
                &format!("cargo build ({triple})"),
                build_command(ctx, triple),
            );
            if !ctx.facts.rust_target(triple) && ctx.config.toolchains.auto_install {
                let install_id = format!("rustup-target-{triple}");
                plan = plan.step(
                    BuildStep::command(
                        &install_id,
                        &format!("rustup target add {triple}"),
                        CommandSpec::new("rustup", format!("install {triple}"))
                            .args(["target", "add", triple])
                            .timeout_secs(600)
                            .retryable(true),
                    )
                    .cache(crate::planner::CachePolicy::Dependency),
                );
                step = step.depends_on(&[&install_id]);
            } else if !ctx.facts.rust_target(triple) {
                plan = plan.note(format!(
                    "Rust target {triple} is not installed; `rustup target add {triple}` is required"
                ));
            }
            for package in meta.all_binaries() {
                for binary in &package.binaries {
                    let path = cargo_binary_path(target_dir, triple, profile_dir, binary, suffix);
                    step = step.output(OutputSpec::required(path, ArtifactKind::Executable));
                }
            }
            plan = plan.step(step);
            build_ids.push(step_id);
        }

        if ctx.target.universal {
            for package in meta.all_binaries() {
                for binary in &package.binaries {
                    let inputs: Vec<PathBuf> = triples
                        .iter()
                        .map(|triple| {
                            cargo_binary_path(target_dir, triple, profile_dir, binary, suffix)
                        })
                        .collect();
                    let output = ctx.work_dir.join(format!("{binary}{suffix}"));
                    let merge_id = format!("merge-{binary}");
                    let step = BuildStep::internal(
                        &merge_id,
                        &format!("merge Mach-O {binary}"),
                        InternalAction::MergeMachO {
                            inputs,
                            output: output.clone(),
                        },
                    )
                    .depends_on(&build_ids.iter().map(String::as_str).collect::<Vec<_>>())
                    .output(OutputSpec::required(output, ArtifactKind::Executable));
                    plan = plan.step(step);
                    plan.expected_artifacts
                        .push(ctx.work_dir.join(format!("{binary}{suffix}")));
                }
            }
        } else {
            for package in meta.all_binaries() {
                for binary in &package.binaries {
                    plan.expected_artifacts.push(cargo_binary_path(
                        target_dir,
                        &triples[0],
                        profile_dir,
                        binary,
                        suffix,
                    ));
                }
            }
        }

        plan.validate()?;
        Ok(plan)
    }
}

fn build_command(ctx: &PlanningContext<'_>, triple: &str) -> CommandSpec {
    let root = match &ctx.project.metadata {
        ProjectMetadata::Rust(meta) => meta.workspace_root.clone(),
        _ => ctx.project.root.clone(),
    };
    let mut spec = CommandSpec::new("cargo", format!("cargo build --target {triple}"))
        .cwd(&root)
        .env("CARGO_TERM_COLOR", "never")
        .env(
            "CARGO_NET_OFFLINE",
            if ctx.offline { "true" } else { "false" },
        )
        .timeout_secs(ctx.config.build.timeout_secs.unwrap_or(1800));
    spec = spec.args(["build", "--target", triple, "--bins"]);
    if let ProjectMetadata::Rust(meta) = &ctx.project.metadata {
        if meta.is_workspace {
            spec = spec.arg("--workspace");
        }
        if meta.workspace_root.join("Cargo.lock").is_file() {
            spec = spec.arg("--locked");
        }
    }
    for arg in ctx.profile_args() {
        spec = spec.arg(arg);
    }
    if ctx.offline {
        spec = spec.arg("--offline");
    }
    spec
}
