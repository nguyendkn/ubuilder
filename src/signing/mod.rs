//! Code signing and notarization for release artifacts.
//!
//! Signing is performed only by real platform tools; a missing tool yields an
//! `Unsupported` report with a remediation, never a fabricated signature.

use crate::config::SigningMode;
use crate::error::{ErrorCode, Result, UbuilderError};
use crate::target::{Target, TargetOs};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SigningStatus {
    NotRun,
    Signed,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigningReport {
    pub path: PathBuf,
    pub mode: SigningMode,
    pub status: SigningStatus,
    pub tool: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct SigningOptions {
    pub identity: Option<String>,
    pub certificate: Option<PathBuf>,
    pub password_env: Option<String>,
    pub timestamp_url: Option<String>,
    pub notarize: bool,
}

pub fn default_timestamp_url() -> &'static str {
    "http://timestamp.digicert.com"
}

/// Sign one artifact. Returns `Unsupported` when the target cannot be signed on
/// this host or the mode needs a tool that is not installed.
pub fn sign(
    path: &Path,
    target: &Target,
    mode: SigningMode,
    options: &SigningOptions,
) -> Result<SigningReport> {
    let mut report = SigningReport {
        path: path.to_path_buf(),
        mode,
        status: SigningStatus::NotRun,
        tool: None,
        detail: String::new(),
    };
    if mode == SigningMode::None {
        report.status = SigningStatus::NotRun;
        report.detail = "signing disabled".into();
        return Ok(report);
    }
    match target.os {
        TargetOs::Macos => sign_macos(path, target, mode, options, &mut report)?,
        TargetOs::Windows => sign_windows(path, mode, options, &mut report)?,
        TargetOs::Linux => {
            report.status = SigningStatus::Unsupported;
            report.detail =
                "Linux signing is not implemented; provenance hashes are produced instead".into();
        }
    }
    Ok(report)
}

fn sign_macos(
    path: &Path,
    target: &Target,
    mode: SigningMode,
    options: &SigningOptions,
    report: &mut SigningReport,
) -> Result<()> {
    let codesign = match which::which("codesign") {
        Ok(path) => path,
        Err(_) => {
            report.status = SigningStatus::Unsupported;
            report.detail = "codesign is not installed (macOS only)".into();
            return Ok(());
        }
    };
    report.tool = Some(codesign.display().to_string());
    let mut args = vec![
        "--force".to_string(),
        "--deep".to_string(),
        "--sign".to_string(),
    ];
    match mode {
        SigningMode::Adhoc => args.push("-".into()),
        SigningMode::DeveloperId => {
            let identity = options.identity.as_ref().ok_or_else(|| {
                UbuilderError::new(
                    ErrorCode::SigningUnsupported,
                    "Apple Developer ID signing requires [release].identity",
                )
            })?;
            args.push(identity.clone());
            args.push("--options".into());
            args.push("runtime".into());
            if options.timestamp_url.is_some() || target.os == TargetOs::Macos {
                args.push("--timestamp".into());
            }
        }
        _ => {
            report.status = SigningStatus::Unsupported;
            report.detail = format!("{:?} is not a macOS signing mode", mode);
            return Ok(());
        }
    }
    args.push(path.display().to_string());
    run(&codesign, &args)?;
    report.status = SigningStatus::Signed;
    report.detail = format!("signed {:?}", mode);

    let verify = std::process::Command::new(&codesign)
        .args([
            "--verify",
            "--deep",
            "--strict",
            path.to_str().unwrap_or_default(),
        ])
        .output()
        .map_err(|e| UbuilderError::io("cannot run codesign --verify", e))?;
    if !verify.status.success() {
        return Err(UbuilderError::new(
            ErrorCode::NotVerified,
            format!(
                "codesign verification failed: {}",
                String::from_utf8_lossy(&verify.stderr).trim()
            ),
        ));
    }

    if options.notarize && mode == SigningMode::DeveloperId {
        notarize(path, report)?;
    }
    Ok(())
}

fn notarize(path: &Path, report: &mut SigningReport) -> Result<()> {
    let notarytool = which::which("xcrun").map_err(|_| {
        UbuilderError::new(
            ErrorCode::ToolchainMissing,
            "xcrun is required for notarization",
        )
    })?;
    let profile = std::env::var("UBUILDER_NOTARY_PROFILE").map_err(|_| {
        UbuilderError::new(
            ErrorCode::SigningUnsupported,
            "notarization requires UBUILDER_NOTARY_PROFILE (stored notarytool credentials)",
        )
        .remediation("Run: xcrun notarytool store-credentials <profile> --apple-id ...")
    })?;
    let archive = if path.is_dir() {
        let zip = path.with_extension("zip");
        let file = std::fs::File::create(&zip)
            .map_err(|e| UbuilderError::io("cannot create notarization zip", e))?;
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for entry in walkdir::WalkDir::new(path).into_iter().flatten() {
            if entry.file_type().is_file() {
                let rel = entry.path().strip_prefix(path).unwrap_or(entry.path());
                writer
                    .start_file(rel.to_string_lossy().replace('\\', "/"), options)
                    .map_err(|e| UbuilderError::new(ErrorCode::Io, e.to_string()))?;
                let mut input = std::fs::File::open(entry.path())
                    .map_err(|e| UbuilderError::io("cannot read bundle file", e))?;
                std::io::copy(&mut input, &mut writer)
                    .map_err(|e| UbuilderError::new(ErrorCode::Io, e.to_string()))?;
            }
        }
        writer
            .finish()
            .map_err(|e| UbuilderError::new(ErrorCode::Io, e.to_string()))?;
        zip
    } else {
        path.to_path_buf()
    };
    run(
        &notarytool,
        &[
            "notarytool".into(),
            "submit".into(),
            archive.display().to_string(),
            "--wait".into(),
            "--keychain-profile".into(),
            profile,
        ],
    )?;
    let _ = std::process::Command::new("xcrun")
        .args(["stapler", "staple", &archive.display().to_string()])
        .output();
    report.detail.push_str("; notarized");
    Ok(())
}

fn sign_windows(
    path: &Path,
    mode: SigningMode,
    options: &SigningOptions,
    report: &mut SigningReport,
) -> Result<()> {
    let signtool = which::which("signtool").map_err(|_| {
        UbuilderError::new(
            ErrorCode::ToolchainMissing,
            "signtool is not installed (Windows SDK)",
        )
    })?;
    report.tool = Some(signtool.display().to_string());
    let timestamp = options
        .timestamp_url
        .clone()
        .unwrap_or_else(|| default_timestamp_url().into());
    let mut args: Vec<String> = vec![
        "sign".into(),
        "/fd".into(),
        "SHA256".into(),
        "/tr".into(),
        timestamp,
        "/td".into(),
        "SHA256".into(),
    ];
    match mode {
        SigningMode::Authenticode => {
            if let Some(cert) = &options.certificate {
                args.push("/f".into());
                args.push(cert.display().to_string());
                if let Some(env_name) = &options.password_env {
                    if let Ok(password) = std::env::var(env_name) {
                        args.push("/p".into());
                        args.push(password);
                    }
                }
            } else if let Some(identity) = &options.identity {
                args.push("/sha1".into());
                args.push(identity.clone());
            } else {
                args.push("/a".into());
            }
        }
        SigningMode::Adhoc => {
            report.status = SigningStatus::Unsupported;
            report.detail = "adhoc signing is only meaningful on macOS".into();
            return Ok(());
        }
        _ => {
            report.status = SigningStatus::Unsupported;
            report.detail = format!("{:?} is not a Windows signing mode", mode);
            return Ok(());
        }
    }
    args.push(path.display().to_string());
    run(&signtool, &args)?;
    let verify = std::process::Command::new(&signtool)
        .args(["verify", "/pa", path.to_str().unwrap_or_default()])
        .output()
        .map_err(|e| UbuilderError::io("cannot run signtool verify", e))?;
    if !verify.status.success() {
        return Err(UbuilderError::new(
            ErrorCode::NotVerified,
            format!(
                "signtool verification failed: {}",
                String::from_utf8_lossy(&verify.stderr).trim()
            ),
        ));
    }
    report.status = SigningStatus::Signed;
    report.detail = "Authenticode signature verified".into();
    Ok(())
}

fn run(program: &Path, args: &[String]) -> Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| UbuilderError::io(format!("cannot run {}", program.display()), e))?;
    if !output.status.success() {
        return Err(UbuilderError::new(
            ErrorCode::ExecutionFailed,
            format!(
                "{} failed: {}",
                program.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_mode_is_not_run() {
        let target = Target::parse("windows-x64").unwrap();
        let report = sign(
            Path::new("missing.exe"),
            &target,
            SigningMode::None,
            &SigningOptions::default(),
        )
        .unwrap();
        assert_eq!(report.status, SigningStatus::NotRun);
    }

    #[test]
    fn linux_signing_is_unsupported() {
        let target = Target::parse("linux-x64").unwrap();
        let report = sign(
            Path::new("missing"),
            &target,
            SigningMode::Adhoc,
            &SigningOptions::default(),
        )
        .unwrap();
        assert_eq!(report.status, SigningStatus::Unsupported);
    }
}
