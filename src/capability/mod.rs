//! Multi-dimensional capability model.
//!
//! A successful compile is not the same as a distributable artifact. Every
//! host/target/framework combination is scored independently for compilation,
//! packaging, signing and native testing.

use crate::host::{Architecture, Host, HostPlatform};
use crate::project::{DotnetFlavor, FrameworkKind};
use crate::target::{PackagingFormat, Target, TargetOs};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SupportLevel {
    Supported,
    Conditional,
    Experimental,
    Unsupported,
}

impl SupportLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "SUPPORTED",
            Self::Conditional => "CONDITIONAL",
            Self::Experimental => "EXPERIMENTAL",
            Self::Unsupported => "UNSUPPORTED",
        }
    }

    pub fn is_buildable(self) -> bool {
        self != Self::Unsupported
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub description: String,
    pub satisfied: bool,
    pub remediation: Option<String>,
}

impl Requirement {
    pub fn new(id: &str, description: impl Into<String>, satisfied: bool) -> Self {
        Self {
            id: id.to_string(),
            description: description.into(),
            satisfied,
            remediation: None,
        }
    }

    pub fn remediation(mut self, text: impl Into<String>) -> Self {
        self.remediation = Some(text.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityWarning {
    pub code: String,
    pub message: String,
}

impl CapabilityWarning {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildCapability {
    pub compilation: SupportLevel,
    pub packaging: SupportLevel,
    pub signing: SupportLevel,
    pub native_testing: SupportLevel,
    pub requirements: Vec<Requirement>,
    pub warnings: Vec<CapabilityWarning>,
}

impl BuildCapability {
    pub fn overall(&self) -> SupportLevel {
        self.compilation.max(self.packaging)
    }

    pub fn unsatisfied(&self) -> Vec<&Requirement> {
        self.requirements.iter().filter(|r| !r.satisfied).collect()
    }
}

/// Facts collected by the toolchain probe layer. The capability resolver never
/// invokes subprocesses itself, which keeps it unit-testable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Facts {
    pub rustc_version: Option<String>,
    pub cargo_version: Option<String>,
    pub installed_rust_targets: Vec<String>,
    pub commands: BTreeSet<String>,
    pub dotnet_sdk: Option<String>,
    pub node_version: Option<String>,
    pub npm_version: Option<String>,
    pub electron_builder_version: Option<String>,
    pub tauri_cli_version: Option<String>,
    pub xcode_version: Option<String>,
    pub codesign: bool,
    pub notarytool: bool,
    pub signtool: bool,
    pub makensis: bool,
    pub appimagetool: bool,
    pub rpmbuild: bool,
    pub dpkg_deb: bool,
    pub hdiutil: bool,
    pub lipo: bool,
    pub osxcross: bool,
    pub zig_version: Option<String>,
    pub cross_version: Option<String>,
    pub xwin_version: Option<String>,
}

impl Facts {
    pub fn has(&self, command: &str) -> bool {
        let normalized = command.to_ascii_lowercase();
        let bare = normalized
            .strip_suffix(".exe")
            .unwrap_or(&normalized)
            .to_string();
        self.commands.contains(&normalized) || self.commands.contains(&bare)
    }

    pub fn rust_target(&self, triple: &str) -> bool {
        self.installed_rust_targets.iter().any(|t| t == triple)
    }

    pub fn with_commands(commands: &[&str]) -> Self {
        Self {
            commands: commands.iter().map(|c| c.to_string()).collect(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone)]
pub struct CapabilityInput {
    pub framework: FrameworkKind,
    pub dotnet_flavor: Option<DotnetFlavor>,
    pub native_aot: bool,
    pub has_native_node_modules: bool,
}

impl CapabilityInput {
    pub fn new(framework: FrameworkKind) -> Self {
        Self {
            framework,
            dotnet_flavor: None,
            native_aot: false,
            has_native_node_modules: false,
        }
    }
}

pub fn resolve(
    host: &Host,
    target: &Target,
    input: &CapabilityInput,
    facts: &Facts,
) -> BuildCapability {
    match input.framework {
        FrameworkKind::Rust => resolve_rust(host, target, facts),
        FrameworkKind::Dotnet => resolve_dotnet(host, target, input, facts),
        FrameworkKind::Electron => resolve_electron(host, target, input, facts),
        FrameworkKind::Tauri => resolve_tauri(host, target, facts),
    }
}

fn signing_capability(host: &Host, target: &Target, facts: &Facts) -> SupportLevel {
    match target.os {
        TargetOs::Windows => {
            if host.platform == HostPlatform::Windows && facts.signtool {
                SupportLevel::Conditional
            } else {
                SupportLevel::Unsupported
            }
        }
        TargetOs::Macos => {
            if host.platform == HostPlatform::Macos && facts.codesign {
                SupportLevel::Conditional
            } else {
                SupportLevel::Unsupported
            }
        }
        TargetOs::Linux => SupportLevel::Unsupported,
    }
}

fn native_testing(host: &Host, target: &Target) -> SupportLevel {
    if target.is_native(host) {
        SupportLevel::Supported
    } else {
        SupportLevel::Unsupported
    }
}

fn linker_for(host: &Host, target: &Target, facts: &Facts) -> bool {
    match (host.platform, target.os) {
        (HostPlatform::Windows, TargetOs::Windows) => {
            facts.has("link.exe") || facts.has("cl.exe") || facts.has("lld-link")
        }
        (HostPlatform::Linux, TargetOs::Linux) => {
            facts.has("cc") || facts.has("gcc") || facts.has("clang") || facts.has("zig")
        }
        (HostPlatform::Macos, TargetOs::Macos) => {
            facts.has("cc") || facts.has("clang") || facts.has("xcrun")
        }
        _ => false,
    }
}

fn packaging_support(host: &Host, target: &Target, facts: &Facts) -> SupportLevel {
    // Portable archives are produced by ubuilder itself and work for every target.
    let mut levels = Vec::new();
    for format in &target.packaging_formats {
        levels.push(format_support(format, host, target, facts).0);
    }
    if levels.is_empty() {
        SupportLevel::Unsupported
    } else {
        levels
            .into_iter()
            .min()
            .unwrap_or(SupportLevel::Unsupported)
    }
}

/// Capability of one packaging format. Debian packages and macOS app bundles are
/// assembled by ubuilder; rpm/NSIS/AppImage/DMG depend on external tools.
pub fn format_support(
    format: &PackagingFormat,
    host: &Host,
    target: &Target,
    facts: &Facts,
) -> (SupportLevel, Vec<Requirement>) {
    match format {
        PackagingFormat::PortableZip | PackagingFormat::MacZip => (SupportLevel::Supported, vec![]),
        PackagingFormat::Directory => (SupportLevel::Supported, vec![]),
        PackagingFormat::TarGz => (SupportLevel::Supported, vec![]),
        PackagingFormat::Deb => (
            SupportLevel::Supported,
            vec![Requirement::new(
                "deb-format",
                "internal Debian archive writer",
                true,
            )],
        ),
        PackagingFormat::Rpm => (
            if facts.rpmbuild {
                SupportLevel::Conditional
            } else {
                SupportLevel::Unsupported
            },
            vec![
                Requirement::new("rpmbuild", "rpmbuild from the rpm package", facts.rpmbuild)
                    .remediation("Install rpm tooling (e.g. `rpm` package) or choose tar-gz"),
            ],
        ),
        PackagingFormat::AppImage => (
            if facts.appimagetool {
                SupportLevel::Conditional
            } else {
                SupportLevel::Unsupported
            },
            vec![
                Requirement::new("appimagetool", "appimagetool on PATH", facts.appimagetool)
                    .remediation("Install appimagetool or choose tar-gz"),
            ],
        ),
        PackagingFormat::Nsis => (
            if facts.makensis && target.os == TargetOs::Windows {
                SupportLevel::Conditional
            } else {
                SupportLevel::Unsupported
            },
            vec![
                Requirement::new("makensis", "NSIS makensis on PATH", facts.makensis)
                    .remediation("Install NSIS or use portable-zip"),
            ],
        ),
        PackagingFormat::Msi | PackagingFormat::Msix => (
            SupportLevel::Unsupported,
            vec![Requirement::new(
                "msi-provider",
                "an MSI/MSIX packaging provider",
                false,
            )],
        ),
        PackagingFormat::Dmg => (
            if host.platform == HostPlatform::Macos && facts.hdiutil {
                SupportLevel::Conditional
            } else {
                SupportLevel::Unsupported
            },
            vec![Requirement::new(
                "hdiutil",
                "hdiutil on a macOS host",
                host.platform == HostPlatform::Macos && facts.hdiutil,
            )
            .remediation("Build the DMG on macOS, or use mac-zip")],
        ),
        PackagingFormat::AppBundle => (SupportLevel::Supported, vec![]),
    }
}

fn resolve_rust(host: &Host, target: &Target, facts: &Facts) -> BuildCapability {
    let mut reqs = Vec::new();
    let mut warnings = Vec::new();
    let rustc = facts.rustc_version.is_some();
    reqs.push(
        Requirement::new("rustc", "Rust compiler (rustc)", rustc)
            .remediation("Install Rust with rustup: https://rustup.rs"),
    );
    let triples = target.component_triples();
    let all_targets = triples.iter().all(|t| facts.rust_target(t));
    reqs.push(
        Requirement::new(
            "rust-target",
            format!("Rust target(s) {}", triples.join(", ")),
            all_targets,
        )
        .remediation(format!("rustup target add {}", triples.join(" "))),
    );

    let linker = linker_for(host, target, facts);
    reqs.push(
        Requirement::new("linker", "native linker", linker)
            .remediation("Install the platform linker (MSVC build tools, GCC/Clang, or Xcode)"),
    );

    let native = target.is_native(host);
    let mut level = SupportLevel::Supported;

    if !rustc {
        level = SupportLevel::Unsupported;
    } else if native {
        if !linker {
            level = SupportLevel::Conditional;
            warnings.push(CapabilityWarning::new(
                "linker-unverified",
                "rustc exists but no platform linker was found on PATH",
            ));
        }
        if !all_targets {
            level = SupportLevel::Conditional;
        }
    } else {
        let same_platform = target.os.host_platform() == host.platform;
        level = match (host.platform, target.os) {
            (HostPlatform::Windows, TargetOs::Linux) => {
                if all_targets
                    && (facts.zig_version.is_some()
                        || facts.cross_version.is_some()
                        || facts.has("clang")
                        || facts.has("gcc")
                        || facts.has("cc"))
                {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                }
            }
            (HostPlatform::Windows, TargetOs::Macos) => {
                if facts.osxcross && all_targets {
                    SupportLevel::Experimental
                } else {
                    SupportLevel::Unsupported
                }
            }
            (HostPlatform::Linux, TargetOs::Windows) => {
                if (facts.xwin_version.is_some() || facts.has("x86_64-w64-mingw32-gcc"))
                    && all_targets
                {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                }
            }
            (HostPlatform::Linux, TargetOs::Macos) => {
                if facts.osxcross && all_targets {
                    SupportLevel::Experimental
                } else {
                    SupportLevel::Unsupported
                }
            }
            (HostPlatform::Macos, TargetOs::Windows) => {
                if (facts.xwin_version.is_some() || facts.has("x86_64-w64-mingw32-gcc"))
                    && all_targets
                {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                }
            }
            (HostPlatform::Macos, TargetOs::Linux) => {
                if all_targets && (facts.zig_version.is_some() || facts.cross_version.is_some()) {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                }
            }
            _ if same_platform => {
                if all_targets && linker {
                    SupportLevel::Conditional
                } else {
                    SupportLevel::Unsupported
                }
            }
            _ => SupportLevel::Unsupported,
        };
        if level.is_buildable() {
            warnings.push(CapabilityWarning::new(
                "cross-compile",
                format!(
                    "cross-compiling {} from {} is capability-checked, not a native test",
                    target.id,
                    host.display()
                ),
            ));
        }
    }

    if target.universal {
        if host.platform != HostPlatform::Macos {
            if facts.osxcross && all_targets {
                level = SupportLevel::Experimental;
            } else {
                level = SupportLevel::Unsupported;
            }
        } else if !all_targets || !linker {
            level = SupportLevel::Conditional;
        }
        warnings.push(CapabilityWarning::new(
            "universal-merge",
            "macos-universal is produced by merging both slices; merge verification is required",
        ));
    }

    if target.arches.len() == 1
        && target.arch() == Some(Architecture::Arm64)
        && target.os == TargetOs::Windows
    {
        warnings.push(CapabilityWarning::new(
            "windows-arm64",
            "Windows ARM64 needs ARM64 MSVC libraries and an ARM64-capable host or linker",
        ));
    }

    BuildCapability {
        compilation: level,
        packaging: packaging_support(host, target, facts),
        signing: signing_capability(host, target, facts),
        native_testing: native_testing(host, target),
        requirements: reqs,
        warnings,
    }
}

fn resolve_dotnet(
    host: &Host,
    target: &Target,
    input: &CapabilityInput,
    facts: &Facts,
) -> BuildCapability {
    let mut reqs = Vec::new();
    let mut warnings = Vec::new();
    let sdk = facts.dotnet_sdk.is_some();
    reqs.push(
        Requirement::new("dotnet-sdk", ".NET SDK", sdk)
            .remediation("Install the .NET SDK: https://dotnet.microsoft.com/download"),
    );
    let flavor = input.dotnet_flavor.unwrap_or(DotnetFlavor::Console);
    let mut level = if !sdk {
        SupportLevel::Unsupported
    } else if target.is_native(host) {
        SupportLevel::Supported
    } else {
        SupportLevel::Conditional
    };
    if flavor.is_windows_only() && target.os != TargetOs::Windows {
        level = SupportLevel::Unsupported;
        warnings.push(CapabilityWarning::new(
            "windows-desktop",
            format!("{} only runs on Windows", flavor.as_str()),
        ));
    }
    if flavor == DotnetFlavor::Maui {
        level = level.min(SupportLevel::Conditional);
        warnings.push(CapabilityWarning::new(
            "maui-workload",
            ".NET MAUI requires workloads and platform-specific tooling; not verified by ubuilder",
        ));
    }
    if input.native_aot && target.os.host_platform() != host.platform {
        level = SupportLevel::Unsupported;
        warnings.push(CapabilityWarning::new(
            "native-aot-cross-os",
            "Native AOT cross-OS publishing is not supported by this adapter",
        ));
    }
    if target.universal {
        if host.platform == HostPlatform::Macos && sdk {
            level = SupportLevel::Conditional;
        } else {
            level = SupportLevel::Unsupported;
        }
        warnings.push(CapabilityWarning::new(
            "universal-merge",
            "universal .NET apps require both RID publishes and a Mach-O merge",
        ));
    }
    if let Some(rid) = &target.rid {
        reqs.push(Requirement::new(
            "rid",
            format!("runtime identifier {rid}"),
            true,
        ));
    }
    BuildCapability {
        compilation: level,
        packaging: packaging_support(host, target, facts),
        signing: signing_capability(host, target, facts),
        native_testing: native_testing(host, target),
        requirements: reqs,
        warnings,
    }
}

fn resolve_electron(
    host: &Host,
    target: &Target,
    input: &CapabilityInput,
    facts: &Facts,
) -> BuildCapability {
    let mut reqs = Vec::new();
    let mut warnings = Vec::new();
    let node = facts.node_version.is_some();
    let npm = facts.npm_version.is_some();
    reqs.push(Requirement::new("node", "Node.js", node).remediation("Install Node.js 20+"));
    reqs.push(Requirement::new("npm", "npm", npm).remediation("Install npm"));
    let builder = facts.electron_builder_version.is_some();
    reqs.push(
        Requirement::new("electron-builder", "electron-builder", builder)
            .remediation("Add electron-builder to the project's devDependencies"),
    );

    let compilation = if !node || !npm {
        SupportLevel::Unsupported
    } else if input.has_native_node_modules && target.os.host_platform() != host.platform {
        warnings.push(CapabilityWarning::new(
            "native-node-modules",
            "native Node modules must be rebuilt per target OS/architecture",
        ));
        SupportLevel::Conditional
    } else {
        SupportLevel::Supported
    };

    let packaging = if !builder {
        SupportLevel::Unsupported
    } else if target.universal {
        if host.platform == HostPlatform::Macos {
            SupportLevel::Conditional
        } else {
            SupportLevel::Unsupported
        }
    } else if target.os == TargetOs::Macos && host.platform != HostPlatform::Macos {
        warnings.push(CapabilityWarning::new(
            "macos-packaging",
            "electron-builder macOS packaging requires a macOS host",
        ));
        SupportLevel::Unsupported
    } else if target.os.host_platform() == host.platform {
        SupportLevel::Conditional
    } else {
        warnings.push(CapabilityWarning::new(
            "cross-packaging",
            "electron-builder cross-OS packaging is tool- and wine-dependent",
        ));
        SupportLevel::Conditional
    };

    BuildCapability {
        compilation,
        packaging,
        signing: signing_capability(host, target, facts),
        native_testing: native_testing(host, target),
        requirements: reqs,
        warnings,
    }
}

fn resolve_tauri(host: &Host, target: &Target, facts: &Facts) -> BuildCapability {
    let mut reqs = Vec::new();
    let mut warnings = Vec::new();
    let rustc = facts.rustc_version.is_some();
    let node = facts.node_version.is_some();
    reqs.push(
        Requirement::new("rustc", "Rust compiler", rustc).remediation("Install Rust with rustup"),
    );
    reqs.push(
        Requirement::new("node", "Node.js frontend toolchain", node)
            .remediation("Install Node.js 20+"),
    );
    let triples = target.component_triples();
    let all_targets = triples.iter().all(|t| facts.rust_target(t));
    reqs.push(
        Requirement::new(
            "rust-target",
            format!("Rust target(s) {}", triples.join(", ")),
            all_targets,
        )
        .remediation(format!("rustup target add {}", triples.join(" "))),
    );
    let native = target.is_native(host);
    let mut level = if !rustc || !node {
        SupportLevel::Unsupported
    } else if native {
        SupportLevel::Supported
    } else if target.os == TargetOs::Macos && host.platform != HostPlatform::Macos {
        SupportLevel::Unsupported
    } else if target.os == TargetOs::Windows
        && host.platform == HostPlatform::Linux
        && facts.xwin_version.is_some()
        && all_targets
    {
        SupportLevel::Experimental
    } else if target.universal && host.platform == HostPlatform::Macos && all_targets {
        SupportLevel::Conditional
    } else {
        SupportLevel::Unsupported
    };

    match target.os {
        TargetOs::Windows => {
            warnings.push(CapabilityWarning::new(
                "webview2",
                "Tauri Windows runtime requires WebView2 (Evergreen or bundled fixed version)",
            ));
        }
        TargetOs::Linux => {
            let webkit = facts.has("pkg-config");
            reqs.push(
                Requirement::new("webkit2gtk", "WebKitGTK development packages", webkit)
                    .remediation("Install libwebkit2gtk-4.1-dev (Ubuntu) or equivalent"),
            );
            warnings.push(CapabilityWarning::new(
                "webkitgtk",
                "Tauri Linux runtime requires WebKitGTK 4.1 not verified by ubuilder",
            ));
            if !webkit {
                level = level.min(SupportLevel::Conditional);
            }
        }
        TargetOs::Macos => {
            warnings.push(CapabilityWarning::new(
                "wkwebview",
                "Tauri macOS uses the system WKWebView; a macOS host is required",
            ));
        }
    }

    let packaging = if target.universal && host.platform != HostPlatform::Macos {
        SupportLevel::Unsupported
    } else {
        SupportLevel::Conditional
    };

    BuildCapability {
        compilation: level,
        packaging,
        signing: signing_capability(host, target, facts),
        native_testing: native_testing(host, target),
        requirements: reqs,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Architecture, Host};

    fn host(platform: HostPlatform, arch: Architecture) -> Host {
        Host {
            platform,
            arch,
            triple: "test".into(),
            os_version: None,
        }
    }

    fn rust_facts() -> Facts {
        Facts {
            rustc_version: Some("1.99.0".into()),
            cargo_version: Some("1.99.0".into()),
            installed_rust_targets: vec!["x86_64-pc-windows-msvc".into()],
            commands: ["rustc", "cargo", "link.exe", "cc"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..Facts::default()
        }
    }

    #[test]
    fn compilation_and_signing_are_independent() {
        let h = host(HostPlatform::Windows, Architecture::X64);
        let target = Target::parse("windows-x64").unwrap();
        let cap = resolve(
            &h,
            &target,
            &CapabilityInput::new(FrameworkKind::Rust),
            &rust_facts(),
        );
        assert_eq!(cap.compilation, SupportLevel::Supported);
        assert_eq!(cap.signing, SupportLevel::Unsupported);
        assert_eq!(cap.native_testing, SupportLevel::Supported);
    }

    #[test]
    fn mac_target_from_windows_needs_osxcross() {
        let h = host(HostPlatform::Windows, Architecture::X64);
        let target = Target::parse("macos-arm64").unwrap();
        let mut facts = rust_facts();
        facts
            .installed_rust_targets
            .push("aarch64-apple-darwin".into());
        let cap = resolve(
            &h,
            &target,
            &CapabilityInput::new(FrameworkKind::Rust),
            &facts,
        );
        assert_eq!(cap.compilation, SupportLevel::Unsupported);
        facts.osxcross = true;
        let cap = resolve(
            &h,
            &target,
            &CapabilityInput::new(FrameworkKind::Rust),
            &facts,
        );
        assert_eq!(cap.compilation, SupportLevel::Experimental);
    }

    #[test]
    fn wpf_is_windows_only() {
        let h = host(HostPlatform::Linux, Architecture::X64);
        let target = Target::parse("linux-x64").unwrap();
        let facts = Facts {
            dotnet_sdk: Some("10.0".into()),
            ..Default::default()
        };
        let input = CapabilityInput {
            framework: FrameworkKind::Dotnet,
            dotnet_flavor: Some(DotnetFlavor::Wpf),
            native_aot: false,
            has_native_node_modules: false,
        };
        let cap = resolve(&h, &target, &input, &facts);
        assert_eq!(cap.compilation, SupportLevel::Unsupported);
    }

    #[test]
    fn unsupported_format_is_reported_per_format() {
        let h = host(HostPlatform::Windows, Architecture::X64);
        let target = Target::parse("windows-x64").unwrap();
        let facts = rust_facts();
        let (msi, _) = format_support(&PackagingFormat::Msi, &h, &target, &facts);
        assert_eq!(msi, SupportLevel::Unsupported);
        let (zip, _) = format_support(&PackagingFormat::PortableZip, &h, &target, &facts);
        assert_eq!(zip, SupportLevel::Supported);
    }
}
