//! Project discovery and framework detection.
//!
//! Detection is read-only: it inspects manifests and may run `cargo metadata`
//! (which resolves the workspace but runs no build scripts).

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::project::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::{DirEntry, WalkDir};

const MAX_DEPTH: usize = 5;

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "bin",
    "obj",
    "dist",
    "out",
    ".ubuilder",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    "coverage",
];

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveryReport {
    pub root: PathBuf,
    pub candidates: Vec<ProjectCandidate>,
    pub selected: Option<Project>,
}

#[derive(Debug, Clone)]
struct Hit {
    framework: FrameworkKind,
    root: PathBuf,
    manifest: PathBuf,
    reasons: Vec<String>,
}

/// Scan a repository and return every framework candidate found.
pub fn report(root: &Path) -> Result<DiscoveryReport> {
    let root = canonical_dir(root)?;
    let candidates = scan(&root)?;
    let selected = match choose(&root, &candidates, None, None) {
        Ok(Some(candidate)) => build_project(&candidate, &candidates).ok(),
        _ => None,
    };
    Ok(DiscoveryReport {
        root,
        candidates,
        selected,
    })
}

/// Discover the project to build.
///
/// `select` is a path relative to `root` that pins the project directory.
/// `forced` pins the framework when a directory contains several candidates.
pub fn discover(
    root: &Path,
    select: Option<&Path>,
    forced: Option<FrameworkKind>,
) -> Result<Project> {
    let root = canonical_dir(root)?;
    let scan_root = match select {
        Some(rel) => {
            let joined = root.join(rel);
            canonical_dir(&joined).map_err(|e| {
                e.remediation("Pass a path relative to --path that exists and is a directory")
            })?
        }
        None => root.clone(),
    };
    let candidates = scan(&scan_root)?;
    if candidates.is_empty() {
        return Err(UbuilderError::new(
            ErrorCode::ProjectNotFound,
            format!(
                "no supported project detected under {}",
                scan_root.display()
            ),
        )
        .remediation(
            "Expected Cargo.toml, .sln/.csproj, an Electron package.json, or a Tauri config",
        )
        .details(serde_json::json!({ "root": scan_root })));
    }
    match choose(&scan_root, &candidates, select, forced)? {
        Some(candidate) => build_project(&candidate, &candidates),
        None => Err(UbuilderError::new(
            ErrorCode::ProjectNotFound,
            "no project matched the requested framework",
        )
        .remediation("Run `ubuilder inspect --json` to list detected candidates")),
    }
}

fn canonical_dir(path: &Path) -> Result<PathBuf> {
    if !path.exists() {
        return Err(UbuilderError::new(
            ErrorCode::ProjectNotFound,
            format!("path does not exist: {}", path.display()),
        ));
    }
    if !path.is_dir() {
        return Err(UbuilderError::new(
            ErrorCode::Usage,
            format!("path is not a directory: {}", path.display()),
        ));
    }
    canonical_root(path)
        .map_err(|e| UbuilderError::io(format!("cannot resolve {}", path.display()), e))
}

/// Canonicalize without the `\\?\` prefix on Windows: some SDKs (notably
/// `dotnet publish` on .NET 11) fail when given extended-length paths.
fn canonical_root(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    Ok(strip_windows_prefix(canonical))
}

fn strip_windows_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

fn scan(root: &Path) -> Result<Vec<ProjectCandidate>> {
    let mut hits: Vec<Hit> = Vec::new();
    let mut skip: BTreeSet<PathBuf> = BTreeSet::new();
    let dirs: Vec<PathBuf> = WalkDir::new(root)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| !is_skipped(e))
        .flatten()
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.path().to_path_buf())
        .collect();
    for dir in dirs {
        if skip.contains(&dir) {
            continue;
        }
        let detected = detect_at(&dir);
        for hit in &detected {
            if hit.framework == FrameworkKind::Tauri {
                let cargo_dir = hit
                    .manifest
                    .parent()
                    .and_then(|p| p.parent())
                    .filter(|p| p.join("Cargo.toml").exists());
                if let Some(cargo_dir) = cargo_dir {
                    skip.insert(cargo_dir.to_path_buf());
                }
                // Never treat src-tauri as an independent Rust project.
                if dir.join("src-tauri").exists() {
                    skip.insert(dir.join("src-tauri"));
                }
            }
        }
        hits.extend(detected);
    }
    let mut candidates: Vec<ProjectCandidate> = hits
        .into_iter()
        .map(|h| ProjectCandidate {
            framework: h.framework,
            root: h.root.clone(),
            manifest: h.manifest,
            depth: h
                .root
                .components()
                .count()
                .saturating_sub(root.components().count()),
            reasons: h.reasons,
        })
        .collect();
    candidates.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.root.cmp(&b.root))
            .then_with(|| a.framework.as_str().cmp(b.framework.as_str()))
    });
    Ok(candidates)
}

fn is_skipped(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
    if SKIP_DIRS.contains(&name.as_str()) {
        return true;
    }
    // Symlinked build trees can loop; only follow directories is disabled anyway.
    false
}

fn detect_at(dir: &Path) -> Vec<Hit> {
    let mut hits = Vec::new();
    let root = dir.to_path_buf();

    let tauri_config = ["tauri.conf.json", "tauri.conf.json5"]
        .iter()
        .map(|name| dir.join("src-tauri").join(name))
        .chain(
            ["tauri.conf.json", "tauri.conf.json5"]
                .iter()
                .map(|name| dir.join(name)),
        )
        .find(|p| p.is_file());

    let package_json = dir.join("package.json");
    let cargo_toml = dir.join("Cargo.toml");

    if let Some(config) = tauri_config {
        let reasons = vec![format!("found {}", config.display())];
        hits.push(Hit {
            framework: FrameworkKind::Tauri,
            root: root.clone(),
            manifest: config,
            reasons,
        });
        // Tauri wins at a directory that also contains a frontend package.json.
        return hits;
    }

    let electron = if package_json.is_file() {
        electron_metadata(&package_json).ok().filter(|m| {
            m.electron_version.is_some() || m.has_electron_builder || {
                let text = std::fs::read_to_string(&package_json).unwrap_or_default();
                text.contains("\"electron\"")
            }
        })
    } else {
        None
    };

    if let Some(meta) = electron {
        hits.push(Hit {
            framework: FrameworkKind::Electron,
            root: root.clone(),
            manifest: package_json.clone(),
            reasons: vec![format!(
                "package.json declares Electron {} / electron-builder={}",
                meta.electron_version.as_deref().unwrap_or("dependency"),
                meta.has_electron_builder
            )],
        });
    }

    if cargo_toml.is_file() {
        hits.push(Hit {
            framework: FrameworkKind::Rust,
            root: root.clone(),
            manifest: cargo_toml.clone(),
            reasons: vec!["found Cargo.toml".to_string()],
        });
    }

    let dotnet_manifest = find_dotnet_manifest(dir);
    if let Some(manifest) = dotnet_manifest {
        hits.push(Hit {
            framework: FrameworkKind::Dotnet,
            root: root.clone(),
            manifest: manifest.clone(),
            reasons: vec![format!("found {}", manifest.display())],
        });
    }
    hits
}

/// Directory candidates: at most one framework per directory, except when the
/// directory has both a Cargo.toml and an Electron package.json.
fn choose(
    _root: &Path,
    candidates: &[ProjectCandidate],
    selected: Option<&Path>,
    forced: Option<FrameworkKind>,
) -> Result<Option<ProjectCandidate>> {
    let _ = selected;
    let mut pool: Vec<&ProjectCandidate> = candidates
        .iter()
        .filter(|c| forced.map_or(true, |f| c.framework == f))
        .collect();
    if pool.is_empty() {
        return Ok(None);
    }
    let min_depth = pool.iter().map(|c| c.depth).min().unwrap_or(0);
    pool.retain(|c| c.depth == min_depth);

    if pool.len() == 1 {
        return Ok(Some(pool[0].clone()));
    }

    let roots: BTreeSet<&PathBuf> = pool.iter().map(|c| &c.root).collect();
    if roots.len() > 1 {
        let list: Vec<String> = pool
            .iter()
            .map(|c| {
                format!(
                    "{} [{}] ({})",
                    c.root.display(),
                    c.framework.as_str(),
                    c.reasons.join("; ")
                )
            })
            .collect();
        return Err(UbuilderError::new(
            ErrorCode::AmbiguousProject,
            "multiple project candidates found; refusing to choose arbitrarily",
        )
        .remediation(format!(
            "Select one with --project <relative-path> (and --framework if needed). Candidates:\n  {}",
            list.join("\n  ")
        ))
        .details(serde_json::json!({ "candidates": pool })));
    }

    // Several frameworks in one directory. Tauri is unambiguous; otherwise the
    // user must choose.
    if let Some(tauri) = pool.iter().find(|c| c.framework == FrameworkKind::Tauri) {
        return Ok(Some((**tauri).clone()));
    }
    if let Some(forced) = forced {
        if let Some(found) = pool.iter().find(|c| c.framework == forced) {
            return Ok(Some((**found).clone()));
        }
    }
    Err(UbuilderError::new(
        ErrorCode::AmbiguousProject,
        format!(
            "{} contains multiple frameworks: {}",
            pool[0].root.display(),
            pool.iter()
                .map(|c| c.framework.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
    .remediation("Pass --framework rust|dotnet|electron|tauri to choose")
    .details(serde_json::json!({
        "root": pool[0].root,
        "candidates": pool,
    })))
}

fn build_project(candidate: &ProjectCandidate, all: &[ProjectCandidate]) -> Result<Project> {
    let alternatives: Vec<ProjectCandidate> = all
        .iter()
        .filter(|c| !(c.root == candidate.root && c.framework == candidate.framework))
        .cloned()
        .collect();
    let candidate = candidate.clone();
    let (name, version, metadata) = match candidate.framework {
        FrameworkKind::Rust => {
            let meta = rust_metadata(&candidate.root, &candidate.manifest)?;
            let name = if meta.packages.len() == 1 {
                meta.packages[0].name.clone()
            } else {
                file_stem_name(&candidate.root)
            };
            let version = toml_package_version(&candidate.manifest);
            (name, version, ProjectMetadata::Rust(meta))
        }
        FrameworkKind::Dotnet => {
            let meta = dotnet_metadata(&candidate.root)?;
            let name = meta
                .solution
                .as_ref()
                .map(|p| file_stem_name(p))
                .or_else(|| meta.projects.first().map(|p| p.name.clone()))
                .unwrap_or_else(|| file_stem_name(&candidate.root));
            let version = csproj_version(&meta.projects.first().map(|p| p.path.clone()));
            (name, version, ProjectMetadata::Dotnet(meta))
        }
        FrameworkKind::Electron => {
            let meta = electron_metadata(&candidate.manifest)?;
            let json = read_json(&candidate.manifest)?;
            let name = json
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(&file_stem_name(&candidate.root))
                .to_string();
            let version = json
                .get("version")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            (name, version, ProjectMetadata::Electron(meta))
        }
        FrameworkKind::Tauri => {
            let meta = tauri_metadata(&candidate.root, &candidate.manifest)?;
            let name = meta
                .product_name
                .clone()
                .unwrap_or_else(|| file_stem_name(&candidate.root));
            let version = toml_package_version(&meta.cargo_manifest)
                .or_else(|| tauri_config_version(&candidate.manifest));
            (name, version, ProjectMetadata::Tauri(meta))
        }
    };
    Ok(Project {
        root: candidate.root.clone(),
        name,
        version,
        framework: candidate.framework,
        manifest_path: candidate.manifest.clone(),
        metadata,
        alternatives,
    })
}

fn read_json(path: &Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
    serde_json::from_str(&text).map_err(|e| {
        UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("invalid JSON in {}: {e}", path.display()),
        )
    })
}

fn read_json5(path: &Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
    json5::from_str(&text).map_err(|e| {
        UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("invalid JSON5 in {}: {e}", path.display()),
        )
    })
}

fn rust_metadata(root: &Path, manifest: &Path) -> Result<RustProject> {
    if which::which("cargo").is_ok() {
        if let Some(meta) = cargo_metadata(root, manifest) {
            return Ok(meta);
        }
    }
    cargo_toml_fallback(root, manifest)
}

fn cargo_metadata(root: &Path, manifest: &Path) -> Option<RustProject> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let workspace_root = json
        .get("workspace_root")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    let target_directory = json
        .get("target_directory")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let default_members: Vec<String> = json
        .get("workspace_default_members")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let mut packages = Vec::new();
    if let Some(arr) = json.get("packages").and_then(|v| v.as_array()) {
        for pkg in arr {
            let name = pkg.get("name").and_then(|v| v.as_str())?.to_string();
            let manifest_path = pkg
                .get("manifest_path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| manifest.to_path_buf());
            let mut binaries = Vec::new();
            let mut library = false;
            let mut cdylib = false;
            if let Some(targets) = pkg.get("targets").and_then(|v| v.as_array()) {
                for t in targets {
                    let kinds: Vec<&str> = t
                        .get("kind")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                        .unwrap_or_default();
                    let tname = t
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if kinds.contains(&"bin") {
                        binaries.push(tname.clone());
                    }
                    if kinds
                        .iter()
                        .any(|k| matches!(*k, "lib" | "rlib" | "dylib" | "proc-macro"))
                    {
                        library = true;
                    }
                    if kinds.contains(&"cdylib") {
                        cdylib = true;
                    }
                }
            }
            packages.push(RustPackage {
                name,
                manifest_path,
                binaries,
                library,
                cdylib,
            });
        }
    }
    let is_workspace = json
        .get("workspace_members")
        .and_then(|v| v.as_array())
        .map(|a| a.len() > 1)
        .unwrap_or(false)
        || packages.len() > 1;
    Some(RustProject {
        is_workspace,
        packages,
        target_directory,
        workspace_root,
        default_members,
    })
}

fn cargo_toml_fallback(root: &Path, manifest: &Path) -> Result<RustProject> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| UbuilderError::io(format!("cannot read {}", manifest.display()), e))?;
    let value: toml::Value = text.parse().map_err(|e| {
        UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("invalid Cargo.toml in {}: {e}", manifest.display()),
        )
    })?;
    let package = value.get("package");
    let name = package
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| file_stem_name(root));
    let mut binaries = Vec::new();
    if let Some(bins) = value.get("bin").and_then(|v| v.as_array()) {
        for bin in bins {
            if let Some(name) = bin.get("name").and_then(|v| v.as_str()) {
                binaries.push(name.to_string());
            }
        }
    }
    if binaries.is_empty() && root.join("src").join("main.rs").is_file() {
        binaries.push(name.clone());
    }
    let library = value.get("lib").is_some() || root.join("src").join("lib.rs").is_file();
    let is_workspace = value.get("workspace").is_some();
    let workspace_root = root.to_path_buf();
    Ok(RustProject {
        is_workspace,
        packages: vec![RustPackage {
            name,
            manifest_path: manifest.to_path_buf(),
            binaries,
            library,
            cdylib: false,
        }],
        target_directory: workspace_root.join("target"),
        workspace_root,
        default_members: Vec::new(),
    })
}

fn toml_package_version(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let value: toml::Value = text.parse().ok()?;
    value
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("workspace")
                .and_then(|w| w.get("package"))
                .and_then(|p| p.get("version"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

fn find_dotnet_manifest(dir: &Path) -> Option<PathBuf> {
    let mut solutions = Vec::new();
    let mut projects = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            match path.extension().and_then(|e| e.to_str()) {
                Some("sln") | Some("slnx") => solutions.push(path),
                Some("csproj") => projects.push(path),
                _ => {}
            }
        }
    }
    solutions.sort();
    projects.sort();
    solutions
        .into_iter()
        .next()
        .or_else(|| projects.into_iter().next())
}

fn dotnet_metadata(root: &Path) -> Result<DotnetProject> {
    let mut project_paths = Vec::new();
    let mut solution = None;
    for entry in WalkDir::new(root)
        .max_depth(4)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skipped(e))
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        match entry.path().extension().and_then(|e| e.to_str()) {
            Some("sln") | Some("slnx") => {
                solution.get_or_insert_with(|| entry.path().to_path_buf());
            }
            Some("csproj") => project_paths.push(entry.path().to_path_buf()),
            _ => {}
        }
    }
    if project_paths.is_empty() {
        project_paths
            .push(find_dotnet_manifest(root).unwrap_or_else(|| root.join("project.csproj")));
    }
    project_paths.sort();
    project_paths.dedup();

    let mut projects = Vec::new();
    let mut flavor = DotnetFlavor::Other;
    let mut native_aot = false;
    let mut self_contained_hint = false;
    for path in &project_paths {
        let info = csproj_info(path)?;
        if flavor == DotnetFlavor::Other || flavor == DotnetFlavor::Console {
            flavor = detect_flavor(path, &projects);
        }
        if std::fs::read_to_string(path)
            .unwrap_or_default()
            .contains("<PublishAot>true")
        {
            native_aot = true;
        }
        if std::fs::read_to_string(path)
            .unwrap_or_default()
            .contains("<SelfContained>true")
        {
            self_contained_hint = true;
        }
        projects.push(info);
    }
    if projects.len() > 1 && flavor == DotnetFlavor::Other {
        flavor = detect_flavor(&projects[0].path, &projects);
    }
    Ok(DotnetProject {
        projects,
        solution,
        flavor,
        native_aot,
        self_contained_hint,
    })
}

fn csproj_info(path: &Path) -> Result<DotnetProjectInfo> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
    let name = xml_values(&text, "AssemblyName")
        .into_iter()
        .next()
        .or_else(|| xml_values(&text, "RootNamespace").into_iter().next())
        .unwrap_or_else(|| file_stem_name(path));
    let mut tfms: Vec<String> = Vec::new();
    for tag in ["TargetFrameworks", "TargetFramework"] {
        for value in xml_values(&text, tag) {
            for tfm in value.split(';') {
                let tfm = tfm.trim();
                if !tfm.is_empty() && !tfms.iter().any(|t| t == tfm) {
                    tfms.push(tfm.to_string());
                }
            }
        }
    }
    let mut rids = Vec::new();
    for value in xml_values(&text, "RuntimeIdentifiers") {
        for rid in value.split(';') {
            let rid = rid.trim();
            if !rid.is_empty() {
                rids.push(rid.to_string());
            }
        }
    }
    Ok(DotnetProjectInfo {
        name,
        path: path.to_path_buf(),
        target_frameworks: tfms,
        runtime_identifiers: rids,
    })
}

fn detect_flavor(path: &Path, siblings: &[DotnetProjectInfo]) -> DotnetFlavor {
    let mut text = std::fs::read_to_string(path).unwrap_or_default();
    for other in siblings {
        if other.path != path {
            text.push_str(&std::fs::read_to_string(&other.path).unwrap_or_default());
        }
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("usemaui>true") || lower.contains("microsoft.maui") {
        DotnetFlavor::Maui
    } else if lower.contains("usewpf>true")
        || lower.contains("microsoft.windowsdesktop") && lower.contains("wpf")
    {
        DotnetFlavor::Wpf
    } else if lower.contains("usewindowsforms>true") {
        DotnetFlavor::WinForms
    } else if lower.contains("avalonia") {
        DotnetFlavor::Avalonia
    } else if lower.contains("outputtype>exe") || lower.contains("outputtype>winexe") {
        DotnetFlavor::Console
    } else {
        DotnetFlavor::Other
    }
}

fn xml_values(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut cursor = 0;
    while let Some(start_rel) = text[cursor..].find(&open) {
        let start = cursor + start_rel;
        let Some(open_end_rel) = text[start..].find('>') else {
            break;
        };
        let value_start = start + open_end_rel + 1;
        let Some(end_rel) = text[value_start..].find(&close) else {
            break;
        };
        let value = text[value_start..value_start + end_rel].trim().to_string();
        if !value.is_empty() {
            values.push(value);
        }
        cursor = value_start + end_rel + close.len();
    }
    values
}

fn csproj_version(path: &Option<PathBuf>) -> Option<String> {
    let path = path.as_ref()?;
    let text = std::fs::read_to_string(path).ok()?;
    xml_values(&text, "Version")
        .into_iter()
        .next()
        .or_else(|| xml_values(&text, "VersionPrefix").into_iter().next())
}

fn package_manager_at(root: &Path) -> PackageManager {
    if root.join("pnpm-lock.yaml").is_file() {
        PackageManager::Pnpm
    } else if root.join("yarn.lock").is_file() {
        PackageManager::Yarn
    } else if root.join("bun.lockb").is_file() || root.join("bun.lock").is_file() {
        PackageManager::Bun
    } else {
        PackageManager::Npm
    }
}

fn dependency_map(json: &serde_json::Value, key: &str) -> BTreeMap<String, String> {
    json.get(key)
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn electron_metadata(package_json: &Path) -> Result<ElectronProject> {
    let json = read_json(package_json)?;
    let root = package_json.parent().unwrap_or_else(|| Path::new("."));
    let deps = dependency_map(&json, "dependencies");
    let dev_deps = dependency_map(&json, "devDependencies");
    let scripts: BTreeMap<String, String> = json
        .get("scripts")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let has_electron_builder = deps.contains_key("electron-builder")
        || dev_deps.contains_key("electron-builder")
        || scripts.values().any(|s| s.contains("electron-builder"));
    let electron_version = deps
        .get("electron")
        .or_else(|| dev_deps.get("electron"))
        .map(|v| v.trim_start_matches(['^', '~', '=']).to_string());
    let native_markers = [
        "better-sqlite3",
        "node-pty",
        "sharp",
        "canvas",
        "serialport",
        "ffi-napi",
        "node-gyp",
        "node-addon-api",
    ];
    let has_native_modules = deps
        .keys()
        .chain(dev_deps.keys())
        .any(|k| native_markers.contains(&k.as_str()))
        || dev_deps.contains_key("electron-rebuild")
        || scripts
            .get("postinstall")
            .map(|s| s.contains("electron-rebuild"))
            .unwrap_or(false);
    Ok(ElectronProject {
        package_json: package_json.to_path_buf(),
        package_manager: package_manager_at(root),
        electron_version,
        scripts,
        has_electron_builder,
        has_native_modules,
        main: json
            .get("main")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

fn tauri_metadata(root: &Path, config: &Path) -> Result<TauriProject> {
    let json = read_json5(config)?;
    let config_dir = config.parent().unwrap_or(root);
    let product_name = json
        .get("productName")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let identifier = json
        .get("identifier")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let build = json
        .get("build")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let frontend_value = build
        .get("frontendDist")
        .or_else(|| build.get("distDir"))
        .and_then(|v| v.as_str());
    let frontend_dist = frontend_value.map(|v| {
        let p = PathBuf::from(v);
        if p.is_absolute() {
            p
        } else {
            config_dir.join(p)
        }
    });
    let before_build_command = build
        .get("beforeBuildCommand")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let dev_url = build
        .get("devUrl")
        .or_else(|| build.get("devPath"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let bundle_targets = match json.get("bundle").and_then(|b| b.get("targets")) {
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let cargo_manifest = if config_dir.join("Cargo.toml").is_file() {
        config_dir.join("Cargo.toml")
    } else {
        root.join("Cargo.toml")
    };
    let tauri_version = std::fs::read_to_string(&cargo_manifest)
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|value| {
            let dep = value
                .get("dependencies")
                .and_then(|d| d.get("tauri"))
                .or_else(|| value.get("build-dependencies").and_then(|d| d.get("tauri")))?;
            dep.as_str().map(str::to_string).or_else(|| {
                dep.get("version")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
        });
    let package_json = root.join("package.json");
    let frontend_scripts = std::fs::read_to_string(&package_json)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|j| {
            j.get("scripts").and_then(|v| v.as_object()).map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
        })
        .unwrap_or_default();
    Ok(TauriProject {
        config_path: config.to_path_buf(),
        cargo_manifest,
        product_name,
        identifier,
        frontend_dist,
        before_build_command,
        dev_url,
        tauri_version,
        bundle_targets,
        package_manager: package_manager_at(root),
        frontend_scripts,
    })
}

fn tauri_config_version(config: &Path) -> Option<String> {
    read_json5(config)
        .ok()?
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[test]
    fn detects_rust_and_ignores_nested_target_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"demo\"\npath = \"src/main.rs\"\n",
        );
        write(&tmp.path().join("src/main.rs"), "fn main() {}");
        write(
            &tmp.path().join("target/dep/Cargo.toml"),
            "[package]\nname = \"dep\"\n",
        );
        let candidates = scan(tmp.path()).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].framework, FrameworkKind::Rust);
    }

    #[test]
    fn tauri_collapses_src_tauri() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("package.json"), "{\"name\":\"app\"}");
        write(
            &tmp.path().join("src-tauri/Cargo.toml"),
            "[package]\nname=\"app\"\n",
        );
        write(
            &tmp.path().join("src-tauri/tauri.conf.json"),
            "{\"identifier\":\"x.y\",\"build\":{\"frontendDist\":\"../dist\"}}",
        );
        let candidates = scan(tmp.path()).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].framework, FrameworkKind::Tauri);
        assert_eq!(
            candidates[0].root.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn ambiguous_monorepo_is_not_silently_selected() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("apps/a/Cargo.toml"),
            "[package]\nname=\"a\"\n",
        );
        write(&tmp.path().join("apps/a/src/main.rs"), "fn main(){}");
        write(
            &tmp.path().join("apps/b/Cargo.toml"),
            "[package]\nname=\"b\"\n",
        );
        write(&tmp.path().join("apps/b/src/main.rs"), "fn main(){}");
        let err = discover(tmp.path(), None, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::AmbiguousProject);
        let project = discover(tmp.path(), Some(Path::new("apps/a")), None).unwrap();
        assert_eq!(project.framework, FrameworkKind::Rust);
    }

    #[test]
    fn detects_dotnet_solution_and_flavor() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("app.sln"),
            "Microsoft Visual Studio Solution File",
        );
        write(
            &tmp.path().join("src/App.csproj"),
            r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>
            <OutputType>WinExe</OutputType><TargetFramework>net10.0-windows</TargetFramework>
            <UseWPF>true</UseWPF><PublishAot>true</PublishAot></PropertyGroup></Project>"#,
        );
        let project = discover(tmp.path(), None, None).unwrap();
        assert_eq!(project.framework, FrameworkKind::Dotnet);
        let ProjectMetadata::Dotnet(meta) = project.metadata else {
            panic!("expected dotnet metadata")
        };
        assert_eq!(meta.flavor, DotnetFlavor::Wpf);
        assert!(meta.native_aot);
    }

    #[test]
    fn detects_electron_with_builder() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("package.json"),
            r#"{"name":"el","version":"1.0.0","main":"main.js",
                "dependencies":{"electron":"^33.0.0"},
                "devDependencies":{"electron-builder":"^25.0.0"}}"#,
        );
        let project = discover(tmp.path(), None, None).unwrap();
        assert_eq!(project.framework, FrameworkKind::Electron);
        let ProjectMetadata::Electron(meta) = project.metadata else {
            panic!("expected electron metadata")
        };
        assert!(meta.has_electron_builder);
        assert_eq!(meta.electron_version.as_deref(), Some("33.0.0"));
    }

    #[test]
    fn xml_tag_parser_handles_attributes() {
        let text = r#"<TargetFrameworks Condition="'x'">net8.0;net9.0</TargetFrameworks>"#;
        assert_eq!(xml_values(text, "TargetFrameworks"), vec!["net8.0;net9.0"]);
    }
}
