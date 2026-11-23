//! Electron adapter (package install + electron-builder unpacked output).

use super::{new_plan, BuildAdapter, Detection, PlanningContext};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::executor::CommandSpec;
use crate::planner::{ArtifactKind, BuildPlan, BuildStep, CachePolicy, OutputSpec};
use crate::project::{FrameworkKind, PackageManager, Project, ProjectMetadata};
use std::path::{Path, PathBuf};

pub struct ElectronAdapter;

impl BuildAdapter for ElectronAdapter {
    fn id(&self) -> &'static str {
        "electron"
    }

    fn framework(&self) -> FrameworkKind {
        FrameworkKind::Electron
    }

    fn detect(&self, project: &Project) -> Detection {
        if matches!(project.metadata, ProjectMetadata::Electron(_)) {
            Detection::yes(100, "package.json with an Electron dependency")
        } else {
            Detection::no("not an Electron project")
        }
    }

    fn plan(&self, ctx: &PlanningContext<'_>) -> Result<BuildPlan> {
        let ProjectMetadata::Electron(meta) = &ctx.project.metadata else {
            return Err(UbuilderError::new(
                ErrorCode::Internal,
                "electron adapter received a non-electron project",
            ));
        };
        if !meta.has_electron_builder && !has_local_builder(&ctx.project.root) {
            return Err(UbuilderError::new(
                ErrorCode::ToolchainMissing,
                "electron-builder is not installed in this project",
            )
            .remediation(
                "Run `npm install --save-dev electron-builder` or add it to devDependencies",
            ));
        }
        ctx.ensure_work_dir()?;
        let mut plan = new_plan(ctx);
        let output_dir = ctx.work_subdir("electron");

        let install = BuildStep::command(
            "install-deps",
            "install Node dependencies",
            install_spec(meta.package_manager, &ctx.project.root, ctx.offline),
        )
        .cache(CachePolicy::Dependency);
        plan = plan.step(install);

        let mut dependencies = vec!["install-deps".to_string()];
        if meta.scripts.contains_key("build") {
            let build = BuildStep::command(
                "frontend-build",
                "build frontend",
                run_script(meta.package_manager, &ctx.project.root, "build")
                    .timeout_secs(ctx.config.build.timeout_secs.unwrap_or(1800)),
            )
            .depends_on(&["install-deps"]);
            plan = plan.step(build);
            dependencies.push("frontend-build".into());
        } else {
            plan = plan.note("no package.json `build` script; packaging files as-is");
        }

        let args = electron_builder_args(ctx, &output_dir);
        let mut spec = CommandSpec::new("npx", "electron-builder unpacked")
            .cwd(&ctx.project.root)
            .env(
                "ELECTRON_CACHE",
                ctx.cache_dir.join("electron").to_string_lossy().to_string(),
            )
            .env(
                "ELECTRON_BUILDER_CACHE",
                ctx.cache_dir
                    .join("electron-builder")
                    .to_string_lossy()
                    .to_string(),
            )
            .timeout_secs(ctx.config.build.timeout_secs.unwrap_or(1800));
        if let Some(local) = local_builder(&ctx.project.root) {
            spec = CommandSpec {
                program: local.to_string_lossy().to_string(),
                ..spec
            };
        } else {
            spec = spec.arg("--no-install").arg("electron-builder");
        }
        for arg in args {
            spec = spec.arg(arg);
        }
        let builder = BuildStep::command("electron-builder", "electron-builder (unpacked)", spec)
            .depends_on(&dependencies.iter().map(String::as_str).collect::<Vec<_>>())
            .output(OutputSpec::required(
                output_dir.clone(),
                ArtifactKind::Directory,
            ));
        plan = plan.step(builder);
        plan.expected_artifacts.push(output_dir);

        if ctx.target.universal {
            plan = plan.note(
                "electron-builder performs the universal macOS merge; @electron/universal is required",
            );
        }
        plan.validate()?;
        Ok(plan)
    }
}

fn has_local_builder(root: &Path) -> bool {
    local_builder(root).is_some()
}

fn local_builder(root: &Path) -> Option<PathBuf> {
    let base = root.join("node_modules").join(".bin");
    for name in ["electron-builder.cmd", "electron-builder"] {
        let candidate = base.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn install_spec(pm: PackageManager, root: &Path, offline: bool) -> CommandSpec {
    let mut spec = CommandSpec::new(pm.command(), "install dependencies")
        .cwd(root)
        .env("npm_config_audit", "false")
        .env("npm_config_fund", "false")
        .timeout_secs(1800)
        .retryable(true);
    let lock = match pm {
        PackageManager::Npm => "package-lock.json",
        PackageManager::Pnpm => "pnpm-lock.yaml",
        PackageManager::Yarn => "yarn.lock",
        PackageManager::Bun => "bun.lockb",
    };
    let has_lock = root.join(lock).is_file();
    match pm {
        PackageManager::Npm => {
            spec = spec.arg(if has_lock { "ci" } else { "install" });
            if offline {
                spec = spec.arg("--offline");
            }
        }
        PackageManager::Pnpm => {
            spec = spec.arg("install");
            if has_lock {
                spec = spec.arg("--frozen-lockfile");
            }
            if offline {
                spec = spec.arg("--offline");
            }
        }
        PackageManager::Yarn => {
            spec = spec.arg("install");
            if has_lock {
                spec = spec.arg("--frozen-lockfile");
            }
            if offline {
                spec = spec.arg("--offline");
            }
        }
        PackageManager::Bun => {
            spec = spec.arg("install");
            if has_lock {
                spec = spec.arg("--frozen-lockfile");
            }
            if offline {
                spec = spec.arg("--offline");
            }
        }
    }
    if !has_lock {
        spec = spec.arg("--no-audit");
    }
    spec
}

fn run_script(pm: PackageManager, root: &Path, script: &str) -> CommandSpec {
    CommandSpec::new(pm.command(), format!("run {script}"))
        .cwd(root)
        .args(["run", script])
}

fn electron_builder_args(ctx: &PlanningContext<'_>, output_dir: &Path) -> Vec<String> {
    let mut args = vec![
        "--dir".to_string(),
        format!("-c.directories.output={}", output_dir.display()),
        "--publish".to_string(),
        "never".to_string(),
    ];
    match ctx.target.os {
        crate::target::TargetOs::Windows => args.push("--win".into()),
        crate::target::TargetOs::Macos => args.push("--mac".into()),
        crate::target::TargetOs::Linux => args.push("--linux".into()),
    }
    if let Some(arch) = ctx.target.arch() {
        args.push(format!("--{}", arch.as_str()));
    } else if ctx.target.universal {
        args.push("--universal".into());
    }
    args
}
