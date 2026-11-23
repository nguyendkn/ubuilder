//! Target model and cross-compilation target resolution.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::{Architecture, Host, HostPlatform};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetOs {
    Windows,
    Linux,
    Macos,
}

impl TargetOs {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetOs::Windows => "windows",
            TargetOs::Linux => "linux",
            TargetOs::Macos => "macos",
        }
    }

    pub fn host_platform(self) -> HostPlatform {
        match self {
            TargetOs::Windows => HostPlatform::Windows,
            TargetOs::Linux => HostPlatform::Linux,
            TargetOs::Macos => HostPlatform::Macos,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Abi {
    Msvc,
    Gnu,
    Darwin,
}

impl Abi {
    pub fn as_str(self) -> &'static str {
        match self {
            Abi::Msvc => "msvc",
            Abi::Gnu => "gnu",
            Abi::Darwin => "darwin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackagingFormat {
    PortableZip,
    Directory,
    TarGz,
    Deb,
    Rpm,
    AppImage,
    Nsis,
    Msi,
    Msix,
    AppBundle,
    MacZip,
    Dmg,
}

impl PackagingFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            PackagingFormat::PortableZip => "portable-zip",
            PackagingFormat::Directory => "directory",
            PackagingFormat::TarGz => "tar-gz",
            PackagingFormat::Deb => "deb",
            PackagingFormat::Rpm => "rpm",
            PackagingFormat::AppImage => "appimage",
            PackagingFormat::Nsis => "nsis",
            PackagingFormat::Msi => "msi",
            PackagingFormat::Msix => "msix",
            PackagingFormat::AppBundle => "app",
            PackagingFormat::MacZip => "mac-zip",
            PackagingFormat::Dmg => "dmg",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.to_ascii_lowercase().as_str() {
            "portable" | "portable-zip" | "zip" => PackagingFormat::PortableZip,
            "directory" | "dir" | "unpacked" => PackagingFormat::Directory,
            "tar.gz" | "targz" | "tar-gz" => PackagingFormat::TarGz,
            "deb" => PackagingFormat::Deb,
            "rpm" => PackagingFormat::Rpm,
            "appimage" => PackagingFormat::AppImage,
            "nsis" | "exe" => PackagingFormat::Nsis,
            "msi" => PackagingFormat::Msi,
            "msix" => PackagingFormat::Msix,
            "app" | "bundle" => PackagingFormat::AppBundle,
            "mac-zip" | "maczip" => PackagingFormat::MacZip,
            "dmg" => PackagingFormat::Dmg,
            _ => return None,
        })
    }

    pub fn extension(self) -> &'static str {
        match self {
            PackagingFormat::PortableZip => "zip",
            PackagingFormat::Directory => "",
            PackagingFormat::TarGz => "tar.gz",
            PackagingFormat::Deb => "deb",
            PackagingFormat::Rpm => "rpm",
            PackagingFormat::AppImage => "AppImage",
            PackagingFormat::Nsis => "exe",
            PackagingFormat::Msi => "msi",
            PackagingFormat::Msix => "msix",
            PackagingFormat::AppBundle => "app",
            PackagingFormat::MacZip => "zip",
            PackagingFormat::Dmg => "dmg",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SigningRequirement {
    None,
    Optional,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub id: String,
    pub os: TargetOs,
    pub arches: Vec<Architecture>,
    pub abi: Abi,
    pub universal: bool,
    pub triple: Option<String>,
    pub rid: Option<String>,
    pub min_os: Option<String>,
    pub required_sdk: Option<String>,
    pub required_linker: Option<String>,
    pub packaging_formats: Vec<PackagingFormat>,
    pub signing: SigningRequirement,
}

impl Target {
    pub fn parse(value: &str) -> Result<Self> {
        let key = value.trim().to_ascii_lowercase();
        let found = Self::all().into_iter().find(|t| {
            t.id == key
                || t.triple.as_deref() == Some(key.as_str())
                || t.rid.as_deref() == Some(key.as_str())
                || aliases(&t.id).contains(&key.as_str())
        });
        found.ok_or_else(|| {
            UbuilderError::new(
                ErrorCode::UnsupportedTarget,
                format!("unknown build target '{value}'"),
            )
            .remediation(
                "Use one of: windows-x64, windows-arm64, linux-x64, linux-arm64, \
                 macos-x64, macos-arm64, macos-universal",
            )
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn arch(&self) -> Option<Architecture> {
        if self.arches.len() == 1 {
            Some(self.arches[0])
        } else {
            None
        }
    }

    pub fn display_name(&self) -> String {
        if self.universal {
            "macOS Universal (x64 + arm64)".to_string()
        } else {
            format!(
                "{} {}",
                match self.os {
                    TargetOs::Windows => "Windows",
                    TargetOs::Linux => "Linux",
                    TargetOs::Macos => "macOS",
                },
                self.arch().map(Architecture::as_str).unwrap_or("?")
            )
        }
    }

    pub fn is_native(&self, host: &Host) -> bool {
        if self.universal {
            return host.platform == HostPlatform::Macos;
        }
        self.os.host_platform() == host.platform && self.arch() == Some(host.arch)
    }

    pub fn requires_cross_compilation(&self, host: &Host) -> bool {
        if self.universal {
            return true;
        }
        self.os.host_platform() != host.platform || self.arch() != Some(host.arch)
    }

    pub fn executable_suffix(&self) -> &'static str {
        match self.os {
            TargetOs::Windows => ".exe",
            _ => "",
        }
    }

    /// Rust target triples needed to produce this target.
    pub fn component_triples(&self) -> Vec<String> {
        if self.universal {
            vec![
                "x86_64-apple-darwin".to_string(),
                "aarch64-apple-darwin".to_string(),
            ]
        } else {
            self.triple.iter().cloned().collect()
        }
    }

    pub fn component_targets(&self) -> Vec<Target> {
        if !self.universal {
            return vec![self.clone()];
        }
        Self::all()
            .into_iter()
            .filter(|t| t.os == TargetOs::Macos && !t.universal)
            .collect()
    }

    pub fn all() -> Vec<Target> {
        vec![
            Target {
                id: "windows-x64".into(),
                os: TargetOs::Windows,
                arches: vec![Architecture::X64],
                abi: Abi::Msvc,
                universal: false,
                triple: Some("x86_64-pc-windows-msvc".into()),
                rid: Some("win-x64".into()),
                min_os: Some("10.0.17763".into()),
                required_sdk: Some("Windows 10/11 SDK".into()),
                required_linker: Some("link.exe (MSVC) or lld-link".into()),
                packaging_formats: vec![
                    PackagingFormat::PortableZip,
                    PackagingFormat::Nsis,
                    PackagingFormat::Msi,
                    PackagingFormat::Msix,
                ],
                signing: SigningRequirement::Optional,
            },
            Target {
                id: "windows-arm64".into(),
                os: TargetOs::Windows,
                arches: vec![Architecture::Arm64],
                abi: Abi::Msvc,
                universal: false,
                triple: Some("aarch64-pc-windows-msvc".into()),
                rid: Some("win-arm64".into()),
                min_os: Some("10.0.22000".into()),
                required_sdk: Some("Windows 11 SDK".into()),
                required_linker: Some("link.exe (MSVC ARM64 libraries)".into()),
                packaging_formats: vec![
                    PackagingFormat::PortableZip,
                    PackagingFormat::Nsis,
                    PackagingFormat::Msi,
                    PackagingFormat::Msix,
                ],
                signing: SigningRequirement::Optional,
            },
            Target {
                id: "linux-x64".into(),
                os: TargetOs::Linux,
                arches: vec![Architecture::X64],
                abi: Abi::Gnu,
                universal: false,
                triple: Some("x86_64-unknown-linux-gnu".into()),
                rid: Some("linux-x64".into()),
                min_os: Some("glibc 2.17".into()),
                required_sdk: Some("glibc/sysroot".into()),
                required_linker: Some("GCC/Clang or zig".into()),
                packaging_formats: vec![
                    PackagingFormat::TarGz,
                    PackagingFormat::Deb,
                    PackagingFormat::Rpm,
                    PackagingFormat::AppImage,
                ],
                signing: SigningRequirement::None,
            },
            Target {
                id: "linux-arm64".into(),
                os: TargetOs::Linux,
                arches: vec![Architecture::Arm64],
                abi: Abi::Gnu,
                universal: false,
                triple: Some("aarch64-unknown-linux-gnu".into()),
                rid: Some("linux-arm64".into()),
                min_os: Some("glibc 2.17".into()),
                required_sdk: Some("aarch64 sysroot".into()),
                required_linker: Some("aarch64 GCC/Clang or zig".into()),
                packaging_formats: vec![
                    PackagingFormat::TarGz,
                    PackagingFormat::Deb,
                    PackagingFormat::Rpm,
                    PackagingFormat::AppImage,
                ],
                signing: SigningRequirement::None,
            },
            Target {
                id: "macos-x64".into(),
                os: TargetOs::Macos,
                arches: vec![Architecture::X64],
                abi: Abi::Darwin,
                universal: false,
                triple: Some("x86_64-apple-darwin".into()),
                rid: Some("osx-x64".into()),
                min_os: Some("10.15".into()),
                required_sdk: Some("macOS SDK".into()),
                required_linker: Some("ld64 via Xcode/OSXCross".into()),
                packaging_formats: vec![
                    PackagingFormat::AppBundle,
                    PackagingFormat::MacZip,
                    PackagingFormat::Dmg,
                ],
                signing: SigningRequirement::Optional,
            },
            Target {
                id: "macos-arm64".into(),
                os: TargetOs::Macos,
                arches: vec![Architecture::Arm64],
                abi: Abi::Darwin,
                universal: false,
                triple: Some("aarch64-apple-darwin".into()),
                rid: Some("osx-arm64".into()),
                min_os: Some("11.0".into()),
                required_sdk: Some("macOS SDK".into()),
                required_linker: Some("ld64 via Xcode/OSXCross".into()),
                packaging_formats: vec![
                    PackagingFormat::AppBundle,
                    PackagingFormat::MacZip,
                    PackagingFormat::Dmg,
                ],
                signing: SigningRequirement::Optional,
            },
            Target {
                id: "macos-universal".into(),
                os: TargetOs::Macos,
                arches: vec![Architecture::X64, Architecture::Arm64],
                abi: Abi::Darwin,
                universal: true,
                triple: None,
                rid: None,
                min_os: Some("11.0".into()),
                required_sdk: Some("macOS SDK".into()),
                required_linker: Some("ld64 + lipo (or built-in Mach-O merge)".into()),
                packaging_formats: vec![
                    PackagingFormat::AppBundle,
                    PackagingFormat::MacZip,
                    PackagingFormat::Dmg,
                ],
                signing: SigningRequirement::Optional,
            },
        ]
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id)
    }
}

fn aliases(id: &str) -> &'static [&'static str] {
    match id {
        "windows-x64" => &[
            "win-x64",
            "windows-x86_64",
            "x86_64-pc-windows-msvc",
            "win-x86_64",
        ],
        "windows-arm64" => &["win-arm64", "windows-aarch64", "aarch64-pc-windows-msvc"],
        "linux-x64" => &["linux-x86_64", "x86_64-unknown-linux-gnu"],
        "linux-arm64" => &["linux-aarch64", "aarch64-unknown-linux-gnu"],
        "macos-x64" => &["mac-x64", "macos-x86_64", "x86_64-apple-darwin", "osx-x64"],
        "macos-arm64" => &[
            "mac-arm64",
            "macos-aarch64",
            "aarch64-apple-darwin",
            "osx-arm64",
        ],
        "macos-universal" => &["mac-universal", "universal", "universal2"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases_and_triples() {
        assert_eq!(Target::parse("win-x64").unwrap().id, "windows-x64");
        assert_eq!(
            Target::parse("aarch64-apple-darwin").unwrap().id,
            "macos-arm64"
        );
        assert_eq!(
            Target::parse("macos-universal")
                .unwrap()
                .component_triples(),
            vec!["x86_64-apple-darwin", "aarch64-apple-darwin"]
        );
    }

    #[test]
    fn rejects_unknown_target() {
        let err = Target::parse("solaris-sparc").unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedTarget);
    }

    #[test]
    fn universal_is_composite_not_triple() {
        let t = Target::parse("macos-universal").unwrap();
        assert!(t.triple.is_none());
        assert!(t.universal);
        assert_eq!(t.component_targets().len(), 2);
    }
}
