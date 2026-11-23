//! Project domain model: framework identity and per-framework metadata.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameworkKind {
    Rust,
    Dotnet,
    Electron,
    Tauri,
}

impl FrameworkKind {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.to_ascii_lowercase().as_str() {
            "auto" => return None,
            "rust" | "cargo" => Self::Rust,
            "dotnet" | ".net" | "csharp" => Self::Dotnet,
            "electron" => Self::Electron,
            "tauri" => Self::Tauri,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Dotnet => "dotnet",
            Self::Electron => "electron",
            Self::Tauri => "tauri",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Dotnet => ".NET",
            Self::Electron => "Electron",
            Self::Tauri => "Tauri v2",
        }
    }

    pub fn all() -> [FrameworkKind; 4] {
        [
            FrameworkKind::Rust,
            FrameworkKind::Dotnet,
            FrameworkKind::Electron,
            FrameworkKind::Tauri,
        ]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCandidate {
    pub framework: FrameworkKind,
    pub root: PathBuf,
    pub manifest: PathBuf,
    pub depth: usize,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub root: PathBuf,
    pub name: String,
    pub version: Option<String>,
    pub framework: FrameworkKind,
    pub manifest_path: PathBuf,
    pub metadata: ProjectMetadata,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<ProjectCandidate>,
}

impl Project {
    pub fn display_name(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectMetadata {
    Rust(RustProject),
    Dotnet(DotnetProject),
    Electron(ElectronProject),
    Tauri(TauriProject),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustProject {
    pub is_workspace: bool,
    pub packages: Vec<RustPackage>,
    pub target_directory: PathBuf,
    pub workspace_root: PathBuf,
    pub default_members: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustPackage {
    pub name: String,
    pub manifest_path: PathBuf,
    pub binaries: Vec<String>,
    pub library: bool,
    pub cdylib: bool,
}

impl RustProject {
    pub fn all_binaries(&self) -> Vec<&RustPackage> {
        self.packages
            .iter()
            .filter(|p| !p.binaries.is_empty())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DotnetFlavor {
    Console,
    Avalonia,
    Wpf,
    WinForms,
    Maui,
    Other,
}

impl DotnetFlavor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Avalonia => "avalonia",
            Self::Wpf => "wpf",
            Self::WinForms => "winforms",
            Self::Maui => "maui",
            Self::Other => "other",
        }
    }

    pub fn is_windows_only(self) -> bool {
        matches!(self, Self::Wpf | Self::WinForms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DotnetProject {
    pub projects: Vec<DotnetProjectInfo>,
    pub solution: Option<PathBuf>,
    pub flavor: DotnetFlavor,
    pub native_aot: bool,
    pub self_contained_hint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DotnetProjectInfo {
    pub name: String,
    pub path: PathBuf,
    pub target_frameworks: Vec<String>,
    pub runtime_identifiers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

impl PackageManager {
    pub fn command(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElectronProject {
    pub package_json: PathBuf,
    pub package_manager: PackageManager,
    pub electron_version: Option<String>,
    pub scripts: std::collections::BTreeMap<String, String>,
    pub has_electron_builder: bool,
    pub has_native_modules: bool,
    pub main: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TauriProject {
    pub config_path: PathBuf,
    pub cargo_manifest: PathBuf,
    pub product_name: Option<String>,
    pub identifier: Option<String>,
    pub frontend_dist: Option<PathBuf>,
    pub before_build_command: Option<String>,
    pub dev_url: Option<String>,
    pub tauri_version: Option<String>,
    pub bundle_targets: Vec<String>,
    pub package_manager: PackageManager,
    pub frontend_scripts: std::collections::BTreeMap<String, String>,
}

pub fn file_stem_name(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framework_parse_auto_is_none() {
        assert!(FrameworkKind::parse("auto").is_none());
        assert_eq!(FrameworkKind::parse("Tauri"), Some(FrameworkKind::Tauri));
        assert_eq!(FrameworkKind::parse(".NET"), Some(FrameworkKind::Dotnet));
    }
}
