//! Toolchain providers: one per framework/toolchain, each able to probe itself,
//! report feature levels, and propose explicit provisioning commands.

pub mod facts;

use crate::capability::{Facts, SupportLevel};
use crate::host::Host;
use crate::project::{FrameworkKind, Project};
use crate::target::Target;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct ToolchainProbe {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub version: Option<String>,
    pub executable: Option<PathBuf>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderCapability {
    pub feature: String,
    pub level: SupportLevel,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProvisionStep {
    pub id: String,
    pub description: String,
    pub program: String,
    pub args: Vec<String>,
    pub requires_approval: bool,
}

impl ProvisionStep {
    pub fn new(id: &str, description: impl Into<String>, program: &str, args: &[&str]) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            requires_approval: true,
        }
    }

    pub fn command_line(&self) -> String {
        let mut line = self.program.clone();
        for arg in &self.args {
            line.push(' ');
            line.push_str(arg);
        }
        line
    }
}

pub struct ProbeContext<'a> {
    pub host: &'a Host,
    pub project: Option<&'a Project>,
    pub project_root: Option<&'a Path>,
}

impl ProbeContext<'_> {
    pub fn framework_root(&self) -> Option<&Path> {
        self.project_root
            .or_else(|| self.project.map(|p| p.root.as_path()))
    }
}

pub trait ToolchainProvider {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn probe(&self, ctx: &ProbeContext<'_>, facts: &Facts) -> ToolchainProbe;
    fn capabilities(&self, facts: &Facts) -> Vec<ProviderCapability>;
    fn provision_plan(
        &self,
        facts: &Facts,
        target: Option<&Target>,
        project_root: Option<&Path>,
    ) -> Vec<ProvisionStep>;
}

pub fn collect_facts(ctx: &ProbeContext<'_>) -> Facts {
    facts::collect(
        ctx.host,
        ctx.project_root
            .or_else(|| ctx.project.map(|p| p.root.as_path())),
    )
}

pub fn all() -> Vec<Box<dyn ToolchainProvider>> {
    vec![
        Box::new(RustProvider),
        Box::new(DotnetProvider),
        Box::new(ElectronProvider),
        Box::new(TauriProvider),
    ]
}

pub fn provider_for(framework: FrameworkKind) -> Box<dyn ToolchainProvider> {
    match framework {
        FrameworkKind::Rust => Box::new(RustProvider),
        FrameworkKind::Dotnet => Box::new(DotnetProvider),
        FrameworkKind::Electron => Box::new(ElectronProvider),
        FrameworkKind::Tauri => Box::new(TauriProvider),
    }
}

pub fn probe_all(ctx: &ProbeContext<'_>, facts: &Facts) -> Vec<ToolchainProbe> {
    all().iter().map(|p| p.probe(ctx, facts)).collect()
}

fn capability(feature: &str, level: SupportLevel, detail: impl Into<String>) -> ProviderCapability {
    ProviderCapability {
        feature: feature.into(),
        level,
        detail: detail.into(),
    }
}

struct RustProvider;

impl ToolchainProvider for RustProvider {
    fn id(&self) -> &'static str {
        "rust"
    }

    fn display_name(&self) -> &'static str {
        "Rust (cargo)"
    }

    fn probe(&self, _ctx: &ProbeContext<'_>, facts: &Facts) -> ToolchainProbe {
        let mut notes = Vec::new();
        if facts.rustc_version.is_none() {
            notes.push("rustc not found".into());
        }
        if facts.cargo_version.is_none() {
            notes.push("cargo not found".into());
        }
        if facts.has("rustup") {
            notes.push(format!(
                "{} installed target(s)",
                facts.installed_rust_targets.len()
            ));
        } else {
            notes.push("rustup not found; cross targets must be installed manually".into());
        }
        ToolchainProbe {
            id: self.id().into(),
            name: self.display_name().into(),
            available: facts.cargo_version.is_some(),
            version: facts.rustc_version.clone(),
            executable: which::which("cargo").ok(),
            notes,
        }
    }

    fn capabilities(&self, facts: &Facts) -> Vec<ProviderCapability> {
        let available = facts.has("cargo");
        let cross = facts.has("cross")
            || facts.has("cargo-xwin")
            || facts.has("zig")
            || facts.osxcross
            || facts.has("gcc")
            || facts.has("clang")
            || facts.has("lld-link");
        vec![
            capability(
                "compile-native",
                if available {
                    SupportLevel::Supported
                } else {
                    SupportLevel::Unsupported
                },
                "cargo build for the host target",
            ),
            capability(
                "compile-cross",
                if !available {
                    SupportLevel::Unsupported
                } else if cross {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Experimental
                },
                "requires a cross linker or cargo-xwin/cross/zig/osxcross",
            ),
            capability(
                "universal-merge",
                if available {
                    SupportLevel::Supported
                } else {
                    SupportLevel::Unsupported
                },
                "pure-Rust fat Mach-O merge",
            ),
            capability(
                "package",
                if available {
                    SupportLevel::Supported
                } else {
                    SupportLevel::Unsupported
                },
                "zip, tar.gz, deb, app bundle, dmg/nsis/appimage when tools exist",
            ),
            capability(
                "sign",
                if facts.codesign || facts.signtool {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                },
                "codesign or signtool required",
            ),
        ]
    }

    fn provision_plan(
        &self,
        facts: &Facts,
        target: Option<&Target>,
        _project_root: Option<&Path>,
    ) -> Vec<ProvisionStep> {
        let mut steps = Vec::new();
        if let Some(target) = target {
            for triple in target.component_triples() {
                if !facts.rust_target(triple.as_str()) {
                    steps.push(ProvisionStep::new(
                        &format!("rustup-target-{triple}"),
                        format!("Install Rust standard library for {triple}"),
                        "rustup",
                        &["target", "add", triple.as_str()],
                    ));
                }
            }
            if target.id.starts_with("windows-")
                && !facts.has("lld-link")
                && !facts.has("cargo-xwin")
            {
                steps.push(ProvisionStep::new(
                    "cargo-xwin",
                    "Install cargo-xwin for Windows cross-linking",
                    "cargo",
                    &["install", "cargo-xwin"],
                ));
            }
        }
        steps
    }
}

struct DotnetProvider;

impl ToolchainProvider for DotnetProvider {
    fn id(&self) -> &'static str {
        "dotnet"
    }

    fn display_name(&self) -> &'static str {
        ".NET SDK"
    }

    fn probe(&self, _ctx: &ProbeContext<'_>, facts: &Facts) -> ToolchainProbe {
        let mut notes = Vec::new();
        if facts.dotnet_sdk.is_none() {
            notes.push("dotnet SDK not found".into());
        }
        if !facts.has("clang")
            && !facts.has("gcc")
            && cfg!(target_os = "windows")
            && !facts.has("lld-link")
        {
            notes.push("no C compiler/linker; NativeAOT will be unavailable".into());
        }
        ToolchainProbe {
            id: self.id().into(),
            name: self.display_name().into(),
            available: facts.dotnet_sdk.is_some(),
            version: facts.dotnet_sdk.clone(),
            executable: which::which("dotnet").ok(),
            notes,
        }
    }

    fn capabilities(&self, facts: &Facts) -> Vec<ProviderCapability> {
        let available = facts.dotnet_sdk.is_some();
        let native_aot = facts.has("clang") || facts.has("gcc") || facts.has("lld-link");
        vec![
            capability(
                "compile-native",
                if available {
                    SupportLevel::Supported
                } else {
                    SupportLevel::Unsupported
                },
                "dotnet publish --runtime <rid>",
            ),
            capability(
                "compile-cross",
                if available {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                },
                "framework-dependent cross publish; self-contained needs runtime packs",
            ),
            capability(
                "native-aot",
                if available && native_aot {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Experimental
                },
                "requires a matching C toolchain and ILCompiler",
            ),
            capability(
                "package",
                if available {
                    SupportLevel::Supported
                } else {
                    SupportLevel::Unsupported
                },
                "directory payload plus generic formats",
            ),
            capability(
                "sign",
                if facts.signtool || facts.codesign {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                },
                "signtool/codesign required",
            ),
        ]
    }

    fn provision_plan(
        &self,
        _facts: &Facts,
        _target: Option<&Target>,
        _project_root: Option<&Path>,
    ) -> Vec<ProvisionStep> {
        Vec::new()
    }
}

struct ElectronProvider;

impl ToolchainProvider for ElectronProvider {
    fn id(&self) -> &'static str {
        "electron"
    }

    fn display_name(&self) -> &'static str {
        "Electron + electron-builder"
    }

    fn probe(&self, ctx: &ProbeContext<'_>, facts: &Facts) -> ToolchainProbe {
        let mut notes = Vec::new();
        if facts.node_version.is_none() {
            notes.push("node not found".into());
        }
        if facts.electron_builder_version.is_none() {
            notes.push("electron-builder not detected in the project or on PATH".into());
        }
        let has_dep = ctx
            .project
            .and_then(|p| match &p.metadata {
                crate::project::ProjectMetadata::Electron(e) => Some(e.has_electron_builder),
                _ => None,
            })
            .unwrap_or(false);
        if has_dep {
            notes.push("electron-builder declared in package.json".into());
        }
        ToolchainProbe {
            id: self.id().into(),
            name: self.display_name().into(),
            available: facts.node_version.is_some()
                && (facts.electron_builder_version.is_some() || has_dep),
            version: facts
                .electron_builder_version
                .clone()
                .or_else(|| facts.node_version.clone()),
            executable: which::which("node").ok(),
            notes,
        }
    }

    fn capabilities(&self, facts: &Facts) -> Vec<ProviderCapability> {
        let node = facts.node_version.is_some();
        vec![
            capability(
                "compile",
                if node {
                    SupportLevel::Supported
                } else {
                    SupportLevel::Unsupported
                },
                "npm/pnpm/yarn install and frontend build",
            ),
            capability(
                "package-host",
                if node && facts.electron_builder_version.is_some() {
                    SupportLevel::Supported
                } else {
                    SupportLevel::Conditional
                },
                "electron-builder for the host OS",
            ),
            capability(
                "package-cross",
                if node && facts.electron_builder_version.is_some() {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Experimental
                },
                "electron-builder cross-targets; native modules and signing may require the target OS",
            ),
            capability(
                "sign",
                if facts.codesign || facts.signtool {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                },
                "codesign or signtool required",
            ),
        ]
    }

    fn provision_plan(
        &self,
        facts: &Facts,
        _target: Option<&Target>,
        project_root: Option<&Path>,
    ) -> Vec<ProvisionStep> {
        let mut steps = Vec::new();
        if facts.node_version.is_none() {
            steps.push(ProvisionStep::new(
                "node",
                "Install Node.js LTS",
                "winget",
                &["install", "OpenJS.NodeJS.LTS"],
            ));
        }
        let has_local = project_root
            .and_then(|root| facts::local_bin(root, "electron-builder"))
            .is_some();
        if !has_local && facts.electron_builder_version.is_none() {
            steps.push(ProvisionStep::new(
                "electron-builder",
                "Install electron-builder in the project",
                "npm",
                &["install", "--save-dev", "electron-builder"],
            ));
        }
        steps
    }
}

struct TauriProvider;

impl ToolchainProvider for TauriProvider {
    fn id(&self) -> &'static str {
        "tauri"
    }

    fn display_name(&self) -> &'static str {
        "Tauri v2"
    }

    fn probe(&self, _ctx: &ProbeContext<'_>, facts: &Facts) -> ToolchainProbe {
        let mut notes = Vec::new();
        if facts.tauri_cli_version.is_none() {
            notes.push("Tauri CLI not detected; ubuilder drives cargo directly".into());
        }
        if facts.cargo_version.is_none() {
            notes.push("cargo not found".into());
        }
        if facts.node_version.is_none() {
            notes.push("node not found; frontend build must be skipped or pre-built".into());
        }
        ToolchainProbe {
            id: self.id().into(),
            name: self.display_name().into(),
            available: facts.cargo_version.is_some(),
            version: facts.tauri_cli_version.clone(),
            executable: which::which("cargo").ok(),
            notes,
        }
    }

    fn capabilities(&self, facts: &Facts) -> Vec<ProviderCapability> {
        let cargo = facts.has("cargo");
        vec![
            capability(
                "compile",
                if cargo {
                    SupportLevel::Supported
                } else {
                    SupportLevel::Unsupported
                },
                "cargo build on src-tauri",
            ),
            capability(
                "package",
                if cargo {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                },
                "ubuilder produces generic formats; platform WebView2/GTK runtime is required on the target",
            ),
            capability(
                "sign",
                if facts.codesign || facts.signtool {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                },
                "codesign or signtool required",
            ),
            capability(
                "native-test",
                if cargo && facts.cargo_version.is_some() {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                },
                "requires a runnable host target",
            ),
        ]
    }

    fn provision_plan(
        &self,
        facts: &Facts,
        target: Option<&Target>,
        _project_root: Option<&Path>,
    ) -> Vec<ProvisionStep> {
        let Some(target) = target else {
            return Vec::new();
        };
        target
            .component_triples()
            .into_iter()
            .filter(|triple| !facts.rust_target(triple))
            .map(|triple| {
                ProvisionStep::new(
                    &format!("rustup-target-{triple}"),
                    format!("Install Rust standard library for {triple}"),
                    "rustup",
                    &["target", "add", &triple],
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_cover_every_framework() {
        for framework in [
            FrameworkKind::Rust,
            FrameworkKind::Dotnet,
            FrameworkKind::Electron,
            FrameworkKind::Tauri,
        ] {
            assert!(!provider_for(framework).id().is_empty());
        }
    }

    #[test]
    fn rust_provision_plans_rustup_target() {
        let facts = Facts::with_commands(&["cargo"]);
        let target = Target::parse("linux-arm64").unwrap();
        let steps = RustProvider.provision_plan(&facts, Some(&target), None);
        assert!(steps.iter().any(|s| s.program == "rustup"));
        assert!(steps
            .iter()
            .any(|s| s.args.contains(&"aarch64-unknown-linux-gnu".to_string())));
    }

    #[test]
    fn missing_toolchain_is_unsupported() {
        let facts = Facts::default();
        let caps = RustProvider.capabilities(&facts);
        assert_eq!(caps[0].level, SupportLevel::Unsupported);
    }
}
