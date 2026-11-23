//! Execution trust modes and isolation providers.
//!
//! `trusted-native` runs commands with the invoking user's privileges. It is
//! the default for backwards compatibility and is explicitly *not* a sandbox.
//! `isolated` must be enforced by a provider whose capabilities cover the
//! requested policy; if no provider can enforce it, planning fails closed with
//! `SECURITY_CAPABILITY_UNAVAILABLE` before any job starts.

use crate::error::{ErrorCode, Result, UbuilderError};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TrustMode {
    #[default]
    TrustedNative,
    Isolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkPolicy {
    Host,
    #[default]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceAccess {
    ReadOnly,
    #[default]
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HostAccess {
    #[default]
    Denied,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FilesystemPolicy {
    #[serde(default)]
    pub workspace: WorkspaceAccess,
    #[serde(default)]
    pub host: HostAccess,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicy {
    #[serde(default)]
    pub cpu: Option<f64>,
    /// Human-readable limit such as `512MiB` or `4GiB`.
    #[serde(default, deserialize_with = "deserialize_optional_scalar")]
    pub memory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolationPolicy {
    #[serde(default)]
    pub mode: TrustMode,
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default)]
    pub filesystem: FilesystemPolicy,
    #[serde(default)]
    pub resources: ResourcePolicy,
}

impl Default for IsolationPolicy {
    fn default() -> Self {
        Self {
            mode: TrustMode::TrustedNative,
            network: NetworkPolicy::None,
            filesystem: FilesystemPolicy::default(),
            resources: ResourcePolicy::default(),
        }
    }
}

impl IsolationPolicy {
    pub fn validate(&self) -> Result<()> {
        if let Some(cpu) = self.resources.cpu {
            if !(cpu.is_finite() && cpu > 0.0) {
                return Err(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    "security.resources.cpu must be greater than zero",
                ));
            }
        }
        if let Some(memory) = &self.resources.memory {
            parse_memory_mib(memory)?;
        }
        Ok(())
    }

    pub fn memory_mib(&self) -> Option<u64> {
        self.resources
            .memory
            .as_deref()
            .and_then(|memory| parse_memory_mib(memory).ok())
    }

    /// A compact, serializable description used in error details.
    pub fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "mode": self.mode,
            "network": self.network,
            "filesystem": self.filesystem,
            "resources": {
                "cpu": self.resources.cpu,
                "memory": self.resources.memory,
            },
        })
    }
}

fn deserialize_optional_scalar<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Scalar {
        Text(String),
        Number(u64),
    }
    Ok(
        Option::<Scalar>::deserialize(deserializer)?.map(|value| match value {
            Scalar::Text(text) => text,
            Scalar::Number(number) => format!("{number}MiB"),
        }),
    )
}

pub fn parse_memory_mib(value: &str) -> Result<u64> {
    let text = value.trim();
    let split = text
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let amount: f64 = number.parse().map_err(|_| {
        UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("invalid memory limit '{value}'"),
        )
    })?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "mib" | "mb" => 1.0,
        "gib" | "gb" => 1024.0,
        "kib" | "kb" => 1.0 / 1024.0,
        other => {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("unknown memory unit '{other}' (use KiB, MiB or GiB)"),
            ))
        }
    };
    let mib = (amount * multiplier).round();
    if !mib.is_finite() || mib < 1.0 {
        return Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("memory limit '{value}' is too small"),
        ));
    }
    Ok(mib as u64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationCapabilities {
    pub process_tree: bool,
    pub filesystem: bool,
    pub network_none: bool,
    pub workspace_read_only: bool,
    pub cpu_limit: bool,
    pub memory_limit: bool,
}

impl IsolationCapabilities {
    pub fn native() -> Self {
        Self {
            process_tree: true,
            filesystem: false,
            network_none: false,
            workspace_read_only: false,
            cpu_limit: false,
            memory_limit: false,
        }
    }

    pub fn bubblewrap() -> Self {
        Self {
            process_tree: true,
            filesystem: true,
            network_none: true,
            workspace_read_only: true,
            // Bubblewrap creates namespaces but does not enforce cgroup CPU or
            // memory quotas by itself.
            cpu_limit: false,
            memory_limit: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IsolationContext {
    pub workspace: PathBuf,
}

#[derive(Debug, Clone)]
pub struct IsolationSession {
    pub provider: &'static str,
    pub workspace: PathBuf,
    pub prefix: Vec<String>,
    pub env: BTreeMap<String, String>,
}

impl IsolationSession {
    pub fn wrap(&self, program: &str, args: &[String]) -> (String, Vec<String>) {
        if self.prefix.is_empty() {
            return (program.to_string(), args.to_vec());
        }
        let mut wrapped = Vec::with_capacity(self.prefix.len() + args.len() + 2);
        wrapped.extend(self.prefix.iter().cloned());
        wrapped.push(program.to_string());
        wrapped.extend(args.iter().cloned());
        (self.prefix[0].clone(), wrapped[1..].to_vec())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationError {
    pub message: String,
    pub remediation: Option<String>,
    pub details: Option<serde_json::Value>,
}

impl IsolationError {
    pub fn unavailable(message: impl Into<String>, policy: &IsolationPolicy) -> Self {
        Self {
            message: message.into(),
            remediation: Some(
                "Use `security.mode: trusted-native` for trusted sources, or install a verified isolation backend"
                    .into(),
            ),
            details: Some(serde_json::json!({
                "platform": std::env::consts::OS,
                "architecture": std::env::consts::ARCH,
                "policy": policy.describe(),
            })),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            remediation: Some(
                "Reduce the requested policy or select a backend that enforces it".into(),
            ),
            details: None,
        }
    }
}

impl From<IsolationError> for UbuilderError {
    fn from(error: IsolationError) -> Self {
        let IsolationError {
            message,
            remediation,
            details,
        } = error;
        let mut converted = UbuilderError::new(ErrorCode::SecurityCapabilityUnavailable, message);
        if let Some(remediation) = remediation {
            converted = converted.remediation(remediation);
        }
        if let Some(details) = details {
            converted = converted.details(details);
        }
        converted
    }
}

pub trait IsolationProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> IsolationCapabilities;
    fn validate(&self, policy: &IsolationPolicy) -> std::result::Result<(), IsolationError>;
    fn prepare(
        &self,
        context: &IsolationContext,
    ) -> std::result::Result<IsolationSession, IsolationError>;
}

pub struct NativeProvider;

impl IsolationProvider for NativeProvider {
    fn id(&self) -> &'static str {
        "trusted-native"
    }

    fn capabilities(&self) -> IsolationCapabilities {
        IsolationCapabilities::native()
    }

    fn validate(&self, policy: &IsolationPolicy) -> std::result::Result<(), IsolationError> {
        if policy.mode == TrustMode::Isolated {
            return Err(IsolationError::unavailable(
                "native execution cannot enforce `security.mode: isolated`",
                policy,
            ));
        }
        Ok(())
    }

    fn prepare(
        &self,
        context: &IsolationContext,
    ) -> std::result::Result<IsolationSession, IsolationError> {
        Ok(IsolationSession {
            provider: self.id(),
            workspace: context.workspace.clone(),
            prefix: Vec::new(),
            env: BTreeMap::new(),
        })
    }
}

#[cfg(target_os = "linux")]
pub struct BubblewrapProvider {
    binary: PathBuf,
}

#[cfg(target_os = "linux")]
impl BubblewrapProvider {
    fn detect() -> Option<Self> {
        which::which("bwrap").ok().map(|binary| Self { binary })
    }
}

#[cfg(target_os = "linux")]
impl IsolationProvider for BubblewrapProvider {
    fn id(&self) -> &'static str {
        "bubblewrap"
    }

    fn capabilities(&self) -> IsolationCapabilities {
        IsolationCapabilities::bubblewrap()
    }

    fn validate(&self, policy: &IsolationPolicy) -> std::result::Result<(), IsolationError> {
        let capabilities = self.capabilities();
        if policy.mode != TrustMode::Isolated {
            return Ok(());
        }
        if policy.network == NetworkPolicy::None && !capabilities.network_none {
            return Err(IsolationError::unsupported(
                "bubblewrap cannot disable networking",
            ));
        }
        if policy.filesystem.workspace == WorkspaceAccess::ReadOnly
            && !capabilities.workspace_read_only
        {
            return Err(IsolationError::unsupported(
                "bubblewrap cannot make the workspace read-only",
            ));
        }
        if policy.resources.cpu.is_some() || policy.resources.memory.is_some() {
            return Err(IsolationError::unsupported(
                "bubblewrap alone cannot enforce CPU or memory cgroup limits",
            ));
        }
        Ok(())
    }

    fn prepare(
        &self,
        context: &IsolationContext,
    ) -> std::result::Result<IsolationSession, IsolationError> {
        let workspace = context
            .workspace
            .canonicalize()
            .map_err(|error| IsolationError {
                message: format!(
                    "cannot canonicalize workspace {} for isolation: {error}",
                    context.workspace.display()
                ),
                remediation: None,
                details: None,
            })?;
        let mut prefix = vec![
            self.binary.to_string_lossy().into_owned(),
            "--unshare-all".into(),
            "--die-with-parent".into(),
            "--new-session".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--tmpfs".into(),
            "/tmp".into(),
        ];
        for directory in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
            if std::path::Path::new(directory).exists() {
                prefix.extend(["--ro-bind".into(), directory.into(), directory.into()]);
            }
        }
        for variable in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Ok(value) = std::env::var(variable) {
                let path = PathBuf::from(&value);
                if path.is_absolute() && path.exists() {
                    prefix.extend([
                        "--ro-bind".into(),
                        path.to_string_lossy().into_owned(),
                        path.to_string_lossy().into_owned(),
                    ]);
                }
            }
        }
        let flag = "--bind";
        prefix.extend([
            flag.into(),
            workspace.to_string_lossy().into_owned(),
            workspace.to_string_lossy().into_owned(),
            "--chdir".into(),
            workspace.to_string_lossy().into_owned(),
        ]);
        Ok(IsolationSession {
            provider: self.id(),
            workspace,
            prefix,
            env: BTreeMap::new(),
        })
    }
}

/// Select the strongest available provider for the requested policy.
///
/// Isolation never falls back to native execution: an unavailable backend is a
/// hard error so an operator cannot believe a job was sandboxed when it was not.
pub fn select(
    policy: &IsolationPolicy,
) -> std::result::Result<Box<dyn IsolationProvider>, IsolationError> {
    policy.validate().map_err(|error| IsolationError {
        message: error.message,
        remediation: error.remediation,
        details: error.details,
    })?;
    if policy.mode == TrustMode::TrustedNative {
        return Ok(Box::new(NativeProvider));
    }
    #[cfg(target_os = "linux")]
    if let Some(provider) = BubblewrapProvider::detect() {
        provider.validate(policy)?;
        return Ok(Box::new(provider));
    }
    Err(IsolationError::unavailable(
        "no verified isolation backend is available on this host",
        policy,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_is_valid_only_for_trusted_mode() {
        let provider = NativeProvider;
        assert!(provider.validate(&IsolationPolicy::default()).is_ok());
        let isolated = IsolationPolicy {
            mode: TrustMode::Isolated,
            ..IsolationPolicy::default()
        };
        assert!(provider.validate(&isolated).is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn isolated_mode_fails_closed_without_a_provider() {
        if which::which("bwrap").is_ok() {
            return;
        }
        let policy = IsolationPolicy {
            mode: TrustMode::Isolated,
            ..IsolationPolicy::default()
        };
        let error = select(&policy).err().expect("isolated mode must fail");
        assert!(error.message.contains("isolation backend"));
    }

    #[test]
    fn parses_memory_units() {
        assert_eq!(parse_memory_mib("4GiB").unwrap(), 4096);
        assert_eq!(parse_memory_mib("512MiB").unwrap(), 512);
        assert_eq!(parse_memory_mib("4096").unwrap(), 4096);
        assert!(parse_memory_mib("lots").is_err());
    }

    #[test]
    fn yaml_policy_rejects_unknown_fields() {
        let policy: IsolationPolicy = serde_yaml_ng::from_str(
            "mode: isolated\nnetwork: none\nfilesystem:\n  workspace: read-only\n  host: denied\nresources:\n  cpu: 4\n  memory: 4GiB\n",
        )
        .unwrap();
        assert_eq!(policy.mode, TrustMode::Isolated);
        assert_eq!(policy.filesystem.workspace, WorkspaceAccess::ReadOnly);
        assert_eq!(policy.memory_mib(), Some(4096));
        assert!(
            serde_yaml_ng::from_str::<IsolationPolicy>("mode: isolated\nbogus: true\n").is_err()
        );
    }
}
