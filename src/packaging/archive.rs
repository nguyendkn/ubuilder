//! Archive writers that do not depend on external tools.

use crate::artifacts::{atomic_replace, atomic_write, copy_tree};
use crate::error::{ErrorCode, Result, UbuilderError};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use tar::{Builder, Header};
use walkdir::WalkDir;

/// Deterministic `.tar.gz`; `prefix` is prepended to entry names.
pub fn tar_gz_dir(src: &Path, dest: &Path, prefix: &str) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
    }
    let tmp = dest.with_extension("tar.gz.tmp");
    let file = File::create(&tmp)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", tmp.display()), e))?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);
    for entry in WalkDir::new(src)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .flatten()
    {
        let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
        if rel.as_os_str().is_empty() {
            continue;
        }
        let name = format!("{prefix}{}", rel.to_string_lossy().replace('\\', "/"));
        append_tar_entry(&mut builder, entry.path(), &name)?;
    }
    let encoder = builder
        .into_inner()
        .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("tar finalize: {e}")))?;
    encoder
        .finish()
        .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("gzip finalize: {e}")))?;
    atomic_replace(&tmp, dest)?;
    Ok(std::fs::metadata(dest)
        .map_err(|e| UbuilderError::io(format!("cannot stat {}", dest.display()), e))?
        .len())
}

fn append_tar_entry<W: Write>(builder: &mut Builder<W>, path: &Path, name: &str) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| UbuilderError::io(format!("cannot stat {}", path.display()), e))?;
    let mut header = Header::new_gnu();
    if metadata.is_dir() {
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_mode(0o755);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("{name}/"), std::io::empty())
            .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("tar dir: {e}")))?;
    } else if metadata.is_file() {
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(metadata.len());
        header.set_mtime(0);
        header.set_mode(if is_executable(path) { 0o755 } else { 0o644 });
        header.set_cksum();
        let file = File::open(path)
            .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
        builder
            .append_data(&mut header, name, file)
            .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("tar file: {e}")))?;
    }
    Ok(())
}

pub fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
        {
            return true;
        }
    }
    let mut magic = [0u8; 4];
    if let Ok(mut file) = File::open(path) {
        use std::io::Read;
        if file.read_exact(&mut magic).is_ok() {
            return matches!(
                magic,
                [b'M', b'Z', ..]
                    | [0x7f, b'E', b'L', b'F']
                    | [0xce, 0xfa, 0xed, 0xfe]
                    | [0xcf, 0xfa, 0xed, 0xfe]
                    | [0xca, 0xfe, 0xba, 0xbe]
                    | [0xca, 0xfe, 0xba, 0xbf]
                    | [b'#', b'!', ..]
            );
        }
    }
    false
}

/// ZIP a directory with every entry prefixed (used for `.app` bundles).
pub fn zip_with_prefix(src: &Path, dest: &Path, prefix: &str) -> Result<u64> {
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, DateTime, ZipWriter};
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| UbuilderError::io(format!("cannot create {}", parent.display()), e))?;
    }
    let tmp = dest.with_extension("zip.tmp");
    let file = File::create(&tmp)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", tmp.display()), e))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default());
    for entry in WalkDir::new(src)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .flatten()
    {
        let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
        if rel.as_os_str().is_empty() {
            continue;
        }
        let name = format!("{prefix}/{}", rel.to_string_lossy().replace('\\', "/"));
        if entry.file_type().is_dir() {
            zip.add_directory(format!("{name}/"), options)
                .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("zip dir: {e}")))?;
        } else if entry.file_type().is_file() {
            zip.start_file(&name, options)
                .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("zip file: {e}")))?;
            let mut input = File::open(entry.path()).map_err(|e| {
                UbuilderError::io(format!("cannot read {}", entry.path().display()), e)
            })?;
            std::io::copy(&mut input, &mut zip)
                .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("zip write: {e}")))?;
        }
    }
    let mut file = zip
        .finish()
        .map_err(|e| UbuilderError::new(ErrorCode::Io, format!("zip finish: {e}")))?;
    file.flush()
        .map_err(|e| UbuilderError::io(format!("cannot flush {}", tmp.display()), e))?;
    drop(file);
    atomic_replace(&tmp, dest)?;
    Ok(std::fs::metadata(dest)
        .map_err(|e| UbuilderError::io(format!("cannot stat {}", dest.display()), e))?
        .len())
}

pub struct DebInfo<'a> {
    pub package: &'a str,
    pub version: &'a str,
    pub architecture: &'a str,
    pub description: &'a str,
    pub payload: &'a Path,
    pub executable: &'a str,
    pub directory_payload: bool,
}

/// Build a Debian binary package (`ar` + control/data tar.gz) without tooling.
pub fn build_deb(info: &DebInfo<'_>, dest: &Path, work: &Path) -> Result<u64> {
    std::fs::create_dir_all(work)
        .map_err(|e| UbuilderError::io(format!("cannot create {}", work.display()), e))?;
    let control_dir = work.join("deb-control");
    let data_dir = work.join("deb-data");
    let _ = std::fs::remove_dir_all(&control_dir);
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::create_dir_all(&control_dir)
        .map_err(|e| UbuilderError::io("cannot create deb control dir", e))?;
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| UbuilderError::io("cannot create deb data dir", e))?;

    let control = format!(
        "Package: {}\nVersion: {}\nSection: utils\nPriority: optional\nArchitecture: {}\nMaintainer: ubuilder <ubuilder@localhost>\nDescription: {}\n",
        info.package, info.version, info.architecture, info.description
    );
    atomic_write(&control_dir.join("control"), control.as_bytes())?;

    if info.directory_payload {
        copy_tree(info.payload, &data_dir.join("opt").join(info.package))?;
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName={}\nExec={}\nTerminal=false\nCategories=Utility;\n",
            info.package, info.executable
        );
        let application_dir = data_dir.join("usr/share/applications");
        std::fs::create_dir_all(&application_dir)
            .map_err(|e| UbuilderError::io("cannot create applications dir", e))?;
        atomic_write(
            &application_dir.join(format!("{}.desktop", info.package)),
            desktop.as_bytes(),
        )?;
        let bin_dir = data_dir.join("usr/bin");
        std::fs::create_dir_all(&bin_dir)
            .map_err(|e| UbuilderError::io("cannot create bin dir", e))?;
        let launcher = format!(
            "#!/bin/sh\nexec /opt/{}/{} \"$@\"\n",
            info.package, info.executable
        );
        atomic_write(&bin_dir.join(info.package), launcher.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                bin_dir.join(info.package),
                std::fs::Permissions::from_mode(0o755),
            );
        }
    } else {
        let bin_dir = data_dir.join("usr/bin");
        std::fs::create_dir_all(&bin_dir)
            .map_err(|e| UbuilderError::io("cannot create bin dir", e))?;
        std::fs::copy(
            info.payload.join(info.executable),
            bin_dir.join(info.executable),
        )
        .map_err(|e| UbuilderError::io("cannot copy deb payload", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                bin_dir.join(info.executable),
                std::fs::Permissions::from_mode(0o755),
            );
        }
    }

    let control_tar = work.join("control.tar.gz");
    tar_gz_dir(&control_dir, &control_tar, "./")?;
    let data_tar = work.join("data.tar.gz");
    tar_gz_dir(&data_dir, &data_tar, "./")?;

    let mut members: Vec<(&str, Vec<u8>)> = vec![("debian-binary", b"2.0\n".to_vec())];
    members.push((
        "control.tar.gz",
        std::fs::read(&control_tar)
            .map_err(|e| UbuilderError::io("cannot read control archive", e))?,
    ));
    members.push((
        "data.tar.gz",
        std::fs::read(&data_tar).map_err(|e| UbuilderError::io("cannot read data archive", e))?,
    ));
    write_ar(dest, &members)?;
    Ok(std::fs::metadata(dest)
        .map_err(|e| UbuilderError::io("cannot stat deb", e))?
        .len())
}

pub fn write_ar(dest: &Path, members: &[(&str, Vec<u8>)]) -> Result<()> {
    let mut out = Vec::new();
    out.extend_from_slice(b"!<arch>\n");
    for (name, data) in members {
        let name_field = format!("{name:<16}");
        let header = format!(
            "{}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            name_field,
            "0",
            "0",
            "0",
            "100644",
            data.len()
        );
        debug_assert_eq!(header.len(), 60);
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    atomic_write(dest, &out)
}

pub fn deb_arch(target: &crate::target::Target) -> Result<&'static str> {
    use crate::host::Architecture;
    if target.universal {
        return Err(UbuilderError::new(
            ErrorCode::UnsupportedTarget,
            "Debian packages cannot be universal; use macos-x64 and macos-arm64",
        ));
    }
    Ok(match target.arch() {
        Some(Architecture::X64) => "amd64",
        Some(Architecture::Arm64) => "arm64",
        None => {
            return Err(UbuilderError::new(
                ErrorCode::UnsupportedTarget,
                "unknown architecture for Debian package",
            ))
        }
    })
}

pub fn payload_executable(payload: &Path, project: &str) -> Option<PathBuf> {
    for suffix in ["", ".exe"] {
        let candidate = payload.join(format!("{project}{suffix}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    WalkDir::new(payload)
        .max_depth(1)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .find(|e| is_executable(e.path()))
        .map(|e| e.into_path())
}
