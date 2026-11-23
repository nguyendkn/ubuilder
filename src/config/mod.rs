//! `ubuilder.toml` configuration with explicit precedence:
//! CLI flags > environment > project config > user config > built-in defaults.

use crate::error::{ErrorCode, Result, UbuilderError};
use crate::host::Host;
use crate::project::FrameworkKind;
use crate::target::{PackagingFormat, Target};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const CONFIG_FILE: &str = "ubuilder.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseMode {
    Portable,
    Distribution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SigningMode {
    None,
    Adhoc,
    DeveloperId,
    Authenticode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    Restricted,
    Open,
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    pub project: ProjectSettings,
    pub build: BuildSettings,
    pub targets: TargetsSettings,
    pub toolchains: ToolchainSettings,
    pub execution: ExecutionSettings,
    pub packaging: PackagingSettings,
    pub security: SecuritySettings,
    pub release: ReleaseSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub name: Option<String>,
    pub version: Option<String>,
    pub framework: Option<FrameworkKind>,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildSettings {
    pub profile: String,
    pub parallelism: usize,
    pub cache: bool,
    pub timeout_secs: Option<u64>,
    pub continue_on_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsSettings {
    /// Canonical target ids. Empty means "native host target".
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolchainSettings {
    pub auto_detect: bool,
    pub auto_install: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSettings {
    pub prefer_native: bool,
    pub allow_wsl: bool,
    pub allow_container: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackagingSettings {
    pub formats: Vec<PackagingFormat>,
    pub output: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecuritySettings {
    pub network: NetworkMode,
    pub env_allowlist: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseSettings {
    pub mode: ReleaseMode,
    pub allow_unsigned: bool,
    pub require_native_tests: bool,
    pub signing: SigningMode,
    pub identity: Option<String>,
    pub certificate: Option<PathBuf>,
    pub password_env: Option<String>,
    pub timestamp_url: Option<String>,
    pub notarize: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            project: ProjectSettings {
                name: None,
                version: None,
                framework: None,
                path: None,
            },
            build: BuildSettings {
                profile: "release".into(),
                parallelism: default_parallelism(),
                cache: true,
                timeout_secs: Some(1800),
                continue_on_error: false,
            },
            targets: TargetsSettings { enabled: vec![] },
            toolchains: ToolchainSettings {
                auto_detect: true,
                auto_install: false,
            },
            execution: ExecutionSettings {
                prefer_native: true,
                allow_wsl: true,
                allow_container: true,
            },
            packaging: PackagingSettings {
                formats: Vec::new(),
                output: PathBuf::from("dist"),
            },
            security: SecuritySettings {
                network: NetworkMode::Restricted,
                env_allowlist: default_env_allowlist(),
            },
            release: ReleaseSettings {
                mode: ReleaseMode::Portable,
                allow_unsigned: true,
                require_native_tests: false,
                signing: SigningMode::None,
                identity: None,
                certificate: None,
                password_env: None,
                timestamp_url: None,
                notarize: false,
            },
        }
    }
}

fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 8))
        .unwrap_or(2)
}

pub fn default_env_allowlist() -> Vec<String> {
    [
        "PATH",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "SystemRoot",
        "SystemDrive",
        "TEMP",
        "TMP",
        "CARGO_HOME",
        "CARGO_TARGET_DIR",
        "CARGO_NET_OFFLINE",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "DOTNET_ROOT",
        "DOTNET_CLI_HOME",
        "NUGET_PACKAGES",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
        "CommonProgramFiles",
        "PROCESSOR_ARCHITECTURE",
        "NUMBER_OF_PROCESSORS",
        "HOMEDRIVE",
        "HOMEPATH",
        "USERNAME",
        "COMPUTERNAME",
        "NODE_PATH",
        "npm_config_cache",
        "npm_config_registry",
        "LANG",
        "LC_ALL",
        "TERM",
        "NO_COLOR",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Programmatic overrides collected from CLI flags.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub profile: Option<String>,
    pub parallelism: Option<usize>,
    pub cache: Option<bool>,
    pub output: Option<PathBuf>,
    pub targets: Option<Vec<String>>,
    pub framework: Option<String>,
    pub offline: bool,
    pub continue_on_error: bool,
    pub config_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub schema_version: Option<u32>,
    pub project: Option<ProjectLayer>,
    pub build: Option<BuildLayer>,
    pub targets: Option<TargetsLayer>,
    pub toolchains: Option<ToolchainLayer>,
    pub execution: Option<ExecutionLayer>,
    pub packaging: Option<PackagingLayer>,
    pub security: Option<SecurityLayer>,
    pub release: Option<ReleaseLayer>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLayer {
    pub name: Option<String>,
    pub version: Option<String>,
    pub framework: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildLayer {
    pub profile: Option<String>,
    pub parallelism: Option<usize>,
    pub cache: Option<bool>,
    pub timeout_secs: Option<u64>,
    pub continue_on_error: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetsLayer {
    pub enabled: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainLayer {
    pub auto_detect: Option<bool>,
    pub auto_install: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionLayer {
    pub prefer_native: Option<bool>,
    pub allow_wsl: Option<bool>,
    pub allow_container: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackagingLayer {
    pub formats: Option<Vec<String>>,
    pub output: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityLayer {
    pub network: Option<String>,
    pub env_allowlist: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLayer {
    pub mode: Option<String>,
    pub allow_unsigned: Option<bool>,
    pub require_native_tests: Option<bool>,
    pub signing: Option<String>,
    pub identity: Option<String>,
    pub certificate: Option<String>,
    pub password_env: Option<String>,
    pub timestamp_url: Option<String>,
    pub notarize: Option<bool>,
}

impl Config {
    pub fn load(root: &Path, host: &Host, overrides: &Overrides) -> Result<Self> {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        Self::load_with_env(root, host, overrides, &env)
    }

    pub fn load_with_env(
        root: &Path,
        host: &Host,
        overrides: &Overrides,
        env: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let mut config = Config::default();

        let user_path = overrides
            .config_file
            .clone()
            .or_else(|| std::env::var_os("UBUILDER_CONFIG").map(PathBuf::from))
            .unwrap_or_else(|| host.data_dir().join("config.toml"));
        if user_path.is_file() {
            let layer = parse_file(&user_path)?;
            config.apply(&layer, &user_path)?;
        }

        let project_path = root.join(CONFIG_FILE);
        if project_path.is_file() {
            let layer = parse_file(&project_path)?;
            if let Some(version) = layer.schema_version {
                if version != SCHEMA_VERSION {
                    return Err(UbuilderError::new(
                        ErrorCode::ConfigInvalid,
                        format!(
                            "unsupported ubuilder.toml schema_version {version} (expected {SCHEMA_VERSION})"
                        ),
                    )
                    .remediation("Set schema_version = 1"));
                }
            }
            config.apply(&layer, &project_path)?;
        }

        let env_layer = env_layer(env);
        config.apply(&env_layer, Path::new("<environment>"))?;

        let cli_layer = cli_layer(overrides);
        config.apply(&cli_layer, Path::new("<command line>"))?;

        config.validate()?;
        Ok(config)
    }

    fn apply(&mut self, layer: &Layer, origin: &Path) -> Result<()> {
        if let Some(v) = layer.schema_version {
            self.schema_version = v;
        }
        if let Some(p) = &layer.project {
            if let Some(v) = &p.name {
                self.project.name = Some(v.clone());
            }
            if let Some(v) = &p.version {
                self.project.version = Some(v.clone());
            }
            if let Some(v) = &p.framework {
                self.project.framework = Some(
                    FrameworkKind::parse(v)
                        .ok_or_else(|| invalid(origin, format!("unknown framework '{v}'")))?,
                );
            }
            if let Some(v) = &p.path {
                self.project.path = Some(PathBuf::from(v));
            }
        }
        if let Some(b) = &layer.build {
            if let Some(v) = &b.profile {
                self.build.profile = v.clone();
            }
            if let Some(v) = b.parallelism {
                self.build.parallelism = v;
            }
            if let Some(v) = b.cache {
                self.build.cache = v;
            }
            if let Some(v) = b.timeout_secs {
                self.build.timeout_secs = Some(v);
            }
            if let Some(v) = b.continue_on_error {
                self.build.continue_on_error = v;
            }
        }
        if let Some(t) = &layer.targets {
            if let Some(v) = &t.enabled {
                self.targets.enabled = v.clone();
            }
        }
        if let Some(t) = &layer.toolchains {
            if let Some(v) = t.auto_detect {
                self.toolchains.auto_detect = v;
            }
            if let Some(v) = t.auto_install {
                self.toolchains.auto_install = v;
            }
        }
        if let Some(e) = &layer.execution {
            if let Some(v) = e.prefer_native {
                self.execution.prefer_native = v;
            }
            if let Some(v) = e.allow_wsl {
                self.execution.allow_wsl = v;
            }
            if let Some(v) = e.allow_container {
                self.execution.allow_container = v;
            }
        }
        if let Some(p) = &layer.packaging {
            if let Some(v) = &p.formats {
                let mut formats = Vec::new();
                for name in v {
                    formats.push(PackagingFormat::parse(name).ok_or_else(|| {
                        invalid(origin, format!("unknown packaging format '{name}'"))
                    })?);
                }
                self.packaging.formats = formats;
            }
            if let Some(v) = &p.output {
                self.packaging.output = PathBuf::from(v);
            }
        }
        if let Some(s) = &layer.security {
            if let Some(v) = &s.network {
                self.security.network = parse_network(v)
                    .ok_or_else(|| invalid(origin, format!("unknown network mode '{v}'")))?;
            }
            if let Some(v) = &s.env_allowlist {
                self.security.env_allowlist = v.clone();
            }
        }
        if let Some(r) = &layer.release {
            if let Some(v) = &r.mode {
                self.release.mode = match v.as_str() {
                    "portable" => ReleaseMode::Portable,
                    "distribution" => ReleaseMode::Distribution,
                    other => {
                        return Err(invalid(origin, format!("unknown release mode '{other}'")))
                    }
                };
            }
            if let Some(v) = r.allow_unsigned {
                self.release.allow_unsigned = v;
            }
            if let Some(v) = r.require_native_tests {
                self.release.require_native_tests = v;
            }
            if let Some(v) = &r.signing {
                self.release.signing = parse_signing(v)
                    .ok_or_else(|| invalid(origin, format!("unknown signing mode '{v}'")))?;
            }
            if let Some(v) = &r.identity {
                self.release.identity = Some(v.clone());
            }
            if let Some(v) = &r.certificate {
                self.release.certificate = Some(PathBuf::from(v));
            }
            if let Some(v) = &r.password_env {
                self.release.password_env = Some(v.clone());
            }
            if let Some(v) = &r.timestamp_url {
                self.release.timestamp_url = Some(v.clone());
            }
            if let Some(v) = r.notarize {
                self.release.notarize = v;
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "unsupported schema_version {} (expected {SCHEMA_VERSION})",
                    self.schema_version
                ),
            ));
        }
        if !matches!(self.build.profile.as_str(), "debug" | "dev" | "release") {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "unknown build profile '{}'; expected debug or release",
                    self.build.profile
                ),
            ));
        }
        if self.build.parallelism == 0 || self.build.parallelism > 128 {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!(
                    "parallelism must be between 1 and 128 (got {})",
                    self.build.parallelism
                ),
            ));
        }
        for id in &self.targets.enabled {
            Target::parse(id)?;
        }
        Ok(())
    }

    pub fn profile_is_release(&self) -> bool {
        self.build.profile == "release"
    }

    pub fn template(project: &str, framework: FrameworkKind, targets: &[String]) -> String {
        let targets = if targets.is_empty() {
            "[]".to_string()
        } else {
            let list = targets
                .iter()
                .map(|t| format!("  \"{t}\""))
                .collect::<Vec<_>>()
                .join(",\n");
            format!("[\n{list}\n]")
        };
        format!(
            r#"schema_version = 1

[project]
name = "{project}"
framework = "{framework}"

[build]
profile = "release"
parallelism = {parallelism}
cache = true

[targets]
enabled = {targets}

[toolchains]
auto_detect = true
auto_install = false

[execution]
prefer_native = true
allow_wsl = true
allow_container = true

[packaging]
# Empty selects the native format for each target (zip/tar.gz/app).
formats = []

[security]
network = "restricted"

[release]
mode = "portable"
allow_unsigned = true
require_native_tests = false
"#,
            framework = framework.as_str(),
            parallelism = default_parallelism(),
            targets = targets,
        )
    }
}

fn parse_file(path: &Path) -> Result<Layer> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| UbuilderError::io(format!("cannot read {}", path.display()), e))?;
    toml::from_str(&text).map_err(|e| {
        UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!("invalid config {}: {e}", path.display()),
        )
        .remediation("Compare with the documented ubuilder.toml schema")
    })
}

fn invalid(origin: &Path, message: String) -> UbuilderError {
    UbuilderError::new(
        ErrorCode::ConfigInvalid,
        format!("{message} (in {})", origin.display()),
    )
}

fn parse_network(value: &str) -> Option<NetworkMode> {
    Some(match value.to_ascii_lowercase().as_str() {
        "restricted" => NetworkMode::Restricted,
        "open" => NetworkMode::Open,
        "offline" => NetworkMode::Offline,
        _ => return None,
    })
}

fn parse_signing(value: &str) -> Option<SigningMode> {
    Some(match value.to_ascii_lowercase().as_str() {
        "none" => SigningMode::None,
        "adhoc" | "ad-hoc" => SigningMode::Adhoc,
        "developer-id" | "developerid" => SigningMode::DeveloperId,
        "authenticode" => SigningMode::Authenticode,
        _ => return None,
    })
}

fn env_layer(env: &BTreeMap<String, String>) -> Layer {
    let mut layer = Layer::default();
    let get = |key: &str| env.get(key).cloned().filter(|v| !v.is_empty());
    if let Some(v) = get("UBUILDER_PROFILE") {
        layer.build.get_or_insert_with(Default::default).profile = Some(v);
    }
    if let Some(v) = get("UBUILDER_JOBS").and_then(|v| v.parse::<usize>().ok()) {
        layer.build.get_or_insert_with(Default::default).parallelism = Some(v);
    }
    if let Some(v) = get("UBUILDER_CACHE").and_then(|v| parse_bool(&v)) {
        layer.build.get_or_insert_with(Default::default).cache = Some(v);
    }
    if let Some(v) = get("UBUILDER_OUTPUT") {
        layer.packaging.get_or_insert_with(Default::default).output = Some(v);
    }
    if let Some(v) = get("UBUILDER_TARGETS") {
        let list = v
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        layer.targets.get_or_insert_with(Default::default).enabled = Some(list);
    }
    if let Some(v) = get("UBUILDER_FRAMEWORK") {
        layer.project.get_or_insert_with(Default::default).framework = Some(v);
    }
    if let Some(v) = get("UBUILDER_SIGNING") {
        layer.release.get_or_insert_with(Default::default).signing = Some(v);
    }
    if let Some(v) = get("UBUILDER_ALLOW_UNSIGNED").and_then(|v| parse_bool(&v)) {
        layer
            .release
            .get_or_insert_with(Default::default)
            .allow_unsigned = Some(v);
    }
    if get("UBUILDER_OFFLINE")
        .and_then(|v| parse_bool(&v))
        .unwrap_or(false)
    {
        layer.security.get_or_insert_with(Default::default).network = Some("offline".into());
    }
    layer
}

fn cli_layer(overrides: &Overrides) -> Layer {
    let mut layer = Layer::default();
    if let Some(v) = &overrides.profile {
        layer.build.get_or_insert_with(Default::default).profile = Some(v.clone());
    }
    if let Some(v) = overrides.parallelism {
        layer.build.get_or_insert_with(Default::default).parallelism = Some(v);
    }
    if let Some(v) = overrides.cache {
        layer.build.get_or_insert_with(Default::default).cache = Some(v);
    }
    if overrides.continue_on_error {
        layer
            .build
            .get_or_insert_with(Default::default)
            .continue_on_error = Some(true);
    }
    if let Some(v) = &overrides.output {
        layer.packaging.get_or_insert_with(Default::default).output =
            Some(v.to_string_lossy().to_string());
    }
    if let Some(v) = &overrides.targets {
        layer.targets.get_or_insert_with(Default::default).enabled = Some(v.clone());
    }
    if let Some(v) = &overrides.framework {
        layer.project.get_or_insert_with(Default::default).framework = Some(v.clone());
    }
    if overrides.offline {
        layer.security.get_or_insert_with(Default::default).network = Some("offline".into());
    }
    layer
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{Architecture, HostPlatform};

    fn host() -> Host {
        Host {
            platform: HostPlatform::Windows,
            arch: Architecture::X64,
            triple: "x86_64-pc-windows-msvc".into(),
            os_version: None,
        }
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn cli_beats_env_and_project() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(CONFIG_FILE),
            "[build]\nprofile = \"debug\"\nparallelism = 2\n",
        )
        .unwrap();
        let overrides = Overrides {
            parallelism: Some(6),
            ..Overrides::default()
        };
        let env = env(&[("UBUILDER_JOBS", "4"), ("UBUILDER_PROFILE", "release")]);
        let cfg = Config::load_with_env(tmp.path(), &host(), &overrides, &env).unwrap();
        assert_eq!(cfg.build.parallelism, 6);
        assert_eq!(cfg.build.profile, "release");
    }

    #[test]
    fn project_beats_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(CONFIG_FILE),
            "[packaging]\nformats = [\"tar.gz\"]\noutput = \"out\"\n",
        )
        .unwrap();
        let cfg =
            Config::load_with_env(tmp.path(), &host(), &Overrides::default(), &env(&[])).unwrap();
        assert_eq!(cfg.packaging.formats, vec![PackagingFormat::TarGz]);
        assert_eq!(cfg.packaging.output, PathBuf::from("out"));
    }

    #[test]
    fn rejects_unknown_keys_and_schema() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(CONFIG_FILE), "[build]\nbogus = 1\n").unwrap();
        let err = Config::load_with_env(tmp.path(), &host(), &Overrides::default(), &env(&[]))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);

        std::fs::write(tmp.path().join(CONFIG_FILE), "schema_version = 9\n").unwrap();
        let err = Config::load_with_env(tmp.path(), &host(), &Overrides::default(), &env(&[]))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
    }

    #[test]
    fn env_targets_are_split() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env(&[("UBUILDER_TARGETS", "windows-x64, linux-x64")]);
        let cfg = Config::load_with_env(tmp.path(), &host(), &Overrides::default(), &env).unwrap();
        assert_eq!(cfg.targets.enabled, vec!["windows-x64", "linux-x64"]);
    }
}
