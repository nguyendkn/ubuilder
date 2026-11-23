//! Artifact validation: format/architecture checks that never trust a build's
//! exit code alone.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Architecture;
use crate::target::{Target, TargetOs};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    NotRun,
    Passed,
    Partial,
    Failed,
    Unsupported,
}

impl ValidationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationStatus::NotRun => "not_run",
            ValidationStatus::Passed => "passed",
            ValidationStatus::Partial => "partial",
            ValidationStatus::Failed => "failed",
            ValidationStatus::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: ValidationStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    pub path: PathBuf,
    pub kind: String,
    pub status: ValidationStatus,
    pub checks: Vec<Check>,
}

impl ValidationReport {
    fn new(path: &Path, kind: &str) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: kind.into(),
            status: ValidationStatus::NotRun,
            checks: Vec::new(),
        }
    }

    fn push(&mut self, name: &str, status: ValidationStatus, detail: impl Into<String>) {
        self.checks.push(Check {
            name: name.into(),
            status,
            detail: detail.into(),
        });
    }

    fn conclude(mut self) -> Self {
        let failed = self
            .checks
            .iter()
            .any(|c| c.status == ValidationStatus::Failed);
        let passed = self
            .checks
            .iter()
            .filter(|c| c.status == ValidationStatus::Passed)
            .count();
        let unsupported = self
            .checks
            .iter()
            .any(|c| c.status == ValidationStatus::Unsupported);
        self.status = if failed {
            ValidationStatus::Failed
        } else if passed == 0 {
            ValidationStatus::Unsupported
        } else if unsupported {
            ValidationStatus::Partial
        } else {
            ValidationStatus::Passed
        };
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryFormat {
    Pe {
        machine: u16,
        /// CLR runtime header present: IL-only assemblies run on any CPU.
        managed: bool,
    },
    Elf {
        class: u8,
        machine: u16,
    },
    MachO {
        archs: [Option<u32>; 2],
        universal: bool,
    },
}

impl BinaryFormat {
    pub fn kind(self) -> &'static str {
        match self {
            BinaryFormat::Pe { .. } => "pe",
            BinaryFormat::Elf { .. } => "elf",
            BinaryFormat::MachO { .. } => "mach-o",
        }
    }
}

pub fn detect_binary(bytes: &[u8]) -> Option<BinaryFormat> {
    if bytes.len() >= 0x40 && &bytes[0..2] == b"MZ" {
        let e_lfanew =
            u32::from_le_bytes([bytes[0x3c], bytes[0x3d], bytes[0x3e], bytes[0x3f]]) as usize;
        if bytes.len() >= e_lfanew + 6 && &bytes[e_lfanew..e_lfanew + 4] == b"PE\0\0" {
            let machine = u16::from_le_bytes([bytes[e_lfanew + 4], bytes[e_lfanew + 5]]);
            return Some(BinaryFormat::Pe {
                machine,
                managed: pe_is_managed(bytes, e_lfanew),
            });
        }
        return None;
    }
    if bytes.len() >= 20 && &bytes[0..4] == b"\x7fELF" {
        return Some(BinaryFormat::Elf {
            class: bytes[4],
            machine: u16::from_le_bytes([bytes[18], bytes[19]]),
        });
    }
    parse_macho(bytes)
}

fn parse_macho(bytes: &[u8]) -> Option<BinaryFormat> {
    if bytes.len() < 8 {
        return None;
    }
    let be = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let le = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if matches!(be, 0xcafe_babe | 0xcafe_babf) || matches!(le, 0xcafe_babe | 0xcafe_babf) {
        let big_endian = matches!(be, 0xcafe_babe | 0xcafe_babf);
        let is_64 = matches!(be, 0xcafe_babf) || matches!(le, 0xcafe_babf);
        let count = if big_endian {
            u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize
        } else {
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize
        };
        let entry_size = if is_64 { 32 } else { 20 };
        if count == 0 || count > 32 || bytes.len() < 8 + count * entry_size {
            return None;
        }
        let mut archs = [None; 2];
        for (index, slot) in archs.iter_mut().enumerate().take(count.min(2)) {
            let base = 8 + index * entry_size;
            let cpu = if big_endian {
                u32::from_be_bytes([
                    bytes[base],
                    bytes[base + 1],
                    bytes[base + 2],
                    bytes[base + 3],
                ])
            } else {
                u32::from_le_bytes([
                    bytes[base],
                    bytes[base + 1],
                    bytes[base + 2],
                    bytes[base + 3],
                ])
            };
            *slot = Some(cpu);
        }
        return Some(BinaryFormat::MachO {
            archs,
            universal: true,
        });
    }
    // Thin Mach-O: magic is little-endian for modern slices.
    if matches!(le, 0xfeed_face | 0xfeed_facf) {
        let cpu = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        return Some(BinaryFormat::MachO {
            archs: [Some(cpu), None],
            universal: false,
        });
    }
    if matches!(be, 0xfeed_face | 0xfeed_facf) {
        let cpu = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        return Some(BinaryFormat::MachO {
            archs: [Some(cpu), None],
            universal: false,
        });
    }
    None
}

const PE_X86: u16 = 0x014c;
const PE_X64: u16 = 0x8664;
const PE_ARM64: u16 = 0xaa64;
const ELF_X64: u16 = 62;
const ELF_ARM64: u16 = 183;
const MACHO_X64: u32 = 0x0100_0007;
const MACHO_ARM64: u32 = 0x0100_000c;

fn arch_name(arch: Architecture) -> &'static str {
    match arch {
        Architecture::X64 => "x64",
        Architecture::Arm64 => "arm64",
    }
}

fn pe_is_managed(bytes: &[u8], pe: usize) -> bool {
    const COM_DESCRIPTOR_DIR: usize = 14;
    let optional = pe + 24;
    if bytes.len() < optional + 4 {
        return false;
    }
    let magic = u16::from_le_bytes([bytes[optional], bytes[optional + 1]]);
    let (count_at, dirs_at) = match magic {
        0x10b => (optional + 92, optional + 96),
        0x20b => (optional + 108, optional + 112),
        _ => return false,
    };
    if bytes.len() < count_at + 4 {
        return false;
    }
    let count = u32::from_le_bytes([
        bytes[count_at],
        bytes[count_at + 1],
        bytes[count_at + 2],
        bytes[count_at + 3],
    ]) as usize;
    if count <= COM_DESCRIPTOR_DIR {
        return false;
    }
    let entry = dirs_at + COM_DESCRIPTOR_DIR * 8;
    if bytes.len() < entry + 8 {
        return false;
    }
    let rva = u32::from_le_bytes([
        bytes[entry],
        bytes[entry + 1],
        bytes[entry + 2],
        bytes[entry + 3],
    ]);
    rva != 0
}

pub fn check_binary(bytes: &[u8], target: &Target, label: &str, report: &mut ValidationReport) {
    let Some(format) = detect_binary(bytes) else {
        report.push(
            &format!("{label}:format"),
            ValidationStatus::Unsupported,
            "not a recognised PE/ELF/Mach-O executable",
        );
        return;
    };
    report.push(
        &format!("{label}:format"),
        ValidationStatus::Passed,
        format.kind(),
    );
    let os_matches = matches!(
        (format, target.os),
        (BinaryFormat::Pe { .. }, TargetOs::Windows)
            | (BinaryFormat::Elf { .. }, TargetOs::Linux)
            | (BinaryFormat::MachO { .. }, TargetOs::Macos)
    );
    if !os_matches {
        report.push(
            &format!("{label}:os"),
            ValidationStatus::Failed,
            format!("{} binary for target {}", format.kind(), target.id),
        );
    }
    let mut present: Vec<Architecture> = Vec::new();
    let mut any_cpu = false;
    match format {
        BinaryFormat::Pe { machine, managed } => {
            if managed && machine == PE_X86 {
                any_cpu = true;
                present.push(Architecture::X64);
                present.push(Architecture::Arm64);
            } else if machine == PE_X64 {
                present.push(Architecture::X64);
            } else if machine == PE_ARM64 {
                present.push(Architecture::Arm64);
            }
        }
        BinaryFormat::Elf { class, machine } => {
            if class != 2 {
                report.push(
                    &format!("{label}:bits"),
                    ValidationStatus::Failed,
                    format!("ELF class {class}, expected 64-bit"),
                );
            }
            if machine == ELF_X64 {
                present.push(Architecture::X64);
            } else if machine == ELF_ARM64 {
                present.push(Architecture::Arm64);
            }
        }
        BinaryFormat::MachO { archs, universal } => {
            for cpu in archs.into_iter().flatten() {
                if cpu == MACHO_X64 {
                    present.push(Architecture::X64);
                } else if cpu == MACHO_ARM64 {
                    present.push(Architecture::Arm64);
                }
            }
            if target.universal && !universal {
                report.push(
                    &format!("{label}:universal"),
                    ValidationStatus::Failed,
                    "thin Mach-O found but the target requires a universal binary",
                );
            }
        }
    }
    if present.is_empty() {
        report.push(
            &format!("{label}:arch"),
            ValidationStatus::Failed,
            "unrecognised architecture",
        );
        return;
    }
    for expected in &target.arches {
        if !present.contains(expected) {
            report.push(
                &format!("{label}:arch"),
                ValidationStatus::Failed,
                format!(
                    "missing {} slice (found {})",
                    arch_name(*expected),
                    present
                        .iter()
                        .map(|a| arch_name(*a))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            return;
        }
    }
    report.push(
        &format!("{label}:arch"),
        ValidationStatus::Passed,
        if any_cpu {
            "anycpu".to_string()
        } else {
            present
                .iter()
                .map(|a| arch_name(*a))
                .collect::<Vec<_>>()
                .join(" + ")
        },
    );
}

/// Validate one artifact path against the target it claims to support.
pub fn validate_artifact(path: &Path, target: &Target) -> Result<ValidationReport> {
    if !path.exists() {
        return Err(UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            format!("artifact does not exist: {}", path.display()),
        ));
    }
    if path.is_dir() {
        return validate_directory(path, target);
    }
    let mut header = [0u8; 512];
    let read = read_prefix(path, &mut header)?;
    let header = &header[..read];
    if header.starts_with(b"PK\x03\x04") {
        return validate_zip(path, target);
    }
    if header.starts_with(&[0x1f, 0x8b]) {
        return validate_tar_gz(path, target);
    }
    if header.starts_with(b"!<arch>") {
        return validate_deb(path, target);
    }
    if detect_binary(header).is_some() {
        let mut report = ValidationReport::new(path, "binary");
        let bytes = std::fs::read(path)
            .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
        check_binary(&bytes, target, "binary", &mut report);
        return Ok(report.conclude());
    }
    let mut report = ValidationReport::new(path, "unknown");
    report.push(
        "format",
        ValidationStatus::Unsupported,
        "file is neither a known executable nor a supported package",
    );
    Ok(report.conclude())
}

fn read_prefix(path: &Path, buffer: &mut [u8]) -> Result<usize> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| UbuilderError::io(format!("cannot open {}", path.display()), e))?;
    let mut total = 0;
    while total < buffer.len() {
        match file.read(&mut buffer[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) => {
                return Err(UbuilderError::io(
                    format!("cannot read {}", path.display()),
                    e,
                ))
            }
        }
    }
    Ok(total)
}

fn validate_directory(path: &Path, target: &Target) -> Result<ValidationReport> {
    let mut report = ValidationReport::new(path, "directory");
    let plist = path.join("Contents/Info.plist");
    if plist.is_file() {
        report.push(
            "bundle",
            ValidationStatus::Passed,
            "Contents/Info.plist present",
        );
        let macos = path.join("Contents/MacOS");
        let mut checked = false;
        if let Ok(entries) = std::fs::read_dir(&macos) {
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    let bytes = std::fs::read(entry.path())
                        .map_err(|e| UbuilderError::io("cannot read bundle executable", e))?;
                    check_binary(&bytes, target, "bundle-exe", &mut report);
                    checked = true;
                }
            }
        }
        if !checked {
            report.push(
                "bundle-exe",
                ValidationStatus::Failed,
                "Contents/MacOS has no executable",
            );
        }
    } else {
        let binaries: Vec<PathBuf> = walkdir::WalkDir::new(path)
            .max_depth(3)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("exe"))
                    .unwrap_or(false)
                    || crate::packaging::archive::is_executable(e.path())
            })
            .map(|e| e.into_path())
            .collect();
        if binaries.is_empty() {
            report.push(
                "binary",
                ValidationStatus::Unsupported,
                "directory contains no executable",
            );
        }
        for binary in binaries.iter().take(8) {
            let bytes = std::fs::read(binary)
                .map_err(|e| UbuilderError::io("cannot read directory binary", e))?;
            let label = binary
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("binary");
            check_binary(&bytes, target, label, &mut report);
        }
    }
    Ok(report.conclude())
}

fn validate_zip(path: &Path, target: &Target) -> Result<ValidationReport> {
    let mut report = ValidationReport::new(path, "zip");
    let file = std::fs::File::open(path)
        .map_err(|e| UbuilderError::io(format!("cannot open {}", path.display()), e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            format!("{} is not a valid ZIP: {e}", path.display()),
        )
    })?;
    let mut binaries = 0usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| {
            UbuilderError::new(ErrorCode::ArtifactInvalid, format!("zip entry error: {e}"))
        })?;
        if entry.is_dir() {
            continue;
        }
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|e| UbuilderError::new(ErrorCode::ArtifactInvalid, format!("zip crc: {e}")))?;
        if detect_binary(&bytes).is_some() {
            binaries += 1;
            let name = entry.name().to_string();
            check_binary(&bytes, target, &name, &mut report);
            bytes.clear();
        }
    }
    if binaries == 0 {
        report.push(
            "binary",
            ValidationStatus::Unsupported,
            "archive contains no executable",
        );
    }
    Ok(report.conclude())
}

fn validate_tar_gz(path: &Path, target: &Target) -> Result<ValidationReport> {
    let mut report = ValidationReport::new(path, "tar.gz");
    let file = std::fs::File::open(path)
        .map_err(|e| UbuilderError::io(format!("cannot open {}", path.display()), e))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut binaries = 0usize;
    let entries = archive.entries().map_err(|e| {
        UbuilderError::new(
            ErrorCode::ArtifactInvalid,
            format!("{} is not a valid tar.gz: {e}", path.display()),
        )
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|e| {
            UbuilderError::new(ErrorCode::ArtifactInvalid, format!("tar entry: {e}"))
        })?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| {
            UbuilderError::new(ErrorCode::ArtifactInvalid, format!("tar read: {e}"))
        })?;
        if detect_binary(&bytes).is_some() {
            binaries += 1;
            let name = entry
                .path()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            check_binary(&bytes, target, &name, &mut report);
            bytes.clear();
        }
    }
    if binaries == 0 {
        report.push(
            "binary",
            ValidationStatus::Unsupported,
            "archive contains no executable",
        );
    }
    Ok(report.conclude())
}

/// Minimal `ar` reader that confirms a Debian package's required members.
fn validate_deb(path: &Path, target: &Target) -> Result<ValidationReport> {
    let mut report = ValidationReport::new(path, "deb");
    let bytes = std::fs::read(path)
        .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
    if !bytes.starts_with(b"!<arch>\n") {
        report.push("ar", ValidationStatus::Failed, "missing ar magic");
        return Ok(report.conclude());
    }
    let mut offset = 8usize;
    let mut members = Vec::new();
    while offset + 60 <= bytes.len() {
        let header = &bytes[offset..offset + 60];
        if &header[58..60] != b"`\n" {
            break;
        }
        let name = String::from_utf8_lossy(&header[0..16]).trim().to_string();
        let size: usize = String::from_utf8_lossy(&header[48..58])
            .trim()
            .parse()
            .unwrap_or(0);
        let start = offset + 60;
        if start + size > bytes.len() {
            report.push("ar", ValidationStatus::Failed, "truncated ar member");
            break;
        }
        members.push((name, &bytes[start..start + size]));
        offset = start + size + (size % 2);
    }
    report.push(
        "ar",
        ValidationStatus::Passed,
        format!("{} members", members.len()),
    );
    if let Some((_, data)) = members
        .iter()
        .find(|(name, _)| name.starts_with("debian-binary"))
    {
        if data.starts_with(b"2.0") {
            report.push("debian-binary", ValidationStatus::Passed, "2.0");
        } else {
            report.push(
                "debian-binary",
                ValidationStatus::Failed,
                "unexpected version",
            );
        }
    } else {
        report.push("debian-binary", ValidationStatus::Failed, "member missing");
    }
    let mut has_data = false;
    for (name, data) in &members {
        if name.starts_with("data.tar") {
            has_data = true;
            let decoder = flate2::read::GzDecoder::new(*data);
            let mut archive = tar::Archive::new(decoder);
            let mut binaries = 0usize;
            if let Ok(entries) = archive.entries() {
                for entry in entries.flatten() {
                    let mut content = Vec::new();
                    let mut entry = entry;
                    let _ = entry.read_to_end(&mut content);
                    if detect_binary(&content).is_some() {
                        binaries += 1;
                        check_binary(&content, target, "payload", &mut report);
                    }
                }
            }
            if binaries == 0 {
                report.push(
                    "payload",
                    ValidationStatus::Partial,
                    "no executable found in data.tar.gz",
                );
            }
        }
        if name.starts_with("control.tar") {
            let decoder = flate2::read::GzDecoder::new(*data);
            let mut archive = tar::Archive::new(decoder);
            let mut has_control = false;
            if let Ok(entries) = archive.entries() {
                for entry in entries.flatten() {
                    if entry
                        .path()
                        .map(|p| p.to_string_lossy().ends_with("control"))
                        .unwrap_or(false)
                    {
                        has_control = true;
                    }
                }
            }
            report.push(
                "control",
                if has_control {
                    ValidationStatus::Passed
                } else {
                    ValidationStatus::Failed
                },
                "control file",
            );
        }
    }
    if !has_data {
        report.push("data", ValidationStatus::Failed, "data.tar.gz missing");
    }
    Ok(report.conclude())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x100];
        bytes[0] = b'M';
        bytes[1] = b'Z';
        bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    fn macho(cpu: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0xfeed_facfu32.to_le_bytes());
        bytes.extend_from_slice(&cpu.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 32]);
        bytes
    }

    #[test]
    fn detects_pe_architecture() {
        let format = detect_binary(&pe(PE_X64)).unwrap();
        assert!(matches!(
            format,
            BinaryFormat::Pe {
                machine: PE_X64,
                ..
            }
        ));
        let target = Target::parse("windows-x64").unwrap();
        let mut report = ValidationReport::new(Path::new("x"), "binary");
        check_binary(&pe(PE_X64), &target, "x", &mut report);
        assert_eq!(report.conclude().status, ValidationStatus::Passed);
    }

    #[test]
    fn rejects_wrong_architecture() {
        let target = Target::parse("windows-arm64").unwrap();
        let mut report = ValidationReport::new(Path::new("x"), "binary");
        check_binary(&pe(PE_X64), &target, "x", &mut report);
        assert_eq!(report.conclude().status, ValidationStatus::Failed);
    }

    #[test]
    fn macho_universal_requires_both_slices() {
        let mut fat = Vec::new();
        fat.extend_from_slice(&0xcafe_babeu32.to_be_bytes());
        fat.extend_from_slice(&2u32.to_be_bytes());
        // entries pointing past the header for the test parser only
        fat.extend_from_slice(&[0u8; 40]);
        let format = detect_binary(&fat).unwrap();
        assert!(matches!(
            format,
            BinaryFormat::MachO {
                universal: true,
                ..
            }
        ));
        let target = Target::parse("macos-universal").unwrap();
        let mut report = ValidationReport::new(Path::new("x"), "binary");
        check_binary(&fat, &target, "x", &mut report);
        assert_eq!(report.conclude().status, ValidationStatus::Failed);
    }

    #[test]
    fn thin_macho_for_universal_fails() {
        let target = Target::parse("macos-universal").unwrap();
        let mut report = ValidationReport::new(Path::new("x"), "binary");
        check_binary(&macho(MACHO_ARM64), &target, "x", &mut report);
        assert_eq!(report.conclude().status, ValidationStatus::Failed);
    }
}
