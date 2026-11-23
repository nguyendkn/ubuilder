//! Packaging formats that delegate to installed platform tools.

use crate::artifacts::{atomic_write, copy_tree};
use crate::error::{ErrorCode, Result, UbuilderError};
use std::path::{Path, PathBuf};
use std::process::Command;

fn require_tool(name: &str, remediation: &str) -> Result<PathBuf> {
    which::which(name).map_err(|_| {
        UbuilderError::new(
            ErrorCode::ToolchainMissing,
            format!("{name} is not installed or not on PATH"),
        )
        .remediation(remediation)
    })
}

fn run_tool(
    program: &Path,
    args: &[String],
    cwd: Option<&Path>,
    env: &[(&str, &str)],
) -> Result<()> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .output()
        .map_err(|e| UbuilderError::io(format!("cannot run {}", program.display()), e))?;
    if !output.status.success() {
        return Err(UbuilderError::new(
            ErrorCode::ExecutionFailed,
            format!(
                "{} failed with {}: {}",
                program.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        )
        .details(serde_json::json!({
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        })));
    }
    Ok(())
}

pub fn build_nsis(payload: &Path, dest: &Path, name: &str, work: &Path) -> Result<u64> {
    let makensis = require_tool(
        "makensis",
        "Install NSIS (makensis), or use portable-zip packaging",
    )?;
    std::fs::create_dir_all(work)
        .map_err(|e| UbuilderError::io("cannot create NSIS work directory", e))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io("cannot create NSIS output directory", e))?;
    }
    let script_path = work.join("installer.nsi");
    let safe_name = name.replace('"', "'");
    let script = format!(
        "Unicode True\n\
Name \"{name}\"\n\
OutFile \"{out}\"\n\
InstallDir \"$PROGRAMFILES64\\{name}\"\n\
RequestExecutionLevel admin\n\
Page directory\n\
Page instfiles\n\
Section \"Install\"\n\
  SetOutPath \"$INSTDIR\"\n\
  File /r \"{payload}\\*.*\"\n\
  WriteUninstaller \"$INSTDIR\\uninstall.exe\"\n\
SectionEnd\n\
Section \"Uninstall\"\n\
  RMDir /r \"$INSTDIR\"\n\
SectionEnd\n",
        name = safe_name,
        out = dest.display(),
        payload = payload.display(),
    );
    atomic_write(&script_path, script.as_bytes())?;
    run_tool(
        &makensis,
        &["/V2".to_string(), script_path.display().to_string()],
        None,
        &[],
    )?;
    std::fs::metadata(dest)
        .map(|m| m.len())
        .map_err(|e| UbuilderError::io("makensis did not produce an installer", e))
}

pub fn build_appimage(
    payload: &Path,
    dest: &Path,
    name: &str,
    executable: &str,
    work: &Path,
) -> Result<u64> {
    let tool = require_tool(
        "appimagetool",
        "Install appimagetool, or use tar.gz packaging",
    )?;
    let appdir = work.join("AppDir");
    let _ = std::fs::remove_dir_all(&appdir);
    copy_tree(payload, &appdir)?;
    let run_script = format!(
        "#!/bin/sh\nHERE=$(dirname \"$(readlink -f \"$0\")\")\nexec \"$HERE/{executable}\" \"$@\"\n"
    );
    atomic_write(&appdir.join("AppRun"), run_script.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            appdir.join("AppRun"),
            std::fs::Permissions::from_mode(0o755),
        );
    }
    let desktop = format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec={executable}\nIcon={name}\nTerminal=false\nCategories=Utility;\n"
    );
    atomic_write(
        &appdir.join(format!("{}.desktop", name.replace(' ', "-"))),
        desktop.as_bytes(),
    )?;
    atomic_write(&appdir.join(format!("{name}.png")), &ONE_PIXEL_PNG)?;
    let _ = std::fs::copy(appdir.join(format!("{name}.png")), appdir.join(".DirIcon"));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io("cannot create AppImage output directory", e))?;
    }
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    run_tool(
        &tool,
        &[
            "--no-appstream".to_string(),
            appdir.display().to_string(),
            dest.display().to_string(),
        ],
        None,
        &[("ARCH", arch)],
    )?;
    std::fs::metadata(dest)
        .map(|m| m.len())
        .map_err(|e| UbuilderError::io("appimagetool did not produce an AppImage", e))
}

pub fn build_rpm(
    payload: &Path,
    dest: &Path,
    package: &str,
    display_name: &str,
    version: &str,
    architecture: &str,
    work: &Path,
) -> Result<u64> {
    let rpmbuild = require_tool("rpmbuild", "Install rpm-build, or use tar.gz packaging")?;
    let topdir = work.join("rpm");
    let _ = std::fs::remove_dir_all(&topdir);
    std::fs::create_dir_all(topdir.join("SPECS"))
        .map_err(|e| UbuilderError::io("cannot create rpmbuild SPECS", e))?;
    let rpm_version: String = version
        .chars()
        .map(|c| if c == '-' { '_' } else { c })
        .collect();
    let executable = payload
        .read_dir()
        .ok()
        .and_then(|entries| {
            entries.flatten().find_map(|entry| {
                let path = entry.path();
                if path.is_file() && crate::packaging::archive::is_executable(&path) {
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .map(str::to_string)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| package.to_string());
    let spec = format!(
        "Name: {package}\n\
Version: {version}\n\
Release: 1\n\
Summary: {display_name} built by ubuilder\n\
License: Proprietary\n\
BuildArch: {architecture}\n\
%description\n\
{display_name} packaged by ubuilder.\n\
%install\n\
mkdir -p %{{buildroot}}/opt/{package}\n\
cp -a {payload}/. %{{buildroot}}/opt/{package}/\n\
mkdir -p %{{buildroot}}/usr/bin\n\
ln -sf /opt/{package}/{executable} %{{buildroot}}/usr/bin/{package}\n\
%files\n\
/opt/{package}\n\
/usr/bin/{package}\n",
        package = package,
        version = rpm_version,
        display_name = display_name,
        architecture = architecture,
        payload = payload.display(),
        executable = executable,
    );
    let spec_path = topdir.join("SPECS").join(format!("{package}.spec"));
    atomic_write(&spec_path, spec.as_bytes())?;
    run_tool(
        &rpmbuild,
        &[
            "-bb".to_string(),
            "--define".to_string(),
            format!("_topdir {}", topdir.display()),
            spec_path.display().to_string(),
        ],
        None,
        &[],
    )?;
    let rpm = walkdir::WalkDir::new(topdir.join("RPMS"))
        .into_iter()
        .flatten()
        .find(|entry| {
            entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("rpm"))
                .unwrap_or(false)
        })
        .map(|entry| entry.into_path())
        .ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::ExecutionFailed,
                "rpmbuild succeeded but produced no .rpm file",
            )
        })?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io("cannot create rpm output directory", e))?;
    }
    std::fs::copy(&rpm, dest).map_err(|e| UbuilderError::io("cannot copy rpm artifact", e))?;
    std::fs::metadata(dest)
        .map(|m| m.len())
        .map_err(|e| UbuilderError::io("cannot stat rpm artifact", e))
}

const ONE_PIXEL_PNG: [u8; 70] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc, 0xcf, 0xc0, 0x50,
    0x0f, 0x00, 0x04, 0x85, 0x01, 0x80, 0x84, 0xa9, 0x8c, 0x21, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
