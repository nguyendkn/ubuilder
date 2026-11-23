//! Host (build machine) identification.

use crate::error::{Result, UbuilderError};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostPlatform {
    Windows,
    Macos,
    Linux,
}

impl HostPlatform {
    pub fn as_str(self) -> &'static str {
        match self {
            HostPlatform::Windows => "windows",
            HostPlatform::Macos => "macos",
            HostPlatform::Linux => "linux",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            HostPlatform::Windows => "Windows",
            HostPlatform::Macos => "macOS",
            HostPlatform::Linux => "Linux",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Architecture {
    X64,
    Arm64,
}

impl Architecture {
    pub fn as_str(self) -> &'static str {
        match self {
            Architecture::X64 => "x64",
            Architecture::Arm64 => "arm64",
        }
    }
}

/// Alias kept for the documented domain model.
pub type HostArchitecture = Architecture;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub platform: HostPlatform,
    pub arch: Architecture,
    pub triple: String,
    pub os_version: Option<String>,
}

impl Host {
    pub fn detect() -> Result<Self> {
        let platform = if cfg!(target_os = "windows") {
            HostPlatform::Windows
        } else if cfg!(target_os = "macos") {
            HostPlatform::Macos
        } else if cfg!(target_os = "linux") {
            HostPlatform::Linux
        } else {
            return Err(UbuilderError::new(
                crate::error::ErrorCode::UnsupportedTarget,
                format!("unsupported host OS: {}", std::env::consts::OS),
            ));
        };
        let arch = match std::env::consts::ARCH {
            "x86_64" => Architecture::X64,
            "aarch64" => Architecture::Arm64,
            other => {
                return Err(UbuilderError::new(
                    crate::error::ErrorCode::UnsupportedTarget,
                    format!("unsupported host architecture: {other}"),
                ))
            }
        };
        Ok(Self {
            platform,
            arch,
            triple: host_triple(platform, arch).to_string(),
            os_version: None,
        })
    }

    /// Best-effort OS version; only called by diagnostic commands.
    pub fn with_version(mut self) -> Self {
        self.os_version = detect_os_version(self.platform);
        self
    }

    pub fn display(&self) -> String {
        format!("{} {}", self.platform.display_name(), self.arch.as_str())
    }

    pub fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
    }

    pub fn data_dir(&self) -> PathBuf {
        if let Some(dir) = std::env::var_os("UBUILDER_DATA_DIR") {
            return PathBuf::from(dir);
        }
        #[cfg(target_os = "windows")]
        {
            if let Some(appdata) = std::env::var_os("LOCALAPPDATA") {
                return PathBuf::from(appdata).join("ubuilder");
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(home) = self.home_dir() {
                return home.join("Library/Application Support/ubuilder");
            }
        }
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("ubuilder");
        }
        self.home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local/share/ubuilder")
    }

    pub fn cache_dir(&self) -> PathBuf {
        if let Some(dir) = std::env::var_os("UBUILDER_CACHE_DIR") {
            return PathBuf::from(dir);
        }
        self.data_dir().join("cache")
    }
}

fn host_triple(platform: HostPlatform, arch: Architecture) -> &'static str {
    match (platform, arch) {
        (HostPlatform::Windows, Architecture::X64) => "x86_64-pc-windows-msvc",
        (HostPlatform::Windows, Architecture::Arm64) => "aarch64-pc-windows-msvc",
        (HostPlatform::Macos, Architecture::X64) => "x86_64-apple-darwin",
        (HostPlatform::Macos, Architecture::Arm64) => "aarch64-apple-darwin",
        (HostPlatform::Linux, Architecture::X64) => "x86_64-unknown-linux-gnu",
        (HostPlatform::Linux, Architecture::Arm64) => "aarch64-unknown-linux-gnu",
    }
}

fn detect_os_version(platform: HostPlatform) -> Option<String> {
    match platform {
        HostPlatform::Macos => command_output("sw_vers", &["-productVersion"]),
        HostPlatform::Linux => std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|l| l.starts_with("PRETTY_NAME="))
                    .map(|l| {
                        l.trim_start_matches("PRETTY_NAME=")
                            .trim_matches('"')
                            .to_string()
                    })
            }),
        HostPlatform::Windows => {
            // `cmd /c ver` is locale-independent enough for a diagnostic string.
            let out = Command::new("cmd").args(["/c", "ver"]).output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}
