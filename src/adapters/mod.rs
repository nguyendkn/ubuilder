//! Framework adapters: translate a detected project into a build plan.
//!
//! Adapters own framework knowledge (which tool to call, which artifacts to
//! expect). They do not execute anything and contain no scheduler logic.

pub mod dotnet;
pub mod electron;
pub mod rust;
pub mod tauri;

use crate::capability::{self, BuildCapability, CapabilityInput, Facts};
use crate::config::Config;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::planner::{BuildPlan, BuildStep};
use crate::project::{FrameworkKind, Project, ProjectMetadata};
use crate::target::Target;
use std::path::{Path, PathBuf};

pub struct PlanningContext<'a> {
    pub project: &'a Project,
    pub target: &'a Target,
    pub host: &'a Host,
    pub config: &'a Config,
    pub facts: &'a Facts,
    pub build_id: &'a str,
    /// Scratch directory for this target, e.g. `<project>/.ubuilder/work/<target>`.
    pub work_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub profile: String,
    pub offline: bool,
    pub no_cache: bool,
}

impl PlanningContext<'_> {
    pub fn triple(&self) -> Result<&str> {
        self.target.triple.as_deref().ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::UnsupportedTarget,
                format!("target {} has no single toolchain triple", self.target.id),
            )
        })
    }

    pub fn component_triples(&self) -> Vec<String> {
        self.target.component_triples()
    }

    pub fn profile_args(&self) -> Vec<String> {
        match self.profile.as_str() {
            "release" => vec!["--release".into()],
            "debug" | "dev" => vec![],
            other => vec!["--profile".into(), other.to_string()],
        }
    }

    pub fn cargo_profile_dir(&self) -> &'static str {
        match self.profile.as_str() {
            "release" => "release",
            "debug" | "dev" => "debug",
            // Cargo puts custom profiles in a directory of the same name.
            _ => "release",
        }
    }

    pub fn work_subdir(&self, name: &str) -> PathBuf {
        self.work_dir.join(name)
    }

    pub fn ensure_work_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.work_dir)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", self.work_dir.display()), e))
    }

    pub fn rust_target_directory(&self) -> Result<PathBuf> {
        match &self.project.metadata {
            ProjectMetadata::Rust(meta) => Ok(meta.target_directory.clone()),
            ProjectMetadata::Tauri(meta) => Ok(meta
                .cargo_manifest
                .parent()
                .unwrap_or(Path::new("."))
                .join("target")),
            _ => Err(UbuilderError::new(
                ErrorCode::Internal,
                "project is not Rust-based",
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Detection {
    pub detected: bool,
    pub confidence: u8,
    pub reasons: Vec<String>,
}

impl Detection {
    pub fn yes(confidence: u8, reason: impl Into<String>) -> Self {
        Self {
            detected: true,
            confidence,
            reasons: vec![reason.into()],
        }
    }

    pub fn no(reason: impl Into<String>) -> Self {
        Self {
            detected: false,
            confidence: 0,
            reasons: vec![reason.into()],
        }
    }
}

pub trait BuildAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn framework(&self) -> FrameworkKind;
    fn detect(&self, project: &Project) -> Detection;
    fn capabilities(
        &self,
        project: &Project,
        host: &Host,
        target: &Target,
        facts: &Facts,
    ) -> BuildCapability {
        resolve_capabilities(project, host, target, facts)
    }
    fn plan(&self, ctx: &PlanningContext<'_>) -> Result<BuildPlan>;
}

pub fn adapter_for(framework: FrameworkKind) -> Box<dyn BuildAdapter> {
    match framework {
        FrameworkKind::Rust => Box::new(rust::RustAdapter),
        FrameworkKind::Dotnet => Box::new(dotnet::DotnetAdapter),
        FrameworkKind::Electron => Box::new(electron::ElectronAdapter),
        FrameworkKind::Tauri => Box::new(tauri::TauriAdapter),
    }
}

pub fn resolve_capabilities(
    project: &Project,
    host: &Host,
    target: &Target,
    facts: &Facts,
) -> BuildCapability {
    let mut input = CapabilityInput::new(project.framework);
    match &project.metadata {
        ProjectMetadata::Dotnet(meta) => {
            input.dotnet_flavor = Some(meta.flavor);
            input.native_aot = meta.native_aot;
        }
        ProjectMetadata::Electron(meta) => {
            input.has_native_node_modules = meta.has_native_modules;
        }
        _ => {}
    }
    capability::resolve(host, target, &input, facts)
}

pub fn new_plan(ctx: &PlanningContext<'_>) -> BuildPlan {
    BuildPlan::new(
        format!("{}-{}", ctx.build_id, ctx.target.id),
        &ctx.project.name,
        &ctx.project.root,
        ctx.project.framework,
        ctx.target.clone(),
    )
}

/// Expected executable name for a Cargo package.
pub fn cargo_binary_path(
    target_dir: &Path,
    triple: &str,
    profile_dir: &str,
    package_name: &str,
    suffix: &str,
) -> PathBuf {
    target_dir
        .join(triple)
        .join(profile_dir)
        .join(format!("{package_name}{suffix}"))
}

pub fn step_failure_hint(step: &BuildStep, output: &Path) -> UbuilderError {
    UbuilderError::new(
        ErrorCode::ArtifactInvalid,
        format!(
            "step '{}' did not produce expected output {}",
            step.id,
            output.display()
        ),
    )
    .details(serde_json::json!({
        "step": step.id,
        "expected": output.to_string_lossy(),
    }))
}

pub fn read_toml_package_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_binary_path_uses_triple_and_profile() {
        let path = cargo_binary_path(
            Path::new("/tmp/target"),
            "x86_64-apple-darwin",
            "release",
            "demo",
            "",
        );
        assert_eq!(
            path,
            Path::new("/tmp/target/x86_64-apple-darwin/release/demo")
        );
    }
}
