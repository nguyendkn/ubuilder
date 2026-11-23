//! macOS application bundle assembly and DMG creation.

use crate::artifacts::atomic_write;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::packaging::PackageContext;
use std::path::{Path, PathBuf};

/// Assemble `Name.app/Contents/{MacOS,Resources}` around a Mach-O executable.
pub fn assemble_app(
    payload_root: &Path,
    executable: &Path,
    ctx: &PackageContext<'_>,
) -> Result<PathBuf> {
    let bundle_name = format!("{}.app", ctx.project_name.replace(['/', '\\'], "-"));
    let bundle = ctx.work_dir.join("bundle").join(&bundle_name);
    let _ = std::fs::remove_dir_all(&bundle);
    let macos_dir = bundle.join("Contents/MacOS");
    let resources = bundle.join("Contents/Resources");
    std::fs::create_dir_all(&macos_dir)
        .map_err(|e| UbuilderError::io("cannot create Contents/MacOS", e))?;
    std::fs::create_dir_all(&resources)
        .map_err(|e| UbuilderError::io("cannot create Contents/Resources", e))?;

    let source = if executable.is_absolute() {
        executable.to_path_buf()
    } else {
        payload_root.join(executable)
    };
    if !source.is_file() {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            format!("executable for app bundle is missing: {}", source.display()),
        ));
    }
    let exe_name = ctx.project_name.replace(['/', '\\', ' '], "_");
    let dest_exe = macos_dir.join(&exe_name);
    std::fs::copy(&source, &dest_exe)
        .map_err(|e| UbuilderError::io("cannot copy bundle executable", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest_exe, std::fs::Permissions::from_mode(0o755));
    }

    // Copy sibling resources (dotnet dlls, tauri resources) except binaries
    // already placed in Contents/MacOS.
    for entry in std::fs::read_dir(payload_root)
        .map_err(|e| UbuilderError::io("cannot list payload", e))?
        .flatten()
    {
        let path = entry.path();
        if path == source || path.is_dir() {
            continue;
        }
        let target = resources.join(entry.file_name());
        let _ = std::fs::copy(&path, &target);
    }

    let identifier = ctx
        .identifier
        .map(str::to_string)
        .unwrap_or_else(|| format!("io.ubuilder.{}", ctx.deb_slug()));
    let plist = info_plist(
        ctx.project_name,
        &exe_name,
        &identifier,
        ctx.version,
        ctx.target.min_os.as_deref().unwrap_or("11.0"),
    );
    atomic_write(&bundle.join("Contents/Info.plist"), plist.as_bytes())?;
    Ok(bundle)
}

fn info_plist(
    name: &str,
    executable: &str,
    identifier: &str,
    version: &str,
    min_os: &str,
) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
  <key>CFBundleName</key><string>{name}</string>\n\
  <key>CFBundleDisplayName</key><string>{name}</string>\n\
  <key>CFBundleIdentifier</key><string>{identifier}</string>\n\
  <key>CFBundleExecutable</key><string>{executable}</string>\n\
  <key>CFBundlePackageType</key><string>APPL</string>\n\
  <key>CFBundleShortVersionString</key><string>{version}</string>\n\
  <key>CFBundleVersion</key><string>{version}</string>\n\
  <key>LSMinimumSystemVersion</key><string>{min_os}</string>\n\
  <key>NSHighResolutionCapable</key><true/>\n\
</dict>\n</plist>\n",
        name = xml_escape(name),
        executable = xml_escape(executable),
        identifier = xml_escape(identifier),
        version = xml_escape(version),
        min_os = xml_escape(min_os),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Create a compressed DMG; only possible on macOS with `hdiutil`.
pub fn build_dmg(app: &Path, dest: &Path, volume: &str) -> Result<u64> {
    let hdiutil = which::which("hdiutil").map_err(|_| {
        UbuilderError::new(ErrorCode::ToolchainMissing, "hdiutil is not available")
            .remediation("Run dmg packaging on macOS; hdiutil ships with the OS")
    })?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io("cannot create dmg output directory", e))?;
    }
    let output = std::process::Command::new(hdiutil)
        .arg("create")
        .arg("-volname")
        .arg(volume)
        .arg("-srcfolder")
        .arg(app)
        .arg("-ov")
        .arg("-format")
        .arg("UDZO")
        .arg(dest)
        .output()
        .map_err(|e| UbuilderError::io("cannot run hdiutil", e))?;
    if !output.status.success() {
        return Err(UbuilderError::new(
            ErrorCode::ExecutionFailed,
            format!(
                "hdiutil failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(std::fs::metadata(dest)
        .map_err(|e| UbuilderError::io("cannot stat dmg", e))?
        .len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Architecture, HostPlatform};
    use crate::target::Target;

    #[test]
    fn plist_escapes_values() {
        let plist = info_plist("A&B", "A_B", "io.test", "1.0.0", "11.0");
        assert!(plist.contains("A&amp;B"));
        assert!(plist.contains("CFBundleExecutable"));
    }

    #[test]
    fn assembles_bundle_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let payload = tmp.path().join("payload");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("demo"), b"\xcf\xfa\xed\xfe....").unwrap();
        let target = Target::parse("macos-arm64").unwrap();
        let ctx = PackageContext {
            project_name: "demo",
            version: "0.1.0",
            target: &target,
            framework: crate::project::FrameworkKind::Rust,
            output_dir: tmp.path(),
            work_dir: tmp.path(),
            identifier: Some("io.example.demo"),
        };
        let app = assemble_app(&payload, Path::new("demo"), &ctx).unwrap();
        assert!(app.join("Contents/MacOS/demo").is_file());
        assert!(app.join("Contents/Info.plist").is_file());
        let _ = HostPlatform::Macos;
        let _ = Architecture::Arm64;
    }
}
