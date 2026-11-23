//! Packaging: turn build outputs into distributable artifacts.
//!
//! Formats that need a platform tool (makensis, hdiutil, appimagetool,
//! rpmbuild) are produced only when the tool is present; otherwise packaging
//! fails with a remediation instead of fabricating an artifact.

pub mod archive;
pub mod mac;
pub mod macho;
pub mod tools;

use crate::artifacts::{copy_tree, sha256_file, zip_directory};
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::project::FrameworkKind;
use crate::target::{PackagingFormat, Target};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Merge architecture slices into a universal Mach-O binary.
pub fn merge_macho(inputs: &[PathBuf], output: &Path) -> Result<String> {
    let refs: Vec<&Path> = inputs.iter().map(|p| p.as_path()).collect();
    macho::merge(&refs, output)
}

#[derive(Debug, Clone)]
pub struct PackageContext<'a> {
    pub project_name: &'a str,
    pub version: &'a str,
    pub target: &'a Target,
    pub framework: FrameworkKind,
    pub output_dir: &'a Path,
    pub work_dir: &'a Path,
    pub identifier: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Payload {
    pub root: PathBuf,
    pub mac_app: Option<PathBuf>,
    pub executable: Option<PathBuf>,
    pub directory_payload: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageResult {
    pub path: PathBuf,
    pub format: PackagingFormat,
    pub sha256: String,
    pub size: u64,
    pub warnings: Vec<String>,
}

impl PackageContext<'_> {
    pub fn base_name(&self) -> String {
        format!(
            "{}-{}-{}",
            sanitize(self.project_name),
            sanitize_version(self.version),
            self.target.id
        )
    }

    pub fn output_path(&self, base: &str, extension: &str) -> PathBuf {
        if extension.is_empty() {
            self.output_dir.join(base)
        } else {
            self.output_dir.join(format!("{base}.{extension}"))
        }
    }

    pub fn deb_slug(&self) -> String {
        sanitize(self.project_name)
            .to_ascii_lowercase()
            .replace('_', "-")
    }
}

fn sanitize(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['-', '.']).to_string();
    if trimmed.is_empty() {
        "app".into()
    } else {
        trimmed
    }
}

fn sanitize_version(value: &str) -> String {
    let cleaned = value.trim_start_matches(['v', 'V']);
    let result: String = cleaned
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+') {
                c
            } else {
                '.'
            }
        })
        .collect();
    if result.is_empty() {
        "0.0.0".into()
    } else {
        result
    }
}

/// Copy build outputs into one payload directory and locate the primary binary.
pub fn prepare_payload(ctx: &PackageContext<'_>, artifacts: &[PathBuf]) -> Result<Payload> {
    if artifacts.is_empty() {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            "no build artifacts to package",
        ));
    }
    let payload_root = ctx.work_dir.join("payload");
    let _ = std::fs::remove_dir_all(&payload_root);
    std::fs::create_dir_all(&payload_root)
        .map_err(|e| UbuilderError::io("cannot create payload directory", e))?;

    let mut directory_payload = false;
    let mut mac_app = None;
    let mut executable = None;

    for artifact in artifacts {
        if artifact.is_dir() {
            if let Some(app) = find_mac_app(artifact) {
                mac_app = Some(app);
            }
            if artifact.extension().and_then(|e| e.to_str()) == Some("app") {
                mac_app = Some(artifact.clone());
            }
            if artifact
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with("-unpacked"))
                .unwrap_or(false)
            {
                directory_payload = true;
            }
            copy_tree(artifact, &payload_root)?;
            if artifact
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with("-unpacked"))
                .unwrap_or(false)
            {
                directory_payload = true;
            }
        } else if artifact.is_file() {
            if artifact.extension().and_then(|e| e.to_str()) == Some("app") {
                mac_app = Some(artifact.clone());
            }
            if let Some(name) = artifact.file_name() {
                let dest = payload_root.join(name);
                std::fs::copy(artifact, &dest).map_err(|e| {
                    UbuilderError::io(format!("cannot copy {}", artifact.display()), e)
                })?;
            }
        }
    }

    if mac_app.is_none() {
        mac_app = find_mac_app(&payload_root);
    }

    if let Some(found) = archive::payload_executable(&payload_root, ctx.project_name) {
        executable = Some(found);
    }
    if executable.is_none() && mac_app.is_some() {
        executable = Some(PathBuf::from(format!(
            "{}{}",
            ctx.project_name,
            ctx.target.executable_suffix()
        )));
    }

    if ctx.framework == FrameworkKind::Electron || is_app_directory(&payload_root) {
        directory_payload = true;
    }

    Ok(Payload {
        root: payload_root,
        mac_app,
        executable,
        directory_payload,
    })
}

/// Attach license/readme files from the project directory to the payload.
pub fn attach_docs(payload: &Payload, project_root: &Path) -> Result<()> {
    for name in [
        "LICENSE",
        "LICENSE.md",
        "LICENSE.txt",
        "README.md",
        "README.txt",
    ] {
        let source = project_root.join(name);
        if source.is_file() {
            let dest = payload.root.join(name);
            if !dest.exists() {
                std::fs::copy(&source, &dest).map_err(|e| {
                    UbuilderError::io(format!("cannot copy {}", source.display()), e)
                })?;
            }
        }
    }
    Ok(())
}

fn find_mac_app(root: &Path) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    walkdir::WalkDir::new(root)
        .max_depth(5)
        .follow_links(false)
        .into_iter()
        .flatten()
        .find(|entry| {
            entry.file_type().is_dir()
                && entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("app"))
                    .unwrap_or(false)
        })
        .map(|entry| entry.into_path())
}

fn is_app_directory(root: &Path) -> bool {
    let resource_dir = root.join("resources");
    let electron_exe = root
        .read_dir()
        .ok()
        .map(|entries| {
            entries.flatten().any(|e| {
                e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("exe"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    resource_dir.is_dir() && electron_exe
}

/// Produce one package format from a prepared payload.
pub fn package(
    payload: &Payload,
    ctx: &PackageContext<'_>,
    format: PackagingFormat,
) -> Result<PackageResult> {
    std::fs::create_dir_all(ctx.output_dir)
        .map_err(|e| UbuilderError::io("cannot create output directory", e))?;
    let base = ctx.base_name();
    let warnings = Vec::new();

    match format {
        PackagingFormat::Directory => {
            let dest = ctx.output_path(&base, "");
            let _ = std::fs::remove_dir_all(&dest);
            copy_tree(&payload.root, &dest)?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::PortableZip => {
            let dest = ctx.output_path(&base, "zip");
            if let Some(app) = &payload.mac_app {
                let name = app
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("App.app");
                archive::zip_with_prefix(app, &dest, name)?;
            } else {
                zip_directory(&payload.root, &dest)?;
            }
            finish(&dest, format, warnings)
        }
        PackagingFormat::MacZip => {
            let app = ensure_mac_app(payload, ctx)?;
            let dest = ctx.output_path(&format!("{base}-mac"), "zip");
            let name = app
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("App.app");
            archive::zip_with_prefix(&app, &dest, name)?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::AppBundle => {
            let app = ensure_mac_app(payload, ctx)?;
            let name = format!("{}.app", sanitize(ctx.project_name));
            let dest = ctx.output_dir.join(name);
            let _ = std::fs::remove_dir_all(&dest);
            copy_tree(&app, &dest)?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::TarGz => {
            let dest = ctx.output_path(&base, "tar.gz");
            archive::tar_gz_dir(&payload.root, &dest, "")?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::Deb => {
            require_os(ctx, crate::target::TargetOs::Linux, "Debian")?;
            let executable = payload_executable_name(payload, ctx);
            let initial = payload.root.join(&executable);
            if !payload.directory_payload && !initial.is_file() {
                return Err(UbuilderError::new(
                    ErrorCode::ArtifactInvalid,
                    format!(
                        "payload executable {} is missing; cannot build .deb",
                        initial.display()
                    ),
                ));
            }
            let architecture = archive::deb_arch(ctx.target)?;
            let dest = ctx.output_path(&base, "deb");
            let info = archive::DebInfo {
                package: &ctx.deb_slug(),
                version: &sanitize_version(ctx.version),
                architecture,
                description: &format!("{} built by ubuilder", ctx.project_name),
                payload: &payload.root,
                executable: &executable,
                directory_payload: payload.directory_payload,
            };
            let work = ctx.work_dir.join("deb");
            archive::build_deb(&info, &dest, &work)?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::Nsis => {
            require_os(ctx, crate::target::TargetOs::Windows, "NSIS")?;
            let dest = ctx.output_path(&format!("{base}-setup"), "exe");
            tools::build_nsis(&payload.root, &dest, ctx.project_name, ctx.work_dir)?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::Dmg => {
            let app = ensure_mac_app(payload, ctx)?;
            let dest = ctx.output_path(&base, "dmg");
            mac::build_dmg(&app, &dest, ctx.project_name)?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::AppImage => {
            require_os(ctx, crate::target::TargetOs::Linux, "AppImage")?;
            let dest = ctx.output_path(&base, "AppImage");
            let executable = payload_executable_name(payload, ctx);
            tools::build_appimage(
                &payload.root,
                &dest,
                ctx.project_name,
                &executable,
                ctx.work_dir,
            )?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::Rpm => {
            require_os(ctx, crate::target::TargetOs::Linux, "RPM")?;
            let dest = ctx.output_path(&base, "rpm");
            let architecture = archive::deb_arch(ctx.target)?.replace("amd64", "x86_64");
            tools::build_rpm(
                &payload.root,
                &dest,
                &ctx.deb_slug(),
                ctx.project_name,
                &sanitize_version(ctx.version),
                &architecture,
                ctx.work_dir,
            )?;
            finish(&dest, format, warnings)
        }
        PackagingFormat::Msi | PackagingFormat::Msix => {
            Err(UbuilderError::new(
                ErrorCode::UnsupportedTarget,
                format!("{} packaging is not implemented by ubuilder", format.as_str()),
            )
            .remediation(
                "Use portable-zip or nsis; electron-builder projects can request msi/msix through the tool",
            ))
        }
    }
}

pub fn package_all(
    payload: &Payload,
    ctx: &PackageContext<'_>,
    formats: &[PackagingFormat],
) -> Result<Vec<PackageResult>> {
    let mut results = Vec::new();
    for format in formats {
        results.push(package(payload, ctx, *format)?);
    }
    Ok(results)
}

fn payload_executable_name(payload: &Payload, ctx: &PackageContext<'_>) -> String {
    payload
        .executable
        .as_ref()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "{}{}",
                sanitize(ctx.project_name),
                ctx.target.executable_suffix()
            )
        })
}

fn ensure_mac_app(payload: &Payload, ctx: &PackageContext<'_>) -> Result<PathBuf> {
    require_os(ctx, crate::target::TargetOs::Macos, "macOS bundle")?;
    if let Some(app) = &payload.mac_app {
        return Ok(app.clone());
    }
    let executable = payload.executable.clone().ok_or_else(|| {
        UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            "no executable in payload; cannot assemble a .app bundle",
        )
    })?;
    mac::assemble_app(&payload.root, &executable, ctx)
}

fn require_os(
    ctx: &PackageContext<'_>,
    expected: crate::target::TargetOs,
    what: &str,
) -> Result<()> {
    if ctx.target.os != expected {
        return Err(UbuilderError::new(
            ErrorCode::UnsupportedTarget,
            format!("{what} packaging is not valid for target {}", ctx.target.id),
        ));
    }
    Ok(())
}

fn finish(path: &Path, format: PackagingFormat, warnings: Vec<String>) -> Result<PackageResult> {
    if !path.exists() {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            format!(
                "packaging reported success but {} is missing",
                path.display()
            ),
        ));
    }
    let sha256 = if path.is_dir() {
        crate::artifacts::hash_path(path)?
    } else {
        sha256_file(path)?
    };
    let size = if path.is_dir() {
        walkdir::WalkDir::new(path)
            .into_iter()
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum()
    } else {
        std::fs::metadata(path)
            .map_err(|e| UbuilderError::io("cannot stat package", e))?
            .len()
    };
    Ok(PackageResult {
        path: path.to_path_buf(),
        format,
        sha256,
        size,
        warnings,
    })
}

pub fn default_formats(target: &Target) -> Vec<PackagingFormat> {
    match target.os {
        crate::target::TargetOs::Windows => vec![PackagingFormat::PortableZip],
        crate::target::TargetOs::Linux => vec![PackagingFormat::TarGz],
        crate::target::TargetOs::Macos => vec![PackagingFormat::MacZip],
    }
}
